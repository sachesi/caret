//! A window of terminal tabs, with the find bar and the `win.*` actions.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use adw::prelude::*;
use adw::subclass::prelude::*;
use caret_protocol::{Answer, Command, Question, Request, SessionId, Tab, TerminalId};
use gettextrs::gettext;
use gtk::{gio, glib};

use crate::application::Application;
use crate::config;
use crate::connection::connection;
use crate::i18n::{gettext_f, ngettext_f};
use crate::settings::settings;
use crate::terminal_view::TerminalView;

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/sachesi/caret/ui/window.ui")]
    pub struct Window {
        #[template_child]
        pub tab_overview: TemplateChild<adw::TabOverview>,
        #[template_child]
        pub tab_view: TemplateChild<adw::TabView>,
        #[template_child]
        #[expect(
            clippy::struct_field_names,
            reason = "the template's id for its AdwWindowTitle"
        )]
        pub window_title: TemplateChild<adw::WindowTitle>,
        #[template_child]
        pub header_bar: TemplateChild<adw::HeaderBar>,
        #[template_child]
        pub search_bar: TemplateChild<gtk::SearchBar>,
        #[template_child]
        pub search_entry: TemplateChild<gtk::SearchEntry>,
        #[template_child]
        pub toast_overlay: TemplateChild<adw::ToastOverlay>,
        /// Closing was confirmed, or needs no confirmation.
        pub closing: Cell<bool>,
        /// The server's session the window shows, once it has one.
        pub session: Cell<Option<SessionId>>,
        /// The server closed the session.
        pub ended: Cell<bool>,
        /// The window goes but its terminals stay in the server.
        pub detaching: Cell<bool>,
        /// The window is adding a page itself, which needs no telling the server.
        pub adding: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for Window {
        const NAME: &'static str = "CaretWindow";
        type Type = super::Window;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.bind_template_callbacks();
            klass.install_action("win.new-tab", None, |window, _, _| {
                window.new_tab();
            });
            klass.install_action("win.detach", None, |window, _, _| {
                let imp = window.imp();
                imp.detaching.set(true);
                imp.closing.set(true);
                window.close();
            });
            klass.install_action("win.close-tab", None, |window, _, _| {
                let tab_view = &window.imp().tab_view;
                if let Some(page) = tab_view.selected_page() {
                    tab_view.close_page(&page);
                }
            });
            klass.install_action("win.close", None, |window, _, _| window.close());
            klass.install_action("win.rename-tab", None, |window, _, _| window.rename_tab());
            klass.install_action("win.copy", None, |window, _, _| {
                if let Some(view) = window.current_view() {
                    view.copy();
                }
            });
            klass.install_action("win.select-all", None, |window, _, _| {
                if let Some(view) = window.current_view() {
                    view.select_all();
                }
            });
            klass.install_action("win.deselect", None, |window, _, _| {
                if let Some(view) = window.current_view() {
                    view.clear_selection();
                }
            });
            klass.install_action("win.paste", None, |window, _, _| {
                if let Some(view) = window.current_view() {
                    view.paste();
                }
            });
            klass.install_action("win.find", None, |window, _, _| {
                let imp = window.imp();
                imp.search_bar.set_search_mode(true);
                imp.search_entry.grab_focus();
            });
            klass.install_action("win.tab-overview", None, |window, _, _| {
                window.imp().tab_overview.set_open(true);
            });
            klass.install_action("win.zoom-in", None, |window, _, _| window.zoom(Some(1)));
            klass.install_action("win.zoom-out", None, |window, _, _| window.zoom(Some(-1)));
            klass.install_action("win.zoom-reset", None, |window, _, _| window.zoom(None));
            klass.install_property_action("win.fullscreen", "fullscreened");
        }

        fn instance_init(obj: &glib::subclass::InitializingObject<Self>) {
            obj.init_template();
        }
    }

    impl ObjectImpl for Window {
        fn constructed(&self) {
            self.parent_constructed();
            if config::DEVEL {
                self.obj().add_css_class("devel");
            }
            // Only the tab shortcuts few programs want for themselves: Ctrl+Home and
            // Ctrl+End, say, stay with editors.
            self.tab_view.set_shortcuts(
                adw::TabViewShortcuts::CONTROL_PAGE_UP
                    | adw::TabViewShortcuts::CONTROL_PAGE_DOWN
                    | adw::TabViewShortcuts::CONTROL_SHIFT_PAGE_UP
                    | adw::TabViewShortcuts::CONTROL_SHIFT_PAGE_DOWN
                    | adw::TabViewShortcuts::ALT_DIGITS
                    | adw::TabViewShortcuts::ALT_ZERO,
            );
            let obj = self.obj();
            self.tab_view.connect_page_attached(glib::clone!(
                #[weak]
                obj,
                move |_, page, position| obj.page_attached(page, position)
            ));
            settings().connect_changed(
                Some("title-bar"),
                glib::clone!(
                    #[weak]
                    obj,
                    move |_, _| obj.update_header_bar()
                ),
            );
            obj.connect_fullscreened_notify(super::Window::update_header_bar);
            obj.update_header_bar();
            let (width, height) = settings().get::<(i32, i32)>("window-size");
            if width > 0 && height > 0 {
                self.obj().set_default_size(width, height);
            }
            if settings().boolean("window-maximized") {
                self.obj().maximize();
            }
            // libadwaita hides the content for a frame while it changes breakpoints, and the
            // terminal does not get the keyboard back from that; typing would go nowhere.
            self.obj().connect_current_breakpoint_notify(|window| {
                let Some(focus) = gtk::prelude::RootExt::focus(window) else {
                    return;
                };
                let focus = focus.downgrade();
                let frames = Cell::new(0);
                window.add_tick_callback(move |window, _| {
                    frames.set(frames.get() + 1);
                    if frames.get() < 3 {
                        return glib::ControlFlow::Continue;
                    }
                    if let Some(focus) = focus.upgrade()
                        && gtk::prelude::RootExt::focus(window).as_ref() != Some(&focus)
                    {
                        focus.grab_focus();
                    }
                    glib::ControlFlow::Break
                });
            });
        }
    }

    impl WidgetImpl for Window {}

    impl WindowImpl for Window {
        fn close_request(&self) -> glib::Propagation {
            let obj = self.obj();
            let detach = self.detaching.get() || settings().boolean("close-detaches");
            if !self.closing.get() && !detach {
                obj.ask_to_close();
                return glib::Propagation::Stop;
            }
            obj.save_size();
            if let (Some(session), Ok(connection)) = (self.session.get(), connection()) {
                connection.forget_window(session);
                // A window left with no tabs gave its last away, or the server closed
                // them: the session goes once it is empty.
                if !self.ended.get() {
                    let request = if detach || self.tab_view.n_pages() == 0 {
                        Request::Detach { session }
                    } else {
                        Request::CloseSession { session }
                    };
                    connection.send(&request);
                }
            }
            self.parent_close_request()
        }
    }

    impl ApplicationWindowImpl for Window {}
    impl AdwApplicationWindowImpl for Window {}

    #[gtk::template_callbacks]
    impl Window {
        #[template_callback]
        fn on_create_tab(&self) -> adw::TabPage {
            self.obj().new_tab()
        }

        /// The tab menu acts on the selected tab, so the tab it opens for is selected.
        #[template_callback]
        fn on_setup_menu(&self, page: Option<&adw::TabPage>) {
            if let Some(page) = page {
                self.tab_view.set_selected_page(page);
            }
        }

        #[template_callback]
        fn on_create_window(&self) -> adw::TabView {
            let application = self.obj().application().and_downcast::<Application>();
            let window = super::Window::with_session(application.as_ref());
            window.present();
            window.imp().tab_view.clone()
        }

        #[template_callback]
        fn on_close_page(&self, page: &adw::TabPage) -> bool {
            let obj = self.obj();
            let Some(view) = super::view_of(page) else {
                self.tab_view.close_page_finish(page, true);
                return true;
            };
            if view.has_exited() {
                self.tab_view.close_page_finish(page, true);
                return true;
            }
            let page = page.clone();
            view.busy(glib::clone!(
                #[weak]
                obj,
                #[weak]
                view,
                move |busy| match busy {
                    None => obj.close_page(&page, &view),
                    Some(program) => obj.confirm_close_tab(&page, &program),
                }
            ));
            true
        }

        #[template_callback]
        fn on_selected_page(&self) {
            let obj = self.obj();
            if let Some(page) = self.tab_view.selected_page() {
                page.set_needs_attention(false);
            }
            obj.update_title();
            if let Some(view) = obj.current_view() {
                view.grab_focus();
            }
        }

        #[template_callback]
        fn on_n_pages(&self) {
            if self.tab_view.n_pages() == 0 {
                self.closing.set(true);
                self.obj().close();
            }
        }

        #[template_callback]
        fn on_search_mode(&self) {
            if !self.search_bar.is_search_mode()
                && let Some(view) = self.obj().current_view()
            {
                view.clear_selection();
                view.grab_focus();
            }
        }

        #[template_callback]
        fn on_search_changed(&self) {
            if let Some(view) = self.obj().current_view() {
                view.clear_selection();
                let text = self.search_entry.text();
                let empty = text.is_empty();
                let window = self.obj().downgrade();
                view.find(&text, true, move |found| {
                    if let Some(window) = window.upgrade() {
                        window.imp().show_found(found || empty);
                    }
                });
            }
        }

        #[template_callback]
        fn on_search_up(&self) {
            self.search(true);
        }

        #[template_callback]
        fn on_search_down(&self) {
            self.search(false);
        }

        #[template_callback]
        fn on_stop_search(&self) {
            self.search_bar.set_search_mode(false);
        }
    }

    impl Window {
        fn search(&self, upwards: bool) {
            if let Some(view) = self.obj().current_view() {
                let window = self.obj().downgrade();
                view.find(&self.search_entry.text(), upwards, move |found| {
                    if let Some(window) = window.upgrade() {
                        window.imp().show_found(found);
                    }
                });
            }
        }

        fn show_found(&self, found: bool) {
            if found {
                self.search_entry.remove_css_class("error");
            } else {
                self.search_entry.add_css_class("error");
            }
        }
    }
}

glib::wrapper! {
    pub struct Window(ObjectSubclass<imp::Window>)
        @extends adw::ApplicationWindow, gtk::ApplicationWindow, gtk::Window, gtk::Widget,
        @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
                    gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

/// Commands that take this long tell when they end in a window out of sight.
const NOTIFY_AFTER: Duration = Duration::from_secs(10);

/// Outside the `clone!` that uses it, where xgettext would not find the strings.
fn finished_title(status: Option<i32>) -> String {
    match status {
        Some(0) | None => gettext("Command Finished"),
        Some(_) => gettext("Command Failed"),
    }
}

fn view_of(page: &adw::TabPage) -> Option<TerminalView> {
    page.child()
        .downcast::<gtk::ScrolledWindow>()
        .ok()?
        .child()
        .and_downcast::<TerminalView>()
}

impl Window {
    fn new(application: Option<&Application>) -> Self {
        glib::Object::builder()
            .property("application", application)
            .build()
    }

    /// A window with a new session of its own, running `commands` in tabs once the
    /// server made it.
    fn with_session(application: Option<&Application>) -> Self {
        let window = Self::new(application);
        match connection() {
            Ok(connection) => {
                let weak = window.downgrade();
                connection.ask(Question::NewSession, move |answer| {
                    let Some(window) = weak.upgrade() else { return };
                    match answer {
                        Answer::Session { session } => window.attach(session),
                        _ => window.session_closed(),
                    }
                });
            }
            Err(reason) => tracing::warn!("opening a window: {reason}"),
        }
        window
    }

    /// A window running `command` in its first tab.
    pub fn open(application: Option<&Application>, command: Command) -> Self {
        let window = Self::with_session(application);
        window.add_tab(command);
        window
    }

    /// A window showing a session the server has, with its tabs.
    pub fn reattach(application: Option<&Application>, session: SessionId) -> Self {
        let window = Self::new(application);
        window.attach(session);
        window
    }

    fn attach(&self, session: SessionId) {
        let imp = self.imp();
        imp.session.set(Some(session));
        let Ok(connection) = connection() else {
            return;
        };
        connection.register_window(session, self);
        connection.send(&Request::Attach { session });
        // Tabs opened, or dragged here, before the session came.
        let pages = imp.tab_view.pages();
        for index in 0..pages.n_items() {
            if let Some(page) = pages.item(index).and_downcast::<adw::TabPage>() {
                self.page_attached(&page, index as i32);
                if let Some(view) = view_of(&page) {
                    view.try_open();
                }
            }
        }
    }

    pub fn session(&self) -> Option<SessionId> {
        self.imp().session.get()
    }

    /// Where `view`'s tab is among the window's.
    pub fn position_of(&self, view: &TerminalView) -> u32 {
        let pages = self.imp().tab_view.pages();
        (0..pages.n_items())
            .find(|&index| {
                pages
                    .item(index)
                    .and_downcast::<adw::TabPage>()
                    .and_then(|page| view_of(&page))
                    .as_ref()
                    == Some(view)
            })
            .unwrap_or(0)
    }

    /// The server's tabs of the session, as the window attached to it.
    pub fn attached(&self, tabs: Vec<Tab>) {
        for (index, tab) in tabs.into_iter().enumerate() {
            self.tab_opened(index as u32, tab);
        }
    }

    fn page_of(&self, terminal: TerminalId) -> Option<adw::TabPage> {
        let pages = self.imp().tab_view.pages();
        (0..pages.n_items())
            .filter_map(|index| pages.item(index).and_downcast::<adw::TabPage>())
            .find(|page| view_of(page).is_some_and(|view| view.terminal() == Some(terminal)))
    }

    /// A tab of the session the window does not show yet: opened by another window of
    /// the same session, or there before this one attached.
    pub fn tab_opened(&self, index: u32, tab: Tab) {
        if self.page_of(tab.terminal).is_some() {
            return;
        }
        let view = TerminalView::new(tab.terminal, &tab.title, tab.name);
        let page = self.add_view(&view, Some(index));
        if self.imp().tab_view.selected_page().is_none() {
            self.imp().tab_view.set_selected_page(&page);
        }
    }

    pub fn tab_moved(&self, terminal: TerminalId, index: u32) {
        if let Some(page) = self.page_of(terminal) {
            let imp = self.imp();
            imp.adding.set(true);
            imp.tab_view.reorder_page(&page, index as i32);
            imp.adding.set(false);
        }
    }

    /// The server closed the session, with its last tab.
    pub fn session_closed(&self) {
        let imp = self.imp();
        imp.ended.set(true);
        imp.closing.set(true);
        self.close();
    }

    /// A page came to this window, perhaps dragged from another: the server moves its
    /// terminal to this window's session.
    fn page_attached(&self, page: &adw::TabPage, position: i32) {
        let imp = self.imp();
        if imp.adding.get() {
            return;
        }
        let (Some(session), Some(view)) = (imp.session.get(), view_of(page)) else {
            return;
        };
        let (Some(terminal), Ok(connection)) = (view.terminal(), connection()) else {
            return;
        };
        connection.send(&Request::MoveTab {
            terminal,
            session,
            index: position.max(0) as u32,
        });
        view.reshow();
    }

    /// A new tab running the shell where the current tab's is.
    fn new_tab(&self) -> adw::TabPage {
        let current = self.current_view();
        let view = TerminalView::opening(None);
        let page = self.add_view(&view, None);
        self.imp().tab_view.set_selected_page(&page);
        view.grab_focus();
        match current {
            Some(current) => current.next_command(move |command| view.set_command(command)),
            None => view.set_command(Command::default()),
        }
        page
    }

    pub fn current_view(&self) -> Option<TerminalView> {
        view_of(&self.imp().tab_view.selected_page()?)
    }

    /// A tab running `command`, opened by the server once the tab knows its size and the
    /// window its session.
    pub fn add_tab(&self, command: Command) -> adw::TabPage {
        let view = TerminalView::opening(Some(command));
        let page = self.add_view(&view, None);
        self.imp().tab_view.set_selected_page(&page);
        view.grab_focus();
        page
    }

    /// A page for `view`, after the selected one or at `index`.
    fn add_view(&self, view: &TerminalView, index: Option<u32>) -> adw::TabPage {
        let imp = self.imp();
        let tab_view = &imp.tab_view;
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(view)
            .build();
        imp.adding.set(true);
        let page = match (index, tab_view.selected_page()) {
            (Some(index), _) => tab_view.insert(&scrolled, (index as i32).min(tab_view.n_pages())),
            (None, Some(parent)) => tab_view.add_page(&scrolled, Some(&parent)),
            (None, None) => tab_view.append(&scrolled),
        };
        imp.adding.set(false);
        page.connect_title_notify(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |_| window.update_title()
        ));
        view.bind_property("title", &page, "title")
            .sync_create()
            .build();
        view.bind_property("title", &page, "tooltip")
            .sync_create()
            .build();
        view.connect_exited(glib::clone!(
            #[weak]
            tab_view,
            #[weak]
            page,
            move |_| tab_view.close_page(&page)
        ));
        view.connect_bell(glib::clone!(
            #[weak(rename_to = window)]
            self,
            #[weak]
            page,
            move |_| {
                if !page.is_selected() || !window.is_active() {
                    page.set_needs_attention(true);
                }
            }
        ));
        view.connect_command_started(glib::clone!(
            #[weak]
            page,
            move |view| {
                // Only commands that take a while show it, so the tab does not flicker.
                glib::timeout_add_local_once(
                    Duration::from_secs(1),
                    glib::clone!(
                        #[weak]
                        page,
                        #[weak]
                        view,
                        move || page.set_loading(view.command_running())
                    ),
                );
            }
        ));
        view.connect_command_finished(glib::clone!(
            #[weak(rename_to = window)]
            self,
            #[weak]
            page,
            move |view, took, status| {
                page.set_loading(false);
                let seen = page.is_selected() && window.is_active();
                if seen {
                    return;
                }
                page.set_needs_attention(true);
                if took >= NOTIFY_AFTER {
                    window.notify(view, &finished_title(status), &page.title());
                }
            }
        ));
        view.connect_notification(glib::clone!(
            #[weak(rename_to = window)]
            self,
            #[weak]
            page,
            move |view, title, body| {
                if page.is_selected() && window.is_active() {
                    return;
                }
                page.set_needs_attention(true);
                let title = if title.is_empty() {
                    page.title()
                } else {
                    title.into()
                };
                window.notify(view, &title, body);
            }
        ));
        page
    }

    fn rename_tab(&self) {
        let Some(view) = self.current_view() else {
            return;
        };
        let entry = gtk::Entry::builder()
            .text(view.title())
            .activates_default(true)
            .build();
        let dialog = adw::AlertDialog::builder()
            .heading(gettext("Rename Tab"))
            .body(gettext(
                "Without a name, the tab shows the program’s title.",
            ))
            .extra_child(&entry)
            .build();
        dialog.add_responses(&[
            ("cancel", &gettext("_Cancel")),
            ("rename", &gettext("_Rename")),
        ]);
        dialog.set_response_appearance("rename", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("rename"));
        dialog.set_close_response("cancel");
        // The dialog focuses its default response when it maps, after anything set before.
        entry.connect_map(|entry| {
            glib::idle_add_local_once(glib::clone!(
                #[weak]
                entry,
                move || {
                    entry.grab_focus();
                }
            ));
        });
        dialog.choose(Some(self), gio::Cancellable::NONE, move |response| {
            if response == "rename" {
                let name = entry.text().trim().to_owned();
                view.set_name((!name.is_empty()).then_some(name));
            }
            view.grab_focus();
        });
    }

    /// A desktop notification from a tab, which brings it back when clicked.
    fn notify(&self, view: &TerminalView, title: &str, body: &str) {
        let Some(application) = self.application() else {
            return;
        };
        let notification = gio::Notification::new(title);
        notification.set_body(Some(body));
        notification
            .set_default_action_and_target_value("app.show-tab", Some(&view.serial().to_variant()));
        application.send_notification(Some(&format!("tab-{}", view.serial())), &notification);
    }

    /// Selects the tab with the view numbered `serial` and brings its window up; false
    /// when this window has none.
    pub fn show_tab(&self, serial: u64) -> bool {
        let tab_view = &self.imp().tab_view;
        let pages = tab_view.pages();
        let page = (0..pages.n_items())
            .filter_map(|index| pages.item(index).and_downcast::<adw::TabPage>())
            .find(|page| view_of(page).is_some_and(|view| view.serial() == serial));
        let Some(page) = page else {
            return false;
        };
        tab_view.set_selected_page(&page);
        self.present();
        true
    }

    pub fn toast(&self, text: &str) {
        self.imp()
            .toast_overlay
            .add_toast(adw::Toast::builder().title(text).use_markup(false).build());
    }

    /// Whether the title bar is hidden, by the setting or by full screen.
    pub fn header_bar_hidden(&self) -> bool {
        !settings().boolean("title-bar") || self.is_fullscreen()
    }

    fn update_header_bar(&self) {
        self.imp().header_bar.set_visible(!self.header_bar_hidden());
    }

    fn zoom(&self, steps: Option<i32>) {
        if let Some(view) = self.current_view() {
            view.zoom(steps);
        }
    }

    fn update_title(&self) {
        let title = self
            .imp()
            .tab_view
            .selected_page()
            .map(|page| page.title().to_string())
            .filter(|title| !title.is_empty())
            .unwrap_or_else(|| gettext("Caret"));
        self.imp().window_title.set_title(&title);
        self.set_title(Some(&title));
    }

    /// Closes the window once no tab runs a program that closing would interrupt, or
    /// asks first.
    fn ask_to_close(&self) {
        let pages = self.imp().tab_view.pages();
        let views: Vec<TerminalView> = (0..pages.n_items())
            .filter_map(|index| pages.item(index).and_downcast::<adw::TabPage>())
            .filter_map(|page| view_of(&page))
            .collect();
        let left = Rc::new(Cell::new(views.len()));
        let busy = Rc::new(Cell::new(0));
        let answered = glib::clone!(
            #[weak(rename_to = window)]
            self,
            #[strong]
            left,
            #[strong]
            busy,
            move || {
                if left.get() > 0 {
                    return;
                }
                if busy.get() > 0 {
                    window.confirm_close(busy.get());
                } else {
                    window.imp().closing.set(true);
                    window.close();
                }
            }
        );
        if views.is_empty() {
            answered();
            return;
        }
        let answered = Rc::new(answered);
        for view in views {
            let (left, busy, answered) = (left.clone(), busy.clone(), answered.clone());
            view.busy(move |program| {
                left.set(left.get() - 1);
                if program.is_some() {
                    busy.set(busy.get() + 1);
                }
                answered();
            });
        }
    }

    /// Ends the tab's program and closes the page, which the user confirmed or which
    /// needed no confirmation.
    fn close_page(&self, page: &adw::TabPage, view: &TerminalView) {
        if let (Some(terminal), Ok(connection)) = (view.terminal(), connection()) {
            connection.send(&Request::CloseTab { terminal });
        }
        self.imp().tab_view.close_page_finish(page, true);
    }

    fn confirm_close_tab(&self, page: &adw::TabPage, program: &str) {
        let body = if program.is_empty() {
            gettext("A program is still running in this tab and will be stopped.")
        } else {
            gettext_f(
                "“{program}” is still running in this tab and will be stopped.",
                &[("program", program)],
            )
        };
        let dialog = adw::AlertDialog::new(Some(&gettext("Close Tab?")), Some(&body));
        dialog.add_responses(&[
            ("cancel", &gettext("_Cancel")),
            ("close", &gettext("C_lose")),
        ]);
        dialog.set_response_appearance("close", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let page = page.clone();
        dialog.choose(
            Some(self),
            gio::Cancellable::NONE,
            glib::clone!(
                #[weak(rename_to = window)]
                self,
                move |response| {
                    let Some(view) = view_of(&page) else { return };
                    // A program that ended meanwhile asked to close the tab, which had to wait.
                    if view.has_exited() {
                        window.imp().tab_view.close_page_finish(&page, true);
                    } else if response == "close" {
                        window.close_page(&page, &view);
                    } else {
                        window.imp().tab_view.close_page_finish(&page, false);
                    }
                }
            ),
        );
    }

    fn confirm_close(&self, busy: u32) {
        let dialog = adw::AlertDialog::new(
            Some(&gettext("Close Window?")),
            Some(&ngettext_f(
                "A program is still running in {count} tab and will be stopped.",
                "Programs are still running in {count} tabs and will be stopped.",
                busy,
                &[("count", &busy.to_string())],
            )),
        );
        dialog.add_responses(&[
            ("cancel", &gettext("_Cancel")),
            ("close", &gettext("C_lose")),
        ]);
        dialog.set_response_appearance("close", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        dialog.choose(
            Some(self),
            gio::Cancellable::NONE,
            glib::clone!(
                #[weak(rename_to = window)]
                self,
                move |response| {
                    if response == "close" {
                        window.imp().closing.set(true);
                        window.close();
                    }
                }
            ),
        );
    }

    fn save_size(&self) {
        let settings = settings();
        let maximized = self.is_maximized();
        if !maximized && !self.is_fullscreen() {
            let (width, height) = self.default_size();
            if let Err(err) = settings.set("window-size", (width, height)) {
                tracing::warn!("saving the window size {width}×{height}: {err}");
            }
        }
        if let Err(err) = settings.set_boolean("window-maximized", maximized) {
            tracing::warn!("saving whether the window is maximized: {err}");
        }
    }
}
