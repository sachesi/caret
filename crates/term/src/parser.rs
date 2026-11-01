//! Escape sequences out of a stream of bytes: the state machine of DEC's terminals, with
//! UTF-8 text in its ground state, telling a `Perform` what it found.

/// Parameters a sequence keeps; any after are dropped, and the sequence marked ignored.
pub const MAX_PARAMS: usize = 32;
/// Intermediate bytes a sequence keeps, the same way.
pub const MAX_INTERMEDIATES: usize = 2;
/// Fields of an operating system command; the last one takes any `;` after.
pub const MAX_OSC_FIELDS: usize = 16;
/// Bytes of an operating system command kept, so that an endless one cannot take the
/// server's memory. Copies to the clipboard are the largest anyone sends.
pub const MAX_OSC_BYTES: usize = 8 << 20;

/// What a sequence's parameters were: groups split by `;`, each a value and any
/// sub-values after it split by `:`. Values past `u16::MAX` stay at it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Params {
    values: [u16; MAX_PARAMS],
    len: usize,
    /// A bit for each value that starts a group.
    starts: u32,
}

impl Params {
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    /// The number of groups.
    #[must_use]
    pub fn len(&self) -> usize {
        (self.starts & mask(self.len)).count_ones() as usize
    }

    /// Each group: its value, then its sub-values.
    #[must_use]
    pub fn iter(&self) -> Groups<'_> {
        Groups {
            params: self,
            next: 0,
        }
    }

    /// The first value of group `index`, or `default` where it is missing or zero.
    #[must_use]
    pub fn get_or(&self, index: usize, default: u16) -> u16 {
        match self.iter().nth(index) {
            Some([value, ..]) if *value != 0 => *value,
            _ => default,
        }
    }

    fn full(&self) -> bool {
        self.len == MAX_PARAMS
    }

    fn push(&mut self, value: u16, starts_group: bool) {
        if self.full() {
            return;
        }
        if starts_group {
            self.starts |= 1 << self.len;
        }
        self.values[self.len] = value;
        self.len += 1;
    }

    fn clear(&mut self) {
        self.len = 0;
        self.starts = 0;
    }
}

fn mask(len: usize) -> u32 {
    if len >= 32 { u32::MAX } else { (1 << len) - 1 }
}

impl<'a> IntoIterator for &'a Params {
    type Item = &'a [u16];
    type IntoIter = Groups<'a>;

    fn into_iter(self) -> Groups<'a> {
        self.iter()
    }
}

pub struct Groups<'a> {
    params: &'a Params,
    next: usize,
}

impl<'a> Iterator for Groups<'a> {
    type Item = &'a [u16];

    fn next(&mut self) -> Option<&'a [u16]> {
        let start = self.next;
        if start >= self.params.len {
            return None;
        }
        let mut end = start + 1;
        while end < self.params.len && self.params.starts & (1 << end) == 0 {
            end += 1;
        }
        self.next = end;
        Some(&self.params.values[start..end])
    }
}

/// What the parser tells about the stream.
pub trait Perform {
    /// A character to show.
    fn print(&mut self, c: char);

    /// A run of printable ASCII, each to show as `print` would.
    fn print_ascii(&mut self, text: &[u8]) {
        for &byte in text {
            self.print(char::from(byte));
        }
    }

    /// A C0 control.
    fn execute(&mut self, byte: u8);

    /// A control sequence: CSI, its parameters and intermediates, and its final byte.
    /// `ignored` says parameters or intermediates were dropped for being too many.
    fn csi(&mut self, params: &Params, intermediates: &[u8], ignored: bool, action: u8);

    /// An escape sequence and its final byte.
    fn esc(&mut self, intermediates: &[u8], ignored: bool, byte: u8);

    /// An operating system command, split into its fields; `bell` says it ended with BEL
    /// rather than ST, as replies to it should.
    fn osc(&mut self, fields: &[&[u8]], bell: bool);

    /// The start of a device control string.
    fn hook(&mut self, params: &Params, intermediates: &[u8], ignored: bool, action: u8);

    /// A byte of the device control string's data.
    fn put(&mut self, byte: u8);

    /// The end of the device control string.
    fn unhook(&mut self);
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum State {
    #[default]
    Ground,
    Escape,
    EscapeIntermediate,
    CsiEntry,
    CsiParam,
    CsiIntermediate,
    CsiIgnore,
    DcsEntry,
    DcsParam,
    DcsIntermediate,
    DcsPassthrough,
    DcsIgnore,
    OscString,
    /// Start of string, privacy message and application program command: kept by no one.
    IgnoredString,
}

const CAN: u8 = 0x18;
const SUB: u8 = 0x1a;
const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const DEL: u8 = 0x7f;
const REPLACEMENT: char = '\u{fffd}';

#[derive(Debug, Default)]
pub struct Parser {
    state: State,
    params: Params,
    /// The value being read, and whether any byte of a parameter was seen.
    value: u16,
    in_params: bool,
    /// The next value starts a group rather than following a `:`.
    group: bool,
    intermediates: [u8; MAX_INTERMEDIATES],
    intermediates_len: usize,
    ignored: bool,
    osc: Vec<u8>,
    /// Where each field of the command ends in `osc`.
    osc_ends: Vec<usize>,
    utf8: Utf8,
}

impl Parser {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Reads `bytes`, telling `perform` what they hold. A sequence or character cut off at
    /// the end is finished by the next call.
    pub fn advance(&mut self, perform: &mut impl Perform, bytes: &[u8]) {
        let mut index = 0;
        while index < bytes.len() {
            if self.state == State::Ground && !self.utf8.pending() {
                let run = bytes[index..]
                    .iter()
                    .position(|byte| !(0x20..0x7f).contains(byte))
                    .unwrap_or(bytes.len() - index);
                if run > 0 {
                    perform.print_ascii(&bytes[index..index + run]);
                    index += run;
                    continue;
                }
            }
            // Coloured text is mostly the parameters of its colours.
            if matches!(self.state, State::CsiEntry | State::CsiParam) {
                let run = bytes[index..]
                    .iter()
                    .position(|byte| !matches!(byte, b'0'..=b'9' | b':' | b';'))
                    .unwrap_or(bytes.len() - index);
                if run > 0 {
                    for &byte in &bytes[index..index + run] {
                        self.param(byte);
                    }
                    self.state = State::CsiParam;
                    index += run;
                    continue;
                }
            }
            self.byte(perform, bytes[index]);
            index += 1;
        }
    }

    fn byte(&mut self, perform: &mut impl Perform, byte: u8) {
        if self.state == State::Ground {
            self.ground(perform, byte);
            return;
        }
        match byte {
            CAN | SUB => {
                self.leave(perform);
                perform.execute(byte);
                self.state = State::Ground;
                return;
            }
            ESC => {
                self.leave(perform);
                self.enter_escape();
                return;
            }
            _ => {}
        }
        match self.state {
            State::Ground => unreachable!("handled above"),
            State::Escape => self.escape(perform, byte),
            State::EscapeIntermediate => match byte {
                0x00..=0x1f => perform.execute(byte),
                0x20..=0x2f => self.collect(byte),
                0x30..=0x7e => {
                    perform.esc(self.intermediates(), self.ignored, byte);
                    self.state = State::Ground;
                }
                _ => {}
            },
            State::CsiEntry | State::CsiParam | State::CsiIntermediate | State::CsiIgnore => {
                self.csi(perform, byte);
            }
            State::DcsEntry | State::DcsParam | State::DcsIntermediate => self.dcs(perform, byte),
            State::DcsPassthrough => {
                if byte != DEL {
                    perform.put(byte);
                }
            }
            State::DcsIgnore | State::IgnoredString => {}
            State::OscString => match byte {
                BEL => {
                    self.dispatch_osc(perform, true);
                    self.state = State::Ground;
                }
                0x00..=0x1f => {}
                _ => self.osc_byte(byte),
            },
        }
    }

    fn csi(&mut self, perform: &mut impl Perform, byte: u8) {
        match (self.state, byte) {
            (_, 0x00..=0x1f) => perform.execute(byte),
            (State::CsiIgnore, 0x40..=0x7e) => self.state = State::Ground,
            (State::CsiIgnore, _) => {}
            (_, 0x40..=0x7e) => self.dispatch_csi(perform, byte),
            (State::CsiEntry | State::CsiParam, b'0'..=b'9' | b':' | b';') => {
                self.param(byte);
                self.state = State::CsiParam;
            }
            (State::CsiEntry, 0x3c..=0x3f) => {
                self.collect(byte);
                self.state = State::CsiParam;
            }
            (State::CsiParam | State::CsiIntermediate, 0x30..=0x3f) => {
                self.state = State::CsiIgnore;
            }
            (_, 0x20..=0x2f) => {
                self.collect(byte);
                self.state = State::CsiIntermediate;
            }
            _ => {}
        }
    }

    fn dcs(&mut self, perform: &mut impl Perform, byte: u8) {
        match (self.state, byte) {
            (_, 0x40..=0x7e) => self.hook(perform, byte),
            (State::DcsEntry | State::DcsParam, b'0'..=b'9' | b':' | b';') => {
                self.param(byte);
                self.state = State::DcsParam;
            }
            (State::DcsEntry, 0x3c..=0x3f) => {
                self.collect(byte);
                self.state = State::DcsParam;
            }
            (State::DcsParam | State::DcsIntermediate, 0x30..=0x3f) => {
                self.state = State::DcsIgnore;
            }
            (_, 0x20..=0x2f) => {
                self.collect(byte);
                self.state = State::DcsIntermediate;
            }
            _ => {}
        }
    }

    fn ground(&mut self, perform: &mut impl Perform, byte: u8) {
        if self.utf8.pending() {
            match self.utf8.next(byte) {
                Decoded::Pending => return,
                Decoded::Char(c) => {
                    perform.print(c);
                    return;
                }
                // The byte does not continue the character, which is replaced, and is read
                // again on its own.
                Decoded::Broken => perform.print(REPLACEMENT),
            }
        }
        match byte {
            ESC => self.enter_escape(),
            0x00..=0x1f => perform.execute(byte),
            0x20..=0x7e => perform.print(char::from(byte)),
            DEL => {}
            _ => match self.utf8.start(byte) {
                Decoded::Pending => {}
                Decoded::Char(c) => perform.print(c),
                Decoded::Broken => perform.print(REPLACEMENT),
            },
        }
    }

    fn escape(&mut self, perform: &mut impl Perform, byte: u8) {
        match byte {
            0x00..=0x1f => perform.execute(byte),
            0x20..=0x2f => {
                self.collect(byte);
                self.state = State::EscapeIntermediate;
            }
            b'P' => {
                self.clear();
                self.state = State::DcsEntry;
            }
            b'[' => {
                self.clear();
                self.state = State::CsiEntry;
            }
            b']' => {
                self.osc.clear();
                self.osc_ends.clear();
                self.state = State::OscString;
            }
            b'X' | b'^' | b'_' => self.state = State::IgnoredString,
            0x30..=0x7e => {
                perform.esc(self.intermediates(), self.ignored, byte);
                self.state = State::Ground;
            }
            _ => {}
        }
    }

    /// Ends what the state was in when an escape, cancel or substitute cuts it short:
    /// an operating system command is carried out, a device control string closed.
    fn leave(&mut self, perform: &mut impl Perform) {
        match self.state {
            State::OscString => self.dispatch_osc(perform, false),
            State::DcsPassthrough => perform.unhook(),
            _ => {}
        }
    }

    fn enter_escape(&mut self) {
        self.clear();
        self.state = State::Escape;
    }

    fn clear(&mut self) {
        self.params.clear();
        self.value = 0;
        self.in_params = false;
        self.group = true;
        self.intermediates_len = 0;
        self.ignored = false;
    }

    fn intermediates(&self) -> &[u8] {
        &self.intermediates[..self.intermediates_len]
    }

    fn collect(&mut self, byte: u8) {
        if self.intermediates_len == MAX_INTERMEDIATES {
            self.ignored = true;
        } else {
            self.intermediates[self.intermediates_len] = byte;
            self.intermediates_len += 1;
        }
    }

    fn param(&mut self, byte: u8) {
        self.in_params = true;
        match byte {
            b';' | b':' => {
                self.end_value();
                self.group = byte == b';';
            }
            _ => {
                let digit = u16::from(byte - b'0');
                self.value = self.value.saturating_mul(10).saturating_add(digit);
            }
        }
    }

    fn end_value(&mut self) {
        if self.params.full() {
            self.ignored = true;
        } else {
            self.params.push(self.value, self.group);
        }
        self.value = 0;
    }

    fn finish_params(&mut self) {
        if self.in_params {
            self.end_value();
        }
    }

    fn dispatch_csi(&mut self, perform: &mut impl Perform, action: u8) {
        self.finish_params();
        perform.csi(&self.params, self.intermediates(), self.ignored, action);
        self.state = State::Ground;
    }

    fn hook(&mut self, perform: &mut impl Perform, action: u8) {
        self.finish_params();
        perform.hook(&self.params, self.intermediates(), self.ignored, action);
        self.state = State::DcsPassthrough;
    }

    fn osc_byte(&mut self, byte: u8) {
        if self.osc.len() >= MAX_OSC_BYTES {
            return;
        }
        if byte == b';' && self.osc_ends.len() < MAX_OSC_FIELDS - 1 {
            self.osc_ends.push(self.osc.len());
        } else {
            self.osc.push(byte);
        }
    }

    fn dispatch_osc(&mut self, perform: &mut impl Perform, bell: bool) {
        let mut fields: [&[u8]; MAX_OSC_FIELDS] = [&[]; MAX_OSC_FIELDS];
        let mut start = 0;
        let mut count = 0;
        for &end in self.osc_ends.iter().chain(std::iter::once(&self.osc.len())) {
            fields[count] = &self.osc[start..end];
            start = end;
            count += 1;
        }
        perform.osc(&fields[..count], bell);
    }
}

#[derive(Debug, Default)]
struct Utf8 {
    code: u32,
    /// Continuation bytes still to come.
    needed: u8,
    /// The range the next continuation byte has to be in, narrower after some lead bytes
    /// so that overlong forms, surrogates and values past U+10FFFF are refused.
    low: u8,
    high: u8,
}

enum Decoded {
    Pending,
    Char(char),
    Broken,
}

impl Utf8 {
    fn pending(&self) -> bool {
        self.needed > 0
    }

    fn start(&mut self, byte: u8) -> Decoded {
        let (needed, bits, low, high) = match byte {
            0xc2..=0xdf => (1, byte & 0x1f, 0x80, 0xbf),
            0xe0 => (2, byte & 0x0f, 0xa0, 0xbf),
            0xed => (2, byte & 0x0f, 0x80, 0x9f),
            0xe1..=0xef => (2, byte & 0x0f, 0x80, 0xbf),
            0xf0 => (3, byte & 0x07, 0x90, 0xbf),
            0xf4 => (3, byte & 0x07, 0x80, 0x8f),
            0xf1..=0xf3 => (3, byte & 0x07, 0x80, 0xbf),
            _ => return Decoded::Broken,
        };
        self.code = u32::from(bits);
        self.needed = needed;
        self.low = low;
        self.high = high;
        Decoded::Pending
    }

    fn next(&mut self, byte: u8) -> Decoded {
        if !(self.low..=self.high).contains(&byte) {
            self.needed = 0;
            return Decoded::Broken;
        }
        self.code = (self.code << 6) | u32::from(byte & 0x3f);
        self.needed -= 1;
        self.low = 0x80;
        self.high = 0xbf;
        if self.needed > 0 {
            return Decoded::Pending;
        }
        char::from_u32(self.code).map_or(Decoded::Broken, Decoded::Char)
    }
}

#[cfg(test)]
mod tests;
