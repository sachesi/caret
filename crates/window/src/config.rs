//! The settings in `config.toml`, in Caret's directory of the user's configuration.

use std::collections::HashMap;
use std::fmt::Write as _;
use std::mem::MaybeUninit;
use std::os::fd::{AsFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::process::Command;

use caret_protocol::CursorShape;
use harfrust::Feature;
use rustix::fs::inotify;
use toml_edit::{DocumentMut, Item, TableLike, Value};

use crate::palette;
use crate::shortcuts::{self, Action, Shortcut};

/// The font size when neither the settings nor the desktop name one.
const POINTS: f32 = 11.0;

/// The terminal's font as the settings and the desktop choose it.
#[derive(Debug, Clone, PartialEq)]
pub struct FontChoice {
    /// None for the family Caret picks.
    pub family: Option<String>,
    pub points: f32,
    pub features: Vec<Feature>,
    pub ligatures: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorScheme {
    /// As the desktop prefers.
    System,
    Light,
    Dark,
}

/// What a click on a session's tab does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionClick {
    /// Shows the session's last tab, or lists its tabs if it is shown already.
    Switch,
    /// Lists its tabs.
    Popup,
}

#[derive(Debug, Clone, PartialEq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each is a separate setting, on or off"
)]
pub struct Config {
    /// The terminal's font family and size; none for the desktop's.
    pub font: Option<String>,
    pub font_size: Option<f32>,
    /// OpenType features of the font turned on or off.
    pub font_features: Vec<Feature>,
    /// Symbols the font joins, such as `->` and `!=`, are drawn joined.
    pub ligatures: bool,
    pub color_scheme: ColorScheme,
    /// The names of the colour schemes of the light and the dark style.
    pub light_scheme: String,
    pub dark_scheme: String,
    /// How opaque the terminal's background is, from 0 to 1.
    pub opacity: f32,
    pub cursor_shape: CursorShape,
    pub scrollback_lines: u32,
    /// Run in new tabs instead of the shell; empty for the shell.
    pub custom_command: String,
    pub scroll_on_output: bool,
    pub scroll_on_keystroke: bool,
    /// Programs may set the clipboard, and show notifications while out of sight.
    pub program_clipboard: bool,
    pub program_notifications: bool,
    /// The bell a program rings makes a sound.
    pub audible_bell: bool,
    /// Closing a window leaves its programs running, to attach to again.
    pub close_detaches: bool,
    pub title_bar: bool,
    /// The compositor draws the window's frame where it offers to.
    pub server_side_decorations: bool,
    /// What a click on a session's tab does, while a window shows several.
    pub session_click: SessionClick,
    /// Each session has a colour of its own, on its tab and around the panes.
    pub session_colors: bool,
    /// The ring around the focused pane in control mode has rounded corners.
    pub rounded_pane_ring: bool,
    /// A middle click on a session's tab detaches the session.
    pub middle_click_detach: bool,
    /// A session's list of tabs shows what each one holds.
    pub tab_previews: bool,
    /// The keys of the actions bound to others than Caret's.
    pub keys: HashMap<Action, Vec<Shortcut>>,
    /// The names of the profiles, sets of settings windows may be opened with, in the
    /// file's order.
    pub profiles: Vec<String>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            font: None,
            font_size: None,
            font_features: Vec::new(),
            ligatures: true,
            color_scheme: ColorScheme::System,
            light_scheme: palette::OWN.to_owned(),
            dark_scheme: palette::OWN.to_owned(),
            opacity: 1.0,
            cursor_shape: CursorShape::Block,
            scrollback_lines: 10_000,
            custom_command: String::new(),
            scroll_on_output: false,
            scroll_on_keystroke: true,
            program_clipboard: true,
            program_notifications: true,
            audible_bell: true,
            close_detaches: false,
            title_bar: true,
            server_side_decorations: true,
            session_click: SessionClick::Switch,
            session_colors: true,
            rounded_pane_ring: true,
            middle_click_detach: false,
            tab_previews: false,
            keys: HashMap::new(),
            profiles: Vec::new(),
        }
    }
}

impl Config {
    /// The settings in `config.toml`, those of `profile` over the rest, defaults for
    /// those it lacks or gets wrong, and all defaults when there is no such file.
    pub fn load(profile: Option<&str>) -> Self {
        let Some(path) = path() else {
            return Self::default();
        };
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&path, &text, profile),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Self::default(),
            Err(error) => {
                tracing::warn!("reading {}: {error}", path.display());
                Self::default()
            }
        }
    }

    /// The settings in `text`, read from `path`, with those of `profile` over the rest;
    /// each one missing or wrong, warned of, keeps its default.
    fn parse(path: &Path, text: &str, profile: Option<&str>) -> Self {
        let mut config = Self::default();
        let document = match text.parse::<DocumentMut>() {
            Ok(document) => document,
            Err(error) => {
                tracing::warn!("{}: {error}", path.display());
                return config;
            }
        };
        let profiles = document.get("profiles").and_then(Item::as_table_like);
        if let Some(profiles) = profiles {
            config.profiles = profiles.iter().map(|(name, _)| name.to_owned()).collect();
        } else if document.contains_key("profiles") {
            tracing::warn!("{}: `profiles` is not a table", path.display());
        }
        config.take(path, "", document.as_table());
        let Some(profile) = profile else {
            return config;
        };
        if let Some(settings) = profiles.and_then(|profiles| profiles.get(profile)?.as_table_like())
        {
            config.take(path, &format!("profiles.{profile}."), settings);
        } else {
            tracing::warn!("{}: there is no profile `{profile}`", path.display());
        }
        config
    }

    /// Takes the settings in `table` of the file at `path`, whose keys start with
    /// `prefix` there.
    fn take(&mut self, path: &Path, prefix: &str, table: &dyn TableLike) {
        for (key, item) in table.iter() {
            match key {
                "keys" => self.bind(path, prefix, item),
                // Read apart, by `parse`.
                "profiles" if prefix.is_empty() => {}
                _ => {
                    if let Err(expected) = self.set(key, item) {
                        tracing::warn!("{}: `{prefix}{key}` is not {expected}", path.display());
                    }
                }
            }
        }
    }

    /// Binds the actions in the `[keys]` table `item` of the file at `path` to the keys
    /// it gives them, a key or a list of them; an empty list leaves an action unbound.
    fn bind(&mut self, path: &Path, prefix: &str, item: &Item) {
        let Some(table) = item.as_table_like() else {
            tracing::warn!("{}: `{prefix}keys` is not a table", path.display());
            return;
        };
        for (name, keys) in table.iter() {
            let Some(action) = Action::named(name) else {
                tracing::warn!("{}: `{prefix}keys.{name}` is not an action", path.display());
                continue;
            };
            let written: Option<Vec<&str>> = match keys.as_value() {
                Some(Value::String(key)) => Some(vec![key.value().as_str()]),
                Some(Value::Array(keys)) => keys.iter().map(Value::as_str).collect(),
                _ => None,
            };
            let shortcuts: Option<Vec<Shortcut>> =
                written.and_then(|keys| keys.into_iter().map(Shortcut::parse).collect());
            if let Some(shortcuts) = shortcuts {
                self.keys.insert(action, shortcuts);
            } else {
                tracing::warn!(
                    "{}: `{prefix}keys.{name}` is not a key such as \"Ctrl+Shift+T\" or a list of them",
                    path.display()
                );
            }
        }
    }

    /// Sets setting `key` to `item`, or says what it should have been.
    fn set(&mut self, key: &str, item: &Item) -> Result<(), &'static str> {
        let boolean = || item.as_bool().ok_or("true or false");
        match key {
            "font" => {
                let text = item.as_str().ok_or("a font name and size")?;
                (self.font, self.font_size) = font(text);
            }
            "font-features" => {
                self.font_features = item
                    .as_str()
                    .and_then(features)
                    .ok_or("OpenType features such as \"zero, ss01, calt=0\"")?;
            }
            "ligatures" => self.ligatures = boolean()?,
            "color-scheme" => {
                self.color_scheme = match item.as_str() {
                    Some("system") => ColorScheme::System,
                    Some("light") => ColorScheme::Light,
                    Some("dark") => ColorScheme::Dark,
                    _ => return Err("\"system\", \"light\" or \"dark\""),
                };
            }
            "light-scheme" | "dark-scheme" => {
                let name = item.as_str().ok_or("the name of a colour scheme")?;
                let scheme = if key == "light-scheme" {
                    &mut self.light_scheme
                } else {
                    &mut self.dark_scheme
                };
                name.clone_into(scheme);
            }
            "opacity" => {
                self.opacity = item
                    .as_float()
                    .or_else(|| item.as_integer().map(|opacity| opacity as f64))
                    .filter(|opacity| (0.0..=1.0).contains(opacity))
                    .ok_or("a number from 0 to 1")? as f32;
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
            "program-notifications" => self.program_notifications = boolean()?,
            "audible-bell" => self.audible_bell = boolean()?,
            "close-detaches" => self.close_detaches = boolean()?,
            "title-bar" => self.title_bar = boolean()?,
            "server-side-decorations" => self.server_side_decorations = boolean()?,
            "session-click" => {
                self.session_click = match item.as_str() {
                    Some("switch") => SessionClick::Switch,
                    Some("popup") => SessionClick::Popup,
                    _ => return Err("\"switch\" or \"popup\""),
                };
            }
            "session-colors" => self.session_colors = boolean()?,
            "rounded-pane-ring" => self.rounded_pane_ring = boolean()?,
            "middle-click-detach" => self.middle_click_detach = boolean()?,
            "tab-previews" => self.tab_previews = boolean()?,
            _ => return Err("a setting Caret knows"),
        }
        Ok(())
    }

    /// The terminal's font: the family and size the settings name, else those of
    /// `desktop`'s monospace font, as `"Family Size"`. Without a family, Caret picks one.
    pub fn font_for(&self, desktop: Option<&str>) -> FontChoice {
        let (family, size) = desktop.map(font).unwrap_or_default();
        FontChoice {
            family: self.font.clone().or(family),
            points: self.font_size.or(size).unwrap_or(POINTS),
            features: self.font_features.clone(),
            ligatures: self.ligatures,
        }
    }

    /// Whether the window is dark, given whether `desktop` prefers dark styles.
    pub fn dark(&self, desktop: bool) -> bool {
        match self.color_scheme {
            ColorScheme::System => desktop,
            ColorScheme::Light => false,
            ColorScheme::Dark => true,
        }
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

/// What `config.toml` starts as when it is opened to edit: every setting at its
/// default, commented out, so that the file says what can be set.
const STARTER: &str = r##"# Caret's settings. Take the # off a setting to change it; open windows follow
# the file as it is saved.

# The terminal's font family and size, such as "Adwaita Mono 11"; empty for the
# desktop's monospace font.
#font = ""
# OpenType features of the font to turn on or off, such as "zero, ss01, calt=0".
#font-features = ""
# Whether symbols the font joins, such as -> and !=, are drawn joined.
#ligatures = true
# "system", "light" or "dark".
#color-scheme = "system"
# The colours of the light and the dark style: "caret", Caret's own, or the name
# of a file in the schemes directory beside this file, "mine" for schemes/mine.toml.
# A scheme sets foreground, background and the 16 colours of palette, and may set
# selection, each as "#rrggbb".
#light-scheme = "caret"
#dark-scheme = "caret"
# How opaque the terminal's background is, from 0 for clear to 1.
#opacity = 1.0
# "block", "underline" or "beam".
#cursor-shape = "block"
#scrollback-lines = 10000
# What new tabs run instead of the shell, such as "fish --login".
#custom-command = ""
#scroll-on-output = false
#scroll-on-keystroke = true
# Whether programs may put text on the clipboard, and show notifications while
# their tab is out of sight.
#program-clipboard = true
#program-notifications = true
#audible-bell = true
# Whether closing a window leaves its programs running, to attach to again with
# caret --attach.
#close-detaches = false
#title-bar = true
# Whether the compositor draws the window's frame, where it offers to.
#server-side-decorations = true
# While a window shows several sessions, each has one tab. "switch": a click on
# another session's tab shows its last tab, and on the shown session's tab or
# the arrow lists its tabs; "popup": every click lists them.
#session-click = "switch"
# Whether each session has a colour of its own, on its tab, its list of tabs and
# the ring of control mode.
#session-colors = true
# Whether the ring of control mode has rounded corners.
#rounded-pane-ring = true
# Whether a middle click on a session's tab detaches the session.
#middle-click-detach = false
# Whether a session's list of tabs shows a small picture of each.
#tab-previews = false

# Profiles are settings of their own for the windows opened with them, from the
# main menu or with caret --profile NAME. Each is a table of the settings above,
# such as:
# [profiles.work]
# font = "Adwaita Mono 13"
# custom-command = "ssh work"

# The keys of each action: a key such as "Ctrl+Shift+T", a list of them, or []
# for none.
#[keys]
"##;

/// The path of `config.toml`, made as [`STARTER`] if it is missing, to open to edit.
pub fn editable() -> Option<PathBuf> {
    let path = path()?;
    if !path.exists() {
        let made = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&path, starter()));
        if let Err(error) = made {
            tracing::warn!("writing {}: {error}", path.display());
            return None;
        }
    }
    Some(path)
}

fn starter() -> String {
    let mut text = STARTER.to_owned();
    for (name, keys) in shortcuts::defaults() {
        let keys: Vec<String> = keys.iter().map(|key| format!("\"{key}\"")).collect();
        let keys = match keys.as_slice() {
            [key] => key.clone(),
            keys => format!("[{}]", keys.join(", ")),
        };
        let _ = writeln!(text, "#{name} = {keys}");
    }
    text
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

/// Sets `key` to `value` in `config.toml`, keeping the rest of the file as it is.
pub fn write(key: &str, value: impl Into<Value>) {
    let Some(path) = path() else {
        return;
    };
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(error) => {
            tracing::warn!("reading {}: {error}", path.display());
            return;
        }
    };
    let mut document = match text.parse::<DocumentMut>() {
        Ok(document) => document,
        Err(error) => {
            tracing::warn!("not changing {}: {error}", path.display());
            return;
        }
    };
    document[key] = toml_edit::value(value);
    // Written beside it and renamed over it, so that no reader sees half a file.
    let written = path.with_extension("toml.new");
    let saved = std::fs::write(&written, document.to_string())
        .and_then(|()| std::fs::rename(&written, &path));
    if let Err(error) = saved {
        tracing::warn!("writing {}: {error}", path.display());
    }
}

/// The file of the colour scheme `name`.
pub fn scheme_path(name: &str) -> Option<PathBuf> {
    Some(
        path()?
            .parent()?
            .join("schemes")
            .join(format!("{name}.toml")),
    )
}

/// Watches the directory `config.toml` is in and the one of colour schemes in it, made
/// if they are missing, so that changes to the files are seen however an editor saves
/// them.
pub fn watch() -> std::io::Result<OwnedFd> {
    let path = path().ok_or_else(|| std::io::Error::other("no home directory"))?;
    let directory = path.parent().unwrap_or(&path);
    let schemes = directory.join("schemes");
    std::fs::create_dir_all(&schemes)?;
    let watch = inotify::init(inotify::CreateFlags::CLOEXEC | inotify::CreateFlags::NONBLOCK)?;
    for directory in [directory, &schemes] {
        inotify::add_watch(
            &watch,
            directory,
            inotify::WatchFlags::CLOSE_WRITE
                | inotify::WatchFlags::MOVED_TO
                | inotify::WatchFlags::MOVED_FROM
                | inotify::WatchFlags::DELETE,
        )?;
    }
    Ok(watch)
}

/// Reads every event waiting on `watch`, saying whether one was about `config.toml` or
/// a colour scheme's file.
pub fn changed(watch: impl AsFd) -> bool {
    let mut buffer = [MaybeUninit::uninit(); 4096];
    let mut events = inotify::Reader::new(watch, &mut buffer);
    let mut changed = false;
    loop {
        match events.next() {
            Ok(event) => {
                changed |= event
                    .file_name()
                    .is_some_and(|name| name.to_bytes().ends_with(b".toml"));
            }
            Err(rustix::io::Errno::AGAIN) => return changed,
            Err(error) => {
                tracing::warn!("watching the settings: {error}");
                return changed;
            }
        }
    }
}

/// The family and size a font such as `"Adwaita Mono 11"` names.
fn font(text: &str) -> (Option<String>, Option<f32>) {
    let text = text.trim();
    let size = |size: &str| size.parse::<f32>().ok().filter(|&size| size > 0.0);
    let (family, size) = match text.rsplit_once(' ') {
        Some((family, last)) => match size(last) {
            Some(size) => (family.trim(), Some(size)),
            None => (text, None),
        },
        None => match size(text) {
            Some(size) => ("", Some(size)),
            None => (text, None),
        },
    };
    let family = (!family.is_empty()).then(|| family.to_owned());
    (family, size)
}

/// The OpenType features a list such as `"zero, ss01, calt=0"` turns on or off; none if
/// one of them is not a feature.
fn features(text: &str) -> Option<Vec<Feature>> {
    text.split(',')
        .map(str::trim)
        .filter(|feature| !feature.is_empty())
        .map(|feature| feature.parse().ok())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> Config {
        Config::parse(Path::new("config.toml"), text, None)
    }

    fn family_and_size(choice: FontChoice) -> (Option<String>, f32) {
        (choice.family, choice.points)
    }

    #[test]
    fn settings_are_read_and_the_rest_keep_their_defaults() {
        let config = parse(
            "# mine\nfont = \"Adwaita Mono 13\"\ncolor-scheme = \"light\"\n\
             close-detaches = true\nscrollback-lines = 500\nopacity = 0.5\n",
        );
        assert_eq!(
            family_and_size(config.font_for(Some("Monospace 10"))),
            (Some("Adwaita Mono".into()), 13.0)
        );
        assert_eq!(config.color_scheme, ColorScheme::Light);
        assert!(config.close_detaches && config.title_bar);
        assert_eq!(config.scrollback_lines, 500);
        assert!((config.opacity - 0.5).abs() < f32::EPSILON);
        assert!((parse("opacity = 0").opacity).abs() < f32::EPSILON);
    }

    #[test]
    fn a_wrong_value_or_file_keeps_the_defaults() {
        let config =
            parse("title-bar = \"no\"\ncursor-shape = \"star\"\nopacity = 1.5\nunknown = 1\n");
        assert_eq!(config, Config::default());
        assert_eq!(parse("font = "), Config::default());
    }

    #[test]
    fn a_font_names_a_family_a_size_or_both() {
        assert_eq!(
            font("Source Code Pro 12.5"),
            (Some("Source Code Pro".into()), Some(12.5))
        );
        assert_eq!(font("Monaco"), (Some("Monaco".into()), None));
        assert_eq!(font("14"), (None, Some(14.0)));
        assert_eq!(font(""), (None, None));
    }

    #[test]
    fn a_profile_s_settings_go_over_the_rest() {
        let text = "font = \"Adwaita Mono 13\"\nopacity = 0.5\n\
            [profiles.work]\nfont = \"Fira Code 10\"\nkeys = { copy = \"Ctrl+Insert\" }\n\
            [profiles.home]\ncustom-command = \"fish\"\n";
        let base = parse(text);
        assert_eq!(base.profiles, ["work", "home"]);
        assert_eq!(base.font.as_deref(), Some("Adwaita Mono"));
        assert!(base.keys.is_empty());
        let work = Config::parse(Path::new("config.toml"), text, Some("work"));
        assert_eq!(work.font.as_deref(), Some("Fira Code"));
        assert!((work.opacity - 0.5).abs() < f32::EPSILON);
        assert_eq!(work.keys[&Action::Copy].len(), 1);
        let missing = Config::parse(Path::new("config.toml"), text, Some("away"));
        assert_eq!(missing, base);
    }

    #[test]
    fn font_features_are_a_list_and_one_wrong_one_spoils_it() {
        let config = parse("font-features = \"zero, ss01 ,calt=0\"");
        let tags: Vec<_> = config
            .font_features
            .iter()
            .map(|feature| (feature.tag.to_string(), feature.value))
            .collect();
        assert_eq!(
            tags,
            [("zero".into(), 1), ("ss01".into(), 1), ("calt".into(), 0)]
        );
        assert_eq!(parse("font-features = \"zero, =\""), Config::default());
        assert_eq!(parse("font-features = \"\""), Config::default());
    }

    #[test]
    fn what_the_settings_leave_out_of_the_font_comes_from_the_desktop() {
        let sized = parse("font = \"14\"");
        assert_eq!(
            family_and_size(sized.font_for(Some("Adwaita Mono 10"))),
            (Some("Adwaita Mono".into()), 14.0)
        );
        assert_eq!(
            family_and_size(Config::default().font_for(None)),
            (None, POINTS)
        );
    }

    #[test]
    fn settings_changed_in_gsettings_are_brought_over() {
        let listing = "io.github.sachesi.caret font 'Adwaita Mono 13'\n\
            io.github.sachesi.caret ligatures false\n\
            io.github.sachesi.caret font-features 'zero'\n\
            io.github.sachesi.caret color-scheme 'system'\n\
            io.github.sachesi.caret scrollback-lines uint32 5000\n\
            io.github.sachesi.caret custom-command \"tmux new -s 'it'\"\n\
            io.github.sachesi.caret close-detaches true\n\
            io.github.sachesi.caret title-bar true\n\
            io.github.sachesi.caret window-size (0, 0)\n";
        let document = imported(listing).to_string();
        let config = parse(&document);
        assert_eq!(
            family_and_size(config.font_for(None)),
            (Some("Adwaita Mono".into()), 13.0)
        );
        assert_eq!(config.font_features, features("zero").unwrap_or_default());
        assert!(!config.ligatures);
        assert_eq!(config.scrollback_lines, 5000);
        assert_eq!(config.custom_command, "tmux new -s 'it'");
        assert!(config.close_detaches);
        assert!(!document.contains("title-bar") && !document.contains("color-scheme"));
    }

    #[test]
    fn keys_bind_actions_to_one_key_a_list_or_none() {
        let config = parse(
            "[keys]\ncopy = \"Ctrl+Insert\"\npaste = [\"Shift+Insert\", \"Ctrl+Shift+V\"]\n\
             fullscreen = []\nfly = \"F1\"\nzoom-in = \"Ctrl+Nothing\"\n",
        );
        assert_eq!(config.keys[&Action::Copy].len(), 1);
        assert_eq!(config.keys[&Action::Paste].len(), 2);
        assert!(config.keys[&Action::Fullscreen].is_empty());
        assert!(!config.keys.contains_key(&Action::ZoomIn));
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

    #[test]
    fn the_starter_file_holds_every_setting_at_its_default() {
        let uncommented = starter()
            .lines()
            .filter_map(|line| line.strip_prefix('#'))
            .filter(|line| line.starts_with(|c: char| c.is_ascii_lowercase() || c == '['))
            .collect::<Vec<_>>()
            .join("\n");
        let config = parse(&uncommented);
        assert_eq!(
            Config {
                keys: HashMap::new(),
                ..config.clone()
            },
            Config::default()
        );
        assert_eq!(config.keys.len(), shortcuts::defaults().count());
    }
}
