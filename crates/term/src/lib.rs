//! A terminal emulator without input or output of its own: bytes from a program go in,
//! and out come the screen, the history above it, and the replies the program asked for.

pub mod grid;
pub mod history;
pub mod links;
pub mod palette;
pub mod parser;
pub mod search;
pub mod style;
pub mod term;
pub mod view;

pub use term::{Event, Options, Report, Term};
