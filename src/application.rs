use std::ops::ControlFlow;
use std::os::unix::ffi::OsStrExt;
use std::path::PathBuf;

use adw::prelude::*;
use adw::subclass::prelude::*;
use gettextrs::gettext;

use crate::session::Command;
use crate::settings::{self, settings};
use crate::window::TangentWindow;
use crate::{adw, config, gio, glib, gtk};

mod imp {
    use super::*;

    #[derive(Default)]
    pub struct TangentApplication {}

    #[glib::object_subclass]
    impl ObjectSubclass for TangentApplication {
        const NAME: &'static str = "TangentApplication";
        type Type = super::TangentApplication;
        type ParentType = adw::Application;
    }

    impl ObjectImpl for TangentApplication {}

    impl ApplicationImpl for TangentApplication {
        fn startup(&self) {
            self.parent_startup();
            settings::apply_color_scheme();
            settings().connect_changed(Some("color-scheme"), |_, _| settings::apply_color_scheme());
            self.obj().setup_actions();
        }

        /// D-Bus activation without arguments: a window with the shell at home.
        fn activate(&self) {
            self.obj().open_window(Command {
                directory: Some(glib::home_dir()),
                ..Command::default()
            });
        }

        fn handle_local_options(&self, options: &glib::VariantDict) -> ControlFlow<glib::ExitCode> {
            if options.contains("version") {
                println!("tangent {}", config::VERSION);
                return ControlFlow::Break(glib::ExitCode::SUCCESS);
            }
            ControlFlow::Continue(())
        }

        /// Every invocation reaches the running instance here and opens a window, in the
        /// directory it was started from unless it names another.
        fn command_line(&self, cmdline: &gio::ApplicationCommandLine) -> glib::ExitCode {
            let options = cmdline.options_dict();
            let bytes = |value: Vec<u8>| {
                std::ffi::OsStr::from_bytes(value.strip_suffix(&[0]).unwrap_or(&value)).to_owned()
            };
            let directory = options
                .lookup_value("working-directory", Some(glib::VariantTy::BYTE_STRING))
                .and_then(|value| value.get::<Vec<u8>>())
                .map(|value| {
                    let path = PathBuf::from(bytes(value));
                    match cmdline.cwd() {
                        Some(cwd) if path.is_relative() => cwd.join(path),
                        _ => path,
                    }
                })
                .or_else(|| cmdline.cwd());
            let argv = options
                .lookup_value("", Some(glib::VariantTy::BYTE_STRING_ARRAY))
                .and_then(|value| value.get::<Vec<Vec<u8>>>())
                .unwrap_or_default()
                .into_iter()
                .map(|arg| bytes(arg).to_string_lossy().into_owned())
                .collect();
            // Variables that are not UTF-8 cannot be passed on, and are left out.
            let env = cmdline
                .environ()
                .iter()
                .filter_map(|variable| variable.to_str()?.split_once('='))
                .map(|(name, value)| (name.to_owned(), value.to_owned()))
                .collect();
            self.obj().open_window(Command {
                argv,
                directory,
                env,
            });
            glib::ExitCode::SUCCESS
        }
    }

    impl GtkApplicationImpl for TangentApplication {}
    impl AdwApplicationImpl for TangentApplication {}
}

glib::wrapper! {
    pub struct TangentApplication(ObjectSubclass<imp::TangentApplication>)
        @extends adw::Application, gtk::Application, gio::Application,
        @implements gio::ActionGroup, gio::ActionMap;
}

impl Default for TangentApplication {
    fn default() -> Self {
        Self::new()
    }
}

/// The command line with `-e` or `-x`, which run the rest of it as a command the way
/// other terminals and xdg-terminal-exec expect, turned into the `--` GOption knows.
pub fn arguments() -> Vec<String> {
    let mut arguments: Vec<String> = std::env::args_os()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();
    if let Some(command) = arguments
        .iter()
        .skip(1)
        .take_while(|argument| *argument != "--")
        .position(|argument| argument == "-e" || argument == "-x")
    {
        arguments[command + 1] = "--".to_owned();
    }
    arguments
}

impl TangentApplication {
    pub fn new() -> Self {
        let app: Self = glib::Object::builder()
            .property("application-id", config::APP_ID)
            .property(
                "flags",
                gio::ApplicationFlags::HANDLES_COMMAND_LINE
                    | gio::ApplicationFlags::SEND_ENVIRONMENT,
            )
            .property("resource-base-path", config::RESOURCE_PATH)
            .build();
        app.add_main_option(
            "working-directory",
            glib::Char::from(b'w'),
            glib::OptionFlags::NONE,
            glib::OptionArg::Filename,
            &gettext("Start in this directory"),
            Some(&gettext("DIRECTORY")),
        );
        app.add_main_option(
            "version",
            glib::Char::from(0),
            glib::OptionFlags::NONE,
            glib::OptionArg::None,
            &gettext("Print the version and exit"),
            None,
        );
        app.add_main_option(
            "",
            glib::Char::from(0),
            glib::OptionFlags::NONE,
            glib::OptionArg::FilenameArray,
            "",
            Some(&gettext("[-e] [COMMAND [ARGUMENT…]]")),
        );
        app
    }

    fn open_window(&self, command: Command) {
        let window = TangentWindow::new(Some(self));
        window.add_tab(command);
        window.present();
    }

    fn setup_actions(&self) {
        let new_window = gio::ActionEntry::builder("new-window")
            .activate(|app: &Self, _, _| {
                let command = app
                    .active_window()
                    .and_downcast::<TangentWindow>()
                    .map(|window| window.next_command())
                    .unwrap_or_default();
                app.open_window(command);
            })
            .build();
        let about = gio::ActionEntry::builder("about")
            .activate(|app: &Self, _, _| app.show_about())
            .build();
        let preferences = gio::ActionEntry::builder("preferences")
            .activate(|app: &Self, _, _| {
                crate::preferences::preferences_dialog().present(app.active_window().as_ref());
            })
            .build();
        let show_tab = gio::ActionEntry::builder("show-tab")
            .parameter_type(Some(glib::VariantTy::UINT64))
            .activate(|app: &Self, _, serial| {
                let Some(serial) = serial.and_then(|serial| serial.get::<u64>()) else {
                    return;
                };
                for window in app.windows() {
                    if let Ok(window) = window.downcast::<TangentWindow>()
                        && window.show_tab(serial)
                    {
                        return;
                    }
                }
            })
            .build();
        self.add_action_entries([new_window, about, preferences, show_tab]);
        self.add_action(&settings().create_action("title-bar"));

        // Ctrl with Shift: plain Ctrl and a letter belong to the programs in the terminal.
        for (action, accels) in [
            ("app.new-window", &["<Control><Shift>n"][..]),
            ("win.close", &["<Control><Shift>q"]),
            ("app.preferences", &["<Control>comma"]),
            ("win.new-tab", &["<Control><Shift>t"]),
            ("win.close-tab", &["<Control><Shift>w"]),
            ("win.copy", &["<Control><Shift>c"]),
            ("win.paste", &["<Control><Shift>v"]),
            ("win.select-all", &["<Control><Shift>a"]),
            ("win.deselect", &["<Control><Shift>d"]),
            ("win.find", &["<Control><Shift>f"]),
            (
                "win.zoom-in",
                &["<Control>plus", "<Control>equal", "<Control>KP_Add"],
            ),
            ("win.zoom-out", &["<Control>minus", "<Control>KP_Subtract"]),
            ("win.zoom-reset", &["<Control>0", "<Control>KP_0"]),
            ("win.fullscreen", &["F11"]),
            ("win.tab-overview", &["<Control><Shift>o"]),
            ("app.title-bar", &["<Control><Shift>h"]),
        ] {
            self.set_accels_for_action(action, accels);
        }
    }

    fn show_about(&self) {
        let about = adw::AboutDialog::builder()
            .application_name("Tangent")
            .application_icon(config::APP_ID)
            .developer_name("sachesi")
            .version(config::VERSION)
            .website("https://github.com/sachesi/tangent")
            .issue_url("https://github.com/sachesi/tangent/issues")
            .license_type(gtk::License::Gpl30)
            .comments(gettext("A terminal drawn by the GPU"))
            // Translators: put your name here, one per line, optionally with an email address.
            .translator_credits(gettext("translator-credits"))
            .build();
        about.present(self.active_window().as_ref());
    }
}
