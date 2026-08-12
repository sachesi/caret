use tangent::application::{self, TangentApplication};
use tangent::gio::prelude::*;

fn main() -> tangent::glib::ExitCode {
    // SAFETY: the first thing the program does; no thread has been started.
    unsafe { tangent::init_early() };
    TangentApplication::new().run_with_args(&application::arguments())
}
