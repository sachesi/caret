use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Action {
    Print(char),
    Execute(u8),
    Csi(Vec<Vec<u16>>, Vec<u8>, bool, u8),
    Esc(Vec<u8>, bool, u8),
    Osc(Vec<Vec<u8>>, bool),
    Hook(Vec<Vec<u16>>, Vec<u8>, bool, u8),
    Put(u8),
    Unhook,
}

#[derive(Default)]
struct Recorded(Vec<Action>);

impl Perform for Recorded {
    fn print(&mut self, c: char) {
        self.0.push(Action::Print(c));
    }

    fn execute(&mut self, byte: u8) {
        self.0.push(Action::Execute(byte));
    }

    fn csi(&mut self, params: &Params, intermediates: &[u8], ignored: bool, action: u8) {
        let params = params.iter().map(<[u16]>::to_vec).collect();
        self.0
            .push(Action::Csi(params, intermediates.to_vec(), ignored, action));
    }

    fn esc(&mut self, intermediates: &[u8], ignored: bool, byte: u8) {
        self.0
            .push(Action::Esc(intermediates.to_vec(), ignored, byte));
    }

    fn osc(&mut self, fields: &[&[u8]], bell: bool) {
        let fields = fields.iter().map(|field| field.to_vec()).collect();
        self.0.push(Action::Osc(fields, bell));
    }

    fn hook(&mut self, params: &Params, intermediates: &[u8], ignored: bool, action: u8) {
        let params = params.iter().map(<[u16]>::to_vec).collect();
        self.0.push(Action::Hook(
            params,
            intermediates.to_vec(),
            ignored,
            action,
        ));
    }

    fn put(&mut self, byte: u8) {
        self.0.push(Action::Put(byte));
    }

    fn unhook(&mut self) {
        self.0.push(Action::Unhook);
    }
}

impl vte::Perform for Recorded {
    fn print(&mut self, c: char) {
        Perform::print(self, c);
    }

    fn execute(&mut self, byte: u8) {
        Perform::execute(self, byte);
    }

    fn csi_dispatch(
        &mut self,
        params: &vte::Params,
        intermediates: &[u8],
        ignore: bool,
        action: char,
    ) {
        let params = params.iter().map(<[u16]>::to_vec).collect();
        self.0.push(Action::Csi(
            params,
            intermediates.to_vec(),
            ignore,
            action as u8,
        ));
    }

    fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
        Perform::esc(self, intermediates, ignore, byte);
    }

    fn osc_dispatch(&mut self, params: &[&[u8]], bell_terminated: bool) {
        Perform::osc(self, params, bell_terminated);
    }

    fn hook(&mut self, params: &vte::Params, intermediates: &[u8], ignore: bool, action: char) {
        let params = params.iter().map(<[u16]>::to_vec).collect();
        self.0.push(Action::Hook(
            params,
            intermediates.to_vec(),
            ignore,
            action as u8,
        ));
    }

    fn put(&mut self, byte: u8) {
        Perform::put(self, byte);
    }

    fn unhook(&mut self) {
        Perform::unhook(self);
    }
}

fn ours(chunks: &[&[u8]]) -> Vec<Action> {
    let mut parser = Parser::new();
    let mut recorded = Recorded::default();
    for chunk in chunks {
        parser.advance(&mut recorded, chunk);
    }
    recorded.0
}

/// What this parser and vte report, with their intended differences undone. Vte shows
/// DEL in text, which DEC's terminals ignore; carries out a lone byte from 0x80 to 0x9f as
/// an 8-bit control, which in UTF-8 is a broken character; drops such bytes from a device
/// control string, which may carry UTF-8 text; and gives a sequence without parameters a
/// zero one.
fn comparable(actions: Vec<Action>) -> Vec<Action> {
    actions
        .into_iter()
        .filter(|action| {
            *action != Action::Print('\u{7f}') && !matches!(action, Action::Put(0x80..))
        })
        .map(|action| match action {
            Action::Execute(0x80..=0x9f) => Action::Print('\u{fffd}'),
            Action::Csi(params, intermediates, ignored, final_byte) if params == [[0]] => {
                Action::Csi(Vec::new(), intermediates, ignored, final_byte)
            }
            Action::Hook(params, intermediates, ignored, final_byte) if params == [[0]] => {
                Action::Hook(Vec::new(), intermediates, ignored, final_byte)
            }
            action => action,
        })
        .collect()
}

fn theirs(chunks: &[&[u8]]) -> Vec<Action> {
    let mut parser: vte::Parser = vte::Parser::new();
    let mut recorded = Recorded::default();
    for chunk in chunks {
        parser.advance(&mut recorded, chunk);
    }
    recorded.0
}

fn csi(params: &[&[u16]], intermediates: &[u8], action: u8) -> Action {
    Action::Csi(
        params.iter().map(|group| group.to_vec()).collect(),
        intermediates.to_vec(),
        false,
        action,
    )
}

#[test]
fn text_and_controls() {
    assert_eq!(
        ours(&[b"a\xc3\xa9\r\n"]),
        [
            Action::Print('a'),
            Action::Print('é'),
            Action::Execute(b'\r'),
            Action::Execute(b'\n'),
        ]
    );
}

#[test]
fn parameters_and_sub_parameters() {
    assert_eq!(ours(&[b"\x1b[H"]), [csi(&[], b"", b'H')]);
    assert_eq!(ours(&[b"\x1b[;5H"]), [csi(&[&[0], &[5]], b"", b'H')]);
    assert_eq!(ours(&[b"\x1b[5;H"]), [csi(&[&[5], &[0]], b"", b'H')]);
    assert_eq!(
        ours(&[b"\x1b[4:3;38:2::10:20:30m"]),
        [csi(&[&[4, 3], &[38, 2, 0, 10, 20, 30]], b"", b'm')]
    );
    assert_eq!(ours(&[b"\x1b[?1049h"]), [csi(&[&[1049]], b"?", b'h')]);
    assert_eq!(ours(&[b"\x1b[99999A"]), [csi(&[&[u16::MAX]], b"", b'A')]);
}

#[test]
fn sequences_cut_between_reads_are_finished() {
    assert_eq!(
        ours(&[b"\x1b[3", b"8;5;1", b"m\xe2\x82", b"\xac"]),
        [csi(&[&[38], &[5], &[1]], b"", b'm'), Action::Print('€')]
    );
}

#[test]
fn operating_system_commands() {
    assert_eq!(
        ours(&[b"\x1b]0;title; with semicolon\x07"]),
        [Action::Osc(
            vec![
                b"0".to_vec(),
                b"title".to_vec(),
                b" with semicolon".to_vec()
            ],
            true
        )]
    );
    assert_eq!(
        ours(&[b"\x1b]8;;https://example.org\x1b\\"]),
        [
            Action::Osc(
                vec![b"8".to_vec(), b"".to_vec(), b"https://example.org".to_vec()],
                false
            ),
            Action::Esc(vec![], false, b'\\'),
        ]
    );
}

#[test]
fn broken_utf8_is_replaced() {
    assert_eq!(
        ours(&[b"\xff\xe2\x82x\xed\xa0\x80"]),
        [
            Action::Print('\u{fffd}'),
            Action::Print('\u{fffd}'),
            Action::Print('x'),
            Action::Print('\u{fffd}'),
            Action::Print('\u{fffd}'),
            Action::Print('\u{fffd}'),
        ]
    );
}

/// Bytes that make up the sequences terminals see, weighted towards those that move the
/// parser between states.
const ALPHABET: &[&[u8]] = &[
    b"\x1b",
    b"[",
    b"]",
    b"P",
    b"X",
    b"^",
    b"_",
    b"\\",
    b"\x07",
    b"\x18",
    b"\x1a",
    b"0",
    b"1",
    b"38",
    b";",
    b":",
    b"?",
    b">",
    b"=",
    b" ",
    b"!",
    b"\"",
    b"m",
    b"H",
    b"q",
    b"a",
    b"~",
    b"\x7f",
    b"\n",
    b"\r",
    b"\x00",
    b"\x05",
    b"\xc3\xa9",
    b"\xe2\x82\xac",
    b"\xf0\x9f\x98\x80",
    b"\xc3",
    b"\xe2\x82",
    b"\x80",
    b"\x9b",
    b"\xff",
    b"hello",
];

struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        (self.next() % bound as u64) as usize
    }
}

/// The same input through this parser and vte's gives the same actions, read whole and
/// cut at random places.
#[test]
fn agrees_with_vte_on_random_input() {
    let mut random = Random(0x2545_f491_4f6c_dd1d);
    for case in 0..20_000 {
        let mut input = Vec::new();
        for _ in 0..=random.below(40) {
            input.extend_from_slice(ALPHABET[random.below(ALPHABET.len())]);
        }
        let cut = random.below(input.len() + 1);
        let chunks: [&[u8]; 2] = [&input[..cut], &input[cut..]];
        let expected = comparable(theirs(&[&input]));
        let whole = comparable(ours(&[&input]));
        assert_eq!(whole, expected, "case {case}: {input:?}");
        let cut_up = comparable(ours(&chunks));
        assert_eq!(cut_up, expected, "case {case} cut at {cut}: {input:?}");
    }
}
