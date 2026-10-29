//! The Wayland side of a window: its surface, the compositor's configures and frame
//! callbacks, and the loop that waits for them.

use std::num::NonZeroU32;

use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState, FrameCallbackData};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::calloop::{self, EventLoop};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::globals::{BindError, registry_queue_init};
use smithay_client_toolkit::reexports::client::protocol::{wl_output, wl_seat, wl_surface};
use smithay_client_toolkit::reexports::client::{Connection, Proxy, QueueHandle};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shell::xdg::window::{
    Window, WindowConfigure, WindowDecorations, WindowHandler,
};
use smithay_client_toolkit::{delegate_dispatch2, delegate_registry, registry_handlers};

use crate::gpu::{Gpu, GpuError};

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("connecting to the Wayland compositor: {0}")]
    Connect(#[from] smithay_client_toolkit::reexports::client::ConnectError),
    #[error("listing the compositor's globals: {0}")]
    Globals(#[from] smithay_client_toolkit::reexports::client::globals::GlobalError),
    #[error("the compositor lacks {0}")]
    Missing(#[from] BindError),
    #[error("starting the event loop: {0}")]
    Loop(#[from] calloop::Error),
    #[error("Vulkan: {0}")]
    Gpu(#[from] GpuError),
}

/// The size a window opens at before the compositor picks one, in logical pixels.
const DEFAULT_SIZE: (u32, u32) = (800, 500);

/// Adwaita's dark view background, until the terminal's own arrives.
const BACKGROUND: [f32; 4] = [
    0x1e as f32 / 255.0,
    0x1e as f32 / 255.0,
    0x1e as f32 / 255.0,
    1.0,
];

/// Fields drop in order: the device before the surface it draws to, and both before
/// the connection.
struct App {
    gpu: Gpu,
    window: Window,
    #[expect(dead_code, reason = "held so that the display outlives the device")]
    connection: Connection,
    registry: RegistryState,
    seats: SeatState,
    outputs: OutputState,
    /// Logical pixels, as the compositor configures them; none before the first
    /// configure.
    size: Option<(u32, u32)>,
    scale: i32,
    /// Something changed since the last frame was drawn.
    dirty: bool,
    /// A frame callback is pending: the compositor shows the last frame and says when
    /// the next one is worth drawing.
    waiting: bool,
    exit: bool,
}

#[expect(
    unsafe_code,
    reason = "Vulkan draws to the surface through its raw pointers"
)]
pub fn run() -> Result<(), AppError> {
    let connection = Connection::connect_to_env()?;
    let (globals, queue) = registry_queue_init::<App>(&connection)?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh)?;
    let shell = XdgShell::bind(&globals, &qh)?;

    let surface = compositor.create_surface(&qh);
    let window = shell.create_window(surface, WindowDecorations::RequestServer, &qh);
    window.set_title("Caret");
    window.set_app_id(crate::APP_ID);
    window.set_min_size(Some((360, 200)));
    window.commit();

    let display = connection.backend().display_ptr().cast();
    let surface = window.wl_surface().id().as_ptr().cast();
    // SAFETY: `App` holds the device, the window and the connection, and drops them in
    // that order.
    let gpu = unsafe { Gpu::new(display, surface)? };

    let mut app = App {
        registry: RegistryState::new(&globals),
        seats: SeatState::new(&globals, &qh),
        outputs: OutputState::new(&globals, &qh),
        gpu,
        window,
        connection: connection.clone(),
        size: None,
        scale: 1,
        dirty: true,
        waiting: false,
        exit: false,
    };

    let mut events: EventLoop<App> = EventLoop::try_new()?;
    WaylandSource::new(connection, queue)
        .insert(events.handle())
        .map_err(|error| AppError::Loop(error.error))?;
    let signal = events.get_signal();
    events.run(None, &mut app, |app| {
        if app.exit {
            signal.stop();
        }
    })?;
    Ok(())
}

impl App {
    /// Draws a frame if one is due, asking the compositor when to draw the next.
    fn draw(&mut self, qh: &QueueHandle<Self>) {
        let Some((width, height)) = self.size else {
            return;
        };
        if !self.dirty || self.waiting {
            return;
        }
        let surface = self.window.wl_surface();
        surface.frame(qh, FrameCallbackData(surface.clone()));
        surface.set_buffer_scale(self.scale);
        self.waiting = true;
        self.dirty = false;
        let scale = self.scale.unsigned_abs();
        let (width, height) = (width * scale, height * scale);
        if let Err(error) = self.gpu.draw(width, height, BACKGROUND) {
            tracing::error!("drawing the window: {error}");
            self.exit = true;
        }
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        factor: i32,
    ) {
        if factor != self.scale {
            self.scale = factor.max(1);
            self.dirty = true;
            self.draw(qh);
        }
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(&mut self, _: &Connection, qh: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.waiting = false;
        self.draw(qh);
    }

    fn surface_enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: &wl_output::WlOutput,
    ) {
    }
}

impl WindowHandler for App {
    fn request_close(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &Window) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        _: &Window,
        configure: WindowConfigure,
        _: u32,
    ) {
        let (width, height) = configure.new_size;
        let (old_width, old_height) = self.size.unwrap_or(DEFAULT_SIZE);
        self.size = Some((
            width.map_or(old_width, NonZeroU32::get),
            height.map_or(old_height, NonZeroU32::get),
        ));
        self.dirty = true;
        // A configure is answered by a frame of the new size, even while one is pending.
        self.waiting = false;
        self.draw(qh);
    }
}

impl OutputHandler for App {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.outputs
    }

    fn new_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn update_output(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}

    fn output_destroyed(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_output::WlOutput) {}
}

impl SeatHandler for App {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seats
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        _: Capability,
    ) {
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        _: Capability,
    ) {
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: wl_seat::WlSeat) {}
}

impl ProvidesRegistryState for App {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry
    }
    registry_handlers![OutputState, SeatState];
}

delegate_registry!(App);
delegate_dispatch2!(App);
