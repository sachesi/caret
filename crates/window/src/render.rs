//! The window's render thread: it draws each frame the window hands it and waits for the
//! compositor to want the next on a Wayland queue of its own, so that the window's thread
//! goes on taking input while a frame waits for the GPU or the compositor.

use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::sync::mpsc::{self, TryRecvError};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use rustix::event::{PollFd, PollFlags};
use smithay_client_toolkit::reexports::calloop::channel::Sender;
use smithay_client_toolkit::reexports::client::protocol::wl_callback::{self, WlCallback};
use smithay_client_toolkit::reexports::client::protocol::wl_surface::WlSurface;
use smithay_client_toolkit::reexports::client::{Connection, Dispatch, EventQueue, QueueHandle};
use smithay_client_toolkit::reexports::protocols::wp::viewporter::client::wp_viewport::WpViewport;

use crate::gpu::Gpu;
use crate::renderer::Job;

/// How a frame goes on the surface.
pub struct Target {
    /// The frame's size in device pixels.
    pub width: u32,
    pub height: u32,
    /// It may go to the top left of larger images, while the window is resized.
    pub roomy: bool,
    pub scaling: Scaling,
    /// How long the window took to make the frame, for the timings.
    pub built: Duration,
}

/// How device pixels map to the window's logical ones.
pub enum Scaling {
    /// A fractional scale: the viewport shows the frame at this logical size.
    Viewport {
        viewport: WpViewport,
        width: i32,
        height: i32,
    },
    /// A whole one.
    Buffer(i32),
}

/// What the render thread tells the window.
pub enum Rendered {
    /// The compositor wants the next frame.
    Ready,
    /// Drawing failed, and nothing more will be.
    Failed(String),
}

type Frame = (Job, Target);

pub struct RenderThread {
    jobs: Option<mpsc::Sender<Frame>>,
    /// Wakes the thread while it waits for the compositor: a frame came, or the window
    /// is closing.
    wake: UnixStream,
    thread: Option<JoinHandle<()>>,
}

impl RenderThread {
    /// Starts drawing with `gpu` to `surface`, telling `rendered` when the next frame is
    /// due.
    ///
    /// # Errors
    ///
    /// When the thread or its wake-up socket cannot be made.
    pub fn spawn(
        gpu: Gpu,
        connection: Connection,
        surface: WlSurface,
        rendered: Sender<Rendered>,
    ) -> io::Result<Self> {
        let (jobs, taken) = mpsc::channel();
        let (wake, woken) = UnixStream::pair()?;
        // A full socket already holds a wake-up.
        wake.set_nonblocking(true)?;
        woken.set_nonblocking(true)?;
        let thread = std::thread::Builder::new()
            .name("render".to_owned())
            .spawn(move || render(gpu, &connection, &surface, &taken, &woken, &rendered))?;
        Ok(Self {
            jobs: Some(jobs),
            wake,
            thread: Some(thread),
        })
    }

    /// Hands a frame over; it is drawn at once, even while the last one waits for the
    /// compositor.
    pub fn draw(&self, job: Job, target: Target) {
        if let Some(jobs) = &self.jobs
            && jobs.send((job, target)).is_ok()
        {
            let _ = (&self.wake).write(&[1]);
        }
    }
}

impl Drop for RenderThread {
    /// Waits for the thread to let go of the device, which has to go before the surface.
    fn drop(&mut self) {
        self.jobs = None;
        let _ = (&self.wake).write(&[1]);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Whether a frame callback is still awaited.
#[derive(Default)]
struct Paced {
    waiting: bool,
}

impl Dispatch<WlCallback, ()> for Paced {
    fn event(
        paced: &mut Self,
        _: &WlCallback,
        event: wl_callback::Event,
        (): &(),
        _: &Connection,
        _: &QueueHandle<Self>,
    ) {
        if let wl_callback::Event::Done { .. } = event {
            paced.waiting = false;
        }
    }
}

/// How waiting for the compositor ended.
enum Waited {
    /// It wants the next frame.
    Due,
    /// The window handed over another frame first.
    Next(Box<Frame>),
    Closed,
    Lost(String),
}

fn render(
    mut gpu: Gpu,
    connection: &Connection,
    surface: &WlSurface,
    jobs: &mpsc::Receiver<Frame>,
    woken: &UnixStream,
    rendered: &Sender<Rendered>,
) {
    let mut queue: EventQueue<Paced> = connection.new_event_queue();
    let handle = queue.handle();
    let mut paced = Paced::default();
    let mut next = None;
    loop {
        let (mut job, target) = match next.take() {
            Some(frame) => frame,
            None => match jobs.recv() {
                Ok(frame) => frame,
                Err(_) => return,
            },
        };
        let started = Instant::now();
        let drawn = gpu.draw(
            target.width,
            target.height,
            target.roomy,
            &mut job,
            |width, height| {
                surface.frame(&handle, ());
                paced.waiting = true;
                match &target.scaling {
                    Scaling::Viewport {
                        viewport,
                        width: logical_width,
                        height: logical_height,
                    } => {
                        viewport.set_destination(*logical_width, *logical_height);
                        viewport.set_source(0.0, 0.0, f64::from(width), f64::from(height));
                    }
                    Scaling::Buffer(scale) => surface.set_buffer_scale(*scale),
                }
            },
        );
        let presented = match drawn {
            Ok(presented) => presented,
            Err(error) => {
                let _ = rendered.send(Rendered::Failed(error.to_string()));
                return;
            }
        };
        if crate::timings::enabled() {
            crate::timings::frame(target.built, started.elapsed());
        }
        if presented {
            match wait(&mut queue, &mut paced, connection, jobs, woken) {
                Waited::Due => {}
                Waited::Next(frame) => {
                    next = Some(*frame);
                    continue;
                }
                Waited::Closed => return,
                Waited::Lost(error) => {
                    let _ = rendered.send(Rendered::Failed(error));
                    return;
                }
            }
        }
        if rendered.send(Rendered::Ready).is_err() {
            return;
        }
    }
}

/// Waits for the compositor to want the next frame, or for the window to hand one over or
/// close.
fn wait(
    queue: &mut EventQueue<Paced>,
    paced: &mut Paced,
    connection: &Connection,
    jobs: &mpsc::Receiver<Frame>,
    woken: &UnixStream,
) -> Waited {
    let _ = connection.flush();
    loop {
        if let Err(error) = queue.dispatch_pending(paced) {
            return Waited::Lost(error.to_string());
        }
        if !paced.waiting {
            return Waited::Due;
        }
        match jobs.try_recv() {
            Ok(frame) => return Waited::Next(Box::new(frame)),
            Err(TryRecvError::Disconnected) => return Waited::Closed,
            Err(TryRecvError::Empty) => {}
        }
        // None while events read for this queue wait to be dispatched, as above.
        let Some(guard) = queue.prepare_read() else {
            continue;
        };
        let (events, wake) = {
            let fd = guard.connection_fd();
            let mut fds = [
                PollFd::new(&fd, PollFlags::IN),
                PollFd::new(woken, PollFlags::IN),
            ];
            match rustix::event::poll(&mut fds, None) {
                Ok(_) => (!fds[0].revents().is_empty(), !fds[1].revents().is_empty()),
                Err(_) => (false, false),
            }
        };
        if events {
            // Another thread may have read them first.
            let _ = guard.read();
        }
        if wake {
            let mut drained = [0; 64];
            while (&*woken).read(&mut drained).is_ok_and(|read| read > 0) {}
        }
    }
}
