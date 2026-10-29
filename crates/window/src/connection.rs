//! The connection to the terminal server: requests go out through a writer thread, and
//! what the server sends comes into the event loop through a channel.

use std::io::Write;
use std::sync::mpsc;

use caret_protocol::client::ConnectError;
use caret_protocol::{Message, Question, Request};
use smithay_client_toolkit::reexports::calloop::channel::{self, Channel};

/// What the reader thread passes on.
pub enum Received {
    Message(Message),
    /// The connection ended, and why.
    Lost(String),
}

pub struct Connection {
    writer: mpsc::Sender<Vec<u8>>,
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
        let mut stream = caret_protocol::client::connect(app_id)?;
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
                next_question: 1,
            },
            received,
        ))
    }

    pub fn send(&self, request: &Request) {
        let mut bytes = Vec::new();
        // Writing into a vector cannot fail; a message past 4 GiB would.
        let _ = caret_protocol::send(&mut bytes, request);
        // Fails once the writer has ended, which the reader then reports.
        let _ = self.writer.send(bytes);
    }

    /// Asks `question`; the answer comes with the number returned.
    pub fn ask(&mut self, question: Question) -> u32 {
        let id = self.next_question;
        self.next_question = id.wrapping_add(1);
        self.send(&Request::Ask { id, question });
        id
    }
}
