//! `caret session`: what the session list does, from a shell.

use std::ffi::OsString;
use std::fmt::Write as _;
use std::os::unix::ffi::OsStringExt as _;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;

use caret_protocol::{
    Answer, Bytes, Command, Message, Question, Request, SessionEntry, SessionId, Size, TerminalId,
};
use clap::{Args, Subcommand};

use super::{json, lossy};
use crate::config::Config;

/// The first protocol version with what these commands ask beyond listing.
const SESSION_COMMANDS: u16 = 5;

/// The first tab's size until something shows it.
const DETACHED_SIZE: Size = Size {
    columns: 80,
    lines: 24,
    cell_width: 0,
    cell_height: 0,
};

#[derive(Debug, Args)]
pub struct SessionCommand {
    /// Answer in JSON.
    #[arg(short, long, global = true)]
    json: bool,
    #[command(subcommand)]
    verb: Option<SessionVerb>,
}

#[derive(Debug, Subcommand)]
enum SessionVerb {
    /// Shows a session: in the window this runs in, or else in this terminal. Left out,
    /// the only detached one.
    Attach {
        #[arg(value_name = "SESSION")]
        session: Option<String>,
    },
    /// Starts a session with no window and prints its number.
    New {
        /// Its name.
        #[arg(long, value_name = "NAME")]
        name: Option<String>,
        /// Start in DIRECTORY.
        #[arg(
            short = 'w',
            long = "working-directory",
            value_name = "DIRECTORY",
            value_hint = clap::ValueHint::DirPath
        )]
        directory: Option<PathBuf>,
        /// The program its first tab runs, and its arguments; the shell when left out.
        #[arg(value_name = "COMMAND", trailing_var_arg = true)]
        command: Vec<OsString>,
    },
    /// Ends a session and all that runs in it, unless a program runs and --force is not
    /// given. Left out, the session this runs in.
    Close {
        #[arg(value_name = "SESSION")]
        session: Option<String>,
        /// Close it even with programs running.
        #[arg(long)]
        force: bool,
    },
    /// Names a session; an empty NAME takes its name away. SESSION left out, the session
    /// this runs in.
    Rename {
        #[arg(
            value_names = ["SESSION", "NAME"],
            num_args = 1..=2,
            required = true,
        )]
        words: Vec<String>,
    },
    /// Has every window showing a session let it go; it keeps running. Left out, the
    /// session this runs in.
    Detach {
        #[arg(value_name = "SESSION")]
        session: Option<String>,
    },
}

/// Why a command did not do what it was asked.
#[derive(Debug, PartialEq, Eq)]
enum Failure {
    /// The line does not say enough: exit status 2.
    Usage(String),
    Failed(String),
}

impl From<String> for Failure {
    fn from(error: String) -> Self {
        Self::Failed(error)
    }
}

/// What a command leaves for the shell.
enum Done {
    /// Nothing to say.
    Quiet,
    Listed(Vec<SessionEntry>),
    Started(SessionId),
}

pub fn run(command: SessionCommand) -> ExitCode {
    let json = command.json;
    match act(command) {
        Ok(done) => {
            print_done(&done, json);
            ExitCode::SUCCESS
        }
        Err(failure) => {
            let (text, status) = match &failure {
                Failure::Usage(text) => (text, 2),
                Failure::Failed(text) => (text, 1),
            };
            if json {
                println!("{}", json::error(text));
            } else {
                eprintln!("caret: {text}");
            }
            ExitCode::from(status)
        }
    }
}

fn print_done(done: &Done, as_json: bool) {
    match (done, as_json) {
        (Done::Quiet, true) => println!("{}", json::ok(&[])),
        (Done::Quiet, false) => {}
        (Done::Listed(sessions), true) => println!("{}", json::ok(&[json::sessions(sessions)])),
        (Done::Listed(sessions), false) => print!("{}", listing(sessions)),
        (Done::Started(session), true) => {
            println!("{}", json::ok(&[format!("\"session\":{}", session.0)]));
        }
        (Done::Started(session), false) => println!("{}", session.0),
    }
}

fn act(command: SessionCommand) -> Result<Done, Failure> {
    let as_json = command.json;
    let Some(verb) = command.verb else {
        return Ok(Done::Listed(Server::connect()?.sessions()?));
    };
    let mut server = Server::connect()?;
    match verb {
        SessionVerb::Attach { session } => {
            let session = server.attach_target(session.as_deref())?;
            match own_terminal() {
                Some(terminal) => server.show_in_window(terminal, session)?,
                None if as_json => {
                    return Err(Failure::Usage(
                        "a session drawn in this terminal has no JSON to answer in".to_owned(),
                    ));
                }
                None => {
                    drop(server);
                    crate::text::run(session)?;
                }
            }
            Ok(Done::Quiet)
        }
        SessionVerb::New {
            name,
            directory,
            command,
        } => Ok(Done::Started(server.start(name, directory, command)?)),
        SessionVerb::Close { session, force } => {
            let session = server.target(session.as_deref())?;
            if !force {
                let running = server.programs(session)?;
                if !running.is_empty() {
                    return Err(Failure::Failed(format!(
                        "session {} runs {}: --force closes it anyway",
                        session.0,
                        running.join(", "),
                    )));
                }
            }
            server.finish(&Request::CloseSession { session })
        }
        SessionVerb::Rename { words } => {
            let (session, name) = match words.as_slice() {
                [name] => (None, name),
                [session, name] => (Some(session.as_str()), name),
                _ => unreachable!("the line takes one or two words"),
            };
            let session = server.target(session)?;
            server.finish(&Request::NameSession {
                session,
                name: (!name.is_empty()).then(|| name.clone()),
            })
        }
        SessionVerb::Detach { session } => {
            let session = server.target(session.as_deref())?;
            server.needs_commands()?;
            server.finish(&Request::DetachEveryone { session })
        }
    }
}

struct Server {
    stream: UnixStream,
    version: u16,
    asked: u32,
}

impl Server {
    fn connect() -> Result<Self, String> {
        let (stream, version) =
            caret_protocol::client::connect(crate::APP_ID, caret_protocol::NEWEST)
                .map_err(|error| error.to_string())?;
        Ok(Self {
            stream,
            version,
            asked: 0,
        })
    }

    fn send(&mut self, request: &Request) -> Result<(), String> {
        caret_protocol::send(&mut self.stream, request).map_err(|error| error.to_string())
    }

    fn ask(&mut self, question: Question) -> Result<Answer, String> {
        self.asked += 1;
        let id = self.asked;
        self.send(&Request::Ask { id, question })?;
        loop {
            match caret_protocol::receive::<Message>(&mut self.stream) {
                Ok(Some(Message::Answer {
                    id: answered,
                    answer,
                })) if answered == id => {
                    return Ok(answer);
                }
                Ok(Some(_)) => {}
                Ok(None) => return Err("the server closed the connection".to_owned()),
                Err(error) => return Err(error.to_string()),
            }
        }
    }

    fn needs_commands(&self) -> Result<(), String> {
        if self.version < SESSION_COMMANDS {
            return Err("this needs a newer server: end it, and it starts anew".to_owned());
        }
        Ok(())
    }

    /// Sends `request`, and waits until the server has carried it out, so that the next
    /// command of a script finds it done.
    fn finish(&mut self, request: &Request) -> Result<Done, Failure> {
        self.send(request)?;
        self.sessions()?;
        Ok(Done::Quiet)
    }

    fn sessions(&mut self) -> Result<Vec<SessionEntry>, String> {
        let question = if self.version >= 2 {
            Question::SessionList
        } else {
            Question::Sessions
        };
        match self.ask(question)? {
            Answer::SessionList { sessions } => Ok(sessions),
            Answer::Sessions { sessions } => Ok(sessions
                .into_iter()
                .map(|info| SessionEntry {
                    session: info.session,
                    name: None,
                    color: None,
                    panes: u32::try_from(info.tabs.len()).unwrap_or(u32::MAX),
                    tabs: info.tabs,
                    clients: info.clients,
                })
                .collect()),
            other => Err(format!("the server answered {other:?}")),
        }
    }

    /// The session named, or else the one this runs in.
    fn target(&mut self, named: Option<&str>) -> Result<SessionId, Failure> {
        if let Some(named) = named {
            return Ok(find(&self.sessions()?, named)?);
        }
        let own = match own_terminal() {
            Some(terminal) if self.version >= SESSION_COMMANDS => {
                match self.ask(Question::SessionOf { terminal })? {
                    Answer::Session { session } => Some(session),
                    _ => None,
                }
            }
            _ => None,
        };
        own.ok_or_else(|| Failure::Usage("name a session: this runs in none".to_owned()))
    }

    /// The session named, or else the only detached one.
    fn attach_target(&mut self, named: Option<&str>) -> Result<SessionId, Failure> {
        let sessions = self.sessions()?;
        match named {
            Some(named) => Ok(find(&sessions, named)?),
            None => only_detached(&sessions),
        }
    }

    fn programs(&mut self, session: SessionId) -> Result<Vec<String>, String> {
        self.needs_commands()?;
        match self.ask(Question::ProgramNames { session })? {
            Answer::ProgramNames { programs } => Ok(programs),
            other => Err(format!("the server answered {other:?}")),
        }
    }

    /// Asks the window `terminal` was last typed in from to show `session` too.
    fn show_in_window(&mut self, terminal: TerminalId, session: SessionId) -> Result<(), String> {
        if self.version < 2 {
            return Err("showing a session in this window needs a newer server".to_owned());
        }
        self.send(&Request::ShowSession { terminal, session })
    }

    fn start(
        &mut self,
        name: Option<String>,
        directory: Option<PathBuf>,
        command: Vec<OsString>,
    ) -> Result<SessionId, Failure> {
        let Answer::Session { session } = self.ask(Question::NewSession)? else {
            return Err(Failure::Failed("the server made no session".to_owned()));
        };
        if name.is_some() {
            self.send(&Request::NameSession { session, name })?;
        }
        let answer = self.ask(Question::OpenTab {
            session,
            index: 0,
            command: first_command(directory, command),
            size: DETACHED_SIZE,
        })?;
        match answer {
            Answer::Opened { .. } => Ok(session),
            other => Err(Failure::Failed(format!("the server answered {other:?}"))),
        }
    }
}

/// What the first tab of a new session runs, as a window would start it from here.
fn first_command(directory: Option<PathBuf>, command: Vec<OsString>) -> Command {
    let here = std::env::current_dir().unwrap_or_default();
    let directory = directory.map_or_else(|| here.clone(), |directory| here.join(directory));
    let named = !command.is_empty();
    let argv = if named {
        command.into_iter().map(lossy).collect()
    } else {
        Config::load(None).argv()
    };
    Command {
        hold: named,
        argv,
        directory: Some(Bytes(directory.into_os_string().into_vec())),
        env: std::env::vars().collect(),
    }
}

fn only_detached(sessions: &[SessionEntry]) -> Result<SessionId, Failure> {
    let mut detached = sessions
        .iter()
        .filter(|session| session.clients == 0 && !session.tabs.is_empty());
    match (detached.next(), detached.next()) {
        (Some(session), None) => Ok(session.session),
        (None, _) => Err(Failure::Failed("no session is detached".to_owned())),
        (Some(_), Some(_)) => Err(Failure::Usage(
            "name a session: several are detached".to_owned(),
        )),
    }
}

/// A line for each session: its number, name, tabs and panes, whether a window shows it,
/// and its first tab's title.
fn listing(sessions: &[SessionEntry]) -> String {
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
fn find(sessions: &[SessionEntry], target: &str) -> Result<SessionId, String> {
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
fn own_terminal() -> Option<TerminalId> {
    std::env::var("CARET_TERMINAL")
        .ok()?
        .parse()
        .ok()
        .map(TerminalId)
}

#[cfg(test)]
mod tests {
    use caret_protocol::Tab;

    use super::*;

    fn entry(session: u32, name: Option<&str>, clients: u32, tabs: usize) -> SessionEntry {
        SessionEntry {
            session: SessionId(session),
            name: name.map(str::to_owned),
            color: None,
            tabs: (0..tabs)
                .map(|tab| Tab {
                    terminal: TerminalId(session * 10 + tab as u32),
                    title: "zsh".to_owned(),
                    name: None,
                })
                .collect(),
            panes: tabs as u32,
            clients,
        }
    }

    #[test]
    fn a_session_is_found_by_its_number_before_its_name() {
        let sessions = [entry(1, Some("2"), 0, 1), entry(2, Some("build"), 0, 1)];
        assert_eq!(find(&sessions, "2"), Ok(SessionId(2)));
        assert_eq!(find(&sessions, "build"), Ok(SessionId(2)));
        assert!(find(&sessions, "nothing").is_err());
    }

    #[test]
    fn attach_without_a_name_takes_only_a_lone_detached_session() {
        let one = [entry(1, None, 1, 1), entry(2, None, 0, 1)];
        assert_eq!(only_detached(&one), Ok(SessionId(2)));
        let two = [entry(1, None, 0, 1), entry(2, None, 0, 1)];
        assert!(matches!(only_detached(&two), Err(Failure::Usage(_))));
        let none = [entry(1, None, 1, 1), entry(2, None, 0, 0)];
        assert!(matches!(only_detached(&none), Err(Failure::Failed(_))));
    }

    #[test]
    fn the_listing_says_what_a_session_holds() {
        let listed = listing(&[entry(3, Some("build"), 1, 2), entry(4, None, 0, 1)]);
        assert_eq!(
            listed,
            "   3  build         2 tabs, 2 panes  shown  zsh\n   \
             4  -             1 tab, 1 pane  detached  zsh\n"
        );
    }
}
