//! The terminal widget: one session, drawn by the GPU renderer into a texture at the
//! size of the screen's pixels, scrolled through its history by a `GtkScrolledWindow`
//! around it. Input, selection and the clipboard are in `input`.

use std::borrow::Cow;
use std::cell::{Cell, OnceCell, RefCell};
use std::collections::HashMap;
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::ExitStatus;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, LazyLock};
use std::time::{Duration, Instant};

use adw::prelude::*;
use adw::subclass::prelude::*;
use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::grid::{Dimensions, Scroll};
use alacritty_terminal::index::{Boundary, Column, Direction, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::search::RegexSearch;
use alacritty_terminal::term::{self, ClipboardType, Term, TermMode};
use alacritty_terminal::vte::ansi::{Color, CursorShape, CursorStyle, NamedColor, Processor, Rgb};
use gettextrs::gettext;
use glib::subclass::Signal;

use crate::fonts::{Bitmap, Fonts, GlyphKey, GlyphThread};
use crate::frame::{self, Look, Screen};
use crate::gtk::{cairo, graphene, pango};
use crate::palette::Palette;
use crate::renderer::{Quads, Renderer};
use crate::session::{Command, Session};
use crate::settings::{self, settings};
use crate::shell::Report;
use crate::{adw, gdk, gio, glib, gtk};

mod accessible;
mod input;

/// Room between the grid and the edges, in logical pixels.
const PADDING: f64 = 6.0;
/// Each zoom step scales the font by this much.
const ZOOM_STEP: f64 = 1.1;
/// Lines a search goes through while it holds the terminal.
const SEARCH_STEP: usize = 5000;
/// How long a frame rasterises glyphs itself before it leaves the rest to the glyph thread.
const GLYPH_TIME: Duration = Duration::from_millis(8);

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct TerminalView {
        /// The one shown: the name the user gave the tab, or else the program's title.
        pub title: RefCell<String>,
        pub program_title: RefCell<String>,
        pub name: RefCell<Option<String>>,
        pub default_title: RefCell<String>,
        pub hadjustment: RefCell<Option<gtk::Adjustment>>,
        pub vadjustment: RefCell<Option<gtk::Adjustment>>,
        pub adjustment_handler: RefCell<Option<glib::SignalHandlerId>>,
        /// Set while the widget moves the adjustment itself.
        pub updating_adjustment: Cell<bool>,
        /// Counts searches, so that one that is no longer the latest stops.
        pub searches: Arc<AtomicU64>,
        /// History, lines and offset from the last frame, for the scroll bar to catch up
        /// with once the frame is done.
        pub frame_scroll: Cell<Option<(usize, usize, usize)>>,

        /// What to run, until the first allocation says how big the grid is.
        pub command: RefCell<Option<Command>>,
        pub session: RefCell<Option<Session>>,
        /// Why nothing is shown: the program could not be started, or the GPU refused.
        /// Nothing is started or sent to the program while there is one.
        pub failure: RefCell<Option<String>>,
        /// A program named on the command line, whose failure stays on screen.
        pub named_program: RefCell<Option<String>>,
        pub exit_status: Cell<Option<ExitStatus>>,
        /// The program ended and the terminal asked to be closed.
        pub exited: Cell<bool>,

        pub gl: RefCell<Option<(gdk::GLContext, Renderer)>>,
        pub fonts: RefCell<Option<Fonts>>,
        /// What the fonts were made for: the font, the scale and the resolution.
        pub fonts_made_for: RefCell<Option<(String, u64, i32)>>,
        pub glyphs_stale: Cell<bool>,
        pub glyph_thread: RefCell<Option<GlyphThread>>,
        /// Glyphs back from the thread, for the next frame.
        pub finished_glyphs: RefCell<HashMap<GlyphKey, Option<Bitmap>>>,
        pub zoom: Cell<i32>,
        pub palette: Cell<Palette>,
        pub screen: RefCell<Screen>,
        pub quads: RefCell<Quads>,
        /// Columns and lines last given to the program.
        pub grid: Cell<(usize, usize)>,
        pub scale_handler: RefCell<Option<(gdk::Surface, glib::SignalHandlerId)>>,

        pub im: OnceCell<gtk::IMMulticontext>,
        pub focused: Cell<bool>,
        pub cursor_on: Cell<bool>,
        pub blink: RefCell<Option<glib::SourceId>>,

        pub pointer: Cell<(f64, f64)>,
        /// The cell of the screen the pointer is over.
        pub hovered_cell: Cell<Option<Point<usize>>>,
        /// The last frame underlined a link there.
        pub link_underlined: Cell<bool>,
        /// The button whose press went to the program, whose release must follow it.
        pub reported_button: Cell<Option<crate::encode::MouseButton>>,
        pub reported_cell: Cell<Option<(usize, usize)>>,
        pub selecting: Cell<bool>,
        /// Where the shell said it is, which is right where `/proc` is not: in a container
        /// or on another machine the shell was started from.
        pub shell_directory: RefCell<Option<PathBuf>>,
        /// The toolbox or distrobox containers the shell entered, innermost last, by name
        /// and tool.
        pub containers: RefCell<Vec<(String, String)>>,
        /// When the command the shell is running started, from its OSC 133 marks.
        pub command_started: Cell<Option<Instant>>,
        /// Tells views apart in notifications, which outlive them.
        pub serial: Cell<u64>,
        /// Where a selecting drag is, for scrolling on while it stays outside the text.
        pub drag_point: Cell<(f64, f64)>,
        pub autoscroll: RefCell<Option<glib::SourceId>>,
        /// Scrolling not yet worth a line.
        pub scrolled: Cell<f64>,
        pub popover: RefCell<Option<gtk::PopoverMenu>>,
        pub menu_link: RefCell<Option<String>>,
        pub handlers: RefCell<Vec<(glib::Object, glib::SignalHandlerId)>>,
        /// The screen as the last frame showed it, for screen readers.
        pub text: RefCell<accessible::ScreenText>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for TerminalView {
        const NAME: &'static str = "CaretTerminalView";
        type Type = super::TerminalView;
        type ParentType = gtk::Widget;
        type Interfaces = (gtk::Scrollable, gtk::AccessibleText);

        fn class_init(klass: &mut Self::Class) {
            klass.set_css_name("terminal");
            klass.set_accessible_role(gtk::AccessibleRole::Terminal);
            klass.install_action("term.copy", None, |view, _, _| view.copy());
            klass.install_action("term.paste", None, |view, _, _| view.paste());
            klass.install_action("term.select-all", None, |view, _, _| view.select_all());
            klass.install_action("term.deselect", None, |view, _, _| view.clear_selection());
            klass.install_action("term.open-link", None, |view, _, _| view.open_menu_link());
            klass.install_action("term.copy-link", None, |view, _, _| view.copy_menu_link());
        }
    }

    impl ObjectImpl for TerminalView {
        fn properties() -> &'static [glib::ParamSpec] {
            static PROPERTIES: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
                vec![
                    glib::ParamSpecString::builder("title").read_only().build(),
                    glib::ParamSpecOverride::for_interface::<gtk::Scrollable>("hadjustment"),
                    glib::ParamSpecOverride::for_interface::<gtk::Scrollable>("vadjustment"),
                    glib::ParamSpecOverride::for_interface::<gtk::Scrollable>("hscroll-policy"),
                    glib::ParamSpecOverride::for_interface::<gtk::Scrollable>("vscroll-policy"),
                ]
            });
            PROPERTIES.as_ref()
        }

        fn signals() -> &'static [Signal] {
            static SIGNALS: LazyLock<Vec<Signal>> = LazyLock::new(|| {
                vec![
                    Signal::builder("exited").build(),
                    Signal::builder("bell").build(),
                    Signal::builder("notification")
                        .param_types([String::static_type(), String::static_type()])
                        .build(),
                    Signal::builder("command-started").build(),
                    // How long it ran in seconds, and its exit status if the shell said.
                    Signal::builder("command-finished")
                        .param_types([u64::static_type(), i32::static_type(), bool::static_type()])
                        .build(),
                ]
            });
            SIGNALS.as_ref()
        }

        fn set_property(&self, _id: usize, value: &glib::Value, pspec: &glib::ParamSpec) {
            match pspec.name() {
                "hadjustment" => {
                    self.hadjustment
                        .replace(value.get().expect("hadjustment is an adjustment"));
                }
                "vadjustment" => self
                    .obj()
                    .set_vadjustment(value.get().expect("vadjustment is an adjustment")),
                // Scrolling policies do not apply: the grid fills whatever it is given.
                _ => {}
            }
        }

        fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
            match pspec.name() {
                "title" => self.title.borrow().to_value(),
                "hadjustment" => self.hadjustment.borrow().to_value(),
                "vadjustment" => self.vadjustment.borrow().to_value(),
                _ => gtk::ScrollablePolicy::Minimum.to_value(),
            }
        }

        fn constructed(&self) {
            self.parent_constructed();
            let obj = self.obj();
            obj.set_focusable(true);
            obj.set_overflow(gtk::Overflow::Hidden);
            obj.set_cursor_from_name(Some("text"));
            self.cursor_on.set(true);
            static SERIAL: AtomicU64 = AtomicU64::new(0);
            self.serial.set(SERIAL.fetch_add(1, Ordering::Relaxed));
            obj.update_palette();
            obj.setup_input();
            obj.watch_settings();
        }

        fn dispose(&self) {
            // Hangs up on the program.
            self.session.take();
            if let Some(popover) = self.popover.take() {
                popover.unparent();
            }
            if let Some(blink) = self.blink.take() {
                blink.remove();
            }
            if let Some(autoscroll) = self.autoscroll.take() {
                autoscroll.remove();
            }
            if let (Some(adjustment), Some(handler)) =
                (self.vadjustment.take(), self.adjustment_handler.take())
            {
                adjustment.disconnect(handler);
            }
            for (object, handler) in self.handlers.take() {
                object.disconnect(handler);
            }
        }
    }

    impl WidgetImpl for TerminalView {
        fn realize(&self) {
            self.parent_realize();
            let obj = self.obj();
            if let Some(surface) = obj.native().and_then(|native| native.surface()) {
                match renderer_for(&surface) {
                    Ok(gl) => {
                        self.gl.replace(Some(gl));
                        self.glyphs_stale.set(false);
                    }
                    Err(error) => {
                        glib::g_warning!("caret", "drawing with OpenGL: {error}");
                        self.failure.replace(Some(
                            gettext("The terminal cannot be drawn: %s").replace("%s", &error),
                        ));
                    }
                }
                let weak = obj.downgrade();
                let handler = surface.connect_scale_notify(move |_| {
                    if let Some(view) = weak.upgrade() {
                        view.queue_resize();
                    }
                });
                self.scale_handler.replace(Some((surface, handler)));
            }
            if let Some(im) = self.im.get() {
                im.set_client_widget(Some(&*obj));
            }
        }

        fn unrealize(&self) {
            if let Some((context, renderer)) = self.gl.take() {
                context.make_current();
                renderer.destroy();
                gdk::GLContext::clear_current();
            }
            if let Some((surface, handler)) = self.scale_handler.take() {
                surface.disconnect(handler);
            }
            if let Some(im) = self.im.get() {
                im.set_client_widget(None::<&gtk::Widget>);
            }
            self.parent_unrealize();
        }

        fn measure(&self, orientation: gtk::Orientation, _for_size: i32) -> (i32, i32, i32, i32) {
            let obj = self.obj();
            obj.ensure_fonts();
            let fonts = self.fonts.borrow();
            let Some(cell) = fonts.as_ref().map(|fonts| fonts.cell) else {
                return (0, 0, -1, -1);
            };
            let scale = obj.scale();
            let padding = padding(scale);
            let logical = |cells: i32, size: i32| {
                (f64::from(cells * size + 2 * padding) / scale).ceil() as i32
            };
            // A classic 80 by 24 when nothing else decides, and room to shrink to phone width.
            match orientation {
                gtk::Orientation::Horizontal => {
                    (logical(2, cell.width), logical(80, cell.width), -1, -1)
                }
                _ => (logical(1, cell.height), logical(24, cell.height), -1, -1),
            }
        }

        fn size_allocate(&self, width: i32, height: i32, baseline: i32) {
            self.parent_size_allocate(width, height, baseline);
            self.obj().update_grid();
            if let Some(popover) = self.popover.borrow().as_ref() {
                popover.present();
            }
        }

        fn snapshot(&self, snapshot: &gtk::Snapshot) {
            self.obj().draw(snapshot);
        }
    }

    impl ScrollableImpl for TerminalView {}
}

glib::wrapper! {
    pub struct TerminalView(ObjectSubclass<imp::TerminalView>)
        @extends gtk::Widget,
        @implements gtk::Accessible, gtk::AccessibleText, gtk::Buildable, gtk::ConstraintTarget,
                    gtk::Scrollable;
}

impl Default for TerminalView {
    fn default() -> Self {
        Self::new()
    }
}

fn padding(scale: f64) -> i32 {
    (PADDING * scale).round() as i32
}

fn renderer_for(surface: &gdk::Surface) -> Result<(gdk::GLContext, Renderer), String> {
    let context = surface
        .create_gl_context()
        .map_err(|error| error.to_string())?;
    context.set_allowed_apis(gdk::GLAPI::GL | gdk::GLAPI::GLES);
    context.realize().map_err(|error| error.to_string())?;
    context.make_current();
    let renderer = Renderer::new(&context)?;
    Ok((context, renderer))
}

fn rgba(color: Rgb) -> gdk::RGBA {
    gdk::RGBA::new(
        f32::from(color.r) / 255.0,
        f32::from(color.g) / 255.0,
        f32::from(color.b) / 255.0,
        1.0,
    )
}

/// `template` with each `%s` replaced by the next of `values`, which are not searched for
/// placeholders themselves.
fn fill(template: &str, values: &[&str]) -> String {
    let mut filled = String::with_capacity(template.len());
    let mut rest = template;
    for value in values {
        let Some((before, after)) = rest.split_once("%s") else {
            break;
        };
        filled.push_str(before);
        filled.push_str(value);
        rest = after;
    }
    filled.push_str(rest);
    filled
}

/// `text` as a regular expression that matches it literally.
fn literal(text: &str) -> String {
    let mut pattern = String::with_capacity(text.len());
    for c in text.chars() {
        if "\\.+*?()|[]{}^$#&-~".contains(c) {
            pattern.push('\\');
        }
        pattern.push(c);
    }
    pattern
}

impl TerminalView {
    pub fn new() -> Self {
        glib::Object::new()
    }

    pub fn title(&self) -> String {
        self.imp().title.borrow().clone()
    }

    fn set_title(&self, title: &str) {
        self.imp().program_title.replace(title.to_owned());
        self.show_title();
    }

    /// Names the tab, or with `None` lets it show the program's title again.
    pub fn set_name(&self, name: Option<String>) {
        self.imp().name.replace(name);
        self.show_title();
    }

    fn show_title(&self) {
        let imp = self.imp();
        let shown = imp
            .name
            .borrow()
            .clone()
            .unwrap_or_else(|| imp.program_title.borrow().clone());
        if *imp.title.borrow() != shown {
            imp.title.replace(shown);
            self.notify("title");
        }
    }

    pub fn connect_exited<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "exited",
            false,
            glib::closure_local!(move |view: &Self| f(view)),
        )
    }

    pub fn connect_bell<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "bell",
            false,
            glib::closure_local!(move |view: &Self| f(view)),
        )
    }

    /// Runs `command` once the widget has a size, so the program starts at the size it
    /// will be shown at.
    pub fn spawn(&self, mut command: Command) {
        let custom = settings().string("custom-command");
        if command.argv.is_empty() && !custom.trim().is_empty() {
            // A line that does not parse is run whole, so that the tab says it failed.
            command.argv = glib::shell_parse_argv(custom.as_str())
                .map(|argv| {
                    argv.iter()
                        .map(|word| word.to_string_lossy().into_owned())
                        .collect()
                })
                .unwrap_or_else(|_| vec![custom.to_string()]);
        }
        let program = command.program();
        let name = Path::new(&program)
            .file_name()
            .map_or(program.clone(), |name| name.to_string_lossy().into_owned());
        let imp = self.imp();
        imp.default_title.replace(name.clone());
        self.set_title(&name);
        imp.named_program.replace(command.argv.first().cloned());
        imp.command.replace(Some(command));
        self.queue_allocate();
    }

    /// The program ended and the tab was asked to close.
    pub fn has_exited(&self) -> bool {
        self.imp().exited.get()
    }

    /// The program in the foreground if it is not the one started, which closing the
    /// terminal would interrupt.
    pub fn busy(&self) -> Option<String> {
        self.imp().session.borrow().as_ref()?.busy()
    }

    pub fn directory(&self) -> Option<PathBuf> {
        let reported = self.imp().shell_directory.borrow().clone();
        reported
            .filter(|directory| directory.is_dir())
            .or_else(|| self.imp().session.borrow().as_ref()?.directory())
    }

    /// What a new tab from this one runs: the shell, where this one's is, in the same
    /// container.
    pub fn next_command(&self) -> Command {
        let argv = match self.imp().containers.borrow().last() {
            Some((name, runtime)) if ["toolbox", "distrobox"].contains(&runtime.as_str()) => {
                vec![runtime.clone(), "enter".to_owned(), name.clone()]
            }
            _ => Vec::new(),
        };
        Command {
            argv,
            directory: self.directory(),
            ..Command::default()
        }
    }

    pub fn serial(&self) -> u64 {
        self.imp().serial.get()
    }

    pub fn command_running(&self) -> bool {
        self.imp().command_started.get().is_some()
    }

    pub fn connect_notification<F: Fn(&Self, &str, &str) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "notification",
            false,
            glib::closure_local!(move |view: &Self, title: &str, body: &str| f(view, title, body)),
        )
    }

    pub fn connect_command_started<F: Fn(&Self) + 'static>(&self, f: F) -> glib::SignalHandlerId {
        self.connect_closure(
            "command-started",
            false,
            glib::closure_local!(move |view: &Self| f(view)),
        )
    }

    pub fn connect_command_finished<F: Fn(&Self, Duration, Option<i32>) + 'static>(
        &self,
        f: F,
    ) -> glib::SignalHandlerId {
        self.connect_closure(
            "command-finished",
            false,
            glib::closure_local!(move |view: &Self, seconds: u64, status: i32, known: bool| {
                f(view, Duration::from_secs(seconds), known.then_some(status))
            }),
        )
    }

    fn report(&self, report: Report) {
        let imp = self.imp();
        // While a command runs, what reaches the terminal is its output, which may be a
        // file or another machine's: only the shell says where it is.
        let from_command = imp.command_started.get().is_some();
        match report {
            Report::Directory(_) | Report::ContainerEntered { .. } | Report::ContainerLeft
                if from_command => {}
            Report::Directory(directory) => {
                imp.shell_directory.replace(Some(directory));
            }
            Report::Notification { .. } if !settings().boolean("program-notifications") => {}
            Report::Notification { title, body } => {
                self.emit_by_name::<()>("notification", &[&title, &body]);
            }
            Report::CommandStarted => {
                imp.command_started.set(Some(Instant::now()));
                self.emit_by_name::<()>("command-started", &[]);
            }
            Report::CommandFinished(status) => {
                if let Some(started) = imp.command_started.take() {
                    let seconds = started.elapsed().as_secs();
                    self.emit_by_name::<()>(
                        "command-finished",
                        &[&seconds, &status.unwrap_or_default(), &status.is_some()],
                    );
                }
            }
            Report::ContainerEntered { name, runtime } => {
                imp.containers.borrow_mut().push((name, runtime));
            }
            Report::ContainerLeft => {
                imp.containers.borrow_mut().pop();
            }
        }
    }

    fn scale(&self) -> f64 {
        self.native()
            .and_then(|native| native.surface())
            .map(|surface| surface.scale())
            .filter(|&scale| scale > 0.0)
            .unwrap_or_else(|| f64::from(self.scale_factor()))
    }

    /// The widget's top left corner in pixels of its surface.
    fn origin_on_screen(&self, scale: f64) -> (f64, f64) {
        let Some(native) = self.native() else {
            return (0.0, 0.0);
        };
        let point = self
            .compute_point(&native, &graphene::Point::zero())
            .unwrap_or_else(graphene::Point::zero);
        let (x, y) = native.surface_transform();
        (
            (f64::from(point.x()) + x) * scale,
            (f64::from(point.y()) + y) * scale,
        )
    }

    fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        if self.imp().failure.borrow().is_some() {
            return;
        }
        if let Some(session) = self.imp().session.borrow().as_ref() {
            session.write(bytes);
        }
    }

    /// Bytes the user typed or pasted: they bring the view back to the bottom and the
    /// cursor out of its blink.
    fn input(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        if self.imp().failure.borrow().is_some() {
            return;
        }
        if let Some(session) = self.imp().session.borrow().as_ref() {
            let mut term = session.lock();
            if term.grid().display_offset() != 0 && settings().boolean("scroll-on-keystroke") {
                term.scroll_display(Scroll::Bottom);
            }
            drop(term);
            session.write(bytes);
        }
        self.restart_blink();
        self.sync_scroll();
        self.queue_draw();
    }

    fn handle(&self, event: Event) {
        let imp = self.imp();
        match event {
            // The frame takes the terminal's lock once for all the output since the last one.
            Event::Wakeup => {
                if settings().boolean("scroll-on-output")
                    && let Some(session) = imp.session.borrow().as_ref()
                {
                    let mut term = session.lock();
                    if term.grid().display_offset() != 0 {
                        term.scroll_display(Scroll::Bottom);
                    }
                }
                self.queue_draw();
            }
            Event::Title(title) => self.set_title(&title),
            Event::ResetTitle => {
                let title = imp.default_title.borrow().clone();
                self.set_title(&title);
            }
            Event::ClipboardStore(ClipboardType::Clipboard, text) => {
                self.clipboard().set_text(&text)
            }
            Event::ClipboardStore(ClipboardType::Selection, text) => {
                self.primary_clipboard().set_text(&text);
            }
            Event::ColorRequest(index, format) => {
                let color = match index {
                    0..=255 => Color::Indexed(index as u8),
                    256 => Color::Named(NamedColor::Foreground),
                    257 => Color::Named(NamedColor::Background),
                    _ => Color::Named(NamedColor::Cursor),
                };
                let reply = imp.session.borrow().as_ref().map(|session| {
                    let term = session.lock();
                    format(imp.palette.get().resolve(color, term.colors()))
                });
                if let Some(reply) = reply {
                    self.write(reply.into_bytes());
                }
            }
            Event::PtyWrite(text) => self.write(text.into_bytes()),
            Event::TextAreaSizeRequest(format) => {
                if let Some(size) = self.window_size() {
                    self.write(format(size).into_bytes());
                }
            }
            Event::Bell => {
                if settings().boolean("audible-bell")
                    && let Some(surface) = self.native().and_then(|native| native.surface())
                {
                    surface.beep();
                }
                self.emit_by_name::<()>("bell", &[]);
            }
            Event::ChildExit(status) => imp.exit_status.set(Some(status)),
            // After the program's last output has been read.
            Event::Exit => self.program_ended(),
            // OSC 52 reads are refused by the terminal's configuration.
            _ => {}
        }
    }

    /// Closes the tab, unless a program named on the command line failed: then what it
    /// wrote stays, with how it ended under it.
    fn program_ended(&self) {
        let imp = self.imp();
        let failed = imp.exit_status.get().filter(|status| !status.success());
        let (Some(status), Some(program)) = (failed, imp.named_program.borrow().clone()) else {
            imp.exited.set(true);
            self.emit_by_name::<()>("exited", &[]);
            return;
        };
        let program: String = program.chars().filter(|c| !c.is_control()).collect();
        let message = match (status.code(), status.signal()) {
            (Some(code), _) => fill(
                &gettext("“%s” exited with status %s"),
                &[&program, &code.to_string()],
            ),
            (None, Some(signal)) => fill(
                &gettext("“%s” was ended by signal %s"),
                &[&program, &signal.to_string()],
            ),
            (None, None) => fill(&gettext("“%s” ended"), &[&program]),
        };
        if let Some(session) = imp.session.borrow().as_ref() {
            let mut term = session.lock();
            term.scroll_display(Scroll::Bottom);
            let newline = if term.grid().cursor.point.column.0 > 0 {
                "\r\n"
            } else {
                ""
            };
            // Bold, then the cursor hidden: nothing is left to type into.
            let text = format!("{newline}\x1b[1m{message}\x1b[0m\x1b[?25l");
            let mut parser: Processor = Processor::new();
            parser.advance(&mut *term, text.as_bytes());
        }
        self.sync_scroll();
        self.queue_draw();
    }

    fn term_config(&self) -> term::Config {
        let settings = settings();
        let shape = match settings.string("cursor-shape").as_str() {
            "beam" => CursorShape::Beam,
            "underline" => CursorShape::Underline,
            _ => CursorShape::Block,
        };
        let blinking = gtk::Settings::default().is_none_or(|gtk| gtk.is_gtk_cursor_blink());
        term::Config {
            scrolling_history: settings.uint("scrollback-lines") as usize,
            default_cursor_style: CursorStyle { shape, blinking },
            osc52: if settings.boolean("program-clipboard") {
                term::Osc52::OnlyCopy
            } else {
                term::Osc52::Disabled
            },
            ..term::Config::default()
        }
    }

    fn watch_settings(&self) {
        let imp = self.imp();
        let mut handlers = imp.handlers.borrow_mut();

        let settings = settings();
        let weak = self.downgrade();
        let handler = settings.connect_changed(None, move |_, key| {
            let Some(view) = weak.upgrade() else { return };
            match key {
                "font" | "font-features" => view.font_changed(),
                "ligatures" => view.queue_draw(),
                "cursor-shape" | "scrollback-lines" | "program-clipboard" => {
                    let config = view.term_config();
                    if let Some(session) = view.imp().session.borrow().as_ref() {
                        session.lock().set_options(config);
                    }
                    view.queue_draw();
                }
                _ => {}
            }
        });
        handlers.push((settings.upcast(), handler));

        let style = adw::StyleManager::default();
        let weak = self.downgrade();
        let handler = style.connect_dark_notify(move |_| {
            if let Some(view) = weak.upgrade() {
                view.update_palette();
            }
        });
        handlers.push((style.clone().upcast(), handler));
        let weak = self.downgrade();
        let handler = style.connect_monospace_font_name_notify(move |_| {
            if let Some(view) = weak.upgrade() {
                view.font_changed();
            }
        });
        handlers.push((style.upcast(), handler));

        if let Some(gtk_settings) = gtk::Settings::default() {
            for property in ["gtk-xft-dpi", "gtk-xft-hintstyle", "gtk-xft-antialias"] {
                let weak = self.downgrade();
                let handler = gtk_settings.connect_notify_local(Some(property), move |_, _| {
                    if let Some(view) = weak.upgrade() {
                        view.font_changed();
                    }
                });
                handlers.push((gtk_settings.clone().upcast(), handler));
            }
            let weak = self.downgrade();
            let handler = gtk_settings.connect_gtk_cursor_blink_notify(move |_| {
                if let Some(view) = weak.upgrade() {
                    let config = view.term_config();
                    if let Some(session) = view.imp().session.borrow().as_ref() {
                        session.lock().set_options(config);
                    }
                    view.restart_blink();
                }
            });
            handlers.push((gtk_settings.upcast(), handler));
        }
    }

    fn update_palette(&self) {
        let palette = if adw::StyleManager::default().is_dark() {
            Palette::dark()
        } else {
            Palette::light()
        };
        self.imp().palette.set(palette);
        self.queue_draw();
    }

    fn font_changed(&self) {
        self.imp().fonts_made_for.replace(None);
        self.queue_resize();
        self.queue_draw();
    }

    /// Makes the text bigger by `steps`, smaller for negative ones, or its own size again
    /// for none.
    pub fn zoom(&self, steps: Option<i32>) {
        let imp = self.imp();
        let zoom = steps.map_or(0, |steps| (imp.zoom.get() + steps).clamp(-8, 16));
        if zoom != imp.zoom.get() {
            imp.zoom.set(zoom);
            self.font_changed();
        }
    }

    /// Remakes the fonts when the font, the scale or the resolution changed.
    fn ensure_fonts(&self) {
        let imp = self.imp();
        let scale = self.scale();
        let gtk_settings = gtk::Settings::default();
        let dpi = gtk_settings
            .as_ref()
            .map(|settings| settings.gtk_xft_dpi())
            .filter(|&dpi| dpi > 0)
            .unwrap_or(96 * 1024);
        let mut font = settings::font();
        let factor = ZOOM_STEP.powi(imp.zoom.get());
        if font.is_size_absolute() {
            font.set_absolute_size(f64::from(font.size()) * factor * scale);
        } else {
            font.set_size((f64::from(font.size()) * factor).round() as i32);
        }
        let features = settings().string("font-features");
        let made_for = (format!("{font} {features}"), scale.to_bits(), dpi);
        if imp.fonts_made_for.borrow().as_ref() == Some(&made_for) {
            return;
        }
        let Ok(mut options) = cairo::FontOptions::new() else {
            return;
        };
        // Grey antialiasing: glyphs are masks tinted per cell, which subpixel colour
        // fringes would not survive.
        options.set_antialias(cairo::Antialias::Gray);
        options.set_hint_metrics(cairo::HintMetrics::On);
        let hinting = gtk_settings
            .as_ref()
            .and_then(|settings| settings.gtk_xft_hintstyle());
        options.set_hint_style(match hinting.as_deref() {
            Some("hintnone") => cairo::HintStyle::None,
            Some("hintslight") => cairo::HintStyle::Slight,
            Some("hintmedium") => cairo::HintStyle::Medium,
            Some("hintfull") => cairo::HintStyle::Full,
            _ => cairo::HintStyle::Default,
        });
        let resolution = f64::from(dpi) / 1024.0 * scale;
        imp.fonts
            .replace(Some(Fonts::new(&font, resolution, &options, &features)));
        imp.glyph_thread.take();
        imp.finished_glyphs.borrow_mut().clear();
        imp.fonts_made_for.replace(Some(made_for));
        imp.glyphs_stale.set(true);
    }

    /// Has the glyph thread rasterise `key`, starting it the first time, and draws again
    /// as glyphs come back.
    fn request_glyph(&self, fonts: &Fonts, key: &GlyphKey) {
        let mut thread = self.imp().glyph_thread.borrow_mut();
        let thread = thread.get_or_insert_with(|| {
            let (thread, results) = GlyphThread::spawn(fonts);
            glib::spawn_future_local(glib::clone!(
                #[weak(rename_to = view)]
                self,
                async move {
                    while let Ok((key, bitmap)) = results.recv().await {
                        // Closed when the fonts changed: what is left is in the old ones.
                        if results.is_closed() {
                            break;
                        }
                        let imp = view.imp();
                        if let Some(thread) = imp.glyph_thread.borrow_mut().as_mut() {
                            thread.received(&key);
                        }
                        imp.finished_glyphs.borrow_mut().insert(key, bitmap);
                        view.queue_draw();
                    }
                }
            ));
            thread
        });
        thread.request(key);
    }

    fn glyph_pending(&self, key: &GlyphKey) -> bool {
        let thread = self.imp().glyph_thread.borrow();
        thread.as_ref().is_some_and(|thread| thread.is_pending(key))
    }

    fn window_size(&self) -> Option<WindowSize> {
        let fonts = self.imp().fonts.borrow();
        let cell = fonts.as_ref()?.cell;
        let (columns, lines) = self.imp().grid.get();
        Some(WindowSize {
            num_lines: u16::try_from(lines).ok()?,
            num_cols: u16::try_from(columns).ok()?,
            cell_width: u16::try_from(cell.width).ok()?,
            cell_height: u16::try_from(cell.height).ok()?,
        })
    }

    /// Fits the grid to the allocation, and starts the program at the first one.
    fn update_grid(&self) {
        let imp = self.imp();
        if self.width() <= 0 || self.height() <= 0 || imp.failure.borrow().is_some() {
            return;
        }
        self.ensure_fonts();
        let Some(cell) = imp.fonts.borrow().as_ref().map(|fonts| fonts.cell) else {
            return;
        };
        let scale = self.scale();
        let padding = padding(scale);
        let width = (f64::from(self.width()) * scale).ceil() as i32 - 2 * padding;
        let height = (f64::from(self.height()) * scale).ceil() as i32 - 2 * padding;
        let columns = (width / cell.width).clamp(2, i32::from(u16::MAX)) as usize;
        let lines = (height / cell.height).clamp(1, i32::from(u16::MAX)) as usize;
        let previous = imp.grid.replace((columns, lines));
        let Some(size) = self.window_size() else {
            return;
        };

        let pending = imp.command.borrow_mut().take();
        if let Some(command) = pending {
            self.start(&command, size);
        } else if previous != (columns, lines)
            && let Some(session) = imp.session.borrow().as_ref()
        {
            session.resize(size);
            // The history can change with the width.
            glib::idle_add_local_once(glib::clone!(
                #[weak(rename_to = view)]
                self,
                move || view.sync_scroll()
            ));
        }
    }

    fn start(&self, command: &Command, size: WindowSize) {
        let imp = self.imp();
        match Session::spawn(command, size, self.term_config()) {
            Ok((session, events, reports)) => {
                session.lock().is_focused = imp.focused.get();
                imp.session.replace(Some(session));
                let weak = self.downgrade();
                glib::spawn_future_local(async move {
                    while let Ok(event) = events.recv().await {
                        let Some(view) = weak.upgrade() else { break };
                        view.handle(event);
                    }
                });
                let weak = self.downgrade();
                glib::spawn_future_local(async move {
                    while let Ok(report) = reports.recv().await {
                        let Some(view) = weak.upgrade() else { break };
                        view.report(report);
                    }
                });
            }
            Err(error) => {
                let error = error.to_string();
                let message = match command.argv.first() {
                    Some(program) => fill(
                        // Translators: the program, then why it could not start.
                        &gettext("Could not start “%s”: %s"),
                        &[program, &error],
                    ),
                    None => fill(&gettext("Could not start the shell: %s"), &[&error]),
                };
                imp.failure.replace(Some(message));
                self.queue_draw();
            }
        }
    }

    fn set_vadjustment(&self, adjustment: Option<gtk::Adjustment>) {
        let imp = self.imp();
        if let (Some(old), Some(handler)) = (imp.vadjustment.take(), imp.adjustment_handler.take())
        {
            old.disconnect(handler);
        }
        if let Some(adjustment) = &adjustment {
            let weak = self.downgrade();
            let handler = adjustment.connect_value_changed(move |adjustment| {
                if let Some(view) = weak.upgrade() {
                    view.scroll_to(adjustment.value());
                }
            });
            imp.adjustment_handler.replace(Some(handler));
        }
        imp.vadjustment.replace(adjustment);
        self.sync_scroll();
    }

    /// Scrolls the history to where the scroll bar was dragged, in lines from the top.
    fn scroll_to(&self, value: f64) {
        let imp = self.imp();
        if imp.updating_adjustment.get() {
            return;
        }
        if let Some(session) = imp.session.borrow().as_ref() {
            let mut term = session.lock();
            let offset = term.history_size().saturating_sub(value.round() as usize);
            let delta = offset as i32 - term.grid().display_offset() as i32;
            if delta != 0 {
                term.scroll_display(Scroll::Delta(delta));
            }
        }
        self.queue_draw();
    }

    /// Scrolls by `lines`, towards the history for positive ones.
    pub(super) fn scroll_lines(&self, lines: i32) {
        if let Some(session) = self.imp().session.borrow().as_ref() {
            session.lock().scroll_display(Scroll::Delta(lines));
        }
        self.sync_scroll();
        self.queue_draw();
    }

    /// Scrolls to put the previous or next prompt the shell marked at the top; past the
    /// last one, back to the bottom.
    pub fn scroll_to_prompt(&self, up: bool) {
        if let Some(session) = self.imp().session.borrow().as_ref() {
            let mut term = session.lock();
            let grid = term.grid();
            let columns = grid.columns();
            let marked = |line: i32| {
                (0..columns).any(|column| {
                    grid[Line(line)][Column(column)]
                        .hyperlink()
                        .is_some_and(|link| link.uri() == crate::shell::PROMPT)
                })
            };
            let starts =
                |line: i32| marked(line) && (line == term.topmost_line().0 || !marked(line - 1));
            let top = -(grid.display_offset() as i32);
            let found = if up {
                (term.topmost_line().0..top)
                    .rev()
                    .find(|&line| starts(line))
            } else {
                (top + 1..=term.bottommost_line().0).find(|&line| starts(line))
            };
            let scroll = match found {
                Some(line) => Scroll::Delta(-line - grid.display_offset() as i32),
                None if up => return,
                None => Scroll::Bottom,
            };
            term.scroll_display(scroll);
        }
        self.sync_scroll();
        self.queue_draw();
    }

    pub fn scroll_page(&self, up: bool) {
        if let Some(session) = self.imp().session.borrow().as_ref() {
            let scroll = if up { Scroll::PageUp } else { Scroll::PageDown };
            session.lock().scroll_display(scroll);
        }
        self.sync_scroll();
        self.queue_draw();
    }

    /// Puts the scroll bar where the view is in the history.
    fn sync_scroll(&self) {
        let imp = self.imp();
        let Some(adjustment) = imp.vadjustment.borrow().clone() else {
            return;
        };
        let Some(scroll) = imp.session.borrow().as_ref().map(|session| {
            let term = session.lock();
            (
                term.history_size(),
                term.screen_lines(),
                term.grid().display_offset(),
            )
        }) else {
            return;
        };
        self.show_scroll(&adjustment, scroll);
    }

    fn show_scroll(
        &self,
        adjustment: &gtk::Adjustment,
        (history, lines, offset): (usize, usize, usize),
    ) {
        let imp = self.imp();
        let lines = lines as f64;
        imp.updating_adjustment.set(true);
        adjustment.configure(
            history.saturating_sub(offset) as f64,
            0.0,
            history as f64 + lines,
            1.0,
            lines,
            lines,
        );
        imp.updating_adjustment.set(false);
    }

    /// Moves the scroll bar to where a frame found the view, after the frame: changing the
    /// adjustment while drawing would lay out the scroll bar in the middle of a snapshot.
    fn follow_scroll(&self, scroll: (usize, usize, usize)) {
        let imp = self.imp();
        if imp.frame_scroll.replace(Some(scroll)).is_some() {
            return;
        }
        glib::idle_add_local_once(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move || {
                let imp = view.imp();
                if let (Some(scroll), Some(adjustment)) =
                    (imp.frame_scroll.take(), imp.vadjustment.borrow().clone())
                {
                    view.show_scroll(&adjustment, scroll);
                }
            }
        ));
    }

    fn restart_blink(&self) {
        let imp = self.imp();
        if let Some(blink) = imp.blink.take() {
            blink.remove();
        }
        imp.cursor_on.set(true);
        let Some(gtk_settings) = gtk::Settings::default() else {
            return;
        };
        if !imp.focused.get() || !gtk_settings.is_gtk_cursor_blink() {
            return;
        }
        let half = u64::try_from(gtk_settings.gtk_cursor_blink_time() / 2).unwrap_or(600);
        let timeout = u64::try_from(gtk_settings.gtk_cursor_blink_timeout()).unwrap_or(0);
        let started = Instant::now();
        let weak = self.downgrade();
        let blink = glib::timeout_add_local(Duration::from_millis(half.max(50)), move || {
            let Some(view) = weak.upgrade() else {
                return glib::ControlFlow::Break;
            };
            let imp = view.imp();
            // Like GTK's own text, the cursor stops blinking a while after the last key.
            let done = timeout > 0 && started.elapsed() >= Duration::from_secs(timeout);
            imp.cursor_on.set(done || !imp.cursor_on.get());
            if imp.screen.borrow().cursor_blinks {
                view.queue_draw();
            }
            if done {
                imp.blink.take();
                glib::ControlFlow::Break
            } else {
                glib::ControlFlow::Continue
            }
        });
        imp.blink.replace(Some(blink));
    }

    fn draw(&self, snapshot: &gtk::Snapshot) {
        let imp = self.imp();
        let (width, height) = (self.width(), self.height());
        if width <= 0 || height <= 0 {
            return;
        }
        let palette = imp.palette.get();
        let bounds = graphene::Rect::new(0.0, 0.0, width as f32, height as f32);
        if let Some(message) = imp.failure.borrow().as_ref() {
            snapshot.append_color(&rgba(palette.background), &bounds);
            let layout = self.create_pango_layout(Some(message));
            layout.set_width((width - 24).max(1) * pango::SCALE);
            layout.set_wrap(pango::WrapMode::WordChar);
            snapshot.save();
            snapshot.translate(&graphene::Point::new(12.0, 12.0));
            snapshot.append_layout(&layout, &rgba(palette.foreground));
            snapshot.restore();
            return;
        }

        self.ensure_fonts();
        let session = imp.session.borrow();
        let mut gl = imp.gl.borrow_mut();
        let fonts = imp.fonts.borrow();
        let (Some(session), Some((context, renderer)), Some(fonts)) =
            (session.as_ref(), gl.as_mut(), fonts.as_ref())
        else {
            snapshot.append_color(&rgba(palette.background), &bounds);
            return;
        };

        let mut screen = imp.screen.borrow_mut();
        let term = session.lock();
        // Rearmed under the lock: output after the capture wakes the widget again.
        session.rearm();
        // A click on a link goes to a program that reports the mouse, so it is not marked.
        let link = imp
            .hovered_cell
            .get()
            .filter(|_| !term.mode().intersects(TermMode::MOUSE_MODE))
            .and_then(|point| input::link_in(&term, point));
        imp.link_underlined.set(link.is_some());
        screen.capture(&term, &palette, link.as_ref().map(|link| &link.span));
        let scroll = (
            term.history_size(),
            term.screen_lines(),
            term.grid().display_offset(),
        );
        drop(term);
        self.follow_scroll(scroll);
        self.update_accessible_text(&screen);
        let scale = self.scale();
        let look = Look {
            selection: palette.selection,
            focused: imp.focused.get(),
            cursor_on: imp.cursor_on.get() || !screen.cursor_blinks,
            padding: padding(scale),
            scale,
            ligatures: settings().boolean("ligatures"),
        };

        context.make_current();
        if imp.glyphs_stale.replace(false) {
            renderer.clear_glyphs(false);
        }
        let mut quads = imp.quads.borrow_mut();
        let mut finished = imp.finished_glyphs.take();
        let deadline = Instant::now() + GLYPH_TIME;
        let mut rasterize = |key: &GlyphKey| {
            if let Some(bitmap) = finished.remove(key) {
                Some(bitmap)
            } else if self.glyph_pending(key) {
                None
            } else if Instant::now() < deadline {
                Some(fonts.rasterize(key))
            } else {
                self.request_glyph(fonts, key);
                None
            }
        };
        let mut build = |renderer: &mut Renderer, quads: &mut Quads, partial| {
            frame::build(
                &screen,
                &look,
                fonts.cell,
                &|run, style| fonts.ligatures(run, style),
                &mut rasterize,
                renderer,
                quads,
                partial,
            )
        };
        let mut built = build(renderer, &mut quads, false);
        if built.is_err() {
            renderer.clear_glyphs(true);
            built = build(renderer, &mut quads, false);
        }
        if built.is_err() {
            // More glyphs than the largest atlas holds: what fits is shown.
            renderer.clear_glyphs(false);
            let _ = build(renderer, &mut quads, true);
        }

        // The texture starts on the screen pixel at or before the widget's corner, which
        // on a fractional scale can fall between pixels.
        let (x, y) = self.origin_on_screen(scale);
        let (x, y) = (x - x.floor(), y - y.floor());
        let pixels_wide = (x + f64::from(width) * scale).ceil() as i32;
        let pixels_high = (y + f64::from(height) * scale).ceil() as i32;
        let background = screen.background;
        match renderer.render(
            context,
            pixels_wide,
            pixels_high,
            [background.r, background.g, background.b],
            &quads,
        ) {
            Ok(texture) => {
                // One texel to one screen pixel on the pixel grid, so sampling copies them.
                let size = graphene::Rect::new(
                    (-x / scale) as f32,
                    (-y / scale) as f32,
                    (f64::from(pixels_wide) / scale) as f32,
                    (f64::from(pixels_high) / scale) as f32,
                );
                snapshot.push_clip(&bounds);
                snapshot.append_texture(&texture, &size);
                snapshot.pop();
            }
            Err(error) => {
                glib::g_warning!("caret", "drawing a frame: {error}");
                snapshot.append_color(&rgba(background), &bounds);
            }
        }

        if let (Some(cursor), Some(im)) = (screen.cursor, imp.im.get()) {
            let cell = fonts.cell;
            let x = f64::from(look.padding + cursor.column as i32 * cell.width) / scale;
            let y = f64::from(look.padding + cursor.line as i32 * cell.height) / scale;
            im.set_cursor_location(&gdk::Rectangle::new(
                x as i32,
                y as i32,
                (f64::from(cell.width) / scale).ceil() as i32,
                (f64::from(cell.height) / scale).ceil() as i32,
            ));
        }
    }

    pub fn has_selection(&self) -> bool {
        self.selection_text().is_some()
    }

    fn selection_text(&self) -> Option<String> {
        let session = self.imp().session.borrow();
        let text = session.as_ref()?.lock().selection_to_string()?;
        (!text.is_empty()).then_some(text)
    }

    pub fn copy(&self) {
        if let Some(text) = self.selection_text() {
            self.clipboard().set_text(&text);
        }
    }

    pub fn select_all(&self) {
        if let Some(session) = self.imp().session.borrow().as_ref() {
            let mut term = session.lock();
            let top = term.topmost_line();
            let blank = |line: Line| {
                let row = &term.grid()[line];
                (0..term.columns()).all(|column| row[Column(column)].c == ' ')
            };
            let mut bottom = term.bottommost_line();
            while bottom > top && blank(bottom) {
                bottom -= 1;
            }
            let mut selection = Selection::new(
                SelectionType::Simple,
                Point::new(top, Column(0)),
                Side::Left,
            );
            selection.update(Point::new(bottom, term.last_column()), Side::Right);
            term.selection = Some(selection);
        }
        self.queue_draw();
    }

    pub fn paste(&self) {
        self.paste_from(&self.clipboard());
    }

    fn paste_from(&self, clipboard: &gdk::Clipboard) {
        let clipboard = clipboard.clone();
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                if let Ok(Some(text)) = clipboard.read_text_future().await {
                    view.paste_text(text.as_bytes());
                }
            }
        ));
    }

    fn paste_text(&self, text: &[u8]) {
        let bracketed = self.imp().session.borrow().as_ref().is_some_and(|session| {
            session
                .lock()
                .mode()
                .contains(term::TermMode::BRACKETED_PASTE)
        });
        let lines = text.iter().any(|&byte| byte == b'\n' || byte == b'\r');
        if bracketed || !lines {
            self.input(crate::encode::paste(text, bracketed));
            return;
        }
        // Without brackets every line is entered as it arrives, a command run for each.
        let dialog = adw::AlertDialog::new(
            Some(&gettext("Paste Several Lines?")),
            Some(&gettext(
                "The program here does not mark pasted text, so each line runs as if typed and entered.",
            )),
        );
        dialog.add_responses(&[
            ("cancel", &gettext("_Cancel")),
            ("paste", &gettext("_Paste")),
        ]);
        dialog.set_response_appearance("paste", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let text = text.to_vec();
        dialog.choose(
            Some(self),
            gio::Cancellable::NONE,
            glib::clone!(
                #[weak(rename_to = view)]
                self,
                move |response| {
                    if response == "paste" {
                        view.input(crate::encode::paste(&text, false));
                    }
                    view.grab_focus();
                }
            ),
        );
    }

    pub fn clear_selection(&self) {
        self.imp().searches.fetch_add(1, Ordering::SeqCst);
        if let Some(session) = self.imp().session.borrow().as_ref() {
            session.lock().selection = None;
        }
        self.queue_draw();
    }

    /// Selects the next place `text` appears, upwards through the history or back down,
    /// and scrolls to it, searching on a worker thread; then `done` hears whether it
    /// appears anywhere. A later search or a cleared selection cancels it, and then `done`
    /// is never called.
    pub fn find(&self, text: &str, upwards: bool, done: impl FnOnce(bool) + 'static) {
        let imp = self.imp();
        let search = imp.searches.fetch_add(1, Ordering::SeqCst) + 1;
        let Some(term) = imp
            .session
            .borrow()
            .as_ref()
            .map(|session| session.term.clone())
        else {
            return done(false);
        };
        let regex = match RegexSearch::new(&literal(text)) {
            Ok(regex) if !text.is_empty() => regex,
            _ => {
                self.clear_selection();
                return done(false);
            }
        };
        let searches = imp.searches.clone();
        let task = gio::spawn_blocking(move || {
            search_history(&term, regex, upwards, || {
                searches.load(Ordering::SeqCst) != search
            })
        });
        glib::spawn_future_local(glib::clone!(
            #[weak(rename_to = view)]
            self,
            async move {
                let Ok(Some(found)) = task.await else { return };
                if view.imp().searches.load(Ordering::SeqCst) == search {
                    view.sync_scroll();
                    view.queue_draw();
                    done(found);
                }
            }
        ));
    }
}

/// Selects the next match of `regex` after the selection, or from the edge of the view,
/// going round the whole history, and scrolls to it. Whether there is one, or None when
/// `cancelled` says so between steps.
///
/// The lease is held throughout, so that no output moves the lines between steps, and the
/// terminal only for a step at a time; the main context takes it without the lease.
fn search_history<T: EventListener>(
    term: &FairMutex<Term<T>>,
    mut regex: RegexSearch,
    upwards: bool,
    cancelled: impl Fn() -> bool,
) -> Option<bool> {
    let _lease = term.lease();
    'search: loop {
        let (mut from, total, size) = {
            let term = term.lock_unfair();
            (
                search_origin(&term, upwards),
                term.total_lines(),
                (term.columns(), term.screen_lines()),
            )
        };
        let mut searched = 0;
        // One line more than the history, for the part of the first line before the origin.
        while searched <= total {
            let mut term = term.lock_unfair();
            if cancelled() {
                return None;
            }
            // A resize reflows the lines under the step.
            if (term.columns(), term.screen_lines()) != size {
                continue 'search;
            }
            let (found, to) = if upwards {
                let line = (from.line - SEARCH_STEP as i32).max(term.topmost_line());
                let to = term.line_search_left(Point::new(line, Column(0)));
                (term.regex_search_left(&mut regex, from, to), to)
            } else {
                let line = (from.line + SEARCH_STEP as i32).min(term.bottommost_line());
                let to = term.line_search_right(Point::new(line, term.last_column()));
                (term.regex_search_right(&mut regex, from, to), to)
            };
            if let Some(found) = found {
                let mut selection =
                    Selection::new(SelectionType::Simple, *found.start(), Side::Left);
                selection.update(*found.end(), Side::Right);
                term.selection = Some(selection);
                term.scroll_to_point(*found.start());
                return Some(true);
            }
            searched += from.line.0.abs_diff(to.line.0) as usize + 1;
            // Past the end of the history, the next step starts at the other end.
            from = if upwards {
                to.sub(&*term, Boundary::None, 1)
            } else {
                to.add(&*term, Boundary::None, 1)
            };
        }
        return Some(false);
    }
}

/// Where a search starts: next to the selection, or at the edge of the view it goes away
/// from.
fn search_origin<T>(term: &Term<T>, upwards: bool) -> Point {
    let offset = term.grid().display_offset() as i32;
    let current = term
        .selection
        .as_ref()
        .and_then(|selection| selection.to_range(term));
    let (origin, direction) = match current {
        Some(range) if upwards => (range.start.sub(term, Boundary::None, 1), Direction::Left),
        Some(range) => (range.end.add(term, Boundary::None, 1), Direction::Right),
        None if upwards => (
            Point::new(
                Line(term.screen_lines() as i32 - 1 - offset),
                term.last_column(),
            ),
            Direction::Left,
        ),
        None => (Point::new(Line(-offset), Column(0)), Direction::Right),
    };
    term.expand_wide(origin, direction)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alacritty_terminal::event::VoidListener;

    #[test]
    fn placeholders_in_values_are_left_alone() {
        let template = "Could not start “%s”: %s";
        assert_eq!(
            fill(template, &["echo %s", "not found"]),
            "Could not start “echo %s”: not found"
        );
        assert_eq!(fill("%s and %s", &["one"]), "one and %s");
    }

    #[test]
    fn searches_go_round_the_history_in_steps() {
        let config = term::Config {
            scrolling_history: 3 * SEARCH_STEP,
            ..term::Config::default()
        };
        let size = crate::session::GridSize {
            columns: 20,
            lines: 5,
        };
        let term = FairMutex::new(Term::new(config, &size, VoidListener));
        let mut parser: Processor = Processor::new();
        for line in 0..2 * SEARCH_STEP {
            let text = match line {
                100 => "a needle here".to_owned(),
                line if line == SEARCH_STEP + 100 => "needle".to_owned(),
                line => format!("hay {line}"),
            };
            parser.advance(&mut *term.lock(), format!("{text}\r\n").as_bytes());
        }
        let find = |text: &str, upwards: bool| {
            search_history(&term, RegexSearch::new(text).unwrap(), upwards, || false)
        };
        let selected = || {
            let term = term.lock();
            let range = term.selection.as_ref()?.to_range(&*term)?;
            Some((
                range.start.line.0 + term.history_size() as i32,
                range.start.column.0,
            ))
        };
        let needle = |line: usize, column| Some((line as i32, column));

        assert_eq!(find("needle", true), Some(true));
        assert_eq!(selected(), needle(SEARCH_STEP + 100, 0));
        assert_eq!(find("needle", true), Some(true));
        assert_eq!(selected(), needle(100, 2));
        assert_eq!(find("needle", true), Some(true));
        assert_eq!(selected(), needle(SEARCH_STEP + 100, 0));
        assert_eq!(find("needle", false), Some(true));
        assert_eq!(selected(), needle(100, 2));

        assert_eq!(find("thread", true), Some(false));
        let cancelled = search_history(&term, RegexSearch::new("hay").unwrap(), true, || true);
        assert_eq!(cancelled, None);
    }
}
