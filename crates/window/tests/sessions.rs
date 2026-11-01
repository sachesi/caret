//! `caret session` as a shell runs it, against a real server: what its commands answer, and a
//! session drawn in a terminal, read back through the emulator.

#![expect(clippy::unwrap_used, reason = "a test fails by panicking")]

use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use caret_term::{Options, Term};

const PATIENCE: Duration = Duration::from_secs(20);

/// Ctrl+Shift+Space as a terminal that writes keys apart sends it, as the emulator does.
const CONTROL_MODE: &[u8] = b"\x1b[32;6u";

/// A runtime folder of its own, so that the commands start a server of their own. The server
/// ends when the last session here is closed.
struct Sandbox {
    folder: PathBuf,
}

impl Sandbox {
    fn new() -> Self {
        static FOLDERS: AtomicU32 = AtomicU32::new(0);
        let name = format!(
            "caret-sessions-{}-{}",
            std::process::id(),
            FOLDERS.fetch_add(1, Ordering::Relaxed)
        );
        let folder = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&folder).unwrap();
        let program = PathBuf::from(env!("CARGO_BIN_EXE_caret"));
        assert!(
            program.with_file_name("caret-server").is_file(),
            "caret-server is not built: run these with cargo test --workspace"
        );
        Self { folder }
    }

    fn command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_caret"));
        command
            .env("XDG_RUNTIME_DIR", &self.folder)
            .env("XDG_CONFIG_HOME", self.folder.join("config"))
            .env("XDG_STATE_HOME", self.folder.join("state"))
            .env("TERM", "xterm-256color")
            .env_remove("CARET_TERMINAL")
            .env_remove("WAYLAND_DISPLAY");
        command
    }

    fn caret(&self, arguments: &[&str]) -> Output {
        self.command()
            .args(arguments)
            .stdin(Stdio::null())
            .output()
            .unwrap()
    }

    /// What `arguments` print, having succeeded.
    fn says(&self, arguments: &[&str]) -> String {
        let output = self.caret(arguments);
        assert!(output.status.success(), "{arguments:?}: {output:?}");
        String::from_utf8(output.stdout).unwrap()
    }

    fn fails(&self, arguments: &[&str], status: i32, message: &str) {
        let output = self.caret(arguments);
        assert_eq!(output.status.code(), Some(status), "{arguments:?}");
        let said = String::from_utf8_lossy(&output.stderr);
        assert!(said.contains(message), "{arguments:?}: {said}");
    }

    fn eventually(&self, wanted: impl Fn(&str) -> bool) {
        let deadline = Instant::now() + PATIENCE;
        loop {
            let listed = self.says(&["session"]);
            if wanted(&listed) {
                return;
            }
            assert!(Instant::now() < deadline, "{listed}");
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn new_session(&self, program: &[&str]) -> String {
        let mut arguments = vec!["session", "new"];
        arguments.extend_from_slice(program);
        self.says(&arguments).trim().to_owned()
    }

    fn show(&self, session: &str) -> Shown {
        Shown::attach(self, session)
    }
}

impl Drop for Sandbox {
    fn drop(&mut self) {
        let listed = self.caret(&["session"]);
        for line in String::from_utf8_lossy(&listed.stdout).lines() {
            if let Some(session) = line.split_whitespace().next() {
                self.caret(&["session", "close", "--force", session]);
            }
        }
        let _ = std::fs::remove_dir_all(&self.folder);
    }
}

#[expect(unsafe_code, reason = "a pseudoterminal is only reached through libc")]
fn pseudoterminal(lines: u16, columns: u16) -> (File, OwnedFd) {
    let (mut controller, mut terminal) = (-1, -1);
    let mut size = libc::winsize {
        ws_row: lines,
        ws_col: columns,
        ws_xpixel: 0,
        ws_ypixel: 0,
    };
    // SAFETY: the pointers are to live values, and name and termios may be null.
    let status = unsafe {
        libc::openpty(
            &raw mut controller,
            &raw mut terminal,
            std::ptr::null_mut(),
            std::ptr::null(),
            &raw mut size,
        )
    };
    assert_eq!(status, 0, "{}", std::io::Error::last_os_error());
    // SAFETY: openpty made both, and nothing else owns them.
    unsafe {
        (
            File::from_raw_fd(controller),
            OwnedFd::from_raw_fd(terminal),
        )
    }
}

/// A session drawn in a pseudoterminal, with the emulator as the terminal at its other end.
struct Shown {
    child: Child,
    term: Arc<Mutex<Term>>,
    controller: File,
    /// The side the client has, to read its modes.
    terminal: OwnedFd,
}

impl Shown {
    fn attach(sandbox: &Sandbox, session: &str) -> Self {
        let (controller, terminal) = pseudoterminal(24, 80);
        let child = sandbox
            .command()
            .args(["session", "attach", session])
            .stdin(terminal.try_clone().unwrap())
            .stdout(terminal.try_clone().unwrap())
            .stderr(terminal.try_clone().unwrap())
            .spawn()
            .unwrap();
        let term = Arc::new(Mutex::new(Term::new(80, 24, Options::default())));
        let reading = term.clone();
        let mut reader = controller.try_clone().unwrap();
        let mut answers = controller.try_clone().unwrap();
        std::thread::spawn(move || {
            let mut buffer = [0; 4096];
            while let Ok(read) = reader.read(&mut buffer) {
                if read == 0 {
                    return;
                }
                let mut term = reading.lock().unwrap();
                term.advance(&buffer[..read]);
                let replies = term.take_replies();
                drop(term);
                let _ = answers.write_all(&replies);
            }
        });
        Self {
            child,
            term,
            controller,
            terminal,
        }
    }

    fn screen(&self) -> Vec<String> {
        let term = self.term.lock().unwrap();
        term.screen()
            .rows()
            .iter()
            .map(|row| {
                let line: String = row.cells.iter().map(|cell| cell.c).collect();
                line.trim_end().to_owned()
            })
            .collect()
    }

    fn sees(&self, text: &str) {
        let deadline = Instant::now() + PATIENCE;
        while !self.screen().iter().any(|line| line.contains(text)) {
            assert!(
                Instant::now() < deadline,
                "no {text} in {:#?}",
                self.screen()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn type_keys(&mut self, keys: &[u8]) {
        self.controller.write_all(keys).unwrap();
    }

    /// Starts control mode and leaves with its D.
    fn detach(&mut self) {
        self.type_keys(CONTROL_MODE);
        self.sees("CONTROL");
        self.type_keys(b"d");
        assert!(self.ended().success());
    }

    fn ended(&mut self) -> ExitStatus {
        let deadline = Instant::now() + PATIENCE;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "still showing {:#?}",
                self.screen()
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The terminal is as a shell left it: back from the other screen, and typing echoed.
    fn is_given_back(&self) {
        let deadline = Instant::now() + PATIENCE;
        while self.term.lock().unwrap().alt_screen() {
            assert!(Instant::now() < deadline, "still on the other screen");
            std::thread::sleep(Duration::from_millis(5));
        }
        let modes = rustix::termios::tcgetattr(&self.terminal).unwrap();
        assert!(
            modes
                .local_modes
                .contains(rustix::termios::LocalModes::ECHO)
        );
    }

    #[expect(unsafe_code, reason = "signals are only reached through libc")]
    fn signal(&self, signal: i32) {
        let pid = self.child.id() as i32;
        // SAFETY: it takes two numbers, and the child has not been waited for.
        assert_eq!(unsafe { libc::kill(pid, signal) }, 0);
    }

    /// The terminal takes a new size. The kernel tells only a terminal's session of that, and
    /// the client here does not lead one, so the signal is sent by hand.
    #[expect(unsafe_code, reason = "a terminal's size is only set through libc")]
    fn resize(&self, lines: u16, columns: u16) {
        self.term
            .lock()
            .unwrap()
            .resize(usize::from(columns), usize::from(lines));
        let size = libc::winsize {
            ws_row: lines,
            ws_col: columns,
            ws_xpixel: 0,
            ws_ypixel: 0,
        };
        // SAFETY: the descriptor is open and the pointer is to a live value.
        let status = unsafe {
            libc::ioctl(
                self.controller.as_raw_fd(),
                libc::TIOCSWINSZ,
                &raw const size,
            )
        };
        assert_eq!(status, 0, "{}", std::io::Error::last_os_error());
        self.signal(libc::SIGWINCH);
    }
}

impl Drop for Shown {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn a_session_goes_from_new_to_closed_by_commands() {
    let sandbox = Sandbox::new();
    assert_eq!(sandbox.says(&["session"]), "");
    assert_eq!(
        sandbox.says(&["session", "--json"]).trim(),
        r#"{"ok":true,"sessions":[]}"#
    );

    let session = sandbox.new_session(&["--name", "build", "sleep", "1000"]);
    let listed = sandbox.says(&["session"]);
    assert!(listed.contains("build"), "{listed}");
    assert!(listed.contains("1 tab, 1 pane"), "{listed}");
    assert!(listed.contains("detached"), "{listed}");
    let listed = sandbox.says(&["session", "--json"]);
    assert!(listed.contains(r#""name":"build""#), "{listed}");

    sandbox.says(&["session", "rename", "build", "compile"]);
    assert!(sandbox.says(&["session"]).contains("compile"));
    sandbox.says(&["session", "rename", &session, ""]);
    assert!(!sandbox.says(&["session"]).contains("compile"));

    sandbox.fails(&["session", "close", &session], 1, "--force");
    assert!(sandbox.says(&["session"]).contains("1 tab"));
    sandbox.says(&["session", "close", "--force", &session]);
    assert_eq!(sandbox.says(&["session"]), "");
}

#[test]
fn what_cannot_be_done_says_why_with_a_status_scripts_tell_apart() {
    let sandbox = Sandbox::new();
    sandbox.fails(&["session", "attach"], 1, "no session is detached");
    sandbox.fails(&["session", "attach", "none"], 1, "called none");
    sandbox.fails(&["session", "close", "none"], 1, "called none");
    sandbox.fails(&["session", "rename", "none", "x"], 1, "called none");
    sandbox.fails(&["session", "detach", "none"], 1, "called none");
    sandbox.fails(&["session", "close"], 2, "name a session");
    sandbox.fails(&["session", "detach"], 2, "name a session");
    sandbox.fails(&["session", "rename", "x"], 2, "name a session");

    sandbox.new_session(&["sleep", "1000"]);
    sandbox.new_session(&["sleep", "1000"]);
    sandbox.fails(&["session", "attach"], 2, "several are detached");
    let answered = sandbox.caret(&["session", "--json", "close", "none"]);
    assert_eq!(answered.status.code(), Some(1));
    let said = String::from_utf8_lossy(&answered.stdout);
    assert!(said.contains(r#""ok":false"#), "{said}");
}

#[test]
fn a_command_run_in_a_session_acts_on_that_session() {
    let sandbox = Sandbox::new();
    let script = format!(
        "{} session rename from-inside; exec sleep 1000",
        env!("CARGO_BIN_EXE_caret")
    );
    sandbox.new_session(&["sh", "-c", &script]);
    sandbox.eventually(|listed| listed.contains("from-inside"));
}

#[test]
fn a_program_finds_its_parent_called_after_the_terminal() {
    let sandbox = Sandbox::new();
    let session = sandbox.new_session(&[
        "sh",
        "-c",
        "printf '[%s/%s]' \"$(cat /proc/$PPID/comm)\" \"$(tr '\\0' '\\n' </proc/$PPID/cmdline | head -n 1)\"; exec cat",
    ]);
    let shown = sandbox.show(&session);
    shown.sees("[caret/caret]");
}

#[test]
fn a_program_that_cannot_start_leaves_a_session_saying_so() {
    let sandbox = Sandbox::new();
    let session = sandbox.new_session(&["/no/such/program"]);
    let mut shown = sandbox.show(&session);
    shown.sees("/no/such/program");
    shown.signal(libc::SIGTERM);
    shown.ended();
}

#[test]
fn attaching_shows_the_session_and_detaching_leaves_it_running() {
    let sandbox = Sandbox::new();
    let session = sandbox.new_session(&["--name", "here", "sh", "-c", "echo started; exec cat"]);
    let mut shown = sandbox.show(&session);
    shown.sees("started");
    shown.sees("here");
    assert!(sandbox.says(&["session"]).contains("shown"));

    shown.type_keys(b"typed\r");
    shown.sees("typed");

    shown.detach();
    shown.is_given_back();
    sandbox.eventually(|listed| listed.contains("detached"));

    let mut again = sandbox.show(&session);
    again.sees("typed");
    again.detach();
}

#[test]
fn control_mode_splits_a_pane_and_closes_it() {
    let sandbox = Sandbox::new();
    let session = sandbox.new_session(&["sh", "-c", "echo first; exec cat"]);
    let mut shown = sandbox.show(&session);
    shown.sees("first");
    shown.type_keys(CONTROL_MODE);
    shown.sees("CONTROL");
    shown.type_keys(b"v");
    sandbox.eventually(|listed| listed.contains("2 panes"));
    shown.type_keys(b"x");
    sandbox.eventually(|listed| listed.contains("1 pane"));
    shown.type_keys(b"d");
    assert!(shown.ended().success());
}

#[test]
fn a_new_size_reaches_the_program() {
    let sandbox = Sandbox::new();
    let session = sandbox.new_session(&[
        "sh",
        "-c",
        // Not a trap: a shell drops a signal that comes while its trap runs.
        "while :; do stty size; sleep 0.02; done",
    ]);
    let mut shown = sandbox.show(&session);
    shown.sees("23 80");
    shown.resize(40, 120);
    shown.sees("39 120");
    shown.detach();
}

#[test]
fn a_signal_gives_the_terminal_back_and_leaves_the_session() {
    for signal in [libc::SIGTERM, libc::SIGHUP] {
        let sandbox = Sandbox::new();
        let session = sandbox.new_session(&["sh", "-c", "echo up; exec cat"]);
        let mut shown = sandbox.show(&session);
        shown.sees("up");
        shown.signal(signal);
        shown.ended();
        shown.is_given_back();
        assert!(sandbox.says(&["session"]).contains("1 tab"));
    }
}

#[test]
fn a_session_closed_elsewhere_ends_its_view() {
    let sandbox = Sandbox::new();
    let session = sandbox.new_session(&["sh", "-c", "echo up; exec cat"]);
    let mut shown = sandbox.show(&session);
    shown.sees("up");
    sandbox.says(&["session", "close", "--force", &session]);
    shown.ended();
    shown.is_given_back();
}

#[test]
fn a_program_ending_ends_the_view_of_its_session() {
    let sandbox = Sandbox::new();
    let session = sandbox.new_session(&["sh", "-c", "echo up; read line; exit 0"]);
    let mut shown = sandbox.show(&session);
    shown.sees("up");
    shown.type_keys(b"\r");
    assert!(shown.ended().success());
    shown.is_given_back();
    sandbox.eventually(str::is_empty);
}

#[test]
fn the_wheel_scrolls_the_history_and_a_key_brings_it_back() {
    let sandbox = Sandbox::new();
    let session = sandbox.new_session(&["sh", "-c", "seq 1 100; exec cat"]);
    let mut shown = sandbox.show(&session);
    shown.sees("100");
    for _ in 0..10 {
        shown.type_keys(b"\x1b[<64;10;5M");
    }
    let deadline = Instant::now() + PATIENCE;
    while shown.screen().iter().any(|line| line == "100") {
        assert!(Instant::now() < deadline, "{:#?}", shown.screen());
        std::thread::sleep(Duration::from_millis(5));
    }
    let screen = shown.screen();
    assert!(
        screen.last().is_some_and(|line| line.contains("control")),
        "the bar stays under what scrolls: {screen:#?}"
    );
    shown.type_keys(b"x");
    shown.sees("100");
    shown.detach();
}
