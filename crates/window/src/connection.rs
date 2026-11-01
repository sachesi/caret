//! The connection to the terminal server: requests go out through a writer thread, and
//! what the server sends comes into the event loop through a channel.

use std::io::Write;
use std::sync::mpsc;

use caret_protocol::client::ConnectError;
use caret_protocol::{Bytes, Message, Question, Request, TerminalId};
use smithay_client_toolkit::reexports::calloop::channel::{self, Channel};

/// Bytes of input in one message. The server hangs up on a message past
/// [`caret_protocol::LONGEST`], which a large paste could otherwise be.
const INPUT_PIECE: usize = 1 << 20;

/// What the reader thread passes on.
pub enum Received {
    Message(Message),
    /// The connection ended, and why.
    Lost(String),
}

pub struct Connection {
    writer: mpsc::Sender<Vec<u8>>,
    /// The protocol version the server speaks with the window.
    pub version: u16,
    next_question: u32,
}

impl Connection {
    /// Connects to the server of `app_id`, starting it if needed, and the channel its
    /// messages come through.
    ///
    /// # Errors
    ///
    /// When the server cannot be reached or started, or a thread cannot be.
    pub fn open(app_id: &str) -> Result<(Self, Channel<Received>), ConnectError> {
        let (mut stream, version) =
            caret_protocol::client::connect(app_id, caret_protocol::NEWEST)?;
        let mut out = stream.try_clone().map_err(ConnectError::Greet)?;
        let (writer, written) = mpsc::channel::<Vec<u8>>();
        std::thread::Builder::new()
            .name("server writer".to_owned())
            .spawn(move || {
                for bytes in written {
                    if out.write_all(&bytes).is_err() {
                        break;
                    }
                }
            })
            .map_err(ConnectError::Greet)?;
        let (messages, received) = channel::channel();
        std::thread::Builder::new()
            .name("server reader".to_owned())
            .spawn(move || {
                let reason = loop {
                    match caret_protocol::receive::<Message>(&mut stream) {
                        Ok(Some(message)) => {
                            if messages.send(Received::Message(message)).is_err() {
                                return;
                            }
                        }
                        Ok(None) => break "the server closed the connection".to_owned(),
                        Err(error) => break error.to_string(),
                    }
                };
                let _ = messages.send(Received::Lost(reason));
            })
            .map_err(ConnectError::Greet)?;
        Ok((
            Self {
                writer,
                version,
                next_question: 1,
            },
            received,
        ))
    }

    pub fn send(&self, request: &Request) {
        // Fails only for a message past 4 GiB, and pieces of input are far shorter.
        let bytes = caret_protocol::message(request).unwrap_or_default();
        // Fails once the writer has ended, which the reader then reports.
        let _ = self.writer.send(bytes);
    }

    /// Sends what is typed or pasted for `terminal`'s program.
    pub fn input(&self, terminal: TerminalId, bytes: &[u8], bottom: bool) {
        for piece in bytes.chunks(INPUT_PIECE) {
            self.send(&Request::Input {
                terminal,
                bytes: Bytes(piece.to_vec()),
                bottom,
            });
        }
    }

    /// Asks `question`; the answer comes with the number returned.
    pub fn ask(&mut self, question: Question) -> u32 {
        let id = self.next_question;
        self.next_question = id.wrapping_add(1);
        self.send(&Request::Ask { id, question });
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_paste_longer_than_a_message_goes_in_pieces() {
        let (writer, written) = mpsc::channel();
        let connection = Connection {
            writer,
            version: caret_protocol::NEWEST,
            next_question: 1,
        };
        let pasted: Vec<u8> = (0..2 * caret_protocol::LONGEST + 5)
            .map(|at| at as u8)
            .collect();
        connection.input(TerminalId(1), &pasted, true);
        drop(connection);
        let mut received = Vec::new();
        for message in written {
            assert!(message.len() <= caret_protocol::LONGEST);
            let Some(Request::Input { bytes, .. }) =
                caret_protocol::receive(&mut message.as_slice()).unwrap()
            else {
                panic!("not input");
            };
            received.extend_from_slice(&bytes.0);
        }
        assert!(received == pasted);
    }
}
