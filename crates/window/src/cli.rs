//! The command line: where the first tab starts and what it runs, or which sessions the
//! window shows instead of a new one; or listing the sessions and showing one. `-e` and
//! `-x` run the rest of it as a command, as other terminals and `xdg-terminal-exec`
//! expect.

use std::ffi::OsString;
use std::fmt::Write;
use std::path::PathBuf;

use caret_protocol::{Answer, Message, Question, Request, SessionEntry, SessionId, TerminalId};

pub const USAGE: &str = "\
Usage: caret [OPTION…] [-e] [COMMAND [ARGUMENT…]]
       caret sessions
       caret attach SESSION

  -w, --working-directory DIRECTORY   start in DIRECTORY
      --profile NAME                  open with the settings of profile NAME
      --attach                        show the windows that were detached
      --version                       print the version and exit
  -h, --help                          print this and exit";

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Start {
    pub directory: Option<PathBuf>,
    /// The program the first tab runs, and its arguments; the shell when empty.
    pub command: Vec<String>,
    pub attach: Attach,
    /// The profile whose settings the window takes over the others.
    pub profile: Option<String>,
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
    /// Lists the sessions.
    Sessions,
    /// Shows the session of this number or name.
    Attach(String),
}

/// The command line `arguments`, without the program's name.
///
/// # Errors
///
/// An option Caret does not know, or one without the value it takes.
pub fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<Parsed, String> {
    let mut arguments = arguments.into_iter().peekable();
    match arguments.peek().and_then(|first| first.to_str()) {
        Some("sessions") => return Ok(Parsed::Sessions),
        Some("attach") => {
            arguments.next();
            return arguments
                .next()
                .map(|session| Parsed::Attach(lossy(session)))
                .ok_or_else(|| "attach needs a session's number or name".to_owned());
        }
        _ => {}
    }
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
            "--profile" => {
                let profile = arguments.next().ok_or("--profile needs a profile's name")?;
                start.profile = Some(lossy(profile));
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
    let (mut stream, _) = caret_protocol::client::connect(crate::APP_ID, caret_protocol::NEWEST)
        .map_err(|error| error.to_string())?;
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

/// The sessions, as the server lists them.
///
/// # Errors
///
/// When the server cannot be reached, or answers something else.
pub fn sessions() -> Result<Vec<SessionEntry>, String> {
    let (mut stream, version) =
        caret_protocol::client::connect(crate::APP_ID, caret_protocol::NEWEST)
            .map_err(|error| error.to_string())?;
    let question = if version >= 2 {
        Question::SessionList
    } else {
        Question::Sessions
    };
    let ask = Request::Ask { id: 1, question };
    caret_protocol::send(&mut stream, &ask).map_err(|error| error.to_string())?;
    loop {
        match caret_protocol::receive::<Message>(&mut stream) {
            Ok(Some(Message::Answer {
                id: 1,
                answer: Answer::SessionList { sessions },
            })) => return Ok(sessions),
            Ok(Some(Message::Answer {
                id: 1,
                answer: Answer::Sessions { sessions },
            })) => {
                return Ok(sessions
                    .into_iter()
                    .map(|info| SessionEntry {
                        session: info.session,
                        name: None,
                        color: None,
                        panes: u32::try_from(info.tabs.len()).unwrap_or(u32::MAX),
                        tabs: info.tabs,
                        clients: info.clients,
                    })
                    .collect());
            }
            Ok(Some(_)) => {}
            Ok(None) => return Err("the server closed the connection".to_owned()),
            Err(error) => return Err(error.to_string()),
        }
    }
}

/// A line for each session: its number, name, tabs and panes, whether a window shows it,
/// and its first tab's title.
pub fn listing(sessions: &[SessionEntry]) -> String {
    let mut listing = String::new();
    for entry in sessions {
        let count =
            |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        let title = entry.tabs.first().map_or("", |tab| {
            tab.name
                .as_deref()
                .filter(|name| !name.is_empty())
                .unwrap_or(&tab.title)
        });
        let _ = writeln!(
            listing,
            "{:>4}  {:<12}  {}, {}  {}  {}",
            entry.session.0,
            entry.name.as_deref().unwrap_or("-"),
            count(entry.tabs.len(), "tab", "tabs"),
            count(entry.panes as usize, "pane", "panes"),
            if entry.clients > 0 {
                "shown"
            } else {
                "detached"
            },
            title,
        );
    }
    listing
}

/// The session `target` names, by its number or its name.
///
/// # Errors
///
/// When no session has that number or name.
pub fn find(sessions: &[SessionEntry], target: &str) -> Result<SessionId, String> {
    sessions
        .iter()
        .find(|entry| entry.session.0.to_string() == target)
        .or_else(|| {
            sessions
                .iter()
                .find(|entry| entry.name.as_deref() == Some(target))
        })
        .map(|entry| entry.session)
        .ok_or_else(|| format!("no session is called {target}"))
}

/// The terminal this program runs in, when that is one of Caret's.
pub fn own_terminal() -> Option<TerminalId> {
    std::env::var("CARET_TERMINAL")
        .ok()?
        .parse()
        .ok()
        .map(TerminalId)
}

/// Asks the window `terminal` was last typed in from to show `session` too.
///
/// # Errors
///
/// When the server cannot be reached.
pub fn show_in_window(terminal: TerminalId, session: SessionId) -> Result<(), String> {
    let (mut stream, version) =
        caret_protocol::client::connect(crate::APP_ID, caret_protocol::NEWEST)
            .map_err(|error| error.to_string())?;
    if version < 2 {
        return Err("showing a session in this window needs a newer server".to_owned());
    }
    caret_protocol::send(&mut stream, &Request::ShowSession { terminal, session })
        .map_err(|error| error.to_string())
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
        assert_eq!(
            started(&["--profile", "work"]).profile.as_deref(),
            Some("work")
        );
        assert_eq!(parsed(&["sessions"]), Ok(Parsed::Sessions));
        assert_eq!(
            parsed(&["attach", "build"]),
            Ok(Parsed::Attach("build".to_owned()))
        );
        assert!(parsed(&["attach"]).is_err());
        assert!(parsed(&["--frobnicate"]).is_err());
        assert!(parsed(&["--profile"]).is_err());
        assert!(parsed(&["-w"]).is_err());
    }
}
