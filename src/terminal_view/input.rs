//! Keys, the input method, the mouse, scrolling, drops and the context menu.

use std::os::unix::ffi::OsStrExt;

use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line, Point, Side};
use alacritty_terminal::selection::{Selection, SelectionType};
use alacritty_terminal::term::cell::Flags;
use alacritty_terminal::term::{Term, TermMode, viewport_to_point};
use gettextrs::gettext;

use super::*;
use crate::encode::{self, Key, Modifiers, MouseAction, MouseButton};
use crate::gio;
use crate::links;
use crate::session::Listener;

fn modifiers(state: gdk::ModifierType) -> Modifiers {
    Modifiers {
        shift: state.contains(gdk::ModifierType::SHIFT_MASK),
        alt: state.contains(gdk::ModifierType::ALT_MASK),
        ctrl: state.contains(gdk::ModifierType::CONTROL_MASK),
    }
}

fn named_key(keyval: gdk::Key) -> Option<Key> {
    use gdk::Key as K;
    Some(match keyval {
        K::Return | K::KP_Enter | K::ISO_Enter => Key::Enter,
        K::Tab | K::ISO_Left_Tab | K::KP_Tab => Key::Tab,
        K::BackSpace => Key::Backspace,
        K::Escape => Key::Escape,
        K::Up | K::KP_Up => Key::Up,
        K::Down | K::KP_Down => Key::Down,
        K::Right | K::KP_Right => Key::Right,
        K::Left | K::KP_Left => Key::Left,
        K::Home | K::KP_Home => Key::Home,
        K::End | K::KP_End => Key::End,
        K::Insert | K::KP_Insert => Key::Insert,
        K::Delete | K::KP_Delete => Key::Delete,
        K::Page_Up | K::KP_Page_Up => Key::PageUp,
        K::Page_Down | K::KP_Page_Down => Key::PageDown,
        K::F1 => Key::Function(1),
        K::F2 => Key::Function(2),
        K::F3 => Key::Function(3),
        K::F4 => Key::Function(4),
        K::F5 => Key::Function(5),
        K::F6 => Key::Function(6),
        K::F7 => Key::Function(7),
        K::F8 => Key::Function(8),
        K::F9 => Key::Function(9),
        K::F10 => Key::Function(10),
        K::F11 => Key::Function(11),
        K::F12 => Key::Function(12),
        _ => return None,
    })
}

fn mouse_button(button: u32) -> Option<MouseButton> {
    match button {
        gdk::BUTTON_PRIMARY => Some(MouseButton::Left),
        gdk::BUTTON_MIDDLE => Some(MouseButton::Middle),
        gdk::BUTTON_SECONDARY => Some(MouseButton::Right),
        _ => None,
    }
}

/// The address at a point of the view: the hyperlink a program gave the text there, and
/// with `written` also one written out in it.
fn link_in(term: &Term<Listener>, point: Point<usize>, written: bool) -> Option<String> {
    let point = viewport_to_point(term.grid().display_offset(), point);
    if let Some(link) = term.grid()[point].hyperlink() {
        return Some(link.uri().to_owned());
    }
    if !written {
        return None;
    }
    let grid = term.grid();

    let last = term.last_column();
    let wraps = |line: i32| grid[Line(line)][last].flags.contains(Flags::WRAPLINE);
    let mut first = point.line.0;
    while first > term.topmost_line().0 && wraps(first - 1) {
        first -= 1;
    }
    let mut end = point.line.0;
    while end < term.bottommost_line().0 && wraps(end) {
        end += 1;
    }
    let columns = term.columns();
    let mut text = Vec::with_capacity(columns * (end - first + 1) as usize);
    for line in first..=end {
        for column in 0..columns {
            let cell = &grid[Line(line)][Column(column)];
            let spacer = cell
                .flags
                .intersects(Flags::WIDE_CHAR_SPACER | Flags::LEADING_WIDE_CHAR_SPACER);
            text.push(if spacer { '\0' } else { cell.c });
        }
    }
    let index = (point.line.0 - first) as usize * columns + point.column.0;
    let range = links::address_at(&text, index)?;
    Some(text[range].iter().filter(|&&c| c != '\0').collect())
}

impl TerminalView {
    pub(super) fn setup_input(&self) {
        let imp = self.imp();
        let im = gtk::IMMulticontext::new();
        im.connect_commit(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_, text| view.input(text.as_bytes().to_vec())
        ));
        imp.im.set(im.clone()).ok();

        let keys = gtk::EventControllerKey::new();
        keys.set_im_context(Some(&im));
        keys.connect_key_pressed(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |_, keyval, keycode, state| view.key_pressed(keyval, keycode, state)
        ));
        self.add_controller(keys);

        let focus = gtk::EventControllerFocus::new();
        focus.connect_enter(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.focus_changed(true)
        ));
        focus.connect_leave(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |_| view.focus_changed(false)
        ));
        self.add_controller(focus);

        // Presses, with their count for word and line selection; the drag gesture beside
        // it follows the pointer until the release, which a click gesture gives up on
        // once the pointer moves.
        let click = gtk::GestureClick::new();
        click.set_button(0);
        click.connect_pressed(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |gesture, presses, x, y| view.pressed(gesture, presses, x, y)
        ));
        self.add_controller(click);

        let drag = gtk::GestureDrag::new();
        drag.set_button(0);
        drag.connect_drag_update(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |gesture, dx, dy| {
                if let Some((x, y)) = gesture.start_point() {
                    view.dragged(gesture, x + dx, y + dy);
                }
            }
        ));
        drag.connect_drag_end(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |gesture, dx, dy| {
                if let Some((x, y)) = gesture.start_point() {
                    view.released(gesture, x + dx, y + dy);
                }
            }
        ));
        self.add_controller(drag);

        let motion = gtk::EventControllerMotion::new();
        motion.connect_motion(glib::clone!(
            #[weak(rename_to = view)]
            self,
            move |controller, x, y| view.hovered(controller, x, y)
        ));
        self.add_controller(motion);

        let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::VERTICAL);
        scroll.connect_scroll(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[upgrade_or]
            glib::Propagation::Proceed,
            move |controller, _, dy| view.scrolled(controller, dy)
        ));
        self.add_controller(scroll);

        let drop = gtk::DropTarget::new(glib::Type::INVALID, gdk::DragAction::COPY);
        drop.set_types(&[gdk::FileList::static_type(), String::static_type()]);
        drop.connect_drop(glib::clone!(
            #[weak(rename_to = view)]
            self,
            #[upgrade_or]
            false,
            move |_, value, _, _| view.dropped(value)
        ));
        self.add_controller(drop);
    }

    fn mode(&self) -> TermMode {
        self.imp()
            .session
            .borrow()
            .as_ref()
            .map_or(TermMode::empty(), |session| *session.lock().mode())
    }

    fn key_pressed(
        &self,
        keyval: gdk::Key,
        keycode: u32,
        state: gdk::ModifierType,
    ) -> glib::Propagation {
        let modifiers = modifiers(state);
        if keyval == gdk::Key::Menu && modifiers == Modifiers::default() {
            let (x, y) = self.cursor_point();
            self.show_menu(x, y);
            return glib::Propagation::Stop;
        }
        if modifiers.shift && !modifiers.ctrl && !modifiers.alt {
            match keyval {
                gdk::Key::Page_Up | gdk::Key::KP_Page_Up => {
                    self.scroll_page(true);
                    return glib::Propagation::Stop;
                }
                gdk::Key::Page_Down | gdk::Key::KP_Page_Down => {
                    self.scroll_page(false);
                    return glib::Propagation::Stop;
                }
                _ => {}
            }
        }
        let key = match named_key(keyval) {
            Some(key) => key,
            None => match self.typed_char(keyval, keycode, modifiers) {
                Some(c) => Key::Char(c),
                None => return glib::Propagation::Proceed,
            },
        };
        match encode::key(key, modifiers, self.mode()) {
            Some(bytes) => {
                self.input(bytes);
                glib::Propagation::Stop
            }
            None => glib::Propagation::Proceed,
        }
    }

    /// The character of a key the input method did not take. With Ctrl held, a key of a
    /// non-Latin layout counts as the Latin letter it is in another layout, so Ctrl+C
    /// interrupts whatever the layout.
    fn typed_char(&self, keyval: gdk::Key, keycode: u32, modifiers: Modifiers) -> Option<char> {
        let c = keyval.to_unicode()?;
        if !modifiers.ctrl || c.is_ascii() {
            return Some(c);
        }
        self.display()
            .map_keycode(keycode)?
            .into_iter()
            .filter(|(key, _)| key.level() == 0)
            .filter_map(|(_, keyval)| keyval.to_unicode())
            .find(char::is_ascii_graphic)
            .or(Some(c))
    }

    fn focus_changed(&self, focused: bool) {
        let imp = self.imp();
        imp.focused.set(focused);
        if let Some(im) = imp.im.get() {
            if focused {
                im.focus_in();
            } else {
                im.focus_out();
            }
        }
        let report = imp.session.borrow().as_ref().is_some_and(|session| {
            let mut term = session.lock();
            term.is_focused = focused;
            term.mode().contains(TermMode::FOCUS_IN_OUT)
        });
        if report {
            self.write(encode::focus(focused));
        }
        self.restart_blink();
        self.queue_draw();
    }

    /// The middle of the cursor's cell in the widget, or its top left corner when the
    /// cursor is off the screen.
    fn cursor_point(&self) -> (f64, f64) {
        let imp = self.imp();
        let (Some(cursor), Some(cell)) = (
            imp.screen.borrow().cursor,
            imp.fonts.borrow().as_ref().map(|fonts| fonts.cell),
        ) else {
            return (0.0, 0.0);
        };
        let scale = self.scale();
        let padding = f64::from(padding(scale));
        let x = padding + (cursor.column as f64 + 0.5) * f64::from(cell.width);
        let y = padding + (cursor.line as f64 + 0.5) * f64::from(cell.height);
        (x / scale, y / scale)
    }

    /// The cell under a point of the widget, as a line of the screen and a column, and
    /// the half of the cell it is in. Points outside the grid go to its nearest cell.
    pub(super) fn cell_at(&self, x: f64, y: f64) -> Option<(Point<usize>, Side)> {
        let imp = self.imp();
        let cell = imp.fonts.borrow().as_ref()?.cell;
        let (columns, lines) = imp.grid.get();
        if columns == 0 || lines == 0 {
            return None;
        }
        let scale = self.scale();
        let padding = f64::from(padding(scale));
        let x = x * scale - padding;
        let y = y * scale - padding;
        let width = f64::from(cell.width);
        let column = (x / width).floor().clamp(0.0, (columns - 1) as f64) as usize;
        let line = (y / f64::from(cell.height))
            .floor()
            .clamp(0.0, (lines - 1) as f64) as usize;
        let side = if x - column as f64 * width < width / 2.0 {
            Side::Left
        } else {
            Side::Right
        };
        Some((Point::new(line, Column(column)), side))
    }

    /// Sends a mouse event to the program if it asked for them and Shift, which keeps
    /// the mouse for selecting, is not held.
    fn report_mouse(
        &self,
        button: MouseButton,
        action: MouseAction,
        x: f64,
        y: f64,
        state: gdk::ModifierType,
    ) -> bool {
        let modifiers = modifiers(state);
        let mode = self.mode();
        if modifiers.shift || !mode.intersects(TermMode::MOUSE_MODE) {
            return false;
        }
        let Some((point, _)) = self.cell_at(x, y) else {
            return false;
        };
        if action == MouseAction::Motion {
            let cell = (point.line, point.column.0);
            if self.imp().reported_cell.replace(Some(cell)) == Some(cell) {
                return true;
            }
        }
        if let Some(bytes) =
            encode::mouse(button, action, modifiers, point.column.0, point.line, mode)
        {
            self.write(bytes);
        }
        true
    }

    fn pressed(&self, gesture: &gtk::GestureClick, presses: i32, x: f64, y: f64) {
        let imp = self.imp();
        self.grab_focus();
        let state = gesture.current_event_state();
        let Some(button) = mouse_button(gesture.current_button()) else {
            return;
        };
        if self.report_mouse(button, MouseAction::Press, x, y, state) {
            imp.reported_button.set(Some(button));
            return;
        }
        match button {
            MouseButton::Left if state.contains(gdk::ModifierType::CONTROL_MASK) => {
                if let Some(link) = self.link_at(x, y) {
                    self.open(&link);
                }
            }
            MouseButton::Left => {
                let kind = match presses {
                    1 => SelectionType::Simple,
                    2 => SelectionType::Semantic,
                    _ => SelectionType::Lines,
                };
                let extend = presses == 1 && state.contains(gdk::ModifierType::SHIFT_MASK);
                self.select(kind, x, y, extend);
                imp.selecting.set(true);
            }
            MouseButton::Middle => self.paste_from(&self.primary_clipboard()),
            MouseButton::Right => self.show_menu(x, y),
            _ => {}
        }
    }

    fn dragged(&self, gesture: &gtk::GestureDrag, x: f64, y: f64) {
        let imp = self.imp();
        if let Some(button) = imp.reported_button.get() {
            self.report_mouse(
                button,
                MouseAction::Motion,
                x,
                y,
                gesture.current_event_state(),
            );
        } else if imp.selecting.get() {
            imp.drag_point.set((x, y));
            self.select(SelectionType::Simple, x, y, true);
            if self.autoscroll_lines() != 0 && imp.autoscroll.borrow().is_none() {
                let source = glib::timeout_add_local(
                    std::time::Duration::from_millis(50),
                    glib::clone!(
                        #[weak(rename_to = view)]
                        self,
                        #[upgrade_or]
                        glib::ControlFlow::Break,
                        move || view.autoscroll_step()
                    ),
                );
                imp.autoscroll.replace(Some(source));
            }
        }
    }

    /// Lines to scroll while a selecting drag is above the text (positive) or below it,
    /// more the further out it is.
    fn autoscroll_lines(&self) -> i32 {
        let Some(cell) = self.imp().fonts.borrow().as_ref().map(|fonts| fonts.cell) else {
            return 0;
        };
        let line = f64::from(cell.height) / self.scale();
        let y = self.imp().drag_point.get().1;
        let height = f64::from(self.height());
        if y < 0.0 {
            (-y / line).ceil() as i32
        } else if y > height {
            -((y - height) / line).ceil() as i32
        } else {
            0
        }
    }

    fn autoscroll_step(&self) -> glib::ControlFlow {
        let imp = self.imp();
        let lines = self.autoscroll_lines();
        if !imp.selecting.get() || lines == 0 {
            imp.autoscroll.take();
            return glib::ControlFlow::Break;
        }
        self.scroll_lines(lines);
        let (x, y) = imp.drag_point.get();
        self.select(SelectionType::Simple, x, y, true);
        glib::ControlFlow::Continue
    }

    fn released(&self, gesture: &gtk::GestureDrag, x: f64, y: f64) {
        let imp = self.imp();
        if let Some(button) = imp.reported_button.take() {
            self.report_mouse(
                button,
                MouseAction::Release,
                x,
                y,
                gesture.current_event_state(),
            );
            return;
        }
        if let Some(autoscroll) = imp.autoscroll.take() {
            autoscroll.remove();
        }
        if imp.selecting.replace(false)
            && let Some(text) = self.selection_text()
        {
            self.primary_clipboard().set_text(&text);
        }
    }

    fn hovered(&self, controller: &gtk::EventControllerMotion, x: f64, y: f64) {
        let imp = self.imp();
        imp.pointer.set((x, y));
        let state = controller.current_event_state();
        if imp.reported_button.get().is_none()
            && self.mode().contains(TermMode::MOUSE_MOTION)
            && !state.intersects(
                gdk::ModifierType::BUTTON1_MASK
                    | gdk::ModifierType::BUTTON2_MASK
                    | gdk::ModifierType::BUTTON3_MASK,
            )
        {
            self.report_mouse(MouseButton::None, MouseAction::Motion, x, y, state);
        }
        // With Ctrl, any address opens; without, a program's link still shows where it goes,
        // since its text may say otherwise.
        let ctrl = state.contains(gdk::ModifierType::CONTROL_MASK);
        let link = {
            let session = imp.session.borrow();
            let Some(session) = session.as_ref() else {
                return;
            };
            // While output is being read, the pointer keeps what it showed rather than
            // waiting on every move.
            let Some(term) = session.term.try_lock_unfair() else {
                return;
            };
            self.cell_at(x, y)
                .and_then(|(point, _)| link_in(&term, point, ctrl))
        };
        let reporting = self.mode().intersects(TermMode::MOUSE_MODE)
            && !state.contains(gdk::ModifierType::SHIFT_MASK);
        let cursor = if ctrl && link.is_some() {
            "pointer"
        } else if reporting {
            "default"
        } else {
            "text"
        };
        self.set_cursor_from_name(Some(cursor));
        if self.tooltip_text().as_deref() != link.as_deref() {
            self.set_tooltip_text(link.as_deref());
        }
    }

    /// Starts a selection of `kind` at a point, or with `extend` moves the end of the one
    /// there is.
    fn select(&self, kind: SelectionType, x: f64, y: f64, extend: bool) {
        let Some((point, side)) = self.cell_at(x, y) else {
            return;
        };
        if let Some(session) = self.imp().session.borrow().as_ref() {
            let mut term = session.lock();
            let point = viewport_to_point(term.grid().display_offset(), point);
            match term.selection.as_mut() {
                Some(selection) if extend => selection.update(point, side),
                _ => term.selection = Some(Selection::new(kind, point, side)),
            }
        }
        self.queue_draw();
    }

    fn scrolled(&self, controller: &gtk::EventControllerScroll, dy: f64) -> glib::Propagation {
        let imp = self.imp();
        let state = controller.current_event_state();
        let wheel = controller.unit() == gdk::ScrollUnit::Wheel;
        if state.contains(gdk::ModifierType::CONTROL_MASK) {
            if wheel && dy != 0.0 {
                self.zoom(Some(if dy < 0.0 { 1 } else { -1 }));
            }
            return glib::Propagation::Stop;
        }

        // Wheel notches are three lines, touchpad motion goes by the height of a line.
        let lines = if wheel {
            dy * 3.0
        } else {
            let height = imp
                .fonts
                .borrow()
                .as_ref()
                .map_or(16.0, |fonts| f64::from(fonts.cell.height));
            dy * self.scale() / height
        };
        let total = imp.scrolled.get() + lines;
        let whole = total.trunc();
        imp.scrolled.set(total - whole);
        let whole = whole as i32;
        if whole == 0 {
            return glib::Propagation::Stop;
        }

        let mode = self.mode();
        let (x, y) = imp.pointer.get();
        let button = if whole < 0 {
            MouseButton::WheelUp
        } else {
            MouseButton::WheelDown
        };
        if mode.intersects(TermMode::MOUSE_MODE) && !state.contains(gdk::ModifierType::SHIFT_MASK) {
            for _ in 0..whole.unsigned_abs() {
                self.report_mouse(button, MouseAction::Press, x, y, state);
            }
        } else if mode.contains(TermMode::ALT_SCREEN | TermMode::ALTERNATE_SCROLL)
            && !state.contains(gdk::ModifierType::SHIFT_MASK)
        {
            // Programs on the alternate screen have no history to scroll: they get arrows.
            let key = if whole < 0 { Key::Up } else { Key::Down };
            if let Some(bytes) = encode::key(key, Modifiers::default(), mode) {
                self.write(bytes.repeat(whole.unsigned_abs() as usize));
            }
        } else {
            self.scroll_lines(-whole);
        }
        glib::Propagation::Stop
    }

    fn dropped(&self, value: &glib::Value) -> bool {
        let text = if let Ok(files) = value.get::<gdk::FileList>() {
            files
                .files()
                .iter()
                .filter_map(|file| file.path())
                .map(|path| encode::shell_quoted(path.as_os_str().as_bytes()))
                .collect::<Vec<_>>()
                .join(&b' ')
        } else if let Ok(text) = value.get::<String>() {
            text.into_bytes()
        } else {
            return false;
        };
        self.paste_text(&text);
        self.grab_focus();
        true
    }

    /// The address under a point: a hyperlink a program gave the text, or one written
    /// out in it, followed across wrapped lines.
    fn link_at(&self, x: f64, y: f64) -> Option<String> {
        let (point, _) = self.cell_at(x, y)?;
        let session = self.imp().session.borrow();
        link_in(&session.as_ref()?.lock(), point, true)
    }

    fn open(&self, uri: &str) {
        let window = self.root().and_downcast::<gtk::Window>();
        let weak = self.downgrade();
        gtk::UriLauncher::new(uri).launch(window.as_ref(), gio::Cancellable::NONE, move |result| {
            let Err(error) = result else { return };
            let chose_not_to = error.matches(gtk::DialogError::Dismissed)
                || error.matches(gtk::DialogError::Cancelled)
                || error.matches(gio::IOErrorEnum::Cancelled);
            if let Some(view) = weak.upgrade().filter(|_| !chose_not_to) {
                view.toast(&fill(
                    &gettext("Could not open the link: %s"),
                    &[error.message()],
                ));
            }
        });
    }

    /// A message over the window the terminal is in.
    fn toast(&self, text: &str) {
        if let Some(overlay) = self
            .ancestor(adw::ToastOverlay::static_type())
            .and_downcast::<adw::ToastOverlay>()
        {
            overlay.add_toast(adw::Toast::builder().title(text).use_markup(false).build());
        }
    }

    fn show_menu(&self, x: f64, y: f64) {
        let imp = self.imp();
        let link = self.link_at(x, y);
        let menu = gio::Menu::new();
        let edit = gio::Menu::new();
        // The window's Ctrl+Shift shortcuts do the same; shown here, not bound twice.
        for (label, action, accel) in [
            (gettext("_Copy"), "term.copy", "<Control><Shift>c"),
            (gettext("_Paste"), "term.paste", "<Control><Shift>v"),
        ] {
            let item = gio::MenuItem::new(Some(&label), Some(action));
            item.set_attribute_value("accel", Some(&accel.to_variant()));
            edit.append_item(&item);
        }
        menu.append_section(None, &edit);
        if link.is_some() {
            let links = gio::Menu::new();
            links.append(Some(&gettext("_Open Link")), Some("term.open-link"));
            links.append(Some(&gettext("Copy _Link Address")), Some("term.copy-link"));
            menu.append_section(None, &links);
        }
        let window = gio::Menu::new();
        let title_bar = !self
            .root()
            .and_downcast::<crate::window::TangentWindow>()
            .is_some_and(|window| window.header_bar_hidden());
        if !title_bar {
            window.append(Some(&gettext("New _Window")), Some("app.new-window"));
        }
        window.append(Some(&gettext("New _Tab")), Some("win.new-tab"));
        window.append(Some(&gettext("_Find…")), Some("win.find"));
        // What the title bar holds, for when it is hidden.
        if !title_bar {
            window.append(Some(&gettext("Show _All Tabs")), Some("win.tab-overview"));
        }
        menu.append_section(None, &window);
        if !title_bar {
            let app = gio::Menu::new();
            app.append(Some(&gettext("Title _Bar")), Some("app.title-bar"));
            app.append(Some(&gettext("_Full Screen")), Some("win.fullscreen"));
            app.append(Some(&gettext("_Preferences")), Some("app.preferences"));
            app.append(Some(&gettext("_Keyboard Shortcuts")), Some("app.shortcuts"));
            app.append(Some(&gettext("_About Tangent")), Some("app.about"));
            menu.append_section(None, &app);
            let close = gio::Menu::new();
            close.append(Some(&gettext("_Close Window")), Some("win.close"));
            menu.append_section(None, &close);
        }
        imp.menu_link.replace(link);
        self.action_set_enabled("term.copy", self.has_selection());

        let popover = imp
            .popover
            .borrow_mut()
            .get_or_insert_with(|| {
                let popover = gtk::PopoverMenu::from_model(None::<&gio::MenuModel>);
                popover.set_parent(self);
                popover.set_has_arrow(false);
                popover.set_halign(gtk::Align::Start);
                popover
            })
            .clone();
        popover.set_menu_model(Some(&menu));
        popover.set_pointing_to(Some(&gdk::Rectangle::new(x as i32, y as i32, 1, 1)));
        popover.popup();
    }

    pub(super) fn open_menu_link(&self) {
        if let Some(link) = self.imp().menu_link.borrow().clone() {
            self.open(&link);
        }
    }

    pub(super) fn copy_menu_link(&self) {
        if let Some(link) = self.imp().menu_link.borrow().as_ref() {
            self.clipboard().set_text(link);
        }
    }
}
