//! A window and the terminal it shows: the compositor's configures and frame callbacks,
//! the server's frames, keys, and the loop that waits for all of them.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::os::unix::ffi::OsStrExt;

use caret_protocol::encode::{self, Modifiers};
use caret_protocol::{
    Answer, Bytes, Command, CursorShape, Message, Options, Question, Request, SessionId, Size,
    TerminalId,
};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState, FrameCallbackData};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::calloop::{self, EventLoop, LoopHandle};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::client::globals::{BindError, registry_queue_init};
use smithay_client_toolkit::reexports::client::protocol::{
    wl_keyboard, wl_output, wl_seat, wl_surface,
};
use smithay_client_toolkit::reexports::client::{Connection, Proxy, QueueHandle};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::keyboard::{
    KeyEvent, KeyboardHandler, Keysym, Modifiers as KeyModifiers, RawModifiers,
};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::shell::xdg::XdgShell;
use smithay_client_toolkit::shell::xdg::window::{
    Window, WindowConfigure, WindowDecorations, WindowHandler,
};
use smithay_client_toolkit::{delegate_dispatch2, delegate_registry, registry_handlers};

use smithay_client_toolkit::data_device_manager::DataDeviceManagerState;
use smithay_client_toolkit::primary_selection::PrimarySelectionManagerState;
use smithay_client_toolkit::seat::pointer::cursor_shape::CursorShapeManager;

use crate::atlas::Atlas;
use crate::connection::{self, Received};
use crate::fonts::{FontError, Fonts};
use crate::gpu::{FRAMES_IN_FLIGHT, Gpu, GpuError};
use crate::grid::{Grid, packed};
use crate::palette::Palette;
use crate::renderer::{CURSOR_FOCUSED, CURSOR_WIDE, Constants, Scene};

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
    #[error("fonts: {0}")]
    Font(#[from] FontError),
    #[error("the terminal server: {0}")]
    Server(#[from] caret_protocol::client::ConnectError),
}

/// The size a window opens at before the compositor picks one, in logical pixels.
const DEFAULT_SIZE: (u32, u32) = (800, 500);

/// Logical pixels between the window's edge and the cells.
const PADDING: u32 = 6;

/// The font size until settings choose one.
const POINTS: f32 = 11.0;

mod clipboard;
mod pointer;

/// A question the window is waiting for the answer to.
enum Asked {
    Session,
    /// The window's tab, opened at this size.
    Tab(Size),
    /// The selected text, to offer as the clipboard or the primary selection.
    Copy {
        primary: bool,
    },
}

/// Fields drop in order: the device before the surface it draws to, and both before
/// the connection.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is a separate part of the window's state"
)]
struct App {
    gpu: Gpu,
    window: Window,
    #[expect(dead_code, reason = "held so that the display outlives the device")]
    connection: Connection,
    registry: RegistryState,
    seats: SeatState,
    outputs: OutputState,
    qh: QueueHandle<Self>,
    events: LoopHandle<'static, Self>,
    server: connection::Connection,
    asked: HashMap<u32, Asked>,
    session: Option<SessionId>,
    terminal: Option<TerminalId>,
    /// A tab has been asked for and not yet opened.
    opening: bool,
    /// A frame has been asked of the server and not yet come.
    frame_asked: bool,
    fonts: Fonts,
    atlas: Atlas,
    grid: Grid,
    palette: Palette,
    /// Logical pixels, as the compositor configures them; none before the first
    /// configure.
    size: Option<(u32, u32)>,
    scale: i32,
    /// The size of the grid the window fits, as last told to the server.
    grid_size: Option<Size>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    modifiers: Modifiers,
    /// The serial of the last key or button press, which taking a selection needs.
    serial: u32,
    pointer: pointer::Pointer,
    cursor_shapes: Option<CursorShapeManager>,
    clipboard: clipboard::Clipboard,
    focused: bool,
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
    let (mut server, received) = connection::Connection::open(crate::APP_ID)?;
    let fonts = Fonts::new(None, POINTS, 1.0)?;

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

    let palette = Palette::dark();
    server.send(&Request::Look {
        palette: palette.colors(),
        options: Options {
            scrollback: 10_000,
            cursor_shape: CursorShape::Block,
            cursor_blinks: false,
            copy_from_programs: true,
            scroll_on_output: false,
        },
    });
    let mut asked = HashMap::new();
    asked.insert(server.ask(Question::NewSession), Asked::Session);

    let mut events: EventLoop<App> = EventLoop::try_new()?;
    let (pastes, pasted) = calloop::channel::channel();
    let clipboard = clipboard::Clipboard::new(
        DataDeviceManagerState::bind(&globals, &qh).ok(),
        PrimarySelectionManagerState::bind(&globals, &qh).ok(),
        pastes,
    );
    let cursor_shapes = CursorShapeManager::bind(&globals, &qh).ok();
    let mut app = App {
        registry: RegistryState::new(&globals),
        seats: SeatState::new(&globals, &qh),
        outputs: OutputState::new(&globals, &qh),
        gpu,
        window,
        connection: connection.clone(),
        qh,
        events: events.handle(),
        server,
        asked,
        session: None,
        terminal: None,
        opening: false,
        frame_asked: false,
        fonts,
        atlas: Atlas::default(),
        grid: Grid::new(FRAMES_IN_FLIGHT),
        palette,
        size: None,
        scale: 1,
        grid_size: None,
        keyboard: None,
        modifiers: Modifiers::default(),
        serial: 0,
        pointer: pointer::Pointer::default(),
        cursor_shapes,
        clipboard,
        focused: false,
        dirty: true,
        waiting: false,
        exit: false,
    };

    WaylandSource::new(connection, queue)
        .insert(events.handle())
        .map_err(|error| AppError::Loop(error.error))?;
    events
        .handle()
        .insert_source(received, |event, (), app| {
            if let calloop::channel::Event::Msg(received) = event {
                app.received(received);
            }
        })
        .map_err(|error| AppError::Loop(error.error))?;
    events
        .handle()
        .insert_source(pasted, |event, (), app| {
            if let calloop::channel::Event::Msg(bytes) = event {
                app.pasted(&bytes);
            }
        })
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
    fn received(&mut self, received: Received) {
        let message = match received {
            Received::Message(message) => message,
            Received::Lost(reason) => {
                tracing::error!("lost the terminal server: {reason}");
                self.exit = true;
                return;
            }
        };
        match message {
            Message::Answer { id, answer } => match (self.asked.remove(&id), answer) {
                (Some(Asked::Session), Answer::Session { session }) => {
                    self.session = Some(session);
                    self.server.send(&Request::Attach { session });
                    self.open_tab();
                }
                (Some(Asked::Copy { primary }), Answer::Text { text }) => {
                    if let Some(text) = text.filter(|text| !text.is_empty()) {
                        self.copy(text, primary);
                    }
                }
                (Some(Asked::Tab(size)), Answer::Opened { terminal }) => {
                    self.opening = false;
                    self.terminal = Some(terminal);
                    // The window may have changed size while the tab was opening.
                    if let Some(now) = self.grid_size.filter(|&now| now != size) {
                        self.server.send(&Request::Resize {
                            terminal,
                            size: now,
                        });
                    }
                    self.ask_frame();
                }
                (Some(Asked::Copy { .. }) | None, _) => {}
                (Some(_), answer) => {
                    tracing::error!("the server answered {answer:?}");
                    self.exit = true;
                }
            },
            Message::Frame { terminal, frame } if Some(terminal) == self.terminal => {
                self.frame_asked = false;
                let (atlas, fonts) = (&mut self.atlas, &mut self.fonts);
                self.grid.apply(frame, |glyph| atlas.index(glyph, fonts));
                self.dirty = true;
                self.draw();
            }
            Message::Clipboard {
                terminal,
                primary,
                text,
            } if Some(terminal) == self.terminal => self.copy(text, primary),
            Message::Title { terminal, title } if Some(terminal) == self.terminal => {
                self.window.set_title(title);
            }
            Message::Ended { terminal, .. } | Message::TabClosed { terminal, .. }
                if Some(terminal) == self.terminal =>
            {
                self.exit = true;
            }
            Message::SessionClosed { session } if Some(session) == self.session => {
                self.exit = true;
            }
            _ => {}
        }
    }

    /// Asks for the window's tab once there is a session and a size to give it.
    fn open_tab(&mut self) {
        let (Some(session), Some(size)) = (self.session, self.grid_size) else {
            return;
        };
        if self.opening || self.terminal.is_some() {
            return;
        }
        let directory = std::env::current_dir()
            .ok()
            .map(|path| Bytes(path.as_os_str().as_bytes().to_vec()));
        let command = Command {
            argv: Vec::new(),
            directory,
            env: std::env::vars().collect(),
            hold: false,
        };
        let id = self.server.ask(Question::OpenTab {
            session,
            index: 0,
            command,
            size,
        });
        self.asked.insert(id, Asked::Tab(size));
        self.opening = true;
    }

    fn ask_frame(&mut self) {
        if let Some(terminal) = self.terminal
            && !self.frame_asked
        {
            self.server.send(&Request::Frame { terminal });
            self.frame_asked = true;
        }
    }

    /// Fits the grid to the window, telling the server when its size changes.
    fn fit(&mut self) {
        let Some((width, height)) = self.size else {
            return;
        };
        let scale = self.scale.unsigned_abs();
        let cell = self.fonts.cell();
        let padding = PADDING * scale;
        let fit = |pixels: u32, cell: i32| {
            let cells = pixels.saturating_sub(2 * padding) / cell.max(1).unsigned_abs();
            u16::try_from(cells.max(1)).unwrap_or(u16::MAX)
        };
        let size = Size {
            columns: fit(width * scale, cell.width),
            lines: fit(height * scale, cell.height),
            cell_width: u16::try_from(cell.width).unwrap_or(u16::MAX),
            cell_height: u16::try_from(cell.height).unwrap_or(u16::MAX),
        };
        if self.grid_size == Some(size) {
            return;
        }
        self.grid_size = Some(size);
        match self.terminal {
            Some(terminal) => self.server.send(&Request::Resize { terminal, size }),
            None => self.open_tab(),
        }
    }

    fn constants(&self, width: u32, height: u32) -> Constants {
        let cell = self.fonts.cell();
        let padding = (PADDING * self.scale.unsigned_abs()) as i32;
        let (cursor_column, cursor_line, cursor_shape) = match self.grid.cursor {
            Some(cursor) => {
                let mut shape = cursor.shape as u32;
                if cursor.wide {
                    shape |= CURSOR_WIDE;
                }
                if self.focused {
                    shape |= CURSOR_FOCUSED;
                }
                (
                    i32::from(cursor.point.column),
                    i32::from(cursor.point.line),
                    shape,
                )
            }
            None => (-1, -1, 0),
        };
        Constants {
            viewport_width: width as f32,
            viewport_height: height as f32,
            cell_width: cell.width,
            cell_height: cell.height,
            origin_x: padding,
            origin_y: padding,
            columns: self.grid.columns as u32,
            pass: 0,
            cursor_column,
            cursor_line,
            cursor_shape,
            cursor_color: packed(self.grid.cursor_color),
            background: packed(self.grid.background),
            selection: packed(self.palette.selection),
            baseline: cell.baseline,
            underline_top: cell.underline,
            underline_thickness: cell.underline_thickness,
            strikeout_top: cell.strikeout,
            strikeout_thickness: cell.strikeout_thickness,
        }
    }

    /// Draws a frame if one is due, asking the compositor when to draw the next.
    fn draw(&mut self) {
        let Some((width, height)) = self.size else {
            return;
        };
        if !self.dirty || self.waiting {
            return;
        }
        let surface = self.window.wl_surface();
        surface.frame(&self.qh, FrameCallbackData(surface.clone()));
        surface.set_buffer_scale(self.scale);
        self.waiting = true;
        self.dirty = false;
        let scale = self.scale.unsigned_abs();
        let (width, height) = (width * scale, height * scale);
        let constants = self.constants(width, height);
        let background = self.grid.background;
        let mut scene = Scene {
            grid: &mut self.grid,
            atlas: &mut self.atlas,
            constants,
            background: [
                f32::from(background.r) / 255.0,
                f32::from(background.g) / 255.0,
                f32::from(background.b) / 255.0,
                1.0,
            ],
        };
        if let Err(error) = self.gpu.draw(width, height, &mut scene) {
            tracing::error!("drawing the window: {error}");
            self.exit = true;
            return;
        }
        // Drawn, so ready for the next.
        self.ask_frame();
    }

    fn key(&mut self, event: &KeyEvent) {
        let Some(terminal) = self.terminal else {
            return;
        };
        let Modifiers { shift, ctrl, alt } = self.modifiers;
        if ctrl && shift && !alt {
            match event.keysym {
                Keysym::C | Keysym::c => {
                    let id = self.server.ask(Question::SelectionText { terminal });
                    self.asked.insert(id, Asked::Copy { primary: false });
                    return;
                }
                Keysym::V | Keysym::v => {
                    self.paste(false);
                    return;
                }
                _ => {}
            }
        }
        // Text as the keymap and compose sequences make it, dead keys included; with Ctrl
        // or Alt the key itself counts.
        let typed = event
            .utf8
            .as_deref()
            .filter(|text| !text.is_empty() && !text.chars().any(char::is_control));
        if let Some(text) = typed
            && !ctrl
            && !alt
            && crate::keys::named(event.keysym).is_none()
        {
            self.server.send(&Request::Input {
                terminal,
                bytes: Bytes(text.as_bytes().to_vec()),
                bottom: true,
            });
            return;
        }
        let Some(key) = crate::keys::key(event.keysym) else {
            return;
        };
        if let Some(bytes) = encode::key(key, self.modifiers, self.grid.mode) {
            self.server.send(&Request::Input {
                terminal,
                bytes: Bytes(bytes),
                bottom: true,
            });
        }
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        if let Some(terminal) = self.terminal {
            self.server.send(&Request::Focus { terminal, focused });
        }
        self.dirty = true;
        self.draw();
    }
}

impl CompositorHandler for App {
    fn scale_factor_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        factor: i32,
    ) {
        let factor = factor.max(1);
        if factor == self.scale {
            return;
        }
        self.scale = factor;
        match Fonts::new(None, POINTS, factor as f32) {
            Ok(fonts) => {
                self.fonts = fonts;
                // Every glyph is rasterised again at the new size.
                self.atlas = Atlas::default();
                let (atlas, fonts) = (&mut self.atlas, &mut self.fonts);
                self.grid.refill(|glyph| atlas.index(glyph, fonts));
            }
            Err(error) => tracing::error!("loading fonts at scale {factor}: {error}"),
        }
        self.fit();
        self.dirty = true;
        self.draw();
    }

    fn transform_changed(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_surface::WlSurface,
        _: wl_output::Transform,
    ) {
    }

    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {
        self.waiting = false;
        self.draw();
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
        if let Some(session) = self.session {
            self.server.send(&Request::CloseSession { session });
        }
        self.exit = true;
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
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
        self.fit();
        self.dirty = true;
        // A configure is answered by a frame of the new size, even while one is pending.
        self.waiting = false;
        self.draw();
    }
}

impl KeyboardHandler for App {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _: &[Keysym],
    ) {
        self.set_focus(true);
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: &wl_surface::WlSurface,
        _: u32,
    ) {
        self.set_focus(false);
    }

    fn press_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        serial: u32,
        event: KeyEvent,
    ) {
        self.serial = serial;
        self.key(&event);
    }

    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.key(&event);
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        _: KeyEvent,
    ) {
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        modifiers: KeyModifiers,
        _: RawModifiers,
        _: u32,
    ) {
        self.modifiers = Modifiers {
            shift: modifiers.shift,
            alt: modifiers.alt,
            ctrl: modifiers.ctrl,
        };
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

    fn new_seat(&mut self, _: &Connection, qh: &QueueHandle<Self>, seat: wl_seat::WlSeat) {
        self.clipboard.add_seat(qh, &seat);
    }

    fn new_capability(
        &mut self,
        _: &Connection,
        qh: &QueueHandle<Self>,
        seat: wl_seat::WlSeat,
        capability: Capability,
    ) {
        self.clipboard.add_seat(qh, &seat);
        if capability == Capability::Pointer && self.pointer.proxy.is_none() {
            match self.seats.get_pointer(qh, &seat) {
                Ok(pointer) => {
                    self.pointer.shape = self
                        .cursor_shapes
                        .as_ref()
                        .map(|shapes| shapes.get_shape_device(&pointer, qh));
                    self.pointer.proxy = Some(pointer);
                }
                Err(error) => tracing::warn!("taking the pointer: {error}"),
            }
        }
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            let keyboard = self.seats.get_keyboard_with_repeat(
                qh,
                &seat,
                None,
                self.events.clone(),
                Box::new(|app, _, event| app.key(&event)),
            );
            match keyboard {
                Ok(keyboard) => self.keyboard = Some(keyboard),
                Err(error) => tracing::warn!("taking the keyboard: {error}"),
            }
        }
    }

    fn remove_capability(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard
            && let Some(keyboard) = self.keyboard.take()
        {
            keyboard.release();
        }
        if capability == Capability::Pointer {
            if let Some(shape) = self.pointer.shape.take() {
                shape.destroy();
            }
            if let Some(pointer) = self.pointer.proxy.take() {
                pointer.release();
            }
        }
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
