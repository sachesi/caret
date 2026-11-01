//! The terminal the text client runs in: what it tells of itself when asked, and what it
//! sends, split into keys, mouse events and bytes to pass on as they are.

use std::fmt::Write as _;
use std::io::{self, Write};
use std::time::{Duration, Instant};

use caret_protocol::Rgb;
use caret_protocol::encode::{Key, Modifiers, MouseAction, MouseButton};
use rustix::event::{PollFd, PollFlags, Timespec};

/// The byte Ctrl+] sends, which starts control mode where the terminal cannot tell
/// Ctrl+Shift+Space apart.
const CONTROL: u8 = 0x1d;

/// What the terminal said of itself.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Host {
    /// It writes keys apart that would read the same as others, once asked to.
    pub enhanced: bool,
    pub foreground: Option<Rgb>,
    pub background: Option<Rgb>,
    pub ansi: [Option<Rgb>; 16],
    /// What was typed while it answered.
    pub typed: Vec<u8>,
    /// A Linux console, which has no other screen and sixteen colours.
    pub console: bool,
}

/// Asks the terminal for its colours and whether it can write keys apart, and waits a
/// second at most for the answers.
pub fn probe() -> Host {
    let query = questions();
    let mut stdout = io::stdout();
    if stdout
        .write_all(query.as_bytes())
        .and_then(|()| stdout.flush())
        .is_err()
    {
        return Host::default();
    }
    let stdin = io::stdin();
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut answers = Vec::new();
    loop {
        let (host, complete) = parse(&answers);
        let left = deadline.saturating_duration_since(Instant::now());
        if complete || left.is_zero() {
            return host;
        }
        let timeout = Timespec {
            tv_sec: 0,
            tv_nsec: i64::from(left.subsec_nanos()),
        };
        let mut fds = [PollFd::new(&stdin, PollFlags::IN)];
        if !rustix::event::poll(&mut fds, Some(&timeout)).is_ok_and(|ready| ready > 0) {
            return host;
        }
        // Past the buffer of `Stdin`, which would keep answers from the poll.
        let mut chunk = [0; 1024];
        match rustix::io::read(&stdin, &mut chunk) {
            Ok(0) | Err(_) => return host,
            Ok(read) => answers.extend_from_slice(&chunk[..read]),
        }
    }
}

/// Asks for the terminal's colours, and how it writes keys.
fn questions() -> String {
    let mut query = String::from("\x1b]10;?\x1b\\\x1b]11;?\x1b\\");
    for index in 0..16 {
        let _ = write!(query, "\x1b]4;{index};?\x1b\\");
    }
    // Every terminal answers the device attributes, and in order: the end of the answers.
    query.push_str("\x1b[?u\x1b[c");
    query
}

/// Asks for the terminal's colours again, for answers to arrive as input.
pub fn ask_again() {
    let mut stdout = io::stdout();
    let _ = stdout
        .write_all(questions().as_bytes())
        .and_then(|()| stdout.flush());
}

impl Host {
    /// Takes in a colour the terminal answered with, as input gave it.
    pub fn take_color(&mut self, answer: &[u8]) {
        color(self, answer);
    }
}

/// What the answers say, and whether they are all in.
fn parse(answers: &[u8]) -> (Host, bool) {
    let mut host = Host::default();
    let mut complete = false;
    let mut rest = answers;
    while let Some(&first) = rest.first() {
        if first != 0x1b {
            host.typed.push(first);
            rest = &rest[1..];
            continue;
        }
        match rest.get(1) {
            Some(b']') => {
                let body = &rest[2..];
                let Some(end) = body.iter().position(|&byte| byte == 0x07 || byte == 0x1b) else {
                    break;
                };
                color(&mut host, &body[..end]);
                let terminator = if body[end] == 0x07 { 1 } else { 2 };
                rest = &body[(end + terminator).min(body.len())..];
            }
            Some(b'[') => {
                let body = &rest[2..];
                let Some(end) = body.iter().position(|byte| (0x40..=0x7e).contains(byte)) else {
                    break;
                };
                match (body.first(), body[end]) {
                    (Some(b'?'), b'u') => host.enhanced = true,
                    (Some(b'?'), b'c') => complete = true,
                    _ => host.typed.extend_from_slice(&rest[..end + 3]),
                }
                rest = &body[end + 1..];
            }
            _ => {
                host.typed.push(first);
                rest = &rest[1..];
            }
        }
    }
    (host, complete)
}

/// Takes in a colour answer: `10;rgb:…`, `11;rgb:…` or `4;N;rgb:…`.
fn color(host: &mut Host, answer: &[u8]) {
    let Ok(answer) = std::str::from_utf8(answer) else {
        return;
    };
    let mut parts = answer.split(';');
    let slot = match parts.next() {
        Some("10") => &mut host.foreground,
        Some("11") => &mut host.background,
        Some("4") => match parts
            .next()
            .and_then(|index| index.parse::<usize>().ok())
            .and_then(|index| host.ansi.get_mut(index))
        {
            Some(slot) => slot,
            None => return,
        },
        _ => return,
    };
    *slot = parts.next().and_then(rgb);
}

/// `rgb:R/G/B`, each one to four hex digits.
fn rgb(spec: &str) -> Option<Rgb> {
    let mut channels = spec.strip_prefix("rgb:")?.split('/').map(|channel| {
        let value = u32::from_str_radix(channel, 16).ok()?;
        let digits = u32::try_from(channel.len())
            .ok()
            .filter(|&n| (1..=4).contains(&n))?;
        let most = (1 << (4 * digits)) - 1;
        u8::try_from((value * 255 + most / 2) / most).ok()
    });
    let (r, g, b) = (channels.next()??, channels.next()??, channels.next()??);
    Some(Rgb { r, g, b })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Mouse {
    pub button: MouseButton,
    pub action: MouseAction,
    pub modifiers: Modifiers,
    /// The cell, from 0.
    pub column: u16,
    pub line: u16,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Input<'a> {
    /// The key that starts and leaves control mode.
    Control,
    /// A key the terminal wrote apart, which the program may want in another encoding.
    Key(Key, Modifiers),
    Mouse(Mouse),
    /// Text, or a key in the old encoding, to pass on as they are.
    Bytes(&'a [u8]),
    /// The terminal's colours went from dark to light or back.
    SchemeChanged,
    /// The terminal answered with a colour.
    Color(&'a [u8]),
    /// The terminal has answered all it was asked.
    Answered,
}

/// The first input in `bytes`, and how many bytes it took; `enhanced` when the terminal
/// was asked to write keys apart.
pub fn next(bytes: &[u8], enhanced: bool) -> (Input<'_>, usize) {
    if !enhanced && bytes.first() == Some(&CONTROL) {
        return (Input::Control, 1);
    }
    if bytes.first() != Some(&0x1b) {
        let end = bytes
            .iter()
            .position(|&byte| byte == 0x1b || (!enhanced && byte == CONTROL))
            .unwrap_or(bytes.len());
        return (Input::Bytes(&bytes[..end]), end);
    }
    let length = sequence_length(bytes);
    let sequence = &bytes[..length];
    let input = if let Some(mouse) = mouse(sequence) {
        Input::Mouse(mouse)
    } else if matches!(sequence, b"\x1b[?997;1n" | b"\x1b[?997;2n") {
        Input::SchemeChanged
    } else if let Some(answer) = color_answer(sequence) {
        Input::Color(answer)
    } else if sequence.starts_with(b"\x1b[?") && sequence.last() == Some(&b'c') {
        Input::Answered
    } else if late_answer(sequence) {
        Input::Bytes(&[])
    } else if let Some(input) = enhanced.then(|| key(sequence)).flatten() {
        input
    } else if let Some((key, modifiers)) = named_key(sequence) {
        Input::Key(key, modifiers)
    } else {
        Input::Bytes(sequence)
    };
    (input, length)
}

/// The length of the escape sequence `bytes` starts with.
fn sequence_length(bytes: &[u8]) -> usize {
    match bytes.get(1) {
        None => 1,
        // The Linux console's first function keys.
        Some(b'[') if bytes.get(2) == Some(&b'[') => bytes.len().min(4),
        Some(b'[') => bytes
            .iter()
            .skip(2)
            .position(|byte| (0x40..=0x7e).contains(byte))
            .map_or(bytes.len(), |end| end + 3),
        Some(b']') => bytes
            .iter()
            .skip(2)
            .position(|&byte| byte == 0x07 || byte == 0x1b)
            .map_or(bytes.len(), |end| {
                (end + if bytes[end + 2] == 0x07 { 3 } else { 4 }).min(bytes.len())
            }),
        Some(b'O') => bytes.len().min(3),
        Some(_) => 2,
    }
}

/// What is inside an OSC sequence.
fn color_answer(sequence: &[u8]) -> Option<&[u8]> {
    let body = sequence.strip_prefix(b"\x1b]")?;
    Some(
        body.strip_suffix(b"\x07")
            .or_else(|| body.strip_suffix(b"\x1b\\"))
            .unwrap_or(body),
    )
}

/// An answer about the keys, which matter only to [`probe`].
fn late_answer(sequence: &[u8]) -> bool {
    sequence.starts_with(b"\x1b[?") && sequence.last() == Some(&b'u')
}

/// The parameters of a CSI sequence ending in `last`.
fn parameters(sequence: &[u8], last: u8) -> Option<&str> {
    let inner = sequence.strip_prefix(b"\x1b[")?.strip_suffix(&[last])?;
    std::str::from_utf8(inner).ok()
}

/// An SGR mouse report, `CSI < b ; x ; y M` or `m`.
fn mouse(sequence: &[u8]) -> Option<Mouse> {
    let released = sequence.last() == Some(&b'm');
    let inner = parameters(sequence, if released { b'm' } else { b'M' })?.strip_prefix('<')?;
    let mut numbers = inner.split(';').map(|number| number.parse::<u16>().ok());
    let (code, column, line) = (numbers.next()??, numbers.next()??, numbers.next()??);
    let button = match (code & 0b11, code & 64 != 0) {
        (0, true) => MouseButton::WheelUp,
        (1, true) => MouseButton::WheelDown,
        (0, false) => MouseButton::Left,
        (1, false) => MouseButton::Middle,
        (2, false) => MouseButton::Right,
        _ => MouseButton::None,
    };
    let action = if released {
        MouseAction::Release
    } else if code & 32 != 0 {
        MouseAction::Motion
    } else {
        MouseAction::Press
    };
    Some(Mouse {
        button,
        action,
        modifiers: Modifiers {
            shift: code & 4 != 0,
            alt: code & 8 != 0,
            ctrl: code & 16 != 0,
        },
        column: column.checked_sub(1)?,
        line: line.checked_sub(1)?,
    })
}

/// A key written apart, `CSI code ; modifiers u`.
fn key(sequence: &[u8]) -> Option<Input<'static>> {
    let mut fields = parameters(sequence, b'u')?.split(';');
    let code: u32 = fields.next()?.split(':').next()?.parse().ok()?;
    let bits = fields
        .next()
        .and_then(|field| field.split(':').next()?.parse::<u8>().ok())
        .map_or(0, |parameter| parameter.saturating_sub(1));
    let modifiers = Modifiers {
        shift: bits & 1 != 0,
        alt: bits & 2 != 0,
        ctrl: bits & 4 != 0,
    };
    let key = match code {
        27 => Key::Escape,
        13 => Key::Enter,
        9 => Key::Tab,
        127 => Key::Backspace,
        _ => Key::Char(char::from_u32(code).filter(|c| !c.is_control())?),
    };
    if key == Key::Char(' ') && modifiers.ctrl && modifiers.shift && !modifiers.alt {
        return Some(Input::Control);
    }
    Some(Input::Key(key, modifiers))
}

/// A key with a name of its own, whichever way the terminal writes it. The program it goes
/// to may want it written otherwise: arrows differ once it asks for application keys.
fn named_key(sequence: &[u8]) -> Option<(Key, Modifiers)> {
    let plain = |key| Some((key, Modifiers::default()));
    if let Some(&[letter]) = sequence.strip_prefix(b"\x1b[[") {
        return plain(Key::Function(
            letter
                .checked_sub(b'A')?
                .checked_add(1)
                .filter(|n| *n <= 5)?,
        ));
    }
    if let Some(&[letter]) = sequence.strip_prefix(b"\x1bO") {
        return plain(lettered_key(letter)?);
    }
    let body = sequence.strip_prefix(b"\x1b[")?;
    let (&last, parameters) = body.split_last()?;
    let parameters = std::str::from_utf8(parameters).ok()?;
    let mut fields = parameters.split(';');
    let first = fields.next()?;
    let bits = fields.next().map_or(Some(0), |field| {
        Some(field.parse::<u8>().ok()?.saturating_sub(1))
    })?;
    if fields.next().is_some() {
        return None;
    }
    let modifiers = Modifiers {
        shift: bits & 1 != 0,
        alt: bits & 2 != 0,
        ctrl: bits & 4 != 0,
    };
    let key = if last == b'~' {
        match first.parse::<u8>().ok()? {
            1 | 7 => Key::Home,
            2 => Key::Insert,
            3 => Key::Delete,
            4 | 8 => Key::End,
            5 => Key::PageUp,
            6 => Key::PageDown,
            number @ 11..=15 => Key::Function(number - 10),
            number @ 17..=21 => Key::Function(number - 11),
            number @ 23..=24 => Key::Function(number - 12),
            _ => return None,
        }
    } else if first.is_empty() || first == "1" {
        lettered_key(last)?
    } else {
        return None;
    };
    Some((key, modifiers))
}

fn lettered_key(letter: u8) -> Option<Key> {
    Some(match letter {
        b'A' => Key::Up,
        b'B' => Key::Down,
        b'C' => Key::Right,
        b'D' => Key::Left,
        b'H' => Key::Home,
        b'F' => Key::End,
        b'P' => Key::Function(1),
        b'Q' => Key::Function(2),
        b'R' => Key::Function(3),
        b'S' => Key::Function(4),
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn named_keys_are_known_however_the_terminal_writes_them() {
        fn key(sequence: &[u8]) -> Input<'_> {
            next(sequence, false).0
        }
        let plain = |key| Input::Key(key, Modifiers::default());
        assert_eq!(key(b"\x1b[A"), plain(Key::Up));
        assert_eq!(key(b"\x1bOA"), plain(Key::Up));
        assert_eq!(key(b"\x1b[[A"), plain(Key::Function(1)));
        assert_eq!(key(b"\x1b[[E"), plain(Key::Function(5)));
        assert_eq!(key(b"\x1b[1~"), plain(Key::Home));
        assert_eq!(key(b"\x1b[4~"), plain(Key::End));
        assert_eq!(key(b"\x1b[17~"), plain(Key::Function(6)));
        assert_eq!(key(b"\x1b[24~"), plain(Key::Function(12)));
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        assert_eq!(key(b"\x1b[3;2~"), Input::Key(Key::Delete, shift));
        assert_eq!(key(b"\x1b[Z"), Input::Bytes(b"\x1b[Z"));
    }

    #[test]
    fn answers_give_colours_and_keys_written_apart() {
        let (host, complete) =
            parse(b"\x1b]11;rgb:1d1d/1d1d/2020\x1b\\\x1b]4;1;rgb:c0/1c/28\x07a\x1b[?0u\x1b[?62;c");
        assert!(complete);
        assert!(host.enhanced);
        assert_eq!(host.background, Some(Rgb::from_u32(0x1d1d20)));
        assert_eq!(host.ansi[1], Some(Rgb::from_u32(0xc01c28)));
        assert_eq!(host.foreground, None);
        assert_eq!(host.typed, b"a");
        let (host, complete) = parse(b"\x1b]10;rgb:f/f/f\x07");
        assert!(!complete);
        assert_eq!(host.foreground, Some(Rgb::from_u32(0xffffff)));
    }

    #[test]
    fn the_terminal_s_news_of_its_colours_is_not_taken_for_typing() {
        assert_eq!(next(b"\x1b[?997;2n", false).0, Input::SchemeChanged);
        assert_eq!(next(b"\x1b[?997;1n", true).0, Input::SchemeChanged);
        let answer = b"\x1b]11;rgb:ffff/ffff/ffff\x1b\\x";
        let (input, used) = next(answer, false);
        assert_eq!(input, Input::Color(b"11;rgb:ffff/ffff/ffff"));
        assert_eq!(used, answer.len() - 1);
        assert_eq!(next(b"\x1b[?62;22c", false).0, Input::Answered);

        let mut host = Host::default();
        host.take_color(b"11;rgb:ffff/ffff/ffff");
        assert_eq!(
            host.background,
            Some(Rgb {
                r: 255,
                g: 255,
                b: 255
            })
        );
    }

    #[test]
    fn input_splits_into_text_keys_and_mouse_reports() {
        assert_eq!(next(b"ab\x1b[A", false), (Input::Bytes(b"ab"), 2));
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        assert_eq!(next(b"\x1b[1;5Ax", false), (Input::Key(Key::Up, ctrl), 6));
        assert_eq!(next(b"\x1d", false), (Input::Control, 1));
        assert_eq!(next(b"\x1d", true), (Input::Bytes(b"\x1d"), 1));
        assert_eq!(next(b"\x1b[32;6u", true), (Input::Control, 7));
        let ctrl_c = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        assert_eq!(
            next(b"\x1b[99;5u", true),
            (Input::Key(Key::Char('c'), ctrl_c), 7)
        );
        assert_eq!(
            next(b"\x1b[27u", true),
            (Input::Key(Key::Escape, Modifiers::default()), 5)
        );
        assert_eq!(next(b"\x1b[?1u", true), (Input::Bytes(&[]), 5));
        let (input, used) = next(b"\x1b[<40;3;7M", false);
        assert_eq!(used, 10);
        assert_eq!(
            input,
            Input::Mouse(Mouse {
                button: MouseButton::Left,
                action: MouseAction::Motion,
                modifiers: Modifiers {
                    alt: true,
                    ..Modifiers::default()
                },
                column: 2,
                line: 6,
            })
        );
    }
}
