//! Operating system commands and device control strings: titles, colours, links, the
//! clipboard, what the shell reports, and the settings programs ask about.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

use super::{Event, MAX_LINK_BYTES, MAX_TEXT_BYTES, Report, Term};
use crate::palette::{self, BACKGROUND, CURSOR, FOREGROUND};
use crate::parser::Params;

/// Bytes of a device control string kept; the ones answered are short.
const MAX_DCS: usize = 1024;

/// Bytes of the name of the tool that runs a container.
const MAX_RUNTIME_BYTES: usize = 64;

fn shortened(text: &str) -> String {
    text[..text.floor_char_boundary(MAX_TEXT_BYTES)].to_owned()
}

/// A device control string being read.
pub(super) struct Dcs {
    kind: DcsKind,
    data: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DcsKind {
    /// DECRQSS: a program asks for a setting.
    RequestSetting,
    /// XTGETTCAP: a program asks for terminfo capabilities.
    Capabilities,
}

impl Dcs {
    pub(super) fn start(_params: &Params, intermediates: &[u8], action: u8) -> Option<Self> {
        let kind = match (intermediates, action) {
            ([b'$'], b'q') => DcsKind::RequestSetting,
            ([b'+'], b'q') => DcsKind::Capabilities,
            _ => return None,
        };
        Some(Self {
            kind,
            data: Vec::new(),
        })
    }

    pub(super) fn push(&mut self, byte: u8) {
        if self.data.len() < MAX_DCS {
            self.data.push(byte);
        }
    }
}

impl Term {
    pub(super) fn device_control(&mut self, dcs: &Dcs) {
        match dcs.kind {
            DcsKind::RequestSetting => {
                let answer = match &dcs.data[..] {
                    b"m" => Some("0m".to_owned()),
                    b"r" => Some(format!("{};{}r", self.top + 1, self.bottom + 1)),
                    b" q" => {
                        let style = self.cursor_style();
                        let shape = match style.shape {
                            super::CursorShape::Block => 1,
                            super::CursorShape::Underline => 3,
                            super::CursorShape::Beam => 5,
                        } + u8::from(!style.blinking);
                        Some(format!("{shape} q"))
                    }
                    _ => None,
                };
                match answer {
                    Some(answer) => self.reply(&format!("\x1bP1$r{answer}\x1b\\")),
                    None => self.reply("\x1bP0$r\x1b\\"),
                }
            }
            DcsKind::Capabilities => self.reply("\x1bP0+r\x1b\\"),
        }
    }

    pub(super) fn command(&mut self, fields: &[&[u8]], bell: bool) {
        let Some(&number) = fields.first() else {
            return;
        };
        let text =
            |index: usize| String::from_utf8_lossy(fields.get(index).copied().unwrap_or_default());
        match number {
            b"0" | b"2" => self.set_title(text(1).into_owned()),
            b"4" => {
                for pair in fields[1..].chunks(2) {
                    if let [index, spec] = pair
                        && let Some(slot) = number_in(index).filter(|&slot| slot < 256)
                    {
                        self.color(slot, spec, bell, "4", Some(slot));
                    }
                }
            }
            b"10" | b"11" | b"12" => {
                let first = usize::from(number[1] - b'0');
                for (offset, spec) in fields[1..].iter().enumerate() {
                    let which = first + offset;
                    let slot = match which {
                        0 => FOREGROUND,
                        1 => BACKGROUND,
                        2 => CURSOR,
                        _ => break,
                    };
                    self.color(slot, spec, bell, &format!("1{which}"), None);
                }
            }
            b"104" => {
                if fields.len() < 2 || fields[1].is_empty() {
                    self.palette.reset_all();
                } else {
                    for index in &fields[1..] {
                        if let Some(slot) = number_in(index).filter(|&slot| slot < 256) {
                            self.palette.reset(slot);
                        }
                    }
                }
            }
            b"110" | b"111" | b"112" => {
                self.palette.reset(match number {
                    b"110" => FOREGROUND,
                    b"111" => BACKGROUND,
                    _ => CURSOR,
                });
            }
            b"8" => self.hyperlink(fields),
            b"52" => self.clipboard(fields),
            b"7" => {
                if let Some(directory) = directory(&text(1)) {
                    self.events
                        .push(Event::Report(Report::Directory(directory)));
                }
            }
            // Other numbered commands share OSC 9, such as progress reports.
            b"9" if !text(1).starts_with(|c: char| c.is_ascii_digit()) => {
                let body = fields[1..]
                    .iter()
                    .map(|field| String::from_utf8_lossy(field))
                    .collect::<Vec<_>>()
                    .join(";");
                self.events.push(Event::Report(Report::Notification {
                    title: String::new(),
                    body: shortened(&body),
                }));
            }
            b"777" => self.extension(fields),
            b"133" => self.shell_mark(fields),
            _ => {}
        }
    }

    /// Sets the colour in `slot` to `spec`, or answers what it is when `spec` is `?`.
    fn color(&mut self, slot: usize, spec: &[u8], bell: bool, number: &str, index: Option<usize>) {
        if spec == b"?" {
            let Some(color) = self
                .palette
                .slot(slot)
                .or_else(|| self.palette.slot(FOREGROUND))
            else {
                return;
            };
            let end = if bell { "\x07" } else { "\x1b\\" };
            let index = index.map(|index| format!("{index};")).unwrap_or_default();
            let color = palette::format(color);
            self.reply(&format!("\x1b]{number};{index}{color}{end}"));
            return;
        }
        if let Some(color) = palette::parse(spec) {
            self.palette.set(slot, color);
        }
    }

    fn hyperlink(&mut self, fields: &[&[u8]]) {
        let params = String::from_utf8_lossy(fields.get(1).copied().unwrap_or_default());
        // An address may hold `;`, which split it into more fields.
        let uri = fields
            .get(2..)
            .unwrap_or_default()
            .iter()
            .map(|field| String::from_utf8_lossy(field))
            .collect::<Vec<_>>()
            .join(";");
        if uri.is_empty() || uri.len() + params.len() > MAX_LINK_BYTES {
            self.cursor.style.link = 0;
            return;
        }
        let id = params
            .split(':')
            .find_map(|param| param.strip_prefix("id="))
            .unwrap_or_default();
        self.cursor.style.link = self.links.add(id, &uri);
    }

    fn clipboard(&mut self, fields: &[&[u8]]) {
        if !self.options.copy_from_programs {
            return;
        }
        let (Some(&targets), Some(&data)) = (fields.get(1), fields.get(2)) else {
            return;
        };
        // Reading the clipboard is refused: it would hand anything copied to the program.
        if data == b"?" {
            return;
        }
        let Some(text) = base64(data).and_then(|bytes| String::from_utf8(bytes).ok()) else {
            return;
        };
        let clipboard = targets.is_empty() || targets.contains(&b'c');
        self.events.push(Event::Clipboard {
            primary: !clipboard,
            text,
        });
    }

    fn extension(&mut self, fields: &[&[u8]]) {
        let text: Vec<String> = fields[1..]
            .iter()
            .map(|field| String::from_utf8_lossy(field).into_owned())
            .collect();
        let report = match text.iter().map(String::as_str).collect::<Vec<_>>()[..] {
            ["notify", title, ref body @ ..] => Report::Notification {
                title: shortened(title),
                body: shortened(&body.join(";")),
            },
            ["container", "push", name, runtime, ..]
                if container_name(name) && runtime.len() <= MAX_RUNTIME_BYTES =>
            {
                Report::ContainerEntered {
                    name: name.to_owned(),
                    runtime: runtime.to_owned(),
                }
            }
            ["container", "pop", ..] => Report::ContainerLeft,
            _ => return,
        };
        self.events.push(Event::Report(report));
    }

    /// OSC 133: where prompts start, and when commands run and end.
    fn shell_mark(&mut self, fields: &[&[u8]]) {
        match fields.get(1).copied() {
            Some(b"A") if !self.alt_active => self.row().prompt = true,
            Some(b"C") => self.events.push(Event::Report(Report::CommandStarted)),
            Some(b"D") => {
                let status = fields
                    .get(2)
                    .and_then(|field| std::str::from_utf8(field).ok())
                    .and_then(|status| status.parse().ok());
                self.events
                    .push(Event::Report(Report::CommandFinished(status)));
            }
            _ => {}
        }
    }
}

fn number_in(field: &[u8]) -> Option<usize> {
    std::str::from_utf8(field).ok()?.parse().ok()
}

/// Whether `name` is one a container can have, which also keeps it from reading as an
/// option to the tool that enters it.
fn container_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
}

/// The local path of a `file://` address, or none for another host's.
fn directory(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    let slash = rest.find('/')?;
    let host = &rest[..slash];
    let local = host.is_empty()
        || host == "localhost"
        || std::fs::read_to_string("/proc/sys/kernel/hostname")
            .is_ok_and(|name| name.trim_end() == host);
    if !local {
        return None;
    }
    unescape(&rest[slash..]).map(|path| PathBuf::from(OsString::from_vec(path)))
}

/// `text` with its `%XX` escapes decoded; none for a broken escape or a NUL, which no
/// path holds.
fn unescape(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len());
    let mut bytes = text.bytes();
    while let Some(byte) = bytes.next() {
        if byte == b'%' {
            let high = char::from(bytes.next()?).to_digit(16)?;
            let low = char::from(bytes.next()?).to_digit(16)?;
            out.push((high * 16 + low) as u8);
        } else {
            out.push(byte);
        }
    }
    (!out.contains(&0)).then_some(out)
}

/// Standard base64, padding optional; none for anything else.
fn base64(data: &[u8]) -> Option<Vec<u8>> {
    let value = |byte: u8| match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    };
    let data = data
        .strip_suffix(b"==")
        .or_else(|| data.strip_suffix(b"="))
        .unwrap_or(data);
    let mut out = Vec::with_capacity(data.len() * 3 / 4);
    let mut bits = 0u32;
    let mut count = 0;
    for &byte in data {
        bits = (bits << 6) | u32::from(value(byte)?);
        count += 6;
        if count >= 8 {
            count -= 8;
            out.push((bits >> count) as u8);
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_decodes_with_and_without_padding() {
        assert_eq!(base64(b"aGVsbG8=").as_deref(), Some(&b"hello"[..]));
        assert_eq!(base64(b"aGVsbG8").as_deref(), Some(&b"hello"[..]));
        assert_eq!(base64(b"aGk*").as_deref(), None);
    }

    #[test]
    fn containers_with_names_that_read_as_options_are_ignored() {
        assert!(container_name("fedora-toolbox-44"));
        assert!(!container_name("--rm"));
        assert!(!container_name(""));
        assert!(!container_name("a b"));
    }

    #[test]
    fn directories_are_read_from_local_addresses() {
        assert_eq!(
            directory("file:///home/me/My%20Files"),
            Some(PathBuf::from("/home/me/My Files"))
        );
        assert_eq!(directory("file://elsewhere.example/tmp"), None);
        assert_eq!(directory("file:///bad%2"), None);
    }
}
