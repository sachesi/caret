use caret::application::{self, CaretApplication};
use caret::gio::prelude::*;

fn main() -> caret::glib::ExitCode {
    // SAFETY: the first thing the program does; no thread has been started.
    unsafe { caret::init_early() };
    CaretApplication::new().run_with_args(&application::arguments())
}
