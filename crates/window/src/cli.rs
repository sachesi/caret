//! The command line: where the first tab starts and what it runs, or which sessions the
//! window shows instead of a new one. `-e` and `-x` run the rest of it as a command, as
//! other terminals and `xdg-terminal-exec` expect.

use std::ffi::OsString;
use std::path::PathBuf;

use caret_protocol::{Answer, Message, Question, Request, SessionId};

pub const USAGE: &str = "\
Usage: caret [OPTION…] [-e] [COMMAND [ARGUMENT…]]

  -w, --working-directory DIRECTORY   start in DIRECTORY
      --attach                        show the windows that were detached
      --version                       print the version and exit
  -h, --help                          print this and exit";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Start {
    pub directory: Option<PathBuf>,
    /// The program the first tab runs, and its arguments; the shell when empty.
    pub command: Vec<String>,
    pub attach: Attach,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub enum Attach {
    /// A new session.
    #[default]
    No,
    /// Every session no window shows, each in a window of its own.
    Detached,
    /// This session, which another window started with `--attach` handed on.
    Session(SessionId),
}

#[derive(Debug, PartialEq, Eq)]
pub enum Parsed {
    Start(Start),
    Help,
    Version,
}

/// The command line `arguments`, without the program's name.
///
/// # Errors
///
/// An option Caret does not know, or one without the value it takes.
pub fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Parsed, String> {
    let mut arguments = arguments.into_iter();
    let mut start = Start::default();
    while let Some(argument) = arguments.next() {
        let text = argument.to_string_lossy();
        match text.as_ref() {
            "-h" | "--help" => return Ok(Parsed::Help),
            "--version" => return Ok(Parsed::Version),
            "--attach" => start.attach = Attach::Detached,
            "--session" => {
                let session = arguments
                    .next()
                    .and_then(|session| session.to_str()?.parse().ok())
                    .ok_or("--session needs a session's number")?;
                start.attach = Attach::Session(SessionId(session));
            }
            "-w" | "--working-directory" => {
                let directory = arguments
                    .next()
                    .ok_or_else(|| format!("{text} needs a directory"))?;
                start.directory = Some(directory.into());
            }
            "-e" | "-x" | "--" => {
                start.command = arguments.map(lossy).collect();
                break;
            }
            _ => {
                if let Some(directory) = text
                    .strip_prefix("--working-directory=")
                    .or_else(|| text.strip_prefix("-w").filter(|rest| !rest.is_empty()))
                {
                    start.directory = Some(directory.into());
                } else if text.starts_with('-') {
                    return Err(format!("unknown option {text}"));
                } else {
                    start.command = std::iter::once(argument)
                        .chain(arguments)
                        .map(lossy)
                        .collect();
                    break;
                }
            }
        }
    }
    Ok(Parsed::Start(start))
}

/// `argument` as text, with what is not UTF-8 in it replaced.
fn lossy(argument: OsString) -> String {
    argument
        .into_string()
        .unwrap_or_else(|argument| argument.to_string_lossy().into_owned())
}

/// The sessions no window shows that still have tabs, asked of the server.
///
/// # Errors
///
/// When the server cannot be reached, or answers something else.
pub fn detached() -> Result<Vec<SessionId>, String> {
    let mut stream =
        caret_protocol::client::connect(crate::APP_ID).map_err(|error| error.to_string())?;
    let question = Request::Ask {
        id: 1,
        question: Question::Sessions,
    };
    caret_protocol::send(&mut stream, &question).map_err(|error| error.to_string())?;
    loop {
        match caret_protocol::receive::<Message>(&mut stream) {
            Ok(Some(Message::Answer {
                id: 1,
                answer: Answer::Sessions { sessions },
            })) => {
                return Ok(sessions
                    .into_iter()
                    .filter(|session| session.clients == 0 && !session.tabs.is_empty())
                    .map(|session| session.session)
                    .collect());
            }
            Ok(Some(_)) => {}
            Ok(None) => return Err("the server closed the connection".to_owned()),
            Err(error) => return Err(error.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(line: &[&str]) -> Result<Parsed, String> {
        parse(line.iter().map(OsString::from))
    }

    fn started(line: &[&str]) -> Start {
        match parsed(line) {
            Ok(Parsed::Start(start)) => start,
            other => panic!("{line:?} gave {other:?}"),
        }
    }

    #[test]
    fn a_command_is_the_rest_of_the_line_after_e_x_or_its_first_word() {
        assert_eq!(
            started(&["-e", "htop", "-d", "5"]).command,
            ["htop", "-d", "5"]
        );
        assert_eq!(started(&["-x", "ls", "-e"]).command, ["ls", "-e"]);
        assert_eq!(started(&["--", "vim", "--help"]).command, ["vim", "--help"]);
        assert_eq!(started(&["top", "-w"]).command, ["top", "-w"]);
        assert!(started(&[]).command.is_empty());
    }

    #[test]
    fn the_directory_is_given_in_any_of_the_usual_ways() {
        let directory = Some(PathBuf::from("/tmp"));
        for line in [
            &["-w", "/tmp"][..],
            &["-w/tmp"],
            &["--working-directory", "/tmp"],
            &["--working-directory=/tmp"],
        ] {
            assert_eq!(started(line).directory, directory, "{line:?}");
        }
    }

    #[test]
    fn options_that_end_or_attach() {
        assert_eq!(parsed(&["--version"]), Ok(Parsed::Version));
        assert_eq!(parsed(&["-h"]), Ok(Parsed::Help));
        assert_eq!(started(&["--attach"]).attach, Attach::Detached);
        assert_eq!(
            started(&["--session", "4"]).attach,
            Attach::Session(SessionId(4))
        );
        assert!(parsed(&["--frobnicate"]).is_err());
        assert!(parsed(&["-w"]).is_err());
    }
}
