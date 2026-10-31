//! `caret completions`: scripts that complete the command in a shell, printed or put where
//! the shell finds them. Most ask caret itself at each Tab, so that sessions, profiles and
//! presets come from what there is now; Nushell's are written once, without them.

use std::io::Write as _;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;
use std::{env, fs};

use caret_protocol::{Answer, Message, Question, Request};
use clap::{Args, CommandFactory, ValueEnum, builder::StyledStr};
use clap_complete::CompletionCandidate;
use clap_complete::env::{EnvCompleter, Shells};

use super::Cli;
use crate::config::Config;

/// The variable caret is run with to complete.
pub const VAR: &str = "COMPLETE";

/// How long a Tab press waits for the server.
const PATIENCE: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum Shell {
    Bash,
    Zsh,
    Fish,
    Elvish,
    Nushell,
}

#[derive(Debug, Args)]
pub struct CompletionsCommand {
    /// The shell, the one SHELL names when left out.
    shell: Option<Shell>,
    /// Puts the script where the shell finds it, under your home.
    #[arg(long)]
    install: bool,
}

impl Shell {
    fn from_environment() -> Option<Self> {
        let path = PathBuf::from(env::var_os("SHELL")?);
        let name = path.file_name()?.to_str()?;
        Self::from_str(name, true).ok()
    }

    fn name(self) -> &'static str {
        match self {
            Self::Bash => "bash",
            Self::Zsh => "zsh",
            Self::Fish => "fish",
            Self::Elvish => "elvish",
            Self::Nushell => "nushell",
        }
    }

    /// Where the shell looks for the script, under the user's home, and what else it needs,
    /// if anything, to find it there.
    fn place(self) -> Option<(PathBuf, Option<&'static str>)> {
        let home = PathBuf::from(env::var_os("HOME")?);
        let config =
            env::var_os("XDG_CONFIG_HOME").map_or_else(|| home.join(".config"), PathBuf::from);
        let data = env::var_os("XDG_DATA_HOME")
            .map_or_else(|| home.join(".local").join("share"), PathBuf::from);
        Some(match self {
            Self::Bash => (data.join("bash-completion/completions/caret"), None),
            Self::Zsh => (
                data.join("zsh/site-functions/_caret"),
                Some("add ~/.local/share/zsh/site-functions to fpath before compinit in ~/.zshrc"),
            ),
            Self::Fish => (config.join("fish/completions/caret.fish"), None),
            Self::Elvish => (
                config.join("elvish/lib/caret.elv"),
                Some("add `use caret` to ~/.config/elvish/rc.elv"),
            ),
            Self::Nushell => (
                config.join("nushell/completions/caret.nu"),
                Some("add `source ~/.config/nushell/completions/caret.nu` to config.nu"),
            ),
        })
    }

    fn script(self) -> std::io::Result<Vec<u8>> {
        let mut script = Vec::new();
        if self == Self::Nushell {
            clap_complete::generate(
                clap_complete_nushell::Nushell,
                &mut Cli::command(),
                "caret",
                &mut script,
            );
            return Ok(script);
        }
        let shells = Shells::builtins();
        let shell: &dyn EnvCompleter = shells
            .completer(self.name())
            .ok_or_else(|| std::io::Error::other(format!("no completions for {}", self.name())))?;
        shell.write_registration(VAR, "caret", "caret", "caret", &mut script)?;
        // Loaded from fpath, the file is the completion function itself: it registers the
        // one it defines, which completes this first time too. Sourced, there is no
        // completion under way to complete.
        if self == Self::Zsh {
            writeln!(
                script,
                "if (( ${{+compstate}} )); then _clap_dynamic_completer_caret \"$@\"; fi"
            )?;
        }
        Ok(script)
    }
}

pub fn run(command: &CompletionsCommand) -> ExitCode {
    let Some(shell) = command.shell.or_else(Shell::from_environment) else {
        eprintln!("caret: name a shell: bash, zsh, fish, elvish or nushell");
        return ExitCode::from(2);
    };
    let script = match shell.script() {
        Ok(script) => script,
        Err(error) => {
            eprintln!("caret: {error}");
            return ExitCode::FAILURE;
        }
    };
    if !command.install {
        let _ = std::io::stdout().write_all(&script);
        return ExitCode::SUCCESS;
    }
    let Some((path, also)) = shell.place() else {
        eprintln!("caret: HOME is not set");
        return ExitCode::FAILURE;
    };
    let written = path
        .parent()
        .map_or(Ok(()), fs::create_dir_all)
        .and_then(|()| fs::write(&path, &script));
    if let Err(error) = written {
        eprintln!("caret: cannot write {}: {error}", path.display());
        return ExitCode::FAILURE;
    }
    println!("Wrote {}", path.display());
    if let Some(also) = also {
        println!("To load it, {also}");
    }
    ExitCode::SUCCESS
}

fn candidate(value: impl Into<String>, help: impl Into<String>) -> CompletionCandidate {
    let help: String = help.into();
    CompletionCandidate::new(value.into()).help((!help.is_empty()).then(|| StyledStr::from(help)))
}

/// The sessions of a server that runs, by number and by name. A Tab press starts none, and
/// does not wait long for one that does not answer.
pub fn sessions() -> Vec<CompletionCandidate> {
    listed().map_or_else(Vec::new, |sessions| {
        sessions
            .into_iter()
            .flat_map(|session| {
                let title = session.tabs.first().map_or("", |tab| tab.title.as_str());
                let number = candidate(session.session.0.to_string(), title);
                let named = session.name.as_deref().map(|name| candidate(name, title));
                std::iter::once(number).chain(named)
            })
            .collect()
    })
}

fn listed() -> Option<Vec<caret_protocol::SessionEntry>> {
    let path = caret_protocol::socket_path(crate::APP_ID)?;
    let mut stream = UnixStream::connect(path).ok()?;
    stream.set_read_timeout(Some(PATIENCE)).ok()?;
    stream.set_write_timeout(Some(PATIENCE)).ok()?;
    let version = caret_protocol::greet(&mut stream, caret_protocol::NEWEST).ok()?;
    if version < 2 {
        return None;
    }
    let ask = Request::Ask {
        id: 1,
        question: Question::SessionList,
    };
    caret_protocol::send(&mut stream, &ask).ok()?;
    loop {
        if let Message::Answer {
            id: 1,
            answer: Answer::SessionList { sessions },
        } = caret_protocol::receive::<Message>(&mut stream).ok()??
        {
            return Some(sessions);
        }
    }
}

pub fn profiles() -> Vec<CompletionCandidate> {
    Config::load(None)
        .profiles
        .into_iter()
        .map(|profile| candidate(profile, ""))
        .collect()
}

pub fn presets() -> Vec<CompletionCandidate> {
    Config::load(None)
        .presets
        .into_iter()
        .map(|preset| candidate(preset.name, ""))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_shell_comes_from_its_path() {
        for (path, shell) in [
            ("/bin/bash", Shell::Bash),
            ("/usr/bin/zsh", Shell::Zsh),
            ("/usr/bin/fish", Shell::Fish),
            ("elvish", Shell::Elvish),
            ("/usr/bin/nushell", Shell::Nushell),
        ] {
            let name = PathBuf::from(path);
            let name = name.file_name().and_then(|name| name.to_str());
            assert_eq!(Shell::from_str(name.unwrap(), true), Ok(shell));
        }
    }

    #[test]
    fn every_shell_has_a_script_that_names_caret() {
        for shell in [
            Shell::Bash,
            Shell::Zsh,
            Shell::Fish,
            Shell::Elvish,
            Shell::Nushell,
        ] {
            let script = String::from_utf8(shell.script().unwrap()).unwrap();
            assert!(script.contains("caret"), "{}", shell.name());
        }
    }
}
