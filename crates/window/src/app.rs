//! A window and the terminal it shows: the compositor's configures and frame callbacks,
//! the server's frames, keys, and the loop that waits for all of them.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::num::NonZeroU32;
use std::os::unix::ffi::OsStringExt;
use std::os::unix::process::CommandExt;
use std::time::{Duration, Instant};

use caret_protocol::encode::{self, Modifiers, Press};
use caret_protocol::{
    Answer, Bytes, Command, Message, Options, Question, Request, Scroll, Select, SessionId,
    Size, TerminalId,
};
use smithay_client_toolkit::compositor::{
    CompositorHandler, CompositorState, Region,
};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::calloop::generic::Generic;
use smithay_client_toolkit::reexports::calloop::{
    self, EventLoop, Interest, LoopHandle, Mode, PostAction,
};
use smithay_client_toolkit::reexports::calloop_wayland_source::WaylandSource;
use smithay_client_toolkit::reexports::protocols::xdg::activation::v1::client::xdg_activation_v1::XdgActivationV1;
use smithay_client_toolkit::reexports::protocols::xdg::system_bell::v1::client::xdg_system_bell_v1::XdgSystemBellV1;
use smithay_client_toolkit::reexports::protocols::wp::text_input::zv3::client::zwp_text_input_manager_v3::ZwpTextInputManagerV3;
use smithay_client_toolkit::reexports::client::globals::{BindError, GlobalList, registry_queue_init};
use smithay_client_toolkit::reexports::client::protocol::{
    wl_keyboard, wl_output, wl_seat, wl_surface,
};
use smithay_client_toolkit::reexports::client::{Connection, Proxy, QueueHandle};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::keyboard::{
    KeyEvent, KeyboardHandler, Keymap, Keysym, Modifiers as KeyModifiers, RawModifiers,
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
use crate::chrome::{self, Chrome, Dialog, Drawing, WindowButtons};
use crate::cli::{Attach, Start};
use crate::config::{self, Config, FontChoice};
use crate::connection::{self, Received};
use crate::desktop::{self, Desktop};
use crate::fonts::{self, FontError, Fonts};
use crate::gpu::{Gpu, GpuError};
use crate::grid::{Grid, packed};
use crate::notifications::Notifier;
use crate::palette::Palette;
use crate::panes::{Direction, Placed};
use crate::render::{RenderThread, Rendered, Scaling, Target};
use crate::renderer::{CURSOR_FOCUSED, CURSOR_WIDE, Constants, Job, PaneJob};
use crate::shortcuts::{Action, Shortcuts};
use crate::state::WindowState;

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
    #[error("starting the render thread: {0}")]
    Render(std::io::Error),
}

/// The cells a window fits when it opens without a size of its own to open at.
const DEFAULT_CELLS: (u32, u32) = (80, 24);

/// How much larger the font is for each step it is zoomed in, and the most steps.
const ZOOM: f32 = 1.1;
const ZOOM_STEPS: i32 = 12;

/// How long the window waits, before its first frame, for the desktop's preferences.
const DESKTOP_WAIT: Duration = Duration::from_millis(250);

/// Logical pixels between the window's edge and the cells.
const PADDING: u32 = 6;

mod activation;
mod attention;
mod blink;
mod clipboard;
mod confirm;
mod control;
mod menus;
mod pointer;
mod scale;
mod screen_reader;
mod search;
mod sessions;
mod shortcut_list;
mod tabs;
mod text_input;
mod toast;
mod touch;

/// A question the window is waiting for the answer to.
enum Asked {
    Session,
    /// A session for the tab with this key to move to, in a window of its own.
    Window(TerminalId),
    /// A session to show in the window, with a tab of its own.
    SessionHere,
    /// The sessions, to offer those the window does not show from its + button.
    NewMenu,
    /// A tab, opened at this size.
    Tab(Size),
    /// What a new tab runs, or a new pane, beside the focused one or under it.
    NextCommand(Option<bool>),
    /// A pane split from the focused one.
    Pane,
    /// Whether the text of the search this counts was found.
    Find(u32),
    /// The selected text, to offer as the clipboard or the primary selection.
    Copy {
        primary: bool,
    },
    /// The address under the pointer, to open.
    Link,
    /// Whether a program runs in the terminal, to close its pane or ask first.
    Busy(TerminalId),
    /// Whether a program runs in a pane of the tab with this key, to close it or ask
    /// first.
    TabBusy(TerminalId),
    /// Whether a program runs in one of the tabs, to close the window or ask first.
    Closing,
    /// The link where the terminal's menu opens, at this point of the window.
    Menu((f32, f32)),
    /// The sessions, to show those no window shows.
    Reattach,
    /// The sessions, to list in a menu.
    SessionList,
}

/// Fields drop in order: the device before the surface it draws to, and both before
/// the connection.
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is a separate part of the window's state"
)]
struct App {
    /// Ends first, letting go of the device before the surface goes.
    render: RenderThread,
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
    /// The sessions whose tabs the window shows, each one's together in this order: the
    /// first is the one the window started with.
    sessions: Vec<SessionId>,
    /// The names the user gave sessions.
    session_names: HashMap<SessionId, String>,
    /// The colours the user gave sessions, by their place among those there are.
    session_colors: HashMap<SessionId, u8>,
    /// The key of each session's tab shown last, to show again when the session is.
    last_tabs: HashMap<SessionId, TerminalId>,
    /// The session whose tabs the menu open lists.
    popup: Option<SessionId>,
    tabs: Vec<tabs::Tab>,
    selected: usize,
    /// The tab the window opened, to show when the server tells of it.
    opened: Option<TerminalId>,
    /// The pane the window split off, to focus when the server tells of it.
    opened_pane: Option<TerminalId>,
    /// The pane the window took out to a tab of its own, to show when the server
    /// tells of it.
    moved_pane: Option<TerminalId>,
    /// Keys move, resize and close panes until the mode is left.
    control: bool,
    /// The tab about to close once its panes say whether programs run in them.
    closing_tab: Option<confirm::TabClosing>,
    /// What the window's first tab runs, from the command line, until that is asked for.
    first: Option<Command>,
    /// The profile whose settings the window has over the others.
    profile: Option<String>,
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
    /// Counts the commands started, so that a command's timer marks only its own.
    commands: u32,
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
    /// The size it last had neither maximized nor tiled, to go back to and to keep
    /// for the next window.
    floating_size: Option<(u32, u32)>,
    /// Device pixels to the logical one, in 120ths.
    scale: u32,
    /// Fractional scaling, where the compositor offers it.
    fractional: Option<scale::Fractional>,
    /// The size of the grid the window fits, as last told to the server.
    grid_size: Option<Size>,
    keyboard: Option<wl_keyboard::WlKeyboard>,
    /// The layouts of the keyboard's keymap.
    layouts: Option<crate::keys::Layouts>,
    offered: Offered,
    /// Sends desktop notifications, from the first on.
    notifier: Option<Notifier>,
    text_input: Option<text_input::TextInput>,
    modifiers: Modifiers,
    /// The serial of the last key or button press, which taking a selection needs.
    serial: u32,
    blink: blink::Blink,
    pointer: pointer::Pointer,
    touch: touch::Touch,
    clipboard: clipboard::Clipboard,
    focused: bool,
    /// Something changed since the last frame was drawn.
    dirty: bool,
    /// A frame is with the render thread, which says when the compositor wants the next.
    waiting: bool,
    /// The atlas whose entries the render thread has, and how many of them.
    sent_glyphs: (u64, usize),
    exit: bool,
    /// Tells screen readers what the window shows.
    reader: accesskit_unix::Adapter,
    /// A screen reader listens, so is told of each frame.
    listened: bool,
}

/// Protocols the window uses where the compositor offers them.
struct Offered {
    cursor_shapes: Option<CursorShapeManager>,
    text_inputs: Option<ZwpTextInputManagerV3>,
    system_bell: Option<XdgSystemBellV1>,
    activation: Option<XdgActivationV1>,
}

impl Offered {
    fn bind(globals: &GlobalList, qh: &QueueHandle<App>) -> Self {
        Self {
            cursor_shapes: CursorShapeManager::bind(globals, qh).ok(),
            text_inputs: text_input::TextInput::bind(globals, qh),
            system_bell: attention::bind_bell(globals, qh),
            activation: activation::bind(globals, qh),
        }
    }
}

/// The window's toplevel, its frame drawn by the compositor if the settings ask and it
/// can, maximized if the last window was.
fn toplevel(
    compositor: &CompositorState,
    shell: &XdgShell,
    qh: &QueueHandle<App>,
    config: &Config,
    state: WindowState,
) -> Window {
    let decorations = if config.server_side_decorations {
        WindowDecorations::RequestServer
    } else {
        WindowDecorations::RequestClient
    };
    let window = shell.create_window(compositor.create_surface(qh), decorations, qh);
    window.set_title("Caret");
    window.set_app_id(crate::APP_ID);
    window.set_min_size(Some((360, 200)));
    if state.maximized {
        window.set_maximized();
    }
    window
}

#[expect(
    unsafe_code,
    reason = "Vulkan draws to the surface through its raw pointers"
)]
#[expect(
    clippy::too_many_lines,
    reason = "the window's state is set up field by field"
)]
pub fn run(start: Start) -> Result<(), AppError> {
    let connection = Connection::connect_to_env()?;
    let (globals, queue) = registry_queue_init::<App>(&connection)?;
    let qh = queue.handle();
    let compositor = CompositorState::bind(&globals, &qh)?;
    let shell = XdgShell::bind(&globals, &qh)?;
    let (mut server, received) = connection::Connection::open(crate::APP_ID)?;
    let (desktop_changes, desktop_changed) = calloop::channel::channel();
    let first_desktop = desktop::follow(desktop_changes);
    let profile = start.profile.clone();
    let config = Config::load(profile.as_deref());

    let window_state = WindowState::load();
    let window = toplevel(&compositor, &shell, &qh, &config, window_state);
    let fractional = scale::Fractional::new(&globals, &qh, window.wl_surface());
    window.commit();

    // Vulkan takes long to start, long enough to find the fonts meanwhile.
    let finding = std::thread::Builder::new()
        .name("fonts".to_owned())
        .spawn(fonts::installed);
    let display = connection.backend().display_ptr().cast();
    let surface = window.wl_surface().id().as_ptr().cast();
    // SAFETY: `App` holds the device, the window and the connection, and drops them in
    // that order.
    let gpu = unsafe { Gpu::new(display, surface)? };
    let (rendered_sender, rendered) = calloop::channel::channel();
    let render = RenderThread::spawn(
        gpu,
        connection.clone(),
        window.wl_surface().clone(),
        rendered_sender,
    )
    .map_err(AppError::Render)?;

    let desktop = first_desktop.recv_timeout(DESKTOP_WAIT).unwrap_or_default();
    let installed = finding
        .ok()
        .and_then(|finding| finding.join().ok())
        .unwrap_or_else(fonts::installed);
    let fonts = Fonts::new(
        installed,
        &config.font_for(desktop.monospace.as_deref()),
        1.0,
    )?;
    let palette = palette(&config, &desktop);
    server.send(&look(&config, &palette));
    let (asked, first) = begin(&mut server, start);

    let mut events: EventLoop<App> = EventLoop::try_new()?;
    let (pastes, pasted) = calloop::channel::channel();
    let (toaster, toasts) = calloop::channel::channel();
    let (reader, heard) = screen_reader::adapter();
    let clipboard = clipboard::Clipboard::new(
        DataDeviceManagerState::bind(&globals, &qh).ok(),
        PrimarySelectionManagerState::bind(&globals, &qh).ok(),
        pastes,
    );
    let mut app = App {
        registry: RegistryState::new(&globals),
        compositor,
        seats: SeatState::new(&globals, &qh),
        seat: None,
        outputs: OutputState::new(&globals, &qh),
        render,
        window,
        connection: connection.clone(),
        offered: Offered::bind(&globals, &qh),
        notifier: None,
        qh,
        events: events.handle(),
        server,
        asked,
        sessions: Vec::new(),
        session_names: HashMap::new(),
        session_colors: HashMap::new(),
        last_tabs: HashMap::new(),
        popup: None,
        tabs: Vec::new(),
        selected: 0,
        opened: None,
        opened_pane: None,
        moved_pane: None,
        control: false,
        closing_tab: None,
        first: Some(first),
        profile,
        searches: 0,
        confirming: None,
        choices: Vec::new(),
        toasts: 0,
        toaster,
        commands: 0,
        closing: None,
        fonts,
        zoom: 0,
        shortcuts: Shortcuts::new(&config.keys),
        atlas: Atlas::default(),
        grid: Grid::default(),
        palette,
        chrome: Chrome {
            buttons: window_buttons(&desktop),
            title: "Caret".to_owned(),
            header_bar: config.title_bar,
            dark: config.dark(desktop.dark),
            ..Chrome::default()
        },
        config,
        desktop,
        size: None,
        floating_size: window_state.size,
        scale: scale::WHOLE,
        fractional,
        grid_size: None,
        keyboard: None,
        layouts: None,
        text_input: None,
        modifiers: Modifiers::default(),
        serial: 0,
        blink: blink::Blink::default(),
        pointer: pointer::Pointer::default(),
        touch: touch::Touch::default(),
        clipboard,
        focused: false,
        dirty: true,
        waiting: false,
        sent_glyphs: (0, 0),
        exit: false,
        reader,
        listened: false,
    };
    app.activate_at_start();

    WaylandSource::new(connection, queue)
        .insert(events.handle())
        .map_err(|error| AppError::Loop(error.error))?;
    listen(&events.handle(), received, pasted, desktop_changed, toasts)?;
    events
        .handle()
        .insert_source(rendered, |event, (), app| {
            if let calloop::channel::Event::Msg(rendered) = event {
                app.rendered(rendered);
            }
        })
        .map_err(|error| AppError::Loop(error.error))?;
    events
        .handle()
        .insert_source(heard, |event, (), app| {
            if let calloop::channel::Event::Msg(heard) = event {
                app.heard(heard);
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

/// Asks the server for a new session, or to show the one `start` names, and what the
/// first tab of a new one runs.
fn begin(server: &mut connection::Connection, start: Start) -> (HashMap<u32, Asked>, Command) {
    let mut asked = HashMap::new();
    if let Attach::Session(session) = start.attach {
        server.send(&Request::Attach { session });
        return (asked, Command::default());
    }
    asked.insert(server.ask(Question::NewSession), Asked::Session);
    let directory = start.directory.map(|directory| {
        let directory = std::env::current_dir().unwrap_or_default().join(directory);
        Bytes(directory.into_os_string().into_vec())
    });
    let first = Command {
        hold: !start.command.is_empty(),
        argv: start.command,
        directory,
        env: Vec::new(),
    };
    (asked, first)
}

/// The rows that changed in each pane `shown`, drawn with its constants: `window`'s
/// cells for the focused pane of `tab`, the pane's own for the others.
fn pane_jobs(
    window: &mut Grid,
    tab: Option<&mut tabs::Tab>,
    shown: Vec<(TerminalId, Constants)>,
) -> Vec<PaneJob> {
    let mut window = Some(window);
    let mut others: HashMap<TerminalId, &mut Grid> = tab
        .map(|tab| {
            let focused = tab.focused;
            tab.panes
                .iter_mut()
                .filter(|(terminal, _)| **terminal != focused)
                .map(|(terminal, pane)| (*terminal, &mut pane.grid))
                .collect()
        })
        .unwrap_or_default();
    shown
        .into_iter()
        .filter_map(|(terminal, constants)| {
            let grid = others.remove(&terminal).or_else(|| window.take())?;
            let columns = grid.columns;
            let rows = grid
                .take_stale()
                .into_iter()
                .map(|line| {
                    (
                        line,
                        grid.cells[line * columns..(line + 1) * columns].to_vec(),
                    )
                })
                .collect();
            Some(PaneJob {
                terminal: terminal.0,
                columns,
                lines: grid.lines,
                rows,
                constants,
            })
        })
        .collect()
}

/// Opens another window, a program of its own, which takes the focus with `token`.
pub fn new_window(arguments: &[impl AsRef<OsStr>], token: Option<&str>) {
    let started = std::env::current_exe().and_then(|program| {
        let mut command = std::process::Command::new(program);
        command.args(arguments).process_group(0);
        match token {
            Some(token) => command.env("XDG_ACTIVATION_TOKEN", token),
            None => command.env_remove("XDG_ACTIVATION_TOKEN"),
        };
        command.spawn()
    });
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
        Palette::named(&config.dark_scheme, true)
    } else {
        Palette::named(&config.light_scheme, false)
    }
}

/// The buttons the desktop's layout asks for on the header bar.
fn window_buttons(desktop: &Desktop) -> WindowButtons {
    desktop
        .button_layout
        .as_deref()
        .map(WindowButtons::from_layout)
        .unwrap_or_default()
}

/// Tells the server how to show the window's terminals.
fn look(config: &Config, palette: &Palette) -> Request {
    Request::Look {
        palette: palette.colors(),
        options: options(config),
    }
}

/// How the settings want terminals to behave.
pub(crate) fn options(config: &Config) -> Options {
    Options {
        scrollback: config.scrollback_lines,
        cursor_shape: config.cursor_shape,
        cursor_blinks: true,
        copy_from_programs: config.program_clipboard,
        scroll_on_output: config.scroll_on_output,
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
                if desktop.icon_theme != app.desktop.icon_theme {
                    app.atlas.forget_icons();
                }
                app.desktop = desktop;
                app.restyle(&before);
                app.restart_blink();
                app.place_buttons();
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
                        app.reconfigure(Config::load(app.profile.as_deref()));
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
            Message::Answer { id, answer } => self.answered(id, answer),
            Message::Frame { terminal, frame } => self.tab_frame(terminal, frame),
            Message::Clipboard {
                terminal,
                primary,
                text,
            } if self.tab_of(terminal).is_some() => self.copy(text, primary),
            Message::Title { terminal, title } => self.tab_titled(terminal, title),
            Message::Bell { terminal } => self.bell(terminal),
            Message::Notification {
                terminal,
                title,
                body,
            } => self.program_notification(terminal, title, body),
            Message::CommandStarted { terminal } => self.command_started(terminal),
            Message::CommandFinished {
                terminal,
                seconds,
                status,
            } => {
                self.command_ended(terminal);
                self.command_finished(terminal, seconds, status);
            }
            Message::TabNamed { terminal, name } => self.tab_named(terminal, name),
            Message::TabMoved {
                session,
                terminal,
                index,
            } if self.sessions.contains(&session) => self.tab_moved(terminal, index),
            // A window started on a session it was handed learns it is shown here.
            Message::Attached { session, tabs }
                if self.sessions.is_empty() || self.sessions.contains(&session) =>
            {
                if !self.sessions.contains(&session) {
                    self.sessions.push(session);
                }
                for (index, tab) in (0..).zip(tabs) {
                    self.tab_opened(session, index, tab);
                }
                self.open_first_tab();
            }
            Message::TabOpened {
                session,
                index,
                tab,
            } if self.sessions.contains(&session) => self.tab_opened(session, index, tab),
            Message::TabClosed { session, terminal } if self.sessions.contains(&session) => {
                self.tab_closed(terminal);
            }
            Message::SessionClosed { session } if self.sessions.contains(&session) => {
                self.session_gone(session);
            }
            Message::Layout { session, tab, root } if self.sessions.contains(&session) => {
                self.tab_layout(tab, root);
            }
            Message::ShowSession { session } => self.show_session(session),
            Message::SessionNamed { session, name } => {
                match name {
                    Some(name) => self.session_names.insert(session, name),
                    None => self.session_names.remove(&session),
                };
                self.retitle();
            }
            Message::SessionColored { session, color } => {
                match color {
                    Some(color) => self.session_colors.insert(session, color),
                    None => self.session_colors.remove(&session),
                };
                self.retitle();
            }
            Message::SessionRemembered {
                session,
                tab,
                profile,
            } if self.sessions.contains(&session) => {
                self.take_up_session(session, tab, profile);
            }
            _ => {}
        }
    }

    /// Does what the window asked the question `id` for with its answer.
    fn answered(&mut self, id: u32, answer: Answer) {
        match (self.asked.remove(&id), answer) {
            (Some(Asked::Session), Answer::Session { session }) => {
                self.sessions.push(session);
                self.server.send(&Request::Attach { session });
                self.open_first_tab();
            }
            (Some(Asked::Window(key)), Answer::Session { session }) => {
                self.tab_into_window(key, session);
            }
            (Some(Asked::SessionHere), Answer::Session { session }) => {
                self.sessions.push(session);
                self.server.send(&Request::Attach { session });
                self.open_tab_in(session, Command::default());
            }
            (Some(Asked::NewMenu), Answer::SessionList { sessions }) => self.new_menu(&sessions),
            (Some(Asked::NextCommand(None)), Answer::Command { command }) => {
                self.open_tab(command);
            }
            (Some(Asked::NextCommand(None)), _) => self.open_tab(Command::default()),
            (Some(Asked::NextCommand(Some(vertical))), Answer::Command { command }) => {
                self.open_pane(vertical, command);
            }
            (Some(Asked::NextCommand(Some(vertical))), _) => {
                self.open_pane(vertical, Command::default());
            }
            (Some(Asked::Pane), Answer::Opened { terminal }) => {
                self.opened_pane = Some(terminal);
            }
            (Some(Asked::Find(search)), Answer::Found { found }) => self.found(search, found),
            (Some(Asked::Copy { primary }), Answer::Text { text }) => {
                if let Some(text) = text.filter(|text| !text.is_empty()) {
                    self.copy(text, primary);
                }
            }
            (Some(Asked::Tab(size)), Answer::Opened { terminal }) => {
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
            (Some(Asked::TabBusy(key)), answer) => self.tab_busy(key, answer),
            (Some(Asked::Closing), answer) => self.closing_busy(&answer),
            (Some(Asked::Menu(at)), Answer::Link { uri }) => self.terminal_menu(at, uri),
            (Some(Asked::Menu(at)), _) => self.terminal_menu(at, None),
            (Some(Asked::Reattach), Answer::Sessions { sessions }) => self.reattach(&sessions),
            (Some(Asked::SessionList), Answer::SessionList { sessions }) => {
                self.sessions_menu(&sessions);
            }
            (Some(Asked::Copy { .. } | Asked::Find(_) | Asked::Link | Asked::Pane) | None, _) => {}
            (Some(_), answer) => {
                tracing::error!("the server answered {answer:?}");
                self.exit = true;
            }
        }
    }

    /// Starts the atlas again with what is on screen once glyphs scrolled away fill it.
    fn renew_full_atlas(&mut self) {
        if !self.atlas.full {
            return;
        }
        tracing::debug!("starting the glyph atlas again");
        self.atlas = self.atlas.next();
        self.refill_shown();
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
    fn style(&self) -> (FontChoice, Request) {
        let font = self.config.font_for(self.desktop.monospace.as_deref());
        (font, look(&self.config, &self.palette))
    }

    /// Applies what changed of the window's style since it was `before`.
    fn place_buttons(&mut self) {
        self.chrome.buttons = window_buttons(&self.desktop);
        self.dirty = true;
        self.draw();
    }

    fn restyle(&mut self, before: &(FontChoice, Request)) {
        if self.config.font_for(self.desktop.monospace.as_deref()) != before.0 {
            // A font named in the settings may have been installed since.
            self.load_fonts(Some(fonts::installed()));
        }
        // Read again, as a scheme's file may have changed without the settings.
        let palette = palette(&self.config, &self.desktop);
        let look = look(&self.config, &palette);
        if look != before.1 || palette != self.palette {
            self.palette = palette;
            self.server.send(&look);
        }
        self.chrome.dark = self.config.dark(self.desktop.dark);
        self.fit();
        self.dirty = true;
        self.draw();
    }

    /// Loads the font the settings or the desktop name at the window's scale, among the
    /// fonts `installed` or those found before, drawing every glyph again.
    fn load_fonts(&mut self, installed: Option<fontdb::Database>) {
        let mut choice = self.config.font_for(self.desktop.monospace.as_deref());
        choice.points *= ZOOM.powi(self.zoom);
        let scale = self.factor() as f32;
        let loaded = match installed {
            Some(installed) => Fonts::new(installed, &choice, scale),
            None => self.fonts.at(&choice, scale),
        };
        match loaded {
            Ok(fonts) => {
                self.fonts = fonts;
                self.atlas = self.atlas.next();
                self.refill_shown();
            }
            Err(error) => tracing::error!("loading fonts at scale {}: {error}", self.factor()),
        }
    }

    /// Ends the window, and its session unless `detach` keeps it running for another
    /// window to show.
    fn end(&mut self, detach: bool) {
        WindowState {
            size: self.floating_size,
            maximized: self.chrome.maximized,
        }
        .save();
        for session in self.sessions.clone() {
            if detach {
                self.leave_session(session);
            } else {
                self.server.send(&Request::CloseSession { session });
            }
        }
        self.exit = true;
    }

    /// Asks for the window's first tab once there is a session and a size to give it.
    fn open_first_tab(&mut self) {
        if self.sessions.is_empty() || self.grid_size.is_none() {
            return;
        }
        if !self.tabs.is_empty() {
            return;
        }
        if let Some(first) = self.first.take() {
            self.open_tab(first);
        }
    }

    /// Fits the panes to the window, telling the server of those whose size changes.
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
        for index in 0..self.tabs.len() {
            self.resize_panes(index);
        }
        if self.grid_size != Some(size) {
            self.grid_size = Some(size);
            self.open_first_tab();
        }
    }

    /// The size in logical pixels that fits `columns` by `lines` cells.
    fn fitting(&self, (columns, lines): (u32, u32)) -> (u32, u32) {
        let cell = self.fonts.cell();
        let logical = |device: u32| (f64::from(device) / self.factor()).ceil() as u32;
        (
            logical(columns * cell.width.unsigned_abs()) + 2 * PADDING,
            logical(lines * cell.height.unsigned_abs()) + 2 * PADDING + self.chrome.top(),
        )
    }

    /// Device pixels to the logical one.
    fn factor(&self) -> f64 {
        f64::from(self.scale) / f64::from(scale::WHOLE)
    }

    /// The device pixels `logical` pixels take.
    fn device(&self, logical: u32) -> u32 {
        (f64::from(logical) * self.factor()).round() as u32
    }

    /// Where the focused pane's first cell's top left is, in device pixels from the
    /// surface's.
    fn origin(&self) -> (i32, i32) {
        let placed = self.placed();
        self.terminal()
            .and_then(|terminal| placed.rect(terminal))
            .or_else(|| self.area())
            .map_or((0, 0), |rect| (rect.x, rect.y))
    }

    /// Takes the scale the compositor prefers, in 120ths, and draws at it.
    fn set_scale(&mut self, scale: u32) {
        let scale = scale.max(1);
        if scale == self.scale {
            return;
        }
        self.scale = scale;
        self.load_fonts(None);
        self.fit();
        self.dirty = true;
        self.draw();
    }

    /// What `grid`'s pane at `origin` is drawn with in a surface `width` by `height`, its
    /// cursor filled if it is `focused`.
    fn constants(
        &self,
        grid: &Grid,
        (origin_x, origin_y): (i32, i32),
        focused: bool,
        (width, height): (u32, u32),
    ) -> Constants {
        let cell = self.fonts.cell();
        let (cursor_column, cursor_line, cursor_shape) = match grid.cursor {
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
                if focused && self.focused && !typing_in_bar {
                    shape |= CURSOR_FOCUSED;
                }
                if shape & CURSOR_FOCUSED != 0 && grid.cursor_blinks && self.blink.hidden {
                    (-1, -1, 0)
                } else {
                    (
                        i32::from(cursor.point.column),
                        i32::from(cursor.point.line),
                        shape,
                    )
                }
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
            columns: grid.columns as u32,
            pass: 0,
            cursor_column,
            cursor_line,
            cursor_shape,
            cursor_color: packed(grid.cursor_color),
            background: packed(grid.background),
            selection: packed(self.palette.selection),
            baseline: cell.baseline,
            underline_top: cell.underline,
            underline_thickness: cell.underline_thickness,
            strikeout_top: cell.strikeout,
            strikeout_thickness: cell.strikeout_thickness,
            first: 0,
        }
    }

    /// What the render thread says of the frame it was handed.
    fn rendered(&mut self, rendered: Rendered) {
        match rendered {
            Rendered::Ready => {
                self.waiting = false;
                self.draw();
            }
            Rendered::Failed(error) => {
                tracing::error!("drawing the window: {error}");
                self.exit = true;
            }
        }
    }

    /// Hands the render thread a frame if one is due.
    fn draw(&mut self) {
        let Some((width, height)) = self.size else {
            return;
        };
        if !self.dirty || self.waiting {
            return;
        }
        self.waiting = true;
        self.dirty = false;
        let started = Instant::now();
        let margin = 2 * self.chrome.margin();
        let (surface_width, surface_height) =
            (self.device(width + margin), self.device(height + margin));
        let placed = self.placed();
        let focused = self.terminal();
        let shown = self.shown_panes(&placed, (surface_width, surface_height));
        let constants = shown
            .iter()
            .find(|(terminal, _)| Some(*terminal) == focused)
            .map_or(shown[0].1, |&(_, constants)| constants);
        let background = self.grid.background;
        self.place_text_input();
        self.place_preedit();
        let (mut quads, mut under) = self.frame_quads((width, height));
        if self.atlas.full {
            self.renew_full_atlas();
            (quads, under) = self.frame_quads((width, height));
        }
        let dividers = self.dividers(&placed);
        let count = dividers.len();
        quads.splice(under..under, dividers);
        under += count;
        // Over the panes, and under the bars and dialogs.
        if let Some(ring) = self.control_ring(&placed) {
            quads.insert(under, ring);
        }
        quads.extend(self.landing_quad(&placed));
        // Around the window's frame, the surface is clear; without one, it is the
        // terminal's to its edges.
        let clear = if self.chrome.framed() {
            [0.0; 4]
        } else {
            let opacity = self.config.opacity;
            [
                f32::from(background.r) / 255.0 * opacity,
                f32::from(background.g) / 255.0 * opacity,
                f32::from(background.b) / 255.0 * opacity,
                opacity,
            ]
        };
        let panes = pane_jobs(&mut self.grid, self.tabs.get_mut(self.selected), shown);
        let (generation, sent) = self.sent_glyphs;
        let from = if generation == self.atlas.generation {
            sent
        } else {
            0
        };
        let job = Job {
            panes,
            generation: self.atlas.generation,
            entries: self.atlas.entries[from.min(self.atlas.entries.len())..].to_vec(),
            uploads: std::mem::take(&mut self.atlas.uploads),
            constants,
            quads,
            under,
            background: clear,
        };
        self.sent_glyphs = (self.atlas.generation, self.atlas.entries.len());
        let scaling = match &self.fractional {
            Some(fractional) => Scaling::Viewport {
                viewport: fractional.viewport.clone(),
                width: (width + margin) as i32,
                height: (height + margin) as i32,
            },
            None => Scaling::Buffer((self.scale / scale::WHOLE).max(1) as i32),
        };
        let target = Target {
            width: surface_width,
            height: surface_height,
            // Only a viewport can show part of a larger image as the whole window.
            roomy: self.chrome.resizing && self.fractional.is_some(),
            scaling,
            built: started.elapsed(),
        };
        self.render.draw(job, target);
        // Drawn, so ready for the next.
        self.ask_frame();
        self.tell_reader();
    }

    /// The window's frame and bars for a window `size` big, their text and icons put in
    /// the atlas.
    fn frame_quads(&mut self, size: (u32, u32)) -> (Vec<chrome::Quad>, usize) {
        let scale = self.factor() as f32;
        if let Some(search) = &mut self.chrome.search {
            search.field.measure(&self.fonts);
        }
        if let Some(menu) = &mut self.chrome.menu {
            let fonts = &self.fonts;
            menu.fit(|text| fonts.label_advance(text, false) / scale);
        }
        if let Some(field) = self
            .chrome
            .dialog
            .as_mut()
            .and_then(|dialog| dialog.field.as_mut())
        {
            field.measure(&self.fonts);
        }
        if let Some(dialog) = &mut self.chrome.dialog {
            let fonts = &self.fonts;
            let width = Dialog::text_width(size.0 as f32) * scale;
            dialog.lines =
                chrome::wrap(&dialog.body, width, |text| fonts.label_advance(text, false));
            dialog.line = fonts.label_height() as f32 / scale;
        }
        if let Some(list) = &mut self.chrome.shortcut_list {
            list.line = self.fonts.label_height() as f32 / scale;
        }
        let (atlas, fonts) = (&mut self.atlas, &mut self.fonts);
        let icon_theme = self.desktop.icon_theme.as_deref();
        let line = fonts.label_height();
        self.chrome.quads(
            size,
            scale,
            (packed(self.grid.background) << 8) | (self.config.opacity * 255.0).round() as u32,
            line,
            |drawing| {
                let index = atlas.drawing(drawing, || match drawing {
                    Drawing::Label { text, width, bold } => fonts.label(text, *width, *bold),
                    Drawing::Heading { text, width } => fonts.heading(text, *width),
                    Drawing::Icon(icon) => icon_theme
                        .and_then(|theme| chrome::themed_icon(*icon, scale, theme))
                        .or_else(|| Some(chrome::icon(*icon, scale))),
                });
                Some((index, atlas.entry(index)?))
            },
        )
    }

    /// Each pane shown and what it is drawn with, in a surface `size` big.
    fn shown_panes(&self, placed: &Placed, size: (u32, u32)) -> Vec<(TerminalId, Constants)> {
        let focused = self.terminal();
        let mut shown = Vec::new();
        for &(terminal, rect) in &placed.panes {
            let grid = match self.tabs.get(self.selected) {
                Some(tab) if tab.focused != terminal => match tab.panes.get(&terminal) {
                    Some(pane) => &pane.grid,
                    None => continue,
                },
                _ => &self.grid,
            };
            let is_focused = focused == Some(terminal);
            shown.push((
                terminal,
                self.constants(grid, (rect.x, rect.y), is_focused, size),
            ));
        }
        // Before the first tab, the window's empty cells.
        if shown.is_empty() {
            shown.push((
                TerminalId(0),
                self.constants(&self.grid, self.origin(), true, size),
            ));
        }
        shown
    }

    /// A line along the middle of each divider between the panes shown.
    fn dividers(&self, placed: &Placed) -> Vec<chrome::Quad> {
        let thickness = self.device(1).max(1) as f32;
        let color = if self.chrome.dark {
            0xffff_ff26
        } else {
            0x0000_0026
        };
        placed
            .dividers
            .iter()
            .map(|divider| {
                let gap = divider.gap;
                let (x, y, width, height) = if divider.vertical {
                    let middle = gap.x as f32 + (gap.width as f32 - thickness) / 2.0;
                    (middle, gap.y as f32, thickness, gap.height as f32)
                } else {
                    let middle = gap.y as f32 + (gap.height as f32 - thickness) / 2.0;
                    (gap.x as f32, middle, gap.width as f32, thickness)
                };
                chrome::Quad {
                    x,
                    y,
                    width,
                    height,
                    color,
                    ..chrome::Quad::default()
                }
            })
            .collect()
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

    fn key(&mut self, event: &KeyEvent, press: Press) {
        if press != Press::Up {
            self.restart_blink();
        }
        let latin = self.in_latin(event);
        let event = latin.as_ref().unwrap_or(event);
        if press != Press::Up {
            if self.dialog_key(event) || self.menu_key(event) || self.shortcut_list_key(event) {
                return;
            }
            if self.control {
                self.control_key(event);
                return;
            }
            if let Some(action) = self.shortcuts.action(event.keysym, self.modifiers)
                && self.act(action)
            {
                return;
            }
            if self.search_key(event) {
                return;
            }
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
            .filter(|text| !text.is_empty() && !text.chars().any(char::is_control))
            .filter(|_| crate::keys::named(event.keysym).is_none());
        let bytes = match crate::keys::key(event.keysym) {
            Some(key) => encode::key_event(key, self.modifiers, press, typed, self.grid.mode),
            // Text that no single key types, as from a compose sequence.
            None => typed
                .filter(|_| press != Press::Up && !ctrl && !alt)
                .map(|text| text.as_bytes().to_vec()),
        };
        if let Some(bytes) = bytes {
            self.server.send(&Request::Input {
                terminal,
                bytes: Bytes(bytes),
                bottom: press != Press::Up && self.config.scroll_on_keystroke,
            });
            if press == Press::Down {
                crate::timings::key();
            }
        }
    }

    /// `event` as the Latin letter its key types in another layout, where Ctrl is held
    /// and it types a letter of another script: Ctrl+C then interrupts, and shortcuts
    /// work, whatever the layout.
    fn in_latin(&self, event: &KeyEvent) -> Option<KeyEvent> {
        let typed = event.keysym.key_char()?;
        if !self.modifiers.ctrl || typed.is_ascii() || typed.is_control() {
            return None;
        }
        let keysym = self.layouts.as_ref()?.latin(event.raw_code)?;
        Some(KeyEvent {
            keysym,
            utf8: keysym.key_char().map(String::from),
            ..event.clone()
        })
    }

    /// Does what `action` says; false when it does nothing here and its keys go to the
    /// program.
    fn act(&mut self, action: Action) -> bool {
        match action {
            Action::NewWindow => self.new_window(self.profile.as_deref()),
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
            Action::NewSession => self.new_session(),
            Action::Find => self.open_search(),
            Action::CloseTab => self.close_tab(self.selected),
            Action::SplitRight => self.split(true),
            Action::SplitDown => self.split(false),
            Action::ClosePane => {
                if let Some(terminal) = self.terminal() {
                    self.close_pane(terminal);
                }
            }
            Action::FocusLeft => return self.focus_towards(Direction::Left),
            Action::FocusRight => return self.focus_towards(Direction::Right),
            Action::FocusUp => return self.focus_towards(Direction::Up),
            Action::FocusDown => return self.focus_towards(Direction::Down),
            Action::ControlMode => self.enter_control(),
            Action::Sessions => self.ask_sessions(),
            Action::NextTab => self.step_tab(1),
            Action::PreviousTab => self.step_tab(-1),
            Action::ShowTab(number) => {
                let keys = self.session_keys();
                if let Some(index) = usize::from(number)
                    .checked_sub(1)
                    .and_then(|at| keys.get(at))
                    .and_then(|&key| self.tab_of(key))
                {
                    self.show(index);
                }
            }
            Action::SessionTabs => {
                if let Some(session) = self.shown_session() {
                    self.session_popup(session);
                }
            }
            Action::NextSession => self.step_session(1),
            Action::PreviousSession => self.step_session(-1),
            Action::ShowSession(number) => self.nth_session(usize::from(number)),
            Action::ShortcutList => self.open_shortcut_list(),
            Action::PageUp => return self.scroll_view(Scroll::Page { up: true }),
            Action::PageDown => return self.scroll_view(Scroll::Page { up: false }),
            Action::PreviousPrompt => return self.scroll_view(Scroll::Prompt { up: true }),
            Action::NextPrompt => return self.scroll_view(Scroll::Prompt { up: false }),
            Action::TerminalMenu => self.terminal_menu(self.cursor_menu_at(), None),
            Action::Preferences => self.open_preferences(),
        }
        true
    }

    /// Scrolls the focused pane's view as `scroll` says; false without one.
    fn scroll_view(&mut self, scroll: Scroll) -> bool {
        let Some(terminal) = self.terminal() else {
            return false;
        };
        self.server.send(&Request::Scroll { terminal, scroll });
        true
    }

    /// Makes the font `steps` larger or smaller, or its size again.
    fn zoom(&mut self, steps: Option<i32>) {
        self.zoom = steps.map_or(0, |steps| {
            (self.zoom + steps).clamp(-ZOOM_STEPS, ZOOM_STEPS)
        });
        self.load_fonts(None);
        self.fit();
        self.dirty = true;
        self.draw();
    }

    fn set_focus(&mut self, focused: bool) {
        self.focused = focused;
        self.reader.update_window_focus_state(focused);
        if !focused {
            self.close_menu();
        }
        self.restart_blink();
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

    /// Frame callbacks go to the render thread's queue.
    fn frame(&mut self, _: &Connection, _: &QueueHandle<Self>, _: &wl_surface::WlSurface, _: u32) {}

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
        self.chrome.resizing = configure.is_resizing();
        if self.chrome.active {
            self.attended();
        }
        let (width, height) = configure.new_size;
        let earlier = if self.chrome.floating {
            self.floating_size.or(self.size)
        } else {
            self.size
        };
        let (old_width, old_height) = earlier.unwrap_or_else(|| self.fitting(DEFAULT_CELLS));
        self.size = Some((
            width.map_or(old_width, NonZeroU32::get),
            height.map_or(old_height, NonZeroU32::get),
        ));
        if self.chrome.floating {
            self.floating_size = self.size;
        }
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

    fn update_keymap(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        keymap: Keymap<'_>,
    ) {
        self.layouts = crate::keys::Layouts::new(keymap.as_string());
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
        self.key(&event, Press::Down);
    }

    fn repeat_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.key(&event, Press::Repeat);
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        self.key(&event, Press::Up);
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
                        .offered
                        .cursor_shapes
                        .as_ref()
                        .map(|shapes| shapes.get_shape_device(&pointer, qh));
                    self.pointer.proxy = Some(pointer);
                }
                Err(error) => tracing::warn!("taking the pointer: {error}"),
            }
        }
        if capability == Capability::Touch && self.touch.proxy.is_none() {
            self.seat = Some(seat.clone());
            match self.seats.get_touch(qh, &seat) {
                Ok(touch) => self.touch.proxy = Some(touch),
                Err(error) => tracing::warn!("taking the touchscreen: {error}"),
            }
        }
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            self.text_input =
                text_input::TextInput::new(self.offered.text_inputs.as_ref(), &seat, qh);
            let keyboard = self.seats.get_keyboard_with_repeat(
                qh,
                &seat,
                None,
                self.events.clone(),
                Box::new(|app, _, event| app.key(&event, Press::Repeat)),
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
        if capability == Capability::Touch
            && let Some(touch) = self.touch.proxy.take()
        {
            touch.release();
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
