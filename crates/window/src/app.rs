//! A window and the terminal it shows: the compositor's configures and frame callbacks,
//! the server's frames, keys, and the loop that waits for all of them.

use std::collections::HashMap;
use std::num::NonZeroU32;
use std::os::unix::process::CommandExt;
use std::time::Duration;

use caret_protocol::encode::{self, Modifiers};
use caret_protocol::{
    Answer, Bytes, Command, Message, Options, Question, Request, Select, SessionId, Size,
    TerminalId,
};
use smithay_client_toolkit::compositor::{
    CompositorHandler, CompositorState, FrameCallbackData, Region,
};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{
    self, EventLoop, Interest, LoopHandle, Mode, PostAction,
};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::ZwpTextInputManagerV3;
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
use smithay_client_toolkit::shell::xdg::window::{
    DecorationMode, Window, WindowConfigure, WindowDecorations, WindowHandler,
};
use smithay_client_toolkit::shell::xdg::{XdgShell, XdgSurface};
use smithay_client_toolkit::{delegate_dispatch2, delegate_registry, registry_handlers};

use smithay_client_toolkit::data_device_manager::DataDeviceManagerState;
use smithay_client_toolkit::primary_selection::PrimarySelectionManagerState;
use smithay_client_toolkit::seat::pointer::cursor_shape::CursorShapeManager;

use crate::atlas::Atlas;
use crate::chrome::{self, Chrome, Dialog, Drawing};
use crate::config::{self, Config};
use crate::connection::{self, Received};
use crate::desktop::{self, Desktop};
use crate::fonts::{FontError, Fonts};
use crate::gpu::{FRAMES_IN_FLIGHT, Gpu, GpuError};
use crate::grid::{Grid, packed};
use crate::palette::Palette;
use crate::renderer::{CURSOR_FOCUSED, CURSOR_WIDE, Constants, Scene};
use crate::shortcuts::{Action, Shortcuts};

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

/// How much larger the font is for each step it is zoomed in, and the most steps.
const ZOOM: f32 = 1.1;
const ZOOM_STEPS: i32 = 12;

/// How long the window waits, before its first frame, for the desktop's preferences.
const DESKTOP_WAIT: Duration = Duration::from_millis(250);

/// Logical pixels between the window's edge and the cells.
const PADDING: u32 = 6;

mod clipboard;
mod confirm;
mod menus;
mod pointer;
mod scale;
mod search;
mod tabs;
mod text_input;
mod toast;

/// A question the window is waiting for the answer to.
enum Asked {
    Session,
    /// A tab, opened at this size.
    Tab(Size),
    /// What a new tab runs.
    NextCommand,
    /// Whether the text of the search this counts was found.
    Find(u32),
    /// The selected text, to offer as the clipboard or the primary selection.
    Copy {
        primary: bool,
    },
    /// The address under the pointer, to open.
    Link,
    /// Whether a program runs in the terminal, to close its tab or ask first.
    Busy(TerminalId),
    /// Whether a program runs in one of the tabs, to close the window or ask first.
    Closing,
    /// The link where the terminal's menu opens, at this point of the window.
    Menu((f32, f32)),
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
    compositor: CompositorState,
    seats: SeatState,
    /// The seat whose pointer moves and resizes the window.
    seat: Option<wl_seat::WlSeat>,
    outputs: OutputState,
    qh: QueueHandle<Self>,
    events: LoopHandle<'static, Self>,
    server: connection::Connection,
    asked: HashMap<u32, Asked>,
    session: Option<SessionId>,
    tabs: Vec<tabs::Tab>,
    selected: usize,
    /// A tab has been asked for and not yet opened.
    opening: bool,
    /// The tab the window opened, to show when the server tells of it.
    opened: Option<TerminalId>,
    /// Counts searches, so that answers to ones the window moved past are dropped.
    searches: u32,
    /// What the dialog open asks to confirm.
    confirming: Option<confirm::Confirm>,
    /// What each item of the menu open does.
    choices: Vec<menus::Choice>,
    /// Counts toasts, so that a toast's timer hides only its own; and where threads
    /// send theirs.
    toasts: u32,
    toaster: calloop::channel::Sender<String>,
    /// The window is about to close once its tabs say whether programs run in them.
    closing: Option<confirm::Closing>,
    fonts: Fonts,
    /// Steps the font is zoomed in, or out for fewer than none.
    zoom: i32,
    shortcuts: Shortcuts,
    atlas: Atlas,
    grid: Grid,
    palette: Palette,
    chrome: Chrome,
    config: Config,
    desktop: Desktop,
    /// The window's size in logical pixels, as the compositor configures it, without
    /// the room around it for a shadow; none before the first configure.
    size: Option<(u32, u32)>,
    /// Device pixels to the logical one, in 120ths.
    scale: u32,
    /// Fractional scaling, where the compositor offers it.
    fractional: Option<scale::Fractional>,
    /// The size of the grid the window fits, as last told to the server.
    grid_size: Option<Size>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    text_inputs: Option<ZwpTextInputManagerV3>,
    text_input: Option<text_input::TextInput>,
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

/// The window's toplevel, its frame drawn by the compositor if `server_side` and it can.
fn toplevel(
    compositor: &CompositorState,
    shell: &XdgShell,
    qh: &QueueHandle<App>,
    server_side: bool,
) -> Window {
    let decorations = if server_side {
        WindowDecorations::RequestServer
    } else {
        WindowDecorations::RequestClient
    };
    let window = shell.create_window(compositor.create_surface(qh), decorations, qh);
    window.set_title("Caret");
    window.set_app_id(crate::APP_ID);
    window.set_min_size(Some((360, 200)));
    window
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
    let (desktop_changes, desktop_changed) = calloop::channel::channel();
    let first_desktop = desktop::follow(desktop_changes);
    config::import();
    let config = Config::load();

    let window = toplevel(&compositor, &shell, &qh, config.server_side_decorations);
    let fractional = scale::Fractional::new(&globals, &qh, window.wl_surface());
    window.commit();

    let display = connection.backend().display_ptr().cast();
    let surface = window.wl_surface().id().as_ptr().cast();
    // SAFETY: `App` holds the device, the window and the connection, and drops them in
    // that order.
    let gpu = unsafe { Gpu::new(display, surface)? };

    let desktop = first_desktop.recv_timeout(DESKTOP_WAIT).unwrap_or_default();
    let (family, points) = config.font_for(desktop.monospace.as_deref());
    let fonts = Fonts::new(family.as_deref(), points, 1.0)?;
    let palette = palette(&config, &desktop);
    server.send(&look(&config, &palette));
    let mut asked = HashMap::new();
    asked.insert(server.ask(Question::NewSession), Asked::Session);

    let mut events: EventLoop<App> = EventLoop::try_new()?;
    let (pastes, pasted) = calloop::channel::channel();
    let (toaster, toasts) = calloop::channel::channel();
    let clipboard = clipboard::Clipboard::new(
        DataDeviceManagerState::bind(&globals, &qh).ok(),
        PrimarySelectionManagerState::bind(&globals, &qh).ok(),
        pastes,
    );
    let cursor_shapes = CursorShapeManager::bind(&globals, &qh).ok();
    let text_inputs = text_input::TextInput::bind(&globals, &qh);
    let mut app = App {
        registry: RegistryState::new(&globals),
        compositor,
        seats: SeatState::new(&globals, &qh),
        seat: None,
        outputs: OutputState::new(&globals, &qh),
        gpu,
        window,
        connection: connection.clone(),
        qh,
        events: events.handle(),
        server,
        asked,
        session: None,
        tabs: Vec::new(),
        selected: 0,
        opening: false,
        opened: None,
        searches: 0,
        confirming: None,
        choices: Vec::new(),
        toasts: 0,
        toaster,
        closing: None,
        fonts,
        zoom: 0,
        shortcuts: Shortcuts::new(&config.keys),
        atlas: Atlas::default(),
        grid: Grid::new(FRAMES_IN_FLIGHT),
        palette,
        chrome: Chrome {
            title: "Caret".to_owned(),
            header_bar: config.title_bar,
            dark: config.dark(desktop.dark),
            ..Chrome::default()
        },
        config,
        desktop,
        size: None,
        scale: scale::WHOLE,
        fractional,
        grid_size: None,
        keyboard: None,
        text_inputs,
        text_input: None,
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
    listen(&events.handle(), received, pasted, desktop_changed, toasts)?;
    let signal = events.get_signal();
    events.run(None, &mut app, |app| {
        if app.exit {
            signal.stop();
        }
    })?;
    Ok(())
}

/// Opens another window, a program of its own.
fn new_window() {
    let started = std::env::current_exe()
        .and_then(|program| std::process::Command::new(program).process_group(0).spawn());
    match started {
        // Waited for, so that it leaves no zombie when it ends.
        Ok(mut window) => {
            std::thread::spawn(move || window.wait());
        }
        Err(error) => tracing::warn!("opening a window: {error}"),
    }
}

/// The palette of the style the settings and the desktop choose.
fn palette(config: &Config, desktop: &Desktop) -> Palette {
    if config.dark(desktop.dark) {
        Palette::dark()
    } else {
        Palette::light()
    }
}

/// Tells the server how to show the window's terminals.
fn look(config: &Config, palette: &Palette) -> Request {
    Request::Look {
        palette: palette.colors(),
        options: Options {
            scrollback: config.scrollback_lines,
            cursor_shape: config.cursor_shape,
            cursor_blinks: false,
            copy_from_programs: config.program_clipboard,
            scroll_on_output: config.scroll_on_output,
        },
    }
}

/// Takes the server's messages, the pastes read from other programs, and changes to the
/// settings and the desktop's preferences in the loop.
fn listen(
    events: &LoopHandle<'static, App>,
    received: calloop::channel::Channel<Received>,
    pasted: calloop::channel::Channel<Vec<u8>>,
    desktop_changed: calloop::channel::Channel<Desktop>,
    toasts: calloop::channel::Channel<String>,
) -> Result<(), AppError> {
    events
        .insert_source(toasts, |event, (), app| {
            if let calloop::channel::Event::Msg(text) = event {
                app.toast(text);
            }
        })
        .map_err(|error| AppError::Loop(error.error))?;
    events
        .insert_source(desktop_changed, |event, (), app| {
            if let calloop::channel::Event::Msg(desktop) = event {
                let before = app.style();
                app.desktop = desktop;
                app.restyle(&before);
            }
        })
        .map_err(|error| AppError::Loop(error.error))?;
    events
        .insert_source(received, |event, (), app| {
            if let calloop::channel::Event::Msg(received) = event {
                app.received(received);
            }
        })
        .map_err(|error| AppError::Loop(error.error))?;
    events
        .insert_source(pasted, |event, (), app| {
            if let calloop::channel::Event::Msg(bytes) = event {
                app.pasted(&bytes);
            }
        })
        .map_err(|error| AppError::Loop(error.error))?;
    match config::watch() {
        Ok(watch) => {
            let source = Generic::new(watch, Interest::READ, Mode::Level);
            events
                .insert_source(source, |_, watch, app| {
                    if config::changed(watch.as_ref()) {
                        app.reconfigure(Config::load());
                    }
                    Ok(PostAction::Continue)
                })
                .map_err(|error| AppError::Loop(error.error))?;
        }
        Err(error) => tracing::warn!("watching the settings: {error}"),
    }
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
                    self.open_first_tab();
                }
                (Some(Asked::NextCommand), Answer::Command { command }) => self.open_tab(command),
                (Some(Asked::NextCommand), _) => self.open_tab(Command::default()),
                (Some(Asked::Find(search)), Answer::Found { found }) => self.found(search, found),
                (Some(Asked::Copy { primary }), Answer::Text { text }) => {
                    if let Some(text) = text.filter(|text| !text.is_empty()) {
                        self.copy(text, primary);
                    }
                }
                (Some(Asked::Tab(size)), Answer::Opened { terminal }) => {
                    self.opening = false;
                    self.opened = Some(terminal);
                    // The window may have changed size while the tab was opening.
                    if let Some(now) = self.grid_size.filter(|&now| now != size) {
                        self.server.send(&Request::Resize {
                            terminal,
                            size: now,
                        });
                    }
                }
                (Some(Asked::Link), Answer::Link { uri }) => {
                    if let Some(uri) = uri {
                        pointer::open(&uri, &self.toaster);
                    }
                }
                (Some(Asked::Busy(terminal)), answer) => self.busy(terminal, answer),
                (Some(Asked::Closing), answer) => self.closing_busy(&answer),
                (Some(Asked::Menu(at)), Answer::Link { uri }) => self.terminal_menu(at, uri),
                (Some(Asked::Menu(at)), _) => self.terminal_menu(at, None),
                (Some(Asked::Copy { .. } | Asked::Find(_) | Asked::Link) | None, _) => {}
                (Some(_), answer) => {
                    tracing::error!("the server answered {answer:?}");
                    self.exit = true;
                }
            },
            Message::Frame { terminal, frame } => self.tab_frame(terminal, frame),
            Message::Clipboard {
                terminal,
                primary,
                text,
            } if self.tab_of(terminal).is_some() => self.copy(text, primary),
            Message::Title { terminal, title } => self.tab_titled(terminal, title),
            Message::TabNamed { terminal, name } => self.tab_named(terminal, name),
            Message::Attached { session, tabs } if Some(session) == self.session => {
                for (index, tab) in (0..).zip(tabs) {
                    self.tab_opened(index, tab);
                }
            }
            Message::TabOpened {
                session,
                index,
                tab,
            } if Some(session) == self.session => self.tab_opened(index, tab),
            Message::TabClosed { session, terminal } if Some(session) == self.session => {
                self.tab_closed(terminal);
            }
            Message::SessionClosed { session } if Some(session) == self.session => {
                self.exit = true;
            }
            _ => {}
        }
    }

    /// Starts the atlas again with what is on screen once glyphs scrolled away fill it.
    fn renew_full_atlas(&mut self) {
        if !self.atlas.full {
            return;
        }
        tracing::debug!("starting the glyph atlas again");
        self.atlas = self.atlas.next();
        let (atlas, fonts) = (&mut self.atlas, &mut self.fonts);
        self.grid.refill(|glyph| atlas.index(glyph, fonts));
        if self.atlas.full {
            tracing::warn!("the glyphs on screen do not fit the atlas");
            // Left set, every frame would start the atlas again to no avail.
            self.atlas.full = false;
        }
    }

    /// Applies settings changed while the window is open; the others are read when used.
    fn reconfigure(&mut self, config: Config) {
        let before = self.style();
        let old = std::mem::replace(&mut self.config, config);
        let new = &self.config;
        if old.server_side_decorations != new.server_side_decorations {
            self.window
                .request_decoration_mode(Some(if new.server_side_decorations {
                    DecorationMode::Server
                } else {
                    DecorationMode::Client
                }));
        }
        self.chrome.header_bar = self.config.title_bar;
        self.shortcuts = Shortcuts::new(&self.config.keys);
        self.restyle(&before);
    }

    /// What the settings and the desktop make of the window: its font, and how the
    /// server is to show its terminals.
    fn style(&self) -> ((Option<String>, f32), Request) {
        let font = self.config.font_for(self.desktop.monospace.as_deref());
        let palette = palette(&self.config, &self.desktop);
        (font, look(&self.config, &palette))
    }

    /// Applies what changed of the window's style since it was `before`.
    fn restyle(&mut self, before: &((Option<String>, f32), Request)) {
        let (font, look) = self.style();
        if font != before.0 {
            self.load_fonts();
        }
        if look != before.1 {
            self.palette = palette(&self.config, &self.desktop);
            self.chrome.dark = self.config.dark(self.desktop.dark);
            self.server.send(&look);
        }
        self.fit();
        self.dirty = true;
        self.draw();
    }

    /// Loads the font the settings or the desktop name at the window's scale, drawing every glyph
    /// again.
    fn load_fonts(&mut self) {
        let (family, points) = self.config.font_for(self.desktop.monospace.as_deref());
        let points = points * ZOOM.powi(self.zoom);
        match Fonts::new(family.as_deref(), points, self.factor() as f32) {
            Ok(fonts) => {
                self.fonts = fonts;
                self.atlas = self.atlas.next();
                let (atlas, fonts) = (&mut self.atlas, &mut self.fonts);
                self.grid.refill(|glyph| atlas.index(glyph, fonts));
            }
            Err(error) => tracing::error!("loading fonts at scale {}: {error}", self.factor()),
        }
    }

    /// Ends the window, and its session unless the settings keep it running.
    /// Closes the window, and with it the session's tabs unless closing detaches it.
    fn end(&mut self) {
        if let Some(session) = self.session {
            if self.config.close_detaches {
                self.server.send(&Request::Detach { session });
            } else {
                self.server.send(&Request::CloseSession { session });
            }
        }
        self.exit = true;
    }

    /// Asks for the window's first tab once there is a session and a size to give it.
    fn open_first_tab(&mut self) {
        if self.session.is_none() || self.grid_size.is_none() {
            return;
        }
        if self.opening || !self.tabs.is_empty() {
            return;
        }
        self.open_tab(Command::default());
        self.opening = true;
    }

    /// Asks for the next frame of the tab shown.
    fn ask_frame(&mut self) {
        if let Some(tab) = self.tabs.get_mut(self.selected)
            && !tab.frame_asked
        {
            self.server.send(&Request::Frame {
                terminal: tab.terminal,
            });
            tab.frame_asked = true;
        }
    }

    /// Fits the grid to the window, telling the server when its size changes.
    fn fit(&mut self) {
        let Some((width, height)) = self.size else {
            return;
        };
        let cell = self.fonts.cell();
        let padding = self.device(PADDING);
        let fit = |pixels: u32, cell: i32| {
            let cells = pixels.saturating_sub(2 * padding) / cell.max(1).unsigned_abs();
            u16::try_from(cells.max(1)).unwrap_or(u16::MAX)
        };
        let height = height.saturating_sub(self.chrome.top());
        let size = Size {
            columns: fit(self.device(width), cell.width),
            lines: fit(self.device(height), cell.height),
            cell_width: u16::try_from(cell.width).unwrap_or(u16::MAX),
            cell_height: u16::try_from(cell.height).unwrap_or(u16::MAX),
        };
        if self.grid_size == Some(size) {
            return;
        }
        self.grid_size = Some(size);
        for tab in &self.tabs {
            self.server.send(&Request::Resize {
                terminal: tab.terminal,
                size,
            });
        }
        self.open_first_tab();
    }

    /// Device pixels to the logical one.
    fn factor(&self) -> f64 {
        f64::from(self.scale) / f64::from(scale::WHOLE)
    }

    /// The device pixels `logical` pixels take.
    fn device(&self, logical: u32) -> u32 {
        (f64::from(logical) * self.factor()).round() as u32
    }

    /// Where the first cell's top left is, in device pixels from the surface's.
    fn origin(&self) -> (i32, i32) {
        let margin = self.chrome.margin();
        let x = self.device(margin + PADDING);
        let y = self.device(margin + self.chrome.top() + PADDING);
        (x as i32, y as i32)
    }

    /// Takes the scale the compositor prefers, in 120ths, and draws at it.
    fn set_scale(&mut self, scale: u32) {
        let scale = scale.max(1);
        if scale == self.scale {
            return;
        }
        self.scale = scale;
        self.load_fonts();
        self.fit();
        self.dirty = true;
        self.draw();
    }

    fn constants(&self, width: u32, height: u32) -> Constants {
        let cell = self.fonts.cell();
        let (origin_x, origin_y) = self.origin();
        let (cursor_column, cursor_line, cursor_shape) = match self.grid.cursor {
            Some(cursor) => {
                let mut shape = cursor.shape as u32;
                if cursor.wide {
                    shape |= CURSOR_WIDE;
                }
                let typing_in_bar = self
                    .chrome
                    .search
                    .as_ref()
                    .is_some_and(|search| search.focused);
                if self.focused && !typing_in_bar {
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
            origin_x,
            origin_y,
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
        let margin = 2 * self.chrome.margin();
        let surface = self.window.wl_surface();
        surface.frame(&self.qh, FrameCallbackData(surface.clone()));
        match &self.fractional {
            Some(fractional) => {
                let (width, height) = ((width + margin) as i32, (height + margin) as i32);
                fractional.viewport.set_destination(width, height);
            }
            None => surface.set_buffer_scale((self.scale / scale::WHOLE).max(1) as i32),
        }
        self.waiting = true;
        self.dirty = false;
        let (surface_width, surface_height) =
            (self.device(width + margin), self.device(height + margin));
        let constants = self.constants(surface_width, surface_height);
        let background = self.grid.background;
        self.place_text_input();
        self.place_preedit();
        let (mut quads, mut under) = self.frame_quads((width, height));
        if self.atlas.full {
            self.renew_full_atlas();
            (quads, under) = self.frame_quads((width, height));
        }
        // Around the window's frame, the surface is clear; without one, it is the
        // terminal's to its edges.
        let clear = if self.chrome.framed() {
            [0.0; 4]
        } else {
            [
                f32::from(background.r) / 255.0,
                f32::from(background.g) / 255.0,
                f32::from(background.b) / 255.0,
                1.0,
            ]
        };
        let mut scene = Scene {
            grid: &mut self.grid,
            atlas: &mut self.atlas,
            constants,
            quads: &quads,
            under,
            background: clear,
        };
        if let Err(error) = self.gpu.draw(surface_width, surface_height, &mut scene) {
            tracing::error!("drawing the window: {error}");
            self.exit = true;
            return;
        }
        // Drawn, so ready for the next.
        self.ask_frame();
    }

    /// The window's frame and bars for a window `size` big, their text and icons put in
    /// the atlas.
    fn frame_quads(&mut self, size: (u32, u32)) -> (Vec<chrome::Quad>, usize) {
        let scale = self.factor() as f32;
        if let Some(search) = &mut self.chrome.search {
            search.caret = self
                .fonts
                .label_advance(&search.text[..search.cursor], false);
        }
        if let Some(menu) = &mut self.chrome.menu {
            let fonts = &self.fonts;
            menu.fit(|text| fonts.label_advance(text, false) / scale);
        }
        if let Some(dialog) = &mut self.chrome.dialog {
            let fonts = &self.fonts;
            let width = Dialog::text_width(size.0 as f32) * scale;
            dialog.lines =
                chrome::wrap(&dialog.body, width, |text| fonts.label_advance(text, false));
            dialog.line = fonts.label_height() as f32 / scale;
        }
        let (atlas, fonts) = (&mut self.atlas, &mut self.fonts);
        let line = fonts.label_height();
        self.chrome.quads(
            size,
            scale,
            (packed(self.grid.background) << 8) | 0xff,
            line,
            |drawing| {
                let index = atlas.drawing(drawing, || match drawing {
                    Drawing::Label { text, width, bold } => fonts.label(text, *width, *bold),
                    Drawing::Heading { text, width } => fonts.heading(text, *width),
                    Drawing::Icon(icon) => Some(chrome::icon(*icon, scale)),
                });
                Some((index, atlas.entry(index)?))
            },
        )
    }

    /// Tells the compositor where the window is in its surface and where the surface
    /// takes input: the window and the edges around it that resize it.
    fn place(&self) {
        let Some((width, height)) = self.size else {
            return;
        };
        let margin = self.chrome.margin();
        self.window
            .set_window_geometry(margin, margin, width, height);
        let surface = self.window.wl_surface();
        if margin == 0 {
            surface.set_input_region(None);
            return;
        }
        match Region::new(&self.compositor) {
            Ok(region) => {
                let reach = chrome::EDGE as i32;
                let (margin, width, height) = (margin as i32, width as i32, height as i32);
                region.add(
                    margin - reach,
                    margin - reach,
                    width + 2 * reach,
                    height + 2 * reach,
                );
                surface.set_input_region(Some(region.wl_region()));
            }
            Err(error) => tracing::warn!("making the window's input region: {error}"),
        }
    }

    fn key(&mut self, event: &KeyEvent) {
        if self.dialog_key(event) || self.menu_key(event) {
            return;
        }
        if let Some(action) = self.shortcuts.action(event.keysym, self.modifiers) {
            self.act(action);
            return;
        }
        if self.search_key(event) {
            return;
        }
        let Some(terminal) = self.terminal() else {
            return;
        };
        let Modifiers { ctrl, alt, .. } = self.modifiers;
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
                bottom: self.config.scroll_on_keystroke,
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
                bottom: self.config.scroll_on_keystroke,
            });
        }
    }

    fn act(&mut self, action: Action) {
        match action {
            Action::NewWindow => new_window(),
            Action::CloseWindow => self.close(),
            Action::Copy => {
                if let Some(terminal) = self.terminal() {
                    let id = self.server.ask(Question::SelectionText { terminal });
                    self.asked.insert(id, Asked::Copy { primary: false });
                }
            }
            Action::Paste => self.paste(false),
            Action::SelectAll => self.select(Select::All),
            Action::Deselect => self.select(Select::Clear),
            Action::ZoomIn => self.zoom(Some(1)),
            Action::ZoomOut => self.zoom(Some(-1)),
            Action::ZoomReset => self.zoom(None),
            Action::Fullscreen => {
                if self.chrome.fullscreen {
                    self.window.unset_fullscreen();
                } else {
                    self.window.set_fullscreen(None);
                }
            }
            // The file being watched, the window takes the change from there.
            Action::TitleBar => config::write("title-bar", !self.config.title_bar),
            Action::NewTab => self.new_tab(),
            Action::Find => self.open_search(),
            Action::CloseTab => self.close_tab(self.selected),
            Action::NextTab => self.step_tab(1),
            Action::PreviousTab => self.step_tab(-1),
            Action::ShowTab(number) => {
                if let Some(index) = usize::from(number).checked_sub(1) {
                    self.show(index);
                }
            }
        }
    }

    /// Makes the font `steps` larger or smaller, or its size again.
    fn zoom(&mut self, steps: Option<i32>) {
        self.zoom = steps.map_or(0, |steps| {
            (self.zoom + steps).clamp(-ZOOM_STEPS, ZOOM_STEPS)
        });
        self.load_fonts();
        self.fit();
        self.dirty = true;
        self.draw();
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        if !focused {
            self.close_menu();
        }
        if let Some(terminal) = self.terminal() {
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
        // A fractional scale, where there is one, is the finer.
        if self.fractional.is_none() {
            self.set_scale(factor.unsigned_abs() * scale::WHOLE);
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
        self.close();
    }

    fn configure(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &Window,
        configure: WindowConfigure,
        _: u32,
    ) {
        self.chrome.drawn = configure.decoration_mode == DecorationMode::Client;
        self.chrome.fullscreen = configure.is_fullscreen();
        self.chrome.floating =
            !(configure.is_maximized() || configure.is_fullscreen() || configure.is_tiled());
        self.chrome.maximized = configure.is_maximized();
        self.chrome.active = configure.is_activated();
        let (width, height) = configure.new_size;
        let (old_width, old_height) = self.size.unwrap_or(DEFAULT_SIZE);
        self.size = Some((
            width.map_or(old_width, NonZeroU32::get),
            height.map_or(old_height, NonZeroU32::get),
        ));
        self.place();
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
        self.show_link();
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
            self.seat = Some(seat.clone());
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
            self.text_input = text_input::TextInput::new(self.text_inputs.as_ref(), &seat, qh);
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
