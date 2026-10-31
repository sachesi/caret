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
}

/// Asks the terminal for its colours and whether it can write keys apart, and waits a
/// second at most for the answers.
pub fn probe() -> Host {
    let mut query = String::from("\x1b]10;?\x1b\\\x1b]11;?\x1b\\");
    for index in 0..16 {
        let _ = write!(query, "\x1b]4;{index};?\x1b\\");
    }
    // Every terminal answers the device attributes, and in order: the end of the answers.
    query.push_str("\x1b[?u\x1b[c");
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
    } else if late_answer(sequence) {
        Input::Bytes(&[])
    } else if let Some(input) = enhanced.then(|| key(sequence)).flatten() {
        input
    } else {
        Input::Bytes(sequence)
    };
    (input, length)
}

/// The length of the escape sequence `bytes` starts with.
fn sequence_length(bytes: &[u8]) -> usize {
    match bytes.get(1) {
        None => 1,
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

/// An answer to [`probe`] that came after it stopped waiting.
fn late_answer(sequence: &[u8]) -> bool {
    sequence.starts_with(b"\x1b]")
        || (sequence.starts_with(b"\x1b[?") && matches!(sequence.last(), Some(b'c' | b'u')))
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

#[cfg(test)]
mod tests {
    use super::*;

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
    fn input_splits_into_text_keys_and_mouse_reports() {
        assert_eq!(next(b"ab\x1b[A", false), (Input::Bytes(b"ab"), 2));
        assert_eq!(next(b"\x1b[1;5Ax", false), (Input::Bytes(b"\x1b[1;5A"), 6));
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
        assert_eq!(next(b"\x1b[?62c", true), (Input::Bytes(&[]), 6));
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
