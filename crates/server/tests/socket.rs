//! The server as a program: what clients see of it over its socket, and when it ends.

#![expect(clippy::unwrap_used, reason = "a test fails by panicking")]

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};

use caret_protocol::{
    Answer, Bytes, CursorShape, Frame, GlyphText, Message, NEWEST, Options, Palette, Question,
    Request, Rgb, Scroll, SessionId, Size, TerminalId, greet, receive, send,
};

const PATIENCE: Duration = Duration::from_secs(20);

const SIZE: Size = Size {
    columns: 40,
    lines: 5,
    cell_width: 8,
    cell_height: 16,
};

/// A running server with a folder of its own for the socket, both gone at the end of a test.
struct Server {
    child: Child,
    folder: PathBuf,
}

impl Server {
    fn start() -> Self {
        static FOLDERS: AtomicU32 = AtomicU32::new(0);
        let name = format!(
            "caret-socket-{}-{}",
            std::process::id(),
            FOLDERS.fetch_add(1, Ordering::Relaxed)
        );
        let folder = std::env::temp_dir().join(name);
        std::fs::create_dir_all(&folder).unwrap();
        let child = Self::spawn(&folder.join("server"));
        Self { child, folder }
    }

    fn spawn(socket: &std::path::Path) -> Child {
        Command::new(env!("CARGO_BIN_EXE_caret-server"))
            .arg("--socket")
            .arg(socket)
            .stdin(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap()
    }

    fn socket(&self) -> PathBuf {
        self.folder.join("server")
    }

    fn connect(&self, newest: u16) -> Client {
        let deadline = Instant::now() + PATIENCE;
        let mut stream = loop {
            match UnixStream::connect(self.socket()) {
                Ok(stream) => break stream,
                Err(error) => assert!(Instant::now() < deadline, "connecting: {error}"),
            }
            std::thread::sleep(Duration::from_millis(5));
        };
        stream.set_read_timeout(Some(PATIENCE)).unwrap();
        assert_eq!(greet(&mut stream, newest).unwrap(), newest);
        let mut client = Client {
            stream,
            asked: 0,
            rows: Vec::new(),
        };
        client.send(&Request::Look {
            palette: Palette {
                foreground: Rgb::from_u32(0xffffff),
                background: Rgb::from_u32(0),
                ansi: vec![Rgb::from_u32(0x808080); 16],
            },
            options: Options {
                scrollback: 1000,
                cursor_shape: CursorShape::Block,
                cursor_blinks: false,
                copy_from_programs: false,
                scroll_on_output: false,
            },
        });
        client
    }

    fn ended(&mut self) -> ExitStatus {
        let deadline = Instant::now() + PATIENCE;
        loop {
            if let Some(status) = self.child.try_wait().unwrap() {
                return status;
            }
            assert!(Instant::now() < deadline, "the server did not end");
            std::thread::sleep(Duration::from_millis(5));
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.folder);
    }
}

struct Client {
    stream: UnixStream,
    asked: u32,
    /// The text of each line of the screen shown, and the row under it, from its frames.
    rows: Vec<String>,
}

impl Client {
    fn send(&mut self, request: &Request) {
        send(&mut self.stream, request).unwrap();
    }

    fn next(&mut self) -> Message {
        receive::<Message>(&mut self.stream)
            .unwrap()
            .expect("the server hung up")
    }

    fn ask(&mut self, question: Question) -> Answer {
        self.asked += 1;
        let asked = self.asked;
        self.send(&Request::Ask {
            id: asked,
            question,
        });
        loop {
            if let Message::Answer { id, answer } = self.next()
                && id == asked
            {
                return answer;
            }
        }
    }

    /// A new session, shown, with `argv` in a tab of it.
    fn open(&mut self, argv: &[&str]) -> (SessionId, TerminalId) {
        let Answer::Session { session } = self.ask(Question::NewSession) else {
            panic!("no session");
        };
        self.send(&Request::Attach { session });
        let answer = self.ask(Question::OpenTab {
            session,
            index: 0,
            command: caret_protocol::Command {
                argv: argv.iter().map(|&arg| arg.to_owned()).collect(),
                ..caret_protocol::Command::default()
            },
            size: SIZE,
        });
        let Answer::Opened { terminal } = answer else {
            panic!("no terminal: {answer:?}");
        };
        (session, terminal)
    }

    fn type_into(&mut self, terminal: TerminalId, text: &str) {
        self.send(&Request::Input {
            terminal,
            bytes: Bytes(text.as_bytes().to_vec()),
            bottom: true,
        });
    }

    fn take(&mut self, frame: &Frame) {
        self.rows
            .resize(usize::from(frame.lines) + 1, String::new());
        for row in &frame.rows {
            let mut text = String::new();
            for cell in &row.cells {
                let column = usize::from(cell.column);
                text.extend(std::iter::repeat_n(' ', column.saturating_sub(text.len())));
                if let Some(GlyphText::Char(c)) = cell.glyph.as_ref().map(|glyph| &glyph.text) {
                    text.push(*c);
                }
            }
            self.rows[usize::from(row.line)] = text;
        }
    }

    /// Takes frames of `terminal` until `wanted` holds of one.
    fn frame_until(&mut self, terminal: TerminalId, wanted: impl Fn(&Frame, &[String]) -> bool) {
        let deadline = Instant::now() + PATIENCE;
        loop {
            self.send(&Request::Frame { terminal });
            let frame = loop {
                if let Message::Frame {
                    terminal: of,
                    frame,
                } = self.next()
                    && of == terminal
                {
                    break frame;
                }
            };
            self.take(&frame);
            if wanted(&frame, &self.rows) {
                return;
            }
            assert!(Instant::now() < deadline, "not shown in: {:?}", self.rows);
        }
    }

    fn sees(&mut self, terminal: TerminalId, text: &str) {
        self.frame_until(terminal, |_, rows| {
            rows.iter().any(|row| row.contains(text))
        });
    }
}

fn run_with(arguments: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_caret-server"))
        .args(arguments)
        .stdin(Stdio::null())
        .output()
        .unwrap()
}

#[test]
fn it_says_what_it_takes_and_refuses_anything_else() {
    let help = run_with(&["--help"]);
    assert!(help.status.success());
    assert!(String::from_utf8_lossy(&help.stdout).contains("--socket"));
    assert!(run_with(&["--version"]).status.success());
    assert_eq!(run_with(&["--nonsense"]).status.code(), Some(1));
    assert_eq!(run_with(&["--socket"]).status.code(), Some(1));
}

#[test]
fn it_shows_a_program_and_takes_its_input_over_the_socket() {
    let server = Server::start();
    let mut client = server.connect(NEWEST);
    let (_, terminal) = client.open(&["cat"]);
    client.type_into(terminal, "typed here\n");
    client.sees(terminal, "typed here");
}

#[test]
fn a_program_outlives_the_client_and_shows_again_to_the_next() {
    let server = Server::start();
    let mut first = server.connect(NEWEST);
    let (session, terminal) = first.open(&["sh", "-c", "echo left-running; exec cat"]);
    first.sees(terminal, "left-running");
    drop(first);

    let mut next = server.connect(NEWEST);
    next.send(&Request::Attach { session });
    next.sees(terminal, "left-running");
    next.type_into(terminal, "still here\n");
    next.sees(terminal, "still here");
}

#[test]
fn it_ends_with_its_last_terminal_once_no_client_is_left() {
    let mut server = Server::start();
    let mut client = server.connect(NEWEST);
    let (session, _) = client.open(&["cat"]);
    client.send(&Request::CloseSession { session });
    assert_ne!(client.ask(Question::SessionList), Answer::Gone);
    assert!(server.child.try_wait().unwrap().is_none());
    drop(client);
    assert!(server.ended().success());
}

#[test]
fn a_session_left_detached_keeps_it_running() {
    let mut server = Server::start();
    let mut client = server.connect(NEWEST);
    let (session, terminal) = client.open(&["sh", "-c", "echo kept; exec cat"]);
    client.sees(terminal, "kept");
    drop(client);
    std::thread::sleep(Duration::from_millis(100));
    assert!(server.child.try_wait().unwrap().is_none());

    let mut client = server.connect(NEWEST);
    client.send(&Request::CloseSession { session });
    drop(client);
    assert!(server.ended().success());
}

#[test]
fn bytes_from_what_is_no_client_leave_the_others_alone() {
    let server = Server::start();
    let mut client = server.connect(NEWEST);
    let (_, terminal) = client.open(&["cat"]);

    let mut stranger = UnixStream::connect(server.socket()).unwrap();
    stranger.write_all(b"GET / HTTP/1.1\r\n\r\n").unwrap();
    drop(stranger);
    let bad_messages: [&[u8]; 3] = [
        // Longer than any message is.
        &u32::MAX.to_le_bytes(),
        // Cut short.
        &[100, 0, 0, 0, 1, 2, 3],
        // Not a request.
        &[3, 0, 0, 0, 0xff, 0xff, 0xff],
    ];
    for bytes in bad_messages {
        let mut stream = UnixStream::connect(server.socket()).unwrap();
        greet(&mut stream, NEWEST).unwrap();
        stream.write_all(bytes).unwrap();
    }

    client.type_into(terminal, "unharmed\n");
    client.sees(terminal, "unharmed");
}

#[test]
fn a_second_server_leaves_the_first_listening_and_says_why() {
    let server = Server::start();
    // A server with nothing to keep ends with its last client.
    let mut client = server.connect(NEWEST);
    let mut second = Command::new(env!("CARGO_BIN_EXE_caret-server"))
        .arg("--socket")
        .arg(server.socket())
        .stdin(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + PATIENCE;
    let status = loop {
        if let Some(status) = second.try_wait().unwrap() {
            break status;
        }
        if Instant::now() >= deadline {
            second.kill().unwrap();
            second.wait().unwrap();
            panic!("the second server went on running");
        }
        std::thread::sleep(Duration::from_millis(5));
    };
    assert_eq!(status.code(), Some(1));
    let mut said = String::new();
    second
        .stderr
        .take()
        .unwrap()
        .read_to_string(&mut said)
        .unwrap();
    assert!(said.contains("already listens"), "{said}");
    let (_, terminal) = client.open(&["sh", "-c", "echo first; exec cat"]);
    client.sees(terminal, "first");
}

#[test]
fn a_socket_left_behind_by_a_killed_server_is_taken_over() {
    let mut server = Server::start();
    let client = server.connect(NEWEST);
    server.child.kill().unwrap();
    server.child.wait().unwrap();
    drop(client);
    assert!(server.socket().exists());

    server.child = Server::spawn(&server.socket());
    let mut client = server.connect(NEWEST);
    let (_, terminal) = client.open(&["sh", "-c", "echo taken-over; exec cat"]);
    client.sees(terminal, "taken-over");
}

/// A terminal with history, its view far enough up for the last line written to be under it.
fn scrolled_up(client: &mut Client) -> TerminalId {
    let (_, terminal) = client.open(&["sh", "-c", "seq 1 20; echo end; exec cat"]);
    client.sees(terminal, "end");
    client.send(&Request::Scroll {
        terminal,
        scroll: Scroll::Lines { lines: 2 },
    });
    client.frame_until(terminal, |frame, _| frame.offset == 2);
    terminal
}

#[test]
fn the_newest_client_is_sent_the_row_under_a_view_scrolled_up() {
    let server = Server::start();
    let mut client = server.connect(NEWEST);
    scrolled_up(&mut client);
    assert_eq!(client.rows[usize::from(SIZE.lines)], "end");
}

#[test]
fn a_client_of_an_older_version_is_sent_no_more_than_its_screen() {
    let server = Server::start();
    let mut client = server.connect(5);
    scrolled_up(&mut client);
    assert!(
        client.rows[usize::from(SIZE.lines)].is_empty(),
        "{:?}",
        client.rows
    );
}

#[test]
fn two_clients_each_keep_their_own_place_in_the_history() {
    let server = Server::start();
    let mut first = server.connect(NEWEST);
    let (session, terminal) = first.open(&["sh", "-c", "seq 1 30; echo end; exec cat"]);
    first.sees(terminal, "end");
    let mut second = server.connect(NEWEST);
    second.send(&Request::Attach { session });
    second.sees(terminal, "end");

    second.send(&Request::Scroll {
        terminal,
        scroll: Scroll::Lines { lines: 10 },
    });
    second.frame_until(terminal, |frame, _| frame.offset == 10);
    first.type_into(terminal, "at the bottom\n");
    first.sees(terminal, "at the bottom");
    second.frame_until(terminal, |frame, rows| {
        frame.offset >= 10 && !rows.iter().any(|row| row.contains("at the bottom"))
    });
}

#[test]
fn a_client_that_stops_reading_holds_up_no_one_else() {
    let server = Server::start();
    let mut client = server.connect(NEWEST);
    let (session, terminal) = client.open(&["sh", "-c", "seq 1 200000; echo finished; exec cat"]);
    let mut stopped = server.connect(NEWEST);
    stopped.send(&Request::Attach { session });
    stopped.send(&Request::Frame { terminal });
    client.sees(terminal, "finished");
    drop(stopped);
}
