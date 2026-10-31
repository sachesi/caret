//! JSON for scripts, of which Caret only ever writes.

use std::fmt::Write as _;

use caret_protocol::SessionEntry;

/// `text` as a JSON string.
pub fn quoted(text: &str) -> String {
    let mut out = String::with_capacity(text.len() + 2);
    out.push('"');
    for letter in text.chars() {
        match letter {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            letter if u32::from(letter) < 0x20 => {
                let _ = write!(out, "\\u{:04x}", u32::from(letter));
            }
            letter => out.push(letter),
        }
    }
    out.push('"');
    out
}

fn optional(text: Option<&str>) -> String {
    text.map_or_else(|| "null".to_owned(), quoted)
}

/// `{"ok":true}` with `fields` in it, each already written as `"name":value`.
pub fn ok(fields: &[String]) -> String {
    let mut out = "{\"ok\":true".to_owned();
    for field in fields {
        out.push(',');
        out.push_str(field);
    }
    out.push('}');
    out
}

pub fn error(text: &str) -> String {
    format!("{{\"ok\":false,\"error\":{}}}", quoted(text))
}

pub fn sessions(sessions: &[SessionEntry]) -> String {
    let rows: Vec<String> = sessions.iter().map(session).collect();
    format!("\"sessions\":[{}]", rows.join(","))
}

fn session(entry: &SessionEntry) -> String {
    let tabs: Vec<String> = entry
        .tabs
        .iter()
        .map(|tab| {
            format!(
                "{{\"terminal\":{},\"title\":{},\"name\":{}}}",
                tab.terminal.0,
                quoted(&tab.title),
                optional(tab.name.as_deref()),
            )
        })
        .collect();
    format!(
        "{{\"session\":{},\"name\":{},\"color\":{},\"panes\":{},\"clients\":{},\"tabs\":[{}]}}",
        entry.session.0,
        optional(entry.name.as_deref()),
        entry
            .color
            .map_or_else(|| "null".to_owned(), |color| color.to_string()),
        entry.panes,
        entry.clients,
        tabs.join(","),
    )
}

#[cfg(test)]
mod tests {
    use caret_protocol::{SessionId, Tab, TerminalId};

    use super::*;

    #[test]
    fn text_is_escaped_as_json_wants() {
        assert_eq!(quoted("a\"b\\c"), r#""a\"b\\c""#);
        assert_eq!(quoted("a\nb\tc\r"), r#""a\nb\tc\r""#);
        assert_eq!(quoted("\u{1b}[0m"), r#""\u001b[0m""#);
        assert_eq!(quoted("日本語 é"), "\"日本語 é\"");
    }

    #[test]
    fn answers_have_the_shape_scripts_read() {
        assert_eq!(ok(&[]), r#"{"ok":true}"#);
        assert_eq!(
            ok(&["\"session\":3".to_owned()]),
            r#"{"ok":true,"session":3}"#
        );
        assert_eq!(error("no \"x\""), r#"{"ok":false,"error":"no \"x\""}"#);
        let entry = SessionEntry {
            session: SessionId(2),
            name: Some("build".to_owned()),
            color: None,
            tabs: vec![Tab {
                terminal: TerminalId(7),
                title: "make".to_owned(),
                name: None,
            }],
            panes: 1,
            clients: 0,
        };
        assert_eq!(
            sessions(&[entry]),
            r#""sessions":[{"session":2,"name":"build","color":null,"panes":1,"clients":0,"tabs":[{"terminal":7,"title":"make","name":null}]}]"#
        );
    }
}
