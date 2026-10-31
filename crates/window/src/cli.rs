//! The command line: where the first tab starts and what it runs, or which sessions the
//! window shows instead of a new one; or what else `caret` does from a shell. `-e` and `-x`
//! run the rest of it as a command, as other terminals and `xdg-terminal-exec` expect.

mod completions;
mod json;
mod session;

use std::ffi::OsString;
use std::path::PathBuf;

use caret_protocol::{Answer, Message, Question, Request, SessionId};
use clap::{Args, Parser, Subcommand};
use clap_complete::ArgValueCandidates;

pub use completions::{VAR as COMPLETE, run as completions};
pub use session::run as session;

#[derive(Debug, Parser)]
#[command(
    name = "caret",
    version,
    about = "A terminal whose sessions outlive their windows.",
    args_conflicts_with_subcommands = true
)]
pub struct Cli {
    #[command(subcommand)]
    noun: Option<Noun>,
    #[command(flatten)]
    start: StartArguments,
}

#[derive(Debug, Subcommand)]
pub enum Noun {
    /// Lists the sessions, or acts on one.
    Session(session::SessionCommand),
    /// Prints or installs a shell's completions.
    Completions(completions::CompletionsCommand),
}

#[derive(Debug, Args)]
struct StartArguments {
    /// Start in DIRECTORY.
    #[arg(
        short = 'w',
        long = "working-directory",
        value_name = "DIRECTORY",
        value_hint = clap::ValueHint::DirPath
    )]
    directory: Option<PathBuf>,
    /// Open with the settings of profile NAME.
    #[arg(long, value_name = "NAME", add = ArgValueCandidates::new(completions::profiles))]
    profile: Option<String>,
    /// Lay out the first tab as split preset NAME.
    #[arg(
        long,
        value_name = "NAME",
        conflicts_with_all = ["attach", "session"],
        add = ArgValueCandidates::new(completions::presets)
    )]
    preset: Option<String>,
    /// Show the windows that were detached.
    #[arg(long)]
    attach: bool,
    /// This session, which another window started with `--attach` handed on.
    #[arg(long, value_name = "NUMBER", hide = true)]
    session: Option<u32>,
    /// Run the rest of the line as a command.
    #[arg(short = 'e', visible_short_alias = 'x')]
    execute: bool,
    /// The program the first tab runs, and its arguments; the shell when left out.
    #[arg(value_name = "COMMAND", trailing_var_arg = true)]
    command: Vec<OsString>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Start {
    pub directory: Option<PathBuf>,
    /// The program the first tab runs, and its arguments; the shell when empty.
    pub command: Vec<String>,
    pub attach: Attach,
    /// The profile whose settings the window takes over the others.
    pub profile: Option<String>,
    /// The split preset the first tab takes.
    pub preset: Option<String>,
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

impl Cli {
    /// What the line asks for: one of the nouns, or else a window.
    pub fn parsed(self) -> Result<Noun, Start> {
        let start = self.start;
        self.noun.ok_or_else(|| Start {
            directory: start.directory,
            command: start.command.into_iter().map(lossy).collect(),
            attach: match (start.session, start.attach) {
                (Some(session), _) => Attach::Session(SessionId(session)),
                (None, true) => Attach::Detached,
                (None, false) => Attach::No,
            },
            profile: start.profile,
            preset: start.preset,
        })
    }
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

#[cfg(test)]
mod tests {
    use clap::CommandFactory;
    use clap::error::ErrorKind;

    use super::*;

    fn parsed(line: &[&str]) -> Result<Cli, clap::Error> {
        Cli::try_parse_from(std::iter::once(&"caret").chain(line))
    }

    fn started(line: &[&str]) -> Start {
        match parsed(line).map(Cli::parsed) {
            Ok(Err(start)) => start,
            other => panic!("{line:?} gave {other:?}"),
        }
    }

    #[test]
    fn the_command_line_is_well_formed() {
        Cli::command().debug_assert();
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
        assert_eq!(
            started(&["-w", "/tmp", "-e", "top", "-w"]).command,
            ["top", "-w"]
        );
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
    fn options_take_their_values_in_both_forms() {
        for line in [&["--profile", "work"][..], &["--profile=work"]] {
            assert_eq!(started(line).profile.as_deref(), Some("work"), "{line:?}");
        }
        for line in [&["--preset", "dev"][..], &["--preset=dev"]] {
            assert_eq!(started(line).preset.as_deref(), Some("dev"), "{line:?}");
        }
    }

    #[test]
    fn options_that_end_or_attach() {
        for line in [&["--version"][..], &["-V"]] {
            let error = parsed(line).expect_err("it prints the version");
            assert_eq!(error.kind(), ErrorKind::DisplayVersion);
        }
        for line in [&["-h"][..], &["--help"], &["session", "-h"]] {
            let error = parsed(line).expect_err("it prints help");
            assert_eq!(error.kind(), ErrorKind::DisplayHelp, "{line:?}");
        }
        assert_eq!(started(&["--attach"]).attach, Attach::Detached);
        assert_eq!(
            started(&["--session", "4"]).attach,
            Attach::Session(SessionId(4))
        );
    }

    #[test]
    fn a_wrong_line_is_a_usage_error() {
        for line in [
            &["--frobnicate"][..],
            &["--profile"],
            &["-w"],
            &["--preset", "dev", "--attach"],
            &["--preset", "dev", "--session", "4"],
            &["session", "extra"],
            &["session", "close", "a", "b"],
            &["session", "rename"],
            &["session", "rename", "a", "b", "c"],
            &["session", "attach", "a", "b"],
            &["completions", "tcsh"],
        ] {
            let error = parsed(line).expect_err("a usage error");
            assert_eq!(error.exit_code(), 2, "{line:?}");
        }
    }

    #[test]
    fn a_program_called_like_an_old_command_is_only_a_program() {
        assert_eq!(started(&["sessions"]).command, ["sessions"]);
        assert_eq!(started(&["attach", "x"]).command, ["attach", "x"]);
        assert_eq!(started(&["-e", "session"]).command, ["session"]);
    }
}
