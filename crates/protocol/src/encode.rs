//! What the terminal writes to the program for keys, the mouse and pastes, in the
//! encodings xterm set and `xterm-256color` describes.

use std::fmt::Write as _;

use crate::Mode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Char(char),
    Enter,
    Tab,
    Backspace,
    Escape,
    Up,
    Down,
    Right,
    Left,
    Home,
    End,
    Insert,
    Delete,
    PageUp,
    PageDown,
    Function(u8),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Modifiers {
    pub shift: bool,
    pub alt: bool,
    pub ctrl: bool,
}

impl Modifiers {
    /// The parameter xterm adds to a key's sequence, 1 for none.
    fn parameter(self) -> u8 {
        1 + u8::from(self.shift) + 2 * u8::from(self.alt) + 4 * u8::from(self.ctrl)
    }
}

/// The bytes for a key, or nothing for a key the terminal has no sequence for.
#[must_use]
pub fn key(key: Key, modifiers: Modifiers, mode: Mode) -> Option<Vec<u8>> {
    let parameter = modifiers.parameter();
    let bytes: Vec<u8> = match key {
        Key::Char(c) => return Some(char(c, modifiers)),
        Key::Enter if modifiers.alt => b"\x1b\r".to_vec(),
        Key::Enter if mode.contains(Mode::LINE_FEED_NEW_LINE) => b"\r\n".to_vec(),
        Key::Enter => b"\r".to_vec(),
        Key::Tab if modifiers.shift => b"\x1b[Z".to_vec(),
        Key::Tab if modifiers.alt => b"\x1b\t".to_vec(),
        Key::Tab => b"\t".to_vec(),
        Key::Backspace => {
            let erase = if modifiers.ctrl { 0x08 } else { 0x7f };
            if modifiers.alt {
                vec![0x1b, erase]
            } else {
                vec![erase]
            }
        }
        Key::Escape if modifiers.alt => b"\x1b\x1b".to_vec(),
        Key::Escape => b"\x1b".to_vec(),
        Key::Up | Key::Down | Key::Right | Key::Left | Key::Home | Key::End => {
            let letter = match key {
                Key::Up => 'A',
                Key::Down => 'B',
                Key::Right => 'C',
                Key::Left => 'D',
                Key::Home => 'H',
                _ => 'F',
            };
            if parameter > 1 {
                format!("\x1b[1;{parameter}{letter}").into_bytes()
            } else if mode.contains(Mode::APP_CURSOR) {
                format!("\x1bO{letter}").into_bytes()
            } else {
                format!("\x1b[{letter}").into_bytes()
            }
        }
        Key::Insert | Key::Delete | Key::PageUp | Key::PageDown => {
            let number = match key {
                Key::Insert => 2,
                Key::Delete => 3,
                Key::PageUp => 5,
                _ => 6,
            };
            tilde(number, parameter)
        }
        Key::Function(number @ 1..=4) => {
            let letter = char::from(b'P' + number - 1);
            if parameter > 1 {
                format!("\x1b[1;{parameter}{letter}").into_bytes()
            } else {
                format!("\x1bO{letter}").into_bytes()
            }
        }
        Key::Function(number @ 5..=12) => {
            const NUMBERS: [u8; 8] = [15, 17, 18, 19, 20, 21, 23, 24];
            tilde(NUMBERS[usize::from(number - 5)], parameter)
        }
        Key::Function(_) => return None,
    };
    Some(bytes)
}

fn tilde(number: u8, parameter: u8) -> Vec<u8> {
    if parameter > 1 {
        format!("\x1b[{number};{parameter}~").into_bytes()
    } else {
        format!("\x1b[{number}~").into_bytes()
    }
}

/// A character typed with Ctrl or Alt held, which no input method took. Ctrl turns
/// letters and the few symbols that have one into their control code; Alt puts an escape
/// in front.
fn char(c: char, modifiers: Modifiers) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(5);
    if modifiers.alt {
        bytes.push(0x1b);
    }
    let control = modifiers.ctrl.then(|| match c {
        ' ' | '@' | '2' => Some(0x00),
        'a'..='z' => Some(c as u8 - b'a' + 1),
        'A'..='Z' => Some(c as u8 - b'A' + 1),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '~' | '6' => Some(0x1e),
        '_' | '/' | '7' => Some(0x1f),
        '?' | '8' => Some(0x7f),
        _ => None,
    });
    match control.flatten() {
        Some(code) => bytes.push(code),
        None => bytes.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes()),
    }
    bytes
}

/// Whether a key went down, is held down and repeats, or came up.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Press {
    #[default]
    Down,
    Repeat,
    Up,
}

/// Keyboard enhancement flags a program can ask for: keys that would read the same as
/// others in the old encoding are written apart.
pub const DISAMBIGUATE: u8 = 1;
/// Repeats and releases are told too.
pub const EVENT_TYPES: u8 = 2;
/// The shifted character is told beside the key's own.
pub const ALTERNATE_KEYS: u8 = 4;
/// Every key is an escape sequence, text-typing ones included.
pub const ALL_AS_ESCAPES: u8 = 8;
/// With every key an escape sequence, the text it types is told in it.
pub const ASSOCIATED_TEXT: u8 = 16;

/// The bytes for a key event, `text` being what the key types: as [`key`] writes them,
/// or as the keyboard enhancements the program asked for in `mode` write them. Nothing
/// for an event the program is not told of.
#[must_use]
pub fn key_event(
    key: Key,
    modifiers: Modifiers,
    press: Press,
    text: Option<&str>,
    mode: Mode,
) -> Option<Vec<u8>> {
    let flags = mode.keyboard();
    let typed = text.filter(|_| !modifiers.ctrl && !modifiers.alt);
    if flags == 0 {
        if press == Press::Up {
            return None;
        }
        return match typed {
            Some(text) => Some(text.as_bytes().to_vec()),
            None => self::key(key, modifiers, mode),
        };
    }
    if press == Press::Up && flags & EVENT_TYPES == 0 {
        return None;
    }
    if flags & ALL_AS_ESCAPES == 0 {
        // Text stays text, and so do the keys that are not told apart from anything, all
        // without being told when they come up.
        let plain = modifiers == Modifiers::default()
            && matches!(key, Key::Enter | Key::Tab | Key::Backspace);
        if let Some(text) = typed.filter(|_| matches!(key, Key::Char(_))) {
            return (press != Press::Up).then(|| text.as_bytes().to_vec());
        }
        if plain {
            return (press != Press::Up)
                .then(|| self::key(key, modifiers, mode))
                .flatten();
        }
    }

    let mut field = String::new();
    let parameter = modifiers.parameter();
    let event = match press {
        _ if flags & EVENT_TYPES == 0 => "",
        Press::Down => "",
        Press::Repeat => ":2",
        Press::Up => ":3",
    };
    if parameter > 1 || !event.is_empty() {
        let _ = write!(field, "{parameter}{event}");
    }
    let legacy = |letter: char| {
        if field.is_empty() {
            format!("\x1b[{letter}")
        } else {
            format!("\x1b[1;{field}{letter}")
        }
    };
    let tilde = |number: u8| {
        if field.is_empty() {
            format!("\x1b[{number}~")
        } else {
            format!("\x1b[{number};{field}~")
        }
    };
    let sequence = match key {
        Key::Up => legacy('A'),
        Key::Down => legacy('B'),
        Key::Right => legacy('C'),
        Key::Left => legacy('D'),
        Key::Home => legacy('H'),
        Key::End => legacy('F'),
        Key::Function(1) => legacy('P'),
        Key::Function(2) => legacy('Q'),
        Key::Function(3) => tilde(13),
        Key::Function(4) => legacy('S'),
        Key::Insert => tilde(2),
        Key::Delete => tilde(3),
        Key::PageUp => tilde(5),
        Key::PageDown => tilde(6),
        Key::Function(number @ 5..=12) => {
            const NUMBERS: [u8; 8] = [15, 17, 18, 19, 20, 21, 23, 24];
            tilde(NUMBERS[usize::from(number - 5)])
        }
        _ => csi_u(key, modifiers, press, text, flags, &field)?,
    };
    Some(sequence.into_bytes())
}

/// A key as `CSI code u`, with `field` its modifiers and event type when it has any.
fn csi_u(
    key: Key,
    modifiers: Modifiers,
    press: Press,
    text: Option<&str>,
    flags: u8,
    field: &str,
) -> Option<String> {
    let (code, shifted) = match key {
        Key::Escape => (27, None),
        Key::Enter => (13, None),
        Key::Tab => (9, None),
        Key::Backspace => (127, None),
        // The key's own character is the unshifted one.
        Key::Char(c) if c.is_uppercase() => {
            let lower = c.to_lowercase().next().unwrap_or(c);
            (u32::from(lower), Some(u32::from(c)))
        }
        Key::Char(c) => (u32::from(c), None),
        Key::Function(number @ 13..=35) => (57_376 + u32::from(number - 13), None),
        _ => return None,
    };
    let mut sequence = format!("\x1b[{code}");
    if let Some(shifted) = shifted.filter(|_| flags & ALTERNATE_KEYS != 0 && modifiers.shift) {
        let _ = write!(sequence, ":{shifted}");
    }
    let text = text.filter(|_| {
        flags & ALL_AS_ESCAPES != 0 && flags & ASSOCIATED_TEXT != 0 && press != Press::Up
    });
    if !field.is_empty() || text.is_some() {
        let modifiers = if field.is_empty() { "1" } else { field };
        let _ = write!(sequence, ";{modifiers}");
    }
    if let Some(text) = text {
        let codes: Vec<String> = text.chars().map(|c| u32::from(c).to_string()).collect();
        let _ = write!(sequence, ";{}", codes.join(":"));
    }
    sequence.push('u');
    Some(sequence)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    WheelUp,
    WheelDown,
    /// Motion with no button held.
    None,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseAction {
    Press,
    Release,
    Motion,
}

/// A mouse event for a program that asked for them, at a 0-based cell. Nothing when the
/// modes in force do not report it, or the cell is past what the old encoding can say.
#[must_use]
pub fn mouse(
    button: MouseButton,
    action: MouseAction,
    modifiers: Modifiers,
    column: usize,
    line: usize,
    mode: Mode,
) -> Option<Vec<u8>> {
    let wheel = matches!(button, MouseButton::WheelUp | MouseButton::WheelDown);
    let reported = match action {
        MouseAction::Press => mode.intersects(Mode::MOUSE_MODE),
        MouseAction::Release => !wheel && mode.intersects(Mode::MOUSE_MODE),
        MouseAction::Motion if button == MouseButton::None => mode.contains(Mode::MOUSE_MOTION),
        MouseAction::Motion => mode.intersects(Mode::MOUSE_MOTION | Mode::MOUSE_DRAG),
    };
    if !reported {
        return None;
    }

    let mut code: u32 = match button {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
        MouseButton::None => 3,
        MouseButton::WheelUp => 64,
        MouseButton::WheelDown => 65,
    };
    code += 4 * u32::from(modifiers.shift) + 8 * u32::from(modifiers.alt);
    code += 16 * u32::from(modifiers.ctrl);
    if action == MouseAction::Motion {
        code += 32;
    }

    if mode.contains(Mode::SGR_MOUSE) {
        let end = if action == MouseAction::Release {
            'm'
        } else {
            'M'
        };
        let mut sequence = String::with_capacity(16);
        // Writing to a String cannot fail.
        let _ = write!(sequence, "\x1b[<{code};{};{}{end}", column + 1, line + 1);
        return Some(sequence.into_bytes());
    }

    // The old encoding does not say which button went up.
    if action == MouseAction::Release {
        code = (code & !3) | 3;
    }
    let mut bytes = b"\x1b[M".to_vec();
    for value in [code, column as u32 + 1, line as u32 + 1] {
        let value = value + 32;
        if mode.contains(Mode::UTF8_MOUSE) && value >= 0x80 {
            let c = char::from_u32(value).filter(|_| value < 0x800)?;
            bytes.extend_from_slice(c.encode_utf8(&mut [0; 4]).as_bytes());
        } else {
            bytes.push(u8::try_from(value).ok()?);
        }
    }
    Some(bytes)
}

/// Text from the clipboard as it goes to the program. Bracketed, it is stripped of escapes
/// (ESC, and CSI as UTF-8) so it cannot end the bracket early and run as keys; otherwise
/// newlines become the carriage returns Enter sends.
#[must_use]
pub fn paste(text: &[u8], bracketed: bool) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len() + 12);
    if bracketed {
        bytes.extend_from_slice(b"\x1b[200~");
        let mut rest = text;
        while let Some((&byte, tail)) = rest.split_first() {
            rest = match (byte, tail) {
                (0x1b, _) => tail,
                (0xc2, [0x9b, after @ ..]) => after,
                _ => {
                    bytes.push(byte);
                    tail
                }
            };
        }
        bytes.extend_from_slice(b"\x1b[201~");
    } else {
        let mut after_return = false;
        for &byte in text {
            if byte != b'\n' {
                bytes.push(byte);
            } else if !after_return {
                bytes.push(b'\r');
            }
            after_return = byte == b'\r';
        }
    }
    bytes
}

/// A path dropped on the terminal, quoted for a POSIX shell when it needs to be. Bytes,
/// since a file's name need not be UTF-8.
#[must_use]
pub fn shell_quoted(path: &[u8]) -> Vec<u8> {
    let plain = !path.is_empty()
        && path
            .iter()
            .all(|&byte| byte.is_ascii_alphanumeric() || b"/._-+:@%,=".contains(&byte));
    if plain {
        return path.to_vec();
    }
    let mut quoted = vec![b'\''];
    for &byte in path {
        if byte == b'\'' {
            quoted.extend_from_slice(br"'\''");
        } else {
            quoted.push(byte);
        }
    }
    quoted.push(b'\'');
    quoted
}

#[cfg(test)]
mod tests {
    use super::*;

    const NONE: Modifiers = Modifiers {
        shift: false,
        alt: false,
        ctrl: false,
    };
    const CTRL: Modifiers = Modifiers {
        shift: false,
        alt: false,
        ctrl: true,
    };

    fn sent(bytes: Option<Vec<u8>>) -> String {
        String::from_utf8(bytes.expect("a sequence")).unwrap()
    }

    #[test]
    fn arrows_follow_application_cursor_mode() {
        let normal = Mode::default();
        let application = normal | Mode::APP_CURSOR;
        assert_eq!(sent(key(Key::Up, NONE, normal)), "\x1b[A");
        assert_eq!(sent(key(Key::Up, NONE, application)), "\x1bOA");
        assert_eq!(sent(key(Key::Left, CTRL, application)), "\x1b[1;5D");
    }

    #[test]
    fn modifiers_go_into_the_parameter() {
        let shift_alt = Modifiers {
            shift: true,
            alt: true,
            ctrl: false,
        };
        let mode = Mode::default();
        assert_eq!(sent(key(Key::Delete, NONE, mode)), "\x1b[3~");
        assert_eq!(sent(key(Key::PageUp, shift_alt, mode)), "\x1b[5;4~");
        assert_eq!(sent(key(Key::Function(1), NONE, mode)), "\x1bOP");
        assert_eq!(sent(key(Key::Function(5), CTRL, mode)), "\x1b[15;5~");
        assert_eq!(sent(key(Key::Function(12), NONE, mode)), "\x1b[24~");
    }

    fn told(bytes: Option<Vec<u8>>) -> String {
        bytes
            .map(|bytes| String::from_utf8(bytes).unwrap())
            .unwrap_or_default()
    }

    #[test]
    fn without_enhancements_keys_and_text_are_as_before() {
        let none = Modifiers::default();
        let event = |key, press, text| told(key_event(key, none, press, text, Mode::empty()));
        assert_eq!(event(Key::Char('a'), Press::Down, Some("a")), "a");
        assert_eq!(event(Key::Char('a'), Press::Up, Some("a")), "");
        assert_eq!(event(Key::Escape, Press::Repeat, None), "\x1b");
    }

    #[test]
    fn disambiguated_keys_are_told_apart() {
        let mode = Mode::empty().with_keyboard(DISAMBIGUATE);
        assert_eq!(mode.keyboard(), DISAMBIGUATE);
        let none = Modifiers::default();
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        let event = |key, modifiers, text| told(key_event(key, modifiers, Press::Down, text, mode));
        assert_eq!(event(Key::Escape, none, None), "\x1b[27u");
        assert_eq!(event(Key::Char('a'), ctrl, Some("a")), "\x1b[97;5u");
        assert_eq!(event(Key::Char('a'), none, Some("a")), "a");
        assert_eq!(event(Key::Char('A'), shift, Some("A")), "A");
        assert_eq!(event(Key::Enter, none, None), "\r");
        assert_eq!(event(Key::Enter, shift, None), "\x1b[13;2u");
        assert_eq!(event(Key::Up, none, None), "\x1b[A");
        assert_eq!(event(Key::Up, ctrl, None), "\x1b[1;5A");
        assert_eq!(event(Key::Function(5), none, None), "\x1b[15~");
    }

    #[test]
    fn repeats_and_releases_are_told_when_asked_for() {
        let mode = Mode::empty().with_keyboard(DISAMBIGUATE | EVENT_TYPES);
        let none = Modifiers::default();
        let ctrl = Modifiers {
            ctrl: true,
            ..Modifiers::default()
        };
        assert_eq!(
            told(key_event(Key::Up, none, Press::Up, None, mode)),
            "\x1b[1;1:3A"
        );
        assert_eq!(
            told(key_event(
                Key::Char('a'),
                ctrl,
                Press::Repeat,
                Some("a"),
                mode
            )),
            "\x1b[97;5:2u"
        );
        assert_eq!(
            told(key_event(Key::Char('a'), none, Press::Up, Some("a"), mode)),
            ""
        );
        assert_eq!(told(key_event(Key::Enter, none, Press::Up, None, mode)), "");
    }

    #[test]
    fn every_key_can_be_an_escape_with_its_text() {
        let mode = Mode::empty()
            .with_keyboard(DISAMBIGUATE | ALL_AS_ESCAPES | ASSOCIATED_TEXT | ALTERNATE_KEYS);
        let shift = Modifiers {
            shift: true,
            ..Modifiers::default()
        };
        let none = Modifiers::default();
        let event = |key, modifiers, text| told(key_event(key, modifiers, Press::Down, text, mode));
        assert_eq!(event(Key::Char('a'), none, Some("a")), "\x1b[97;1;97u");
        assert_eq!(event(Key::Char('A'), shift, Some("A")), "\x1b[97:65;2;65u");
        assert_eq!(event(Key::Enter, none, None), "\x1b[13u");
    }

    #[test]
    fn ctrl_makes_control_codes() {
        let mode = Mode::default();
        assert_eq!(key(Key::Char('c'), CTRL, mode), Some(vec![0x03]));
        assert_eq!(key(Key::Char('C'), CTRL, mode), Some(vec![0x03]));
        assert_eq!(key(Key::Char(' '), CTRL, mode), Some(vec![0x00]));
        assert_eq!(key(Key::Char('['), CTRL, mode), Some(vec![0x1b]));
        assert_eq!(key(Key::Char('1'), CTRL, mode), Some(b"1".to_vec()));
        let alt = Modifiers { alt: true, ..NONE };
        assert_eq!(key(Key::Char('x'), alt, mode), Some(b"\x1bx".to_vec()));
        assert_eq!(key(Key::Backspace, CTRL, mode), Some(vec![0x08]));
    }

    #[test]
    fn sgr_mouse_reports_press_and_release() {
        let mode = Mode::MOUSE_REPORT_CLICK | Mode::SGR_MOUSE;
        let press = mouse(MouseButton::Left, MouseAction::Press, NONE, 4, 9, mode);
        assert_eq!(sent(press), "\x1b[<0;5;10M");
        let release = mouse(MouseButton::Left, MouseAction::Release, NONE, 4, 9, mode);
        assert_eq!(sent(release), "\x1b[<0;5;10m");
        let wheel = mouse(MouseButton::WheelDown, MouseAction::Press, CTRL, 0, 0, mode);
        assert_eq!(sent(wheel), "\x1b[<81;1;1M");
    }

    #[test]
    fn motion_is_reported_only_when_asked_for() {
        let click = Mode::MOUSE_REPORT_CLICK | Mode::SGR_MOUSE;
        let drag = click | Mode::MOUSE_DRAG;
        let any = click | Mode::MOUSE_MOTION;
        let held = |mode| mouse(MouseButton::Left, MouseAction::Motion, NONE, 0, 0, mode);
        let hover = |mode| mouse(MouseButton::None, MouseAction::Motion, NONE, 0, 0, mode);
        assert_eq!(held(click), None);
        assert_eq!(sent(held(drag)), "\x1b[<32;1;1M");
        assert_eq!(hover(drag), None);
        assert_eq!(sent(hover(any)), "\x1b[<35;1;1M");
    }

    #[test]
    fn the_old_mouse_encoding_offsets_by_32_and_gives_up_past_223() {
        let mode = Mode::MOUSE_REPORT_CLICK;
        let press = mouse(MouseButton::Right, MouseAction::Press, NONE, 0, 1, mode);
        assert_eq!(press, Some(vec![0x1b, b'[', b'M', 34, 33, 34]));
        let release = mouse(MouseButton::Right, MouseAction::Release, NONE, 0, 1, mode);
        assert_eq!(release, Some(vec![0x1b, b'[', b'M', 35, 33, 34]));
        assert_eq!(
            mouse(MouseButton::Left, MouseAction::Press, NONE, 300, 0, mode),
            None
        );
        let utf8 = mode | Mode::UTF8_MOUSE;
        let far = mouse(MouseButton::Left, MouseAction::Press, NONE, 300, 0, utf8).unwrap();
        assert_eq!(&far[4..6], "ō".as_bytes());
    }

    #[test]
    fn dropped_paths_are_quoted_only_when_needed() {
        assert_eq!(shell_quoted(b"/tmp/notes.txt"), b"/tmp/notes.txt");
        assert_eq!(shell_quoted(b"/tmp/my notes"), b"'/tmp/my notes'");
        assert_eq!(shell_quoted(b"it's"), br"'it'\''s'");
        assert_eq!(shell_quoted(b"/tmp/caf\xe9"), b"'/tmp/caf\xe9'");
    }

    #[test]
    fn pastes_cannot_escape_their_brackets() {
        let bytes = paste(b"ls\x1b[201~; rm -rf ~\n", true);
        assert_eq!(bytes, b"\x1b[200~ls[201~; rm -rf ~\n\x1b[201~");
        let bytes = paste("a\u{9b}201~b".as_bytes(), true);
        assert_eq!(bytes, b"\x1b[200~a201~b\x1b[201~");
        assert_eq!(paste(b"a\r\nb\nc", false), b"a\rb\rc");
    }
}
