//! What Caret's terminal server and its clients say to each other over a Unix socket.
//!
//! A connection opens with a handshake: the client writes [`MAGIC`] and the oldest and
//! newest versions it speaks, the server answers with [`MAGIC`] and the version both
//! will speak, or 0 when there is none, and closes. After it, each message is a
//! little-endian `u32` length and that many bytes: a [`Request`] from the client, a
//! [`Message`] from the server, as [`wire`] writes them.
//!
//! A newer version only adds: new variants with new tags, never new fields in old ones.
//! Each side sends the other only what the agreed version has.

pub mod client;
pub mod encode;
pub mod layout;
mod messages;
mod screen;
pub mod wire;

use std::io::{self, Read, Write};
use std::path::PathBuf;

pub use messages::*;
pub use screen::*;
pub use wire::{Bytes, DecodeError, Wire};

pub const MAGIC: [u8; 8] = *b"caret\0\0\0";
pub const OLDEST: u16 = 1;
/// Version 2 adds split panes.
pub const NEWEST: u16 = 3;

/// The largest message either side reads; a frame of a huge screen is a few megabytes.
pub const LONGEST: usize = 64 << 20;

/// Where the server of the application `app_id` listens, in the user's runtime
/// directory; none without one.
#[must_use]
pub fn socket_path(app_id: &str) -> Option<PathBuf> {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").filter(|dir| !dir.is_empty())?;
    Some(PathBuf::from(runtime).join(app_id).join("server"))
}

/// Writes `value` as one message.
///
/// # Errors
///
/// When writing fails.
pub fn send<T: Wire>(out: &mut impl Write, value: &T) -> io::Result<()> {
    let payload = wire::encode(value);
    let length = u32::try_from(payload.len()).map_err(|_| io::ErrorKind::InvalidInput)?;
    let mut message = Vec::with_capacity(4 + payload.len());
    message.extend_from_slice(&length.to_le_bytes());
    message.extend_from_slice(&payload);
    out.write_all(&message)
}

/// Reads one message; none at the end of the stream.
///
/// # Errors
///
/// When reading fails, the stream ends inside a message, or it is longer than
/// [`LONGEST`] or not a `T`.
pub fn receive<T: Wire>(input: &mut impl Read) -> io::Result<Option<T>> {
    let mut length = [0; 4];
    match input.read_exact(&mut length) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
        Err(error) => return Err(error),
    }
    let length = u32::from_le_bytes(length) as usize;
    if length > LONGEST {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("a message of {length} bytes"),
        ));
    }
    let mut payload = vec![0; length];
    input.read_exact(&mut payload)?;
    wire::decode(&payload)
        .map(Some)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))
}

/// The client's half of the handshake, offering versions up to `newest`: the version
/// the server chose.
///
/// # Errors
///
/// When the other end is not a Caret server or speaks none of this client's versions.
pub fn greet(stream: &mut (impl Read + Write), newest: u16) -> io::Result<u16> {
    let mut hello = MAGIC.to_vec();
    hello.extend_from_slice(&OLDEST.to_le_bytes());
    hello.extend_from_slice(&newest.to_le_bytes());
    stream.write_all(&hello)?;
    let mut answer = [0; 10];
    stream.read_exact(&mut answer)?;
    if answer[..8] != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a Caret server",
        ));
    }
    match u16::from_le_bytes([answer[8], answer[9]]) {
        0 => Err(io::Error::new(
            io::ErrorKind::Unsupported,
            format!("the server speaks none of versions {OLDEST} to {newest}"),
        )),
        version => Ok(version),
    }
}

/// The server's half of the handshake: the version both speak, or none, which the
/// client has then been told.
///
/// # Errors
///
/// When the other end is not a Caret client.
pub fn welcome(stream: &mut (impl Read + Write)) -> io::Result<Option<u16>> {
    let mut hello = [0; 12];
    stream.read_exact(&mut hello)?;
    if hello[..8] != MAGIC {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "not a Caret client",
        ));
    }
    let oldest = u16::from_le_bytes([hello[8], hello[9]]);
    let newest = u16::from_le_bytes([hello[10], hello[11]]);
    let version = newest.min(NEWEST);
    let agreed = (version >= oldest.max(OLDEST)).then_some(version);
    let mut answer = MAGIC.to_vec();
    answer.extend_from_slice(&agreed.unwrap_or(0).to_le_bytes());
    stream.write_all(&answer)?;
    Ok(agreed)
}

#[cfg(test)]
mod tests {
    use std::os::unix::net::UnixStream;

    use super::*;

    #[test]
    fn a_client_and_server_agree_on_the_newest_version_both_speak() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || welcome(&mut server).unwrap());
        assert_eq!(greet(&mut client, NEWEST).unwrap(), NEWEST);
        assert_eq!(server.join().unwrap(), Some(NEWEST));
    }

    #[test]
    fn a_client_of_an_older_version_is_spoken_to_in_it() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || welcome(&mut server).unwrap());
        assert_eq!(greet(&mut client, 1).unwrap(), 1);
        assert_eq!(server.join().unwrap(), Some(1));
    }

    #[test]
    fn a_client_too_new_for_the_server_is_told_so() {
        let (mut client, mut server) = UnixStream::pair().unwrap();
        let server = std::thread::spawn(move || welcome(&mut server).unwrap());
        let mut hello = MAGIC.to_vec();
        hello.extend_from_slice(&(NEWEST + 1).to_le_bytes());
        hello.extend_from_slice(&(NEWEST + 5).to_le_bytes());
        client.write_all(&hello).unwrap();
        let mut answer = [0; 10];
        client.read_exact(&mut answer).unwrap();
        assert_eq!(answer[8..], [0, 0]);
        assert_eq!(server.join().unwrap(), None);
    }

    #[test]
    fn messages_go_through_whole() {
        let request = Request::Ask {
            id: 4,
            question: Question::OpenTab {
                session: SessionId(3),
                index: 1,
                command: Command {
                    argv: vec!["htop".to_owned()],
                    directory: Some(Bytes(b"/tmp".to_vec())),
                    env: vec![("TERM".to_owned(), "dumb".to_owned())],
                    hold: true,
                },
                size: Size {
                    columns: 80,
                    lines: 24,
                    cell_width: 9,
                    cell_height: 18,
                },
            },
        };
        let frame = Message::Frame {
            terminal: TerminalId(7),
            frame: Frame {
                columns: 80,
                lines: 24,
                rows: vec![Row {
                    line: 2,
                    cells: vec![Cell {
                        column: 4,
                        glyph: Some(Glyph {
                            text: GlyphText::Cluster("e\u{301}".into()),
                            style: BOLD,
                            wide: false,
                        }),
                        fg: Rgb::from_u32(0xffffff),
                        bg: Rgb::from_u32(0x1d1d20),
                        underline: Rgb::from_u32(0xff0000),
                        flags: CellFlags::UNDERCURL,
                    }],
                }],
                cursor: Some(Cursor {
                    point: Point { line: 2, column: 5 },
                    shape: CursorShape::Beam,
                    wide: false,
                }),
                mode: Mode::APP_CURSOR | Mode::SGR_MOUSE,
                history: 1000,
                offset: 3,
                ..Frame::default()
            },
        };
        let mut buffer = Vec::new();
        send(&mut buffer, &request).unwrap();
        send(&mut buffer, &frame).unwrap();
        let mut input = buffer.as_slice();
        assert_eq!(receive::<Request>(&mut input).unwrap(), Some(request));
        assert_eq!(receive::<Message>(&mut input).unwrap(), Some(frame));
        assert_eq!(receive::<Message>(&mut input).unwrap(), None);
    }

    #[test]
    fn a_message_longer_than_allowed_is_refused_before_it_is_read() {
        let length = (LONGEST as u32 + 1).to_le_bytes();
        let error = receive::<Request>(&mut length.as_slice()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
