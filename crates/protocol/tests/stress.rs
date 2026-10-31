//! Whatever bytes come off the socket, reading them is an error at worst.

use caret_protocol::layout::{Node, Plan};
use caret_protocol::{
    Bytes, Command, Message, Question, Request, SessionId, Size, TerminalId, receive, wire,
};

struct Random(u64);

impl Random {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    fn below(&mut self, limit: usize) -> usize {
        (self.next() >> 33) as usize % limit
    }
}

fn valid() -> Vec<Vec<u8>> {
    let size = Size {
        columns: 80,
        lines: 24,
        cell_width: 8,
        cell_height: 16,
    };
    let command = Command {
        argv: vec!["htop".to_owned(), "é".to_owned()],
        directory: Some(Bytes(b"/tmp".to_vec())),
        env: vec![("A".to_owned(), "b".to_owned())],
        hold: true,
    };
    let plan = Plan::Split {
        vertical: true,
        share: 5000,
        first: Box::new(Plan::Kept),
        second: Box::new(Plan::Run {
            command: command.clone(),
            size,
        }),
    };
    let requests = [
        Request::Ask {
            id: 1,
            question: Question::Arrange {
                terminal: TerminalId(1),
                plan,
            },
        },
        Request::Ask {
            id: 2,
            question: Question::OpenTab {
                session: SessionId(1),
                index: 0,
                command,
                size,
            },
        },
        Request::Input {
            terminal: TerminalId(1),
            bytes: Bytes(vec![1, 2, 255]),
            bottom: true,
        },
    ];
    let root = Node::Split {
        vertical: false,
        share: 2500,
        first: Box::new(Node::Pane {
            terminal: TerminalId(1),
        }),
        second: Box::new(Node::Pane {
            terminal: TerminalId(2),
        }),
    };
    let mut valid: Vec<Vec<u8>> = requests.iter().map(wire::encode).collect();
    valid.push(wire::encode(&Message::Layout {
        session: SessionId(1),
        tab: TerminalId(1),
        root,
    }));
    valid.push(wire::encode(&Message::Title {
        terminal: TerminalId(1),
        title: "日本語".to_owned(),
    }));
    valid
}

fn decode_every_way(bytes: &[u8]) {
    let _ = wire::decode::<Request>(bytes);
    let _ = wire::decode::<Message>(bytes);
    let _ = wire::decode::<Plan>(bytes);
    let _ = wire::decode::<Node>(bytes);
    let mut framed = (bytes.len() as u32).to_le_bytes().to_vec();
    framed.extend_from_slice(bytes);
    let _ = receive::<Request>(&mut framed.as_slice());
    let _ = receive::<Message>(&mut &framed[..framed.len() / 2]);
}

#[test]
fn noise_is_an_error_not_a_panic() {
    let mut random = Random(0x0123_4567_89ab_cdef);
    for _ in 0..200_000 {
        let length = random.below(64);
        let bytes: Vec<u8> = (0..length).map(|_| random.next() as u8).collect();
        decode_every_way(&bytes);
    }
}

#[test]
fn damaged_messages_are_an_error_or_another_message_not_a_panic() {
    let mut random = Random(0xfeed_face_cafe_f00d);
    for message in valid() {
        for _ in 0..50_000 {
            let mut bytes = message.clone();
            for _ in 0..=random.below(4) {
                let at = random.below(bytes.len());
                bytes[at] = random.next() as u8;
            }
            if random.below(4) == 0 {
                bytes.truncate(random.below(bytes.len()) + 1);
            }
            decode_every_way(&bytes);
        }
    }
}

#[test]
fn a_length_that_lies_is_refused_before_anything_is_allocated_for_it() {
    for claim in [1u64, 1 << 16, 1 << 32, u64::MAX] {
        let mut bytes = vec![];
        wire::Wire::put(&claim, &mut bytes);
        assert!(wire::decode::<Vec<Plan>>(&bytes).is_err());
        assert!(wire::decode::<String>(&bytes).is_err());
        assert!(wire::decode::<Bytes>(&bytes).is_err());
    }
}
