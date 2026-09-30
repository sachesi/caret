//! A window of terminal tabs, with the find bar and the `win.*` actions.

use std::cell::Cell;
use std::time::Duration;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::{gettext, ngettext};

use crate::application::CaretApplication;
use crate::session::Command;
use crate::settings::settings;
use crate::terminal_view::TerminalView;
use crate::{adw, gio, glib, gtk};

mod imp {
    use super::*;

    #[derive(Default, gtk::CompositeTemplate)]
    #[template(resource = "/io/github/sachesi/caret/ui/window.ui")]
    pub struct CaretWindow {
        #[template_child]
        pub tab_overview: TemplateChild<adw::TabOverview>,
        #[template_child]
        pub tab_view: TemplateChild<adw::TabView>,
        #[template_child]
        pub window_title: TemplateChild<adw::WindowTitle>,
        #[template_child]
        pub header_bar: TemplateChild<adw::HeaderBar>,
        #[template_child]
        pub search_bar: TemplateChild<gtk::SearchBar>,
        #[template_child]
        pub search_entry: TemplateChild<gtk::SearchEntry>,
        /// Closing was confirmed, or needs no confirmation.
        pub closing: Cell<bool>,
    }

    #[glib::object_subclass]
    impl ObjectSubclass for CaretWindow {
        const NAME: &'static str = "CaretWindow";
        type Type = super::CaretWindow;
        type ParentType = adw::ApplicationWindow;

        fn class_init(klass: &mut Self::Class) {
            klass.bind_template();
            klass.bind_template_callbacks();
            klass.install_action("win.new-tab", None, |window, _, _| {
                window.add_tab(window.next_command());
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

    impl ObjectImpl for CaretWindow {
        fn constructed(&self) {
            self.parent_constructed();
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
            settings().connect_changed(
                Some("title-bar"),
                glib::clone!(
                    #[weak]
                    obj,
                    move |_, _| obj.update_header_bar()
                ),
            );
            obj.connect_fullscreened_notify(|window| window.update_header_bar());
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

    impl WidgetImpl for CaretWindow {}

    impl WindowImpl for CaretWindow {
        fn close_request(&self) -> glib::Propagation {
            let obj = self.obj();
            if !self.closing.get() {
                let busy = obj.busy_tabs();
                if busy > 0 {
                    obj.confirm_close(busy);
                    return glib::Propagation::Stop;
                }
            }
            obj.save_size();
            self.parent_close_request()
        }
    }

    impl ApplicationWindowImpl for CaretWindow {}
    impl AdwApplicationWindowImpl for CaretWindow {}

    #[gtk::template_callbacks]
    impl CaretWindow {
        #[template_callback]
        fn on_create_tab(&self) -> adw::TabPage {
            let obj = self.obj();
            obj.add_tab(obj.next_command())
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
            let application = self.obj().application().and_downcast::<CaretApplication>();
            let window = super::CaretWindow::new(application.as_ref());
            window.present();
            window.imp().tab_view.clone()
        }

        #[template_callback]
        fn on_close_page(&self, page: &adw::TabPage) -> bool {
            let obj = self.obj();
            match super::view_of(page).and_then(|view| view.busy()) {
                None => self.tab_view.close_page_finish(page, true),
                Some(program) => obj.confirm_close_tab(page, &program),
            }
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

    impl CaretWindow {
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
    pub struct CaretWindow(ObjectSubclass<imp::CaretWindow>)
        @extends adw::ApplicationWindow, gtk::ApplicationWindow, gtk::Window, gtk::Widget,
        @implements gio::ActionGroup, gio::ActionMap, gtk::Accessible, gtk::Buildable,
                    gtk::ConstraintTarget, gtk::Native, gtk::Root, gtk::ShortcutManager;
}

/// Commands that take this long tell when they end in a window out of sight.
const NOTIFY_AFTER: Duration = Duration::from_secs(10);

fn view_of(page: &adw::TabPage) -> Option<TerminalView> {
    page.child()
        .downcast::<gtk::ScrolledWindow>()
        .ok()?
        .child()
        .and_downcast::<TerminalView>()
}

impl CaretWindow {
    /// What a new tab runs: the shell, where the current tab's is.
    pub fn next_command(&self) -> Command {
        self.current_view()
            .map(|view| view.next_command())
            .unwrap_or_default()
    }

    pub fn new(application: Option<&CaretApplication>) -> Self {
        glib::Object::builder()
            .property("application", application)
            .build()
    }

    pub fn current_view(&self) -> Option<TerminalView> {
        view_of(&self.imp().tab_view.selected_page()?)
    }

    pub fn add_tab(&self, command: Command) -> adw::TabPage {
        let tab_view = &self.imp().tab_view;
        let view = TerminalView::new();
        let scrolled = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&view)
            .build();
        let page = match tab_view.selected_page() {
            Some(parent) => tab_view.add_page(&scrolled, Some(&parent)),
            None => tab_view.append(&scrolled),
        };
        view.bind_property("title", &page, "title")
            .sync_create()
            .build();
        view.bind_property("title", &page, "tooltip")
            .sync_create()
            .build();
        page.connect_title_notify(glib::clone!(
            #[weak(rename_to = window)]
            self,
            move |_| window.update_title()
        ));
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
                    let title = match status {
                        Some(0) | None => gettext("Command Finished"),
                        Some(_) => gettext("Command Failed"),
                    };
                    window.notify(view, &title, &page.title());
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
        view.spawn(command);
        tab_view.set_selected_page(&page);
        view.grab_focus();
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

    fn busy_tabs(&self) -> u32 {
        let pages = self.imp().tab_view.pages();
        (0..pages.n_items())
            .filter_map(|index| pages.item(index).and_downcast::<adw::TabPage>())
            .filter(|page| view_of(page).is_some_and(|view| view.busy().is_some()))
            .count() as u32
    }

    fn confirm_close_tab(&self, page: &adw::TabPage, program: &str) {
        let body = if program.is_empty() {
            gettext("A program is still running in this tab and will be stopped.")
        } else {
            gettext("“%s” is still running in this tab and will be stopped.").replace("%s", program)
        };
        let dialog = adw::AlertDialog::new(Some(&gettext("Close Tab?")), Some(&body));
        dialog.add_responses(&[
            ("cancel", &gettext("_Cancel")),
            ("close", &gettext("C_lose")),
        ]);
        dialog.set_response_appearance("close", adw::ResponseAppearance::Destructive);
        dialog.set_default_response(Some("cancel"));
        dialog.set_close_response("cancel");
        let tab_view = self.imp().tab_view.clone();
        let page = page.clone();
        dialog.choose(Some(self), gio::Cancellable::NONE, move |response| {
            // A program that ended meanwhile asked to close the tab, which had to wait.
            let exited = view_of(&page).is_some_and(|view| view.has_exited());
            tab_view.close_page_finish(&page, response == "close" || exited);
        });
    }

    fn confirm_close(&self, busy: u32) {
        let dialog = adw::AlertDialog::new(
            Some(&gettext("Close Window?")),
            Some(
                &ngettext(
                    "A program is still running in %d tab and will be stopped.",
                    "Programs are still running in %d tabs and will be stopped.",
                    busy,
                )
                .replace("%d", &busy.to_string()),
            ),
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
            let _ = settings.set("window-size", (width, height));
        }
        let _ = settings.set_boolean("window-maximized", maximized);
    }
}
