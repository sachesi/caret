//! What the shell tells the terminal beside its output: the directory it is in (OSC 7),
//! notifications (OSC 9 and 777), the container it entered (OSC 777) and where prompts
//! and commands begin and end (OSC 133). alacritty_terminal's parser drops these, so the
//! output is read through a filter that takes them out before it.

use std::fs::File;
use std::io::{self, Read};
use std::path::PathBuf;
use std::sync::Arc;

use crate::glib;
use alacritty_terminal::event::{OnResize, WindowSize};
use alacritty_terminal::tty::{self, ChildEvent, EventedPty, EventedReadWrite};
use polling::{Event, PollMode, Poller};

/// The address prompts are marked with, as a hyperlink over their text, so that the grid
/// keeps where each one is through scrolling and resizing.
pub const PROMPT: &str = "caret:prompt";

const PROMPT_START: &[u8] = b"\x1b]8;;caret:prompt\x07";
const PROMPT_END: &[u8] = b"\x1b]8;;\x07";

/// The commands whose bodies are held back to be looked at; any other passes through as
/// soon as its start shows it is not one of them.
const TAKEN: [&[u8]; 4] = [b"7;", b"9;", b"133;", b"777;"];

/// Longer bodies are not what the shell sends, and pass through.
const LONGEST: usize = 4096;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Report {
    Directory(PathBuf),
    Notification {
        title: String,
        body: String,
    },
    CommandStarted,
    /// With the command's exit status, when the shell gave it.
    CommandFinished(Option<i32>),
    /// A toolbox or distrobox container the shell entered, by name and by the tool that
    /// runs it.
    ContainerEntered {
        name: String,
        runtime: String,
    },
    ContainerLeft,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
enum State {
    #[default]
    Ground,
    Escape,
    /// In a command's body, still deciding whether it is one to take.
    Body,
    /// In a taken body, after an escape that may start its terminator.
    BodyEscape,
    /// In a command that is not taken, until its end.
    Passing,
}

#[derive(Debug, Default)]
pub struct Filter {
    state: State,
    body: Vec<u8>,
    in_prompt: bool,
}

impl Filter {
    /// Appends to `out` what the parser gets of `input` and to `reports` what was taken
    /// out of it. A command cut off at the end of `input` is held for the next call.
    pub fn feed(&mut self, input: &[u8], out: &mut Vec<u8>, reports: &mut Vec<Report>) {
        for &byte in input {
            self.byte(byte, out, reports);
        }
    }

    fn byte(&mut self, byte: u8, out: &mut Vec<u8>, reports: &mut Vec<Report>) {
        match self.state {
            State::Ground => {
                if byte == 0x1b {
                    self.state = State::Escape;
                } else {
                    out.push(byte);
                }
            }
            State::Escape => {
                if byte == b']' {
                    self.body.clear();
                    self.state = State::Body;
                } else {
                    out.push(0x1b);
                    self.state = State::Ground;
                    self.byte(byte, out, reports);
                }
            }
            State::Body => match byte {
                0x07 => {
                    self.state = State::Ground;
                    self.finish(out, reports);
                }
                0x1b => self.state = State::BodyEscape,
                // Cancel and substitute end the command in the parser too, which then acts
                // on them; holding on would hide the output after them.
                0x18 | 0x1a => {
                    self.state = State::Ground;
                    self.finish(out, reports);
                    out.push(byte);
                }
                _ => {
                    self.body.push(byte);
                    let taken = TAKEN.iter().any(|prefix| {
                        if self.body.len() < prefix.len() {
                            prefix.starts_with(&self.body)
                        } else {
                            self.body.starts_with(prefix)
                        }
                    });
                    if !taken || self.body.len() > LONGEST {
                        out.extend_from_slice(b"\x1b]");
                        out.append(&mut self.body);
                        self.state = State::Passing;
                    }
                }
            },
            State::BodyEscape => {
                if byte == b'\\' {
                    self.state = State::Ground;
                    self.finish(out, reports);
                } else {
                    // An escape that is not a terminator ends the command, as it does in
                    // the parser, and starts a sequence of its own.
                    self.finish(out, reports);
                    self.state = State::Escape;
                    self.byte(byte, out, reports);
                }
            }
            State::Passing => match byte {
                0x07 | 0x18 | 0x1a => {
                    out.push(byte);
                    self.state = State::Ground;
                }
                0x1b => self.state = State::Escape,
                _ => out.push(byte),
            },
        }
    }

    fn finish(&mut self, out: &mut Vec<u8>, reports: &mut Vec<Report>) {
        let body = String::from_utf8_lossy(&std::mem::take(&mut self.body)).into_owned();
        let Some((command, rest)) = body.split_once(';') else {
            return;
        };
        match command {
            "7" => reports.extend(directory(rest).map(Report::Directory)),
            // ConEmu's progress and other numbered subcommands share OSC 9.
            "9" if !rest.starts_with(|c: char| c.is_ascii_digit()) => {
                reports.push(Report::Notification {
                    title: String::new(),
                    body: rest.to_owned(),
                });
            }
            "777" => {
                let mut fields = rest.split(';');
                match (fields.next(), fields.next()) {
                    (Some("notify"), Some(title)) => reports.push(Report::Notification {
                        title: title.to_owned(),
                        body: fields.collect::<Vec<_>>().join(";"),
                    }),
                    (Some("container"), Some("push")) => {
                        if let (Some(name), Some(runtime)) = (fields.next(), fields.next())
                            && container_name(name)
                        {
                            reports.push(Report::ContainerEntered {
                                name: name.to_owned(),
                                runtime: runtime.to_owned(),
                            });
                        }
                    }
                    (Some("container"), Some("pop")) => reports.push(Report::ContainerLeft),
                    _ => {}
                }
            }
            "133" => {
                let mut fields = rest.split(';');
                let mark = fields.next().unwrap_or_default();
                if mark == "A" {
                    if !self.in_prompt {
                        out.extend_from_slice(PROMPT_START);
                        self.in_prompt = true;
                    }
                    return;
                }
                if self.in_prompt {
                    out.extend_from_slice(PROMPT_END);
                    self.in_prompt = false;
                }
                match mark {
                    "C" => reports.push(Report::CommandStarted),
                    "D" => reports.push(Report::CommandFinished(
                        fields.next().and_then(|status| status.parse().ok()),
                    )),
                    _ => {}
                }
            }
            _ => {}
        }
    }
}

/// Whether `name` is one a container can have, which also keeps it from reading as an
/// option to the tool that enters it.
fn container_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
}

/// The local path of a `file://` address, or none for another host's.
fn directory(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let slash = rest.find('/')?;
    let host = &rest[..slash];
    let local = host.is_empty() || host == "localhost" || glib::host_name().as_str() == host;
    if !local {
        return None;
    }
    let path = glib::Uri::unescape_string(&rest[slash..], None::<&str>)?;
    Some(PathBuf::from(path.as_str()))
}

/// The pseudoterminal with its output read through a `Filter`.
pub struct FilteredPty {
    pty: tty::Pty,
    reader: FilteredReader,
}

pub struct FilteredReader {
    file: File,
    filter: Filter,
    raw: Vec<u8>,
    /// Filtered output that did not fit the last read.
    pending: Vec<u8>,
    reports: Vec<Report>,
    send: Box<dyn Fn(Report) + Send>,
}

impl FilteredPty {
    pub fn new(pty: tty::Pty, send: impl Fn(Report) + Send + 'static) -> io::Result<Self> {
        let file = pty.file().try_clone()?;
        Ok(Self {
            pty,
            reader: FilteredReader {
                file,
                filter: Filter::default(),
                raw: vec![0; 0x1_0000],
                pending: Vec::new(),
                reports: Vec::new(),
                send: Box::new(send),
            },
        })
    }
}

impl Read for FilteredReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        while self.pending.is_empty() {
            // A third of the room: a prompt's mark is at most twice the length of the
            // command it replaces, so what is read fits, and nothing is left over for
            // after the descriptor has been drained, when no poll would come for it.
            let room = (buf.len() / 3).clamp(1, self.raw.len());
            let read = self.file.read(&mut self.raw[..room])?;
            if read == 0 {
                return Ok(0);
            }
            self.filter
                .feed(&self.raw[..read], &mut self.pending, &mut self.reports);
            for report in self.reports.drain(..) {
                (self.send)(report);
            }
        }
        let length = self.pending.len().min(buf.len());
        buf[..length].copy_from_slice(&self.pending[..length]);
        self.pending.drain(..length);
        Ok(length)
    }
}

impl EventedReadWrite for FilteredPty {
    type Reader = FilteredReader;
    type Writer = File;

    unsafe fn register(
        &mut self,
        poll: &Arc<Poller>,
        interest: Event,
        mode: PollMode,
    ) -> io::Result<()> {
        // SAFETY: the caller keeps the promise the wrapped pseudoterminal asks for.
        unsafe { self.pty.register(poll, interest, mode) }
    }

    fn reregister(
        &mut self,
        poll: &Arc<Poller>,
        interest: Event,
        mode: PollMode,
    ) -> io::Result<()> {
        self.pty.reregister(poll, interest, mode)
    }

    fn deregister(&mut self, poll: &Arc<Poller>) -> io::Result<()> {
        self.pty.deregister(poll)
    }

    fn reader(&mut self) -> &mut FilteredReader {
        &mut self.reader
    }

    fn writer(&mut self) -> &mut File {
        self.pty.writer()
    }
}

impl EventedPty for FilteredPty {
    fn next_child_event(&mut self) -> Option<ChildEvent> {
        self.pty.next_child_event()
    }
}

impl OnResize for FilteredPty {
    fn on_resize(&mut self, size: WindowSize) {
        self.pty.on_resize(size);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn filtered(chunks: &[&[u8]]) -> (Vec<u8>, Vec<Report>) {
        let mut filter = Filter::default();
        let (mut out, mut reports) = (Vec::new(), Vec::new());
        for chunk in chunks {
            filter.feed(chunk, &mut out, &mut reports);
        }
        (out, reports)
    }

    #[test]
    fn other_output_passes_unchanged() {
        let input: &[u8] = b"a\x1b[1mb\x1b]0;title\x07c\x1b]8;;http://x\x1b\\d\x1b\x1b]2;t\x07";
        assert_eq!(filtered(&[input]).0, input);
    }

    #[test]
    fn prompts_are_marked_and_commands_reported() {
        let (out, reports) =
            filtered(&[b"\x1b]133;A\x07$ \x1b]133;B\x07ls\r\n\x1b]133;C\x07out\x1b]133;D;2\x07"]);
        assert_eq!(out, b"\x1b]8;;caret:prompt\x07$ \x1b]8;;\x07ls\r\nout");
        assert_eq!(
            reports,
            [Report::CommandStarted, Report::CommandFinished(Some(2))]
        );
    }

    #[test]
    fn commands_cut_between_reads_are_put_together() {
        let (out, reports) = filtered(&[b"x\x1b]7;file://", b"/tmp/a%20b\x1b", b"\\y"]);
        assert_eq!(out, b"xy");
        assert_eq!(reports, [Report::Directory(PathBuf::from("/tmp/a b"))]);
    }

    #[test]
    fn cancel_ends_a_command_as_in_the_parser() {
        let (out, reports) = filtered(&[b"\x1b]7;file:///tmp\x18shown\x1b]9;hi\x1b[1mbold"]);
        assert_eq!(out, b"\x18shown\x1b[1mbold");
        assert_eq!(
            reports,
            [
                Report::Directory(PathBuf::from("/tmp")),
                Report::Notification {
                    title: String::new(),
                    body: "hi".into()
                }
            ]
        );
    }

    #[test]
    fn containers_with_names_that_read_as_options_are_ignored() {
        let (_, reports) = filtered(&[
            b"\x1b]777;container;push;--root;distrobox\x07",
            b"\x1b]777;container;push;a b;toolbox\x07",
        ]);
        assert!(reports.is_empty());
    }

    #[test]
    fn other_hosts_directories_are_ignored() {
        let (_, reports) = filtered(&[b"\x1b]7;file://elsewhere.invalid/tmp\x07"]);
        assert!(reports.is_empty());
    }

    #[test]
    fn notifications_and_containers_are_reported() {
        let (out, reports) = filtered(&[
            b"\x1b]777;notify;Done;it; worked\x07\x1b]9;hi\x07\x1b]9;4;1;50\x07",
            b"\x1b]777;container;push;fedora;toolbox;x\x1b\\\x1b]777;container;pop;;\x07",
        ]);
        assert!(out.is_empty());
        assert_eq!(
            reports,
            [
                Report::Notification {
                    title: "Done".into(),
                    body: "it; worked".into()
                },
                Report::Notification {
                    title: String::new(),
                    body: "hi".into()
                },
                Report::ContainerEntered {
                    name: "fedora".into(),
                    runtime: "toolbox".into()
                },
                Report::ContainerLeft,
            ]
        );
    }
}
