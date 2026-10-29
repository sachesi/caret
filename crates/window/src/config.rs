//! The settings in `config.toml`, in Caret's directory of the user's configuration.

use std::mem::MaybeUninit;
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::Command;

use caret_protocol::CursorShape;
use rustix::fs::inotify;
use toml_edit::{DocumentMut, Item, Value};

/// The font size when the font setting names none.
const POINTS: f32 = 11.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorScheme {
    /// As the desktop prefers.
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a separate setting, on or off"
)]
pub struct Config {
    /// The terminal's font family; none for the first of those Caret prefers.
    pub font: Option<String>,
    pub font_size: f32,
    pub color_scheme: ColorScheme,
    pub cursor_shape: CursorShape,
    pub scrollback_lines: u32,
    /// Run in new tabs instead of the shell; empty for the shell.
    pub custom_command: String,
    pub scroll_on_output: bool,
    pub scroll_on_keystroke: bool,
    /// Programs may set the clipboard.
    pub program_clipboard: bool,
    /// Closing a window leaves its programs running, to attach to again.
    pub close_detaches: bool,
    pub title_bar: bool,
    /// The compositor draws the window's frame where it offers to.
    pub server_side_decorations: bool,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font: None,
            font_size: POINTS,
            color_scheme: ColorScheme::System,
            cursor_shape: CursorShape::Block,
            scrollback_lines: 10_000,
            custom_command: String::new(),
            scroll_on_output: false,
            scroll_on_keystroke: true,
            program_clipboard: true,
            close_detaches: false,
            title_bar: true,
            server_side_decorations: true,
        }
    }
}

impl Config {
    /// The settings in `config.toml`, defaults for those it lacks or gets wrong, and all
    /// defaults when there is no such file.
    pub fn load() -> Self {
        let Some(path) = path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&path, &text),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => {
                tracing::warn!("reading {}: {error}", path.display());
                Self::default()
            }
        }
    }

    /// The settings in `text`, read from `path`; each one missing or wrong, warned of,
    /// keeps its default.
    fn parse(path: &Path, text: &str) -> Self {
        let mut config = Self::default();
        let document = match text.parse::<DocumentMut>() {
            Ok(document) => document,
            Err(error) => {
                tracing::warn!("{}: {error}", path.display());
                return config;
            }
        };
        for (key, item) in document.iter() {
            if let Err(expected) = config.set(key, item) {
                tracing::warn!("{}: `{key}` is not {expected}", path.display());
            }
        }
        config
    }

    /// Sets setting `key` to `item`, or says what it should have been.
    fn set(&mut self, key: &str, item: &Item) -> Result<(), &'static str> {
        let boolean = || item.as_bool().ok_or("true or false");
        match key {
            "font" => {
                let text = item.as_str().ok_or("a font name and size")?;
                (self.font, self.font_size) = font(text);
            }
            "color-scheme" => {
                self.color_scheme = match item.as_str() {
                    Some("system") => ColorScheme::System,
                    Some("light") => ColorScheme::Light,
                    Some("dark") => ColorScheme::Dark,
                    _ => return Err("\"system\", \"light\" or \"dark\""),
                };
            }
            "cursor-shape" => {
                self.cursor_shape = match item.as_str() {
                    Some("block") => CursorShape::Block,
                    Some("underline") => CursorShape::Underline,
                    Some("beam") => CursorShape::Beam,
                    _ => return Err("\"block\", \"underline\" or \"beam\""),
                };
            }
            "scrollback-lines" => {
                self.scrollback_lines = item
                    .as_integer()
                    .and_then(|lines| u32::try_from(lines).ok())
                    .ok_or("a number of lines")?;
            }
            "custom-command" => {
                item.as_str()
                    .ok_or("a command line")?
                    .clone_into(&mut self.custom_command);
            }
            "scroll-on-output" => self.scroll_on_output = boolean()?,
            "scroll-on-keystroke" => self.scroll_on_keystroke = boolean()?,
            "program-clipboard" => self.program_clipboard = boolean()?,
            "close-detaches" => self.close_detaches = boolean()?,
            "title-bar" => self.title_bar = boolean()?,
            "server-side-decorations" => self.server_side_decorations = boolean()?,
            _ => return Err("a setting Caret knows"),
        }
        Ok(())
    }

    /// The custom command split into its words; none for the shell. A line that does
    /// not split is run whole, so that the tab says it failed.
    pub fn argv(&self) -> Vec<String> {
        let line = self.custom_command.trim();
        if line.is_empty() {
            return Vec::new();
        }
        shlex::split(line).unwrap_or_else(|| vec![line.to_owned()])
    }
}

/// `config.toml` in Caret's configuration directory.
pub fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|base| Path::new(base).is_absolute())
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|home| home.join(".config")))?;
    Some(base.join("caret").join("config.toml"))
}

/// Writes the settings the GTK version of Caret kept in `GSettings` to `config.toml`,
/// those changed from their defaults, the first time the window starts: while the
/// directory `config.toml` goes in is missing.
pub fn import() {
    let Some(path) = path() else {
        return;
    };
    let Some(directory) = path.parent().filter(|directory| !directory.exists()) else {
        return;
    };
    if let Err(error) = std::fs::create_dir_all(directory) {
        tracing::warn!("making {}: {error}", directory.display());
        return;
    }
    let listed = Command::new("gsettings")
        .args(["list-recursively", crate::APP_ID])
        .output();
    let listing = match listed {
        Ok(output) if output.status.success() => output.stdout,
        // No gsettings or no schema: nothing to bring over.
        _ => return,
    };
    let document = imported(&String::from_utf8_lossy(&listing));
    if document.is_empty() {
        return;
    }
    if let Err(error) = std::fs::write(&path, document.to_string()) {
        tracing::warn!("writing {}: {error}", path.display());
    }
}

/// The settings in a `gsettings list-recursively` listing that differ from their
/// defaults, as `config.toml` keeps them.
fn imported(listing: &str) -> DocumentMut {
    let defaults = Config::default();
    let mut document = DocumentMut::new();
    for line in listing.lines() {
        let mut words = line.splitn(3, ' ');
        let (Some(_), Some(key), Some(value)) = (words.next(), words.next(), words.next()) else {
            continue;
        };
        let value = value.trim();
        let value: Option<Value> = if let Some(lines) = value.strip_prefix("uint32 ") {
            lines.parse::<i64>().ok().map(Value::from)
        } else if let Ok(on) = value.parse::<bool>() {
            Some(Value::from(on))
        } else {
            quoted(value).map(Value::from)
        };
        let Some(value) = value else {
            continue;
        };
        let item = Item::Value(value);
        let mut config = defaults.clone();
        if config.set(key, &item).is_ok() && config != defaults {
            document.insert(key, item);
        }
    }
    document
}

/// The text of a string as `GVariant` prints it, in single or double quotes with
/// backslash escapes.
fn quoted(value: &str) -> Option<String> {
    let quote = value.chars().next().filter(|&c| matches!(c, '\'' | '"'))?;
    let inner = value.strip_prefix(quote)?.strip_suffix(quote)?;
    let mut text = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        text.push(if c == '\\' { chars.next()? } else { c });
    }
    Some(text)
}

/// Watches the directory `config.toml` is in, made if it is missing, so that changes
/// to the file are seen however an editor saves it.
pub fn watch() -> std::io::Result<OwnedFd> {
    let path = path().ok_or_else(|| std::io::Error::other("no home directory"))?;
    let directory = path.parent().unwrap_or(&path);
    std::fs::create_dir_all(directory)?;
    let watch = inotify::init(inotify::CreateFlags::CLOEXEC | inotify::CreateFlags::NONBLOCK)?;
    inotify::add_watch(
        &watch,
        directory,
        inotify::WatchFlags::CLOSE_WRITE
            | inotify::WatchFlags::MOVED_TO
            | inotify::WatchFlags::MOVED_FROM
            | inotify::WatchFlags::DELETE,
    )?;
    Ok(watch)
}

/// Reads every event waiting on `watch`, saying whether one was about `config.toml`.
pub fn changed(watch: impl AsFd) -> bool {
    let mut buffer = [MaybeUninit::uninit(); 4096];
    let mut events = inotify::Reader::new(watch, &mut buffer);
    let mut changed = false;
    loop {
        match events.next() {
            Ok(event) => {
                changed |= event
                    .file_name()
                    .is_some_and(|name| name.to_bytes() == b"config.toml");
            }
            Err(rustix::io::Errno::AGAIN) => return changed,
            Err(error) => {
                tracing::warn!("watching the settings: {error}");
                return changed;
            }
        }
    }
}

/// The family and size a font setting such as `"Adwaita Mono 11"` names.
fn font(text: &str) -> (Option<String>, f32) {
    let text = text.trim();
    let (family, size) = match text.rsplit_once(' ') {
        Some((family, size)) => match size.parse::<f32>() {
            Ok(size) if size > 0.0 => (family.trim(), size),
            _ => (text, POINTS),
        },
        None => match text.parse::<f32>() {
            Ok(size) if size > 0.0 => ("", size),
            _ => (text, POINTS),
        },
    };
    let family = (!family.is_empty()).then(|| family.to_owned());
    (family, size)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Config {
        Config::parse(Path::new("config.toml"), text)
    }

    #[test]
    fn settings_are_read_and_the_rest_keep_their_defaults() {
        let config = parse(
            "# mine\nfont = \"Adwaita Mono 13\"\ncolor-scheme = \"light\"\n\
             close-detaches = true\nscrollback-lines = 500\n",
        );
        assert_eq!(
            (config.font.as_deref(), config.font_size),
            (Some("Adwaita Mono"), 13.0)
        );
        assert_eq!(config.color_scheme, ColorScheme::Light);
        assert!(config.close_detaches && config.title_bar);
        assert_eq!(config.scrollback_lines, 500);
    }

    #[test]
    fn a_wrong_value_or_file_keeps_the_defaults() {
        let config = parse("title-bar = \"no\"\ncursor-shape = \"star\"\nunknown = 1\n");
        assert_eq!(config, Config::default());
        assert_eq!(parse("font = "), Config::default());
    }

    #[test]
    fn a_font_names_a_family_a_size_or_both() {
        assert_eq!(
            font("Source Code Pro 12.5"),
            (Some("Source Code Pro".into()), 12.5)
        );
        assert_eq!(font("Monaco"), (Some("Monaco".into()), POINTS));
        assert_eq!(font("14"), (None, 14.0));
        assert_eq!(font(""), (None, POINTS));
    }

    #[test]
    fn settings_changed_in_gsettings_are_brought_over() {
        let listing = "io.github.sachesi.caret font 'Adwaita Mono 13'\n\
            io.github.sachesi.caret ligatures true\n\
            io.github.sachesi.caret color-scheme 'system'\n\
            io.github.sachesi.caret scrollback-lines uint32 5000\n\
            io.github.sachesi.caret custom-command \"tmux new -s 'it'\"\n\
            io.github.sachesi.caret close-detaches true\n\
            io.github.sachesi.caret title-bar true\n\
            io.github.sachesi.caret window-size (0, 0)\n";
        let document = imported(listing).to_string();
        let config = parse(&document);
        assert_eq!(config.font.as_deref(), Some("Adwaita Mono"));
        assert_eq!(config.scrollback_lines, 5000);
        assert_eq!(config.custom_command, "tmux new -s 'it'");
        assert!(config.close_detaches);
        assert!(!document.contains("title-bar") && !document.contains("color-scheme"));
    }

    #[test]
    fn the_custom_command_splits_like_a_shell_line() {
        let config = Config {
            custom_command: "tmux new -s 'my work'".into(),
            ..Config::default()
        };
        assert_eq!(config.argv(), ["tmux", "new", "-s", "my work"]);
        assert!(Config::default().argv().is_empty());
        let unsplit = Config {
            custom_command: "echo 'open".into(),
            ..Config::default()
        };
        assert_eq!(unsplit.argv(), ["echo 'open"]);
    }
}
