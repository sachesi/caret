//! Split presets in the window: the submenu that lists them, laying a tab out by one, and
//! saving a tab's layout as one.

use std::collections::HashMap;
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::time::Duration;

use caret_protocol::layout::{Node, Plan};
use caret_protocol::{Answer, Bytes, Command, Question, TerminalId};
use gettextrs::gettext;
use smithay_client_toolkit::reexports::calloop::timer::{TimeoutAction, Timer};
use smithay_client_toolkit::seat::keyboard::{KeyEvent, Keysym};

use super::menus::Choice;
use super::{App, Asked};
use crate::chrome::{Dialog, Item, Menu, Part};
use crate::config;
use crate::i18n::gettext_f;
use crate::panes::Placed;
use crate::presets::{self, Preset, Shape};
use crate::shortcuts::Action;

/// Since this version of the protocol, a server lays out a tab in one step.
const ARRANGING: u16 = 4;
/// How long the submenu stays once the pointer is on another item, so that a line drawn
/// across other items to reach it does not close it.
const SUBMENU_LINGERS: Duration = Duration::from_millis(300);

/// What a tab being opened is to become.
pub struct Arrival {
    pub preset: Preset,
    /// What panes without their own command or directory start with.
    pub base: Command,
}

/// The answers so far about the panes of a layout being saved.
pub struct Saving {
    name: String,
    root: Node,
    focused: TerminalId,
    left: usize,
    programs: HashMap<TerminalId, String>,
    directories: HashMap<TerminalId, String>,
}

impl App {
    /// The item for the presets' submenu: shown while there is one to list, or a layout to
    /// save as the first.
    pub(super) fn presets_item(&self) -> Option<(Item, Choice)> {
        if self.config.presets.is_empty() && !self.tab_is_split() {
            return None;
        }
        let mut item = self.item(&gettext("Presets"), Choice::Presets, true);
        item.0.opens = true;
        Some(item)
    }

    fn tab_is_split(&self) -> bool {
        self.tabs
            .get(self.selected)
            .is_some_and(|tab| matches!(tab.root, Node::Split { .. }))
    }

    /// Opens the submenu beside the menu's item at `row`, for the keys to move in if `keyed`.
    pub(super) fn open_submenu(&mut self, row: usize, keyed: bool) {
        let presets: Vec<(Item, Choice)> = self
            .config
            .presets
            .iter()
            .enumerate()
            .map(|(at, preset)| {
                let mut item = self.item(&preset.name, Choice::Act(Action::Preset(at)), true);
                item.0.picture = Some(preset.picture());
                item
            })
            .collect();
        let save = self
            .tab_is_split()
            .then(|| self.item(&gettext("Save Layout as Preset…"), Choice::SavePreset, true));
        let (sections, choices): (Vec<Vec<Item>>, Vec<Vec<Choice>>) =
            [presets, save.into_iter().collect()]
                .into_iter()
                .map(|section| section.into_iter().unzip())
                .unzip();
        let Some(place) = self.chrome.menu.as_ref().and_then(|menu| menu.placed) else {
            return;
        };
        let mut submenu = Menu::new(sections, place, false);
        submenu.submenu = true;
        if keyed {
            submenu.step(1);
        }
        if let Some(open) = &self.chrome.submenu {
            submenu.keep_place(open);
        }
        self.submenu_row = row;
        self.submenu_choices = choices.into_iter().flatten().collect();
        self.chrome.submenu = Some(submenu);
        self.dirty = true;
        self.draw();
    }

    pub(super) fn close_submenu(&mut self) {
        if self.chrome.submenu.take().is_some() {
            self.submenu_choices.clear();
            self.hover();
            self.dirty = true;
            self.draw();
        }
    }

    /// Opens the submenu when the pointer reaches its item, and closes it a moment after the
    /// pointer goes to another.
    pub(super) fn follow_submenu(&mut self) {
        if self.over_submenu_opener() {
            if self.chrome.submenu.is_none()
                && let Some(Part::MenuItem(row)) = self.chrome.hovered
            {
                self.open_submenu(row, false);
            }
        } else if self.over_other_item() && self.chrome.submenu.is_some() {
            let timer =
                self.events
                    .insert_source(Timer::from_duration(SUBMENU_LINGERS), |_, (), app| {
                        if app.over_other_item() {
                            app.close_submenu();
                        }
                        TimeoutAction::Drop
                    });
            if let Err(error) = timer {
                tracing::warn!("timing a submenu: {error}");
            }
        }
    }

    fn over_submenu_opener(&self) -> bool {
        matches!(self.chrome.hovered, Some(Part::MenuItem(row))
            if self.choices.get(row) == Some(&Choice::Presets))
    }

    fn over_other_item(&self) -> bool {
        matches!(self.chrome.hovered, Some(Part::MenuItem(row))
            if self.choices.get(row) != Some(&Choice::Presets))
    }

    /// Takes a key while the keys are in the submenu: Left or Escape go back to the menu.
    pub(super) fn submenu_key(&mut self, event: &KeyEvent) -> bool {
        let Some(submenu) = &mut self.chrome.submenu else {
            return false;
        };
        if submenu.focused.is_none() {
            // Open from a hover: Escape closes all, and the arrows stay in the menu.
            return false;
        }
        match event.keysym {
            Keysym::Escape | Keysym::Left | Keysym::KP_Left => self.close_submenu(),
            Keysym::Down | Keysym::KP_Down | Keysym::Tab => {
                submenu.step(1);
                self.dirty = true;
                self.draw();
            }
            Keysym::Up | Keysym::KP_Up | Keysym::ISO_Left_Tab => {
                submenu.step(-1);
                self.dirty = true;
                self.draw();
            }
            Keysym::Return
            | Keysym::KP_Enter
            | Keysym::space
            | Keysym::Right
            | Keysym::KP_Right => {
                if let Some(focused) = submenu.focused {
                    self.choose_in_submenu(focused);
                }
            }
            _ => {}
        }
        true
    }

    /// Lays out by the preset at `at` of the settings: this tab if it has one pane, else a
    /// new one.
    pub(super) fn use_preset(&mut self, at: usize) {
        let Some(preset) = self.config.presets.get(at).cloned() else {
            return;
        };
        if self.server.version < ARRANGING {
            self.toast(gettext("Presets need a newer terminal server"));
            return;
        }
        let Some(terminal) = self.terminal() else {
            return;
        };
        let id = self.server.ask(Question::NextCommand { terminal });
        self.asked.insert(id, Asked::PresetBase(preset));
    }

    /// Goes on with `preset`, now that it is known what its panes start from.
    pub(super) fn preset_from(&mut self, preset: &Preset, base: Command) {
        if self.tab_is_split() || preset.opens_tab {
            let Some(session) = self.shown_session() else {
                return;
            };
            let kept = self.pane_command(preset.kept(), &base);
            let arrival = Arrival {
                preset: preset.clone(),
                base,
            };
            self.ask_tab(session, kept, Some(Box::new(arrival)));
        } else if let Some(terminal) = self.terminal() {
            self.arrange(terminal, preset, &base);
        }
    }

    /// Opens the window's first tab as the command line asked: running `first`, laid out by
    /// the preset `name`.
    pub(super) fn open_first_preset(&mut self, name: &str, mut first: Command) {
        let preset = self
            .config
            .presets
            .iter()
            .find(|preset| preset.name == name);
        let (Some(preset), Some(session), true) = (
            preset.cloned(),
            self.shown_session(),
            self.server.version >= ARRANGING,
        ) else {
            let message = if self.server.version < ARRANGING {
                gettext("Presets need a newer terminal server")
            } else {
                gettext_f("There is no preset “{name}”", &[("name", name)])
            };
            self.toast(message);
            self.open_tab(first);
            return;
        };
        if first.directory.is_none() {
            first.directory = std::env::current_dir().ok().as_deref().map(path_bytes);
        }
        let base = Command {
            directory: first.directory.clone(),
            ..Command::default()
        };
        if first.argv.is_empty() {
            first = self.pane_command(preset.kept(), &base);
        }
        let arrival = Arrival { preset, base };
        self.ask_tab(session, first, Some(Box::new(arrival)));
    }

    /// What `pane` runs: its own command and directory, or `base`'s.
    fn pane_command(&self, pane: &presets::PresetPane, base: &Command) -> Command {
        let directory = pane.directory.as_deref().map(|directory| {
            let inherited = base
                .directory
                .as_ref()
                .map(|bytes| PathBuf::from(std::ffi::OsStr::from_bytes(&bytes.0)));
            path_bytes(&resolved(directory, inherited.as_deref()))
        });
        self.filled(Command {
            argv: if pane.command.is_empty() {
                base.argv.clone()
            } else {
                pane.command.clone()
            },
            directory: directory.or_else(|| base.directory.clone()),
            hold: !pane.command.is_empty(),
            env: base.env.clone(),
        })
    }

    /// Asks the server to lay out the tab whose one pane is `terminal`.
    pub(super) fn arrange(&mut self, terminal: TerminalId, preset: &Preset, base: &Command) {
        let tree = preset.shape.tree();
        let placed = self.lay_out(&tree);
        let terminals = tree.panes();
        let mut panes = terminals.iter().copied();
        let plan = self.plan(&preset.shape, base, &placed, &mut panes);
        let id = self.server.ask(Question::Arrange { terminal, plan });
        self.asked.insert(id, Asked::Arranged);
    }

    fn plan(
        &self,
        shape: &Shape,
        base: &Command,
        placed: &Placed,
        terminals: &mut impl Iterator<Item = TerminalId>,
    ) -> Plan {
        match shape {
            Shape::Pane(pane) => {
                let rect = terminals.next().and_then(|terminal| placed.rect(terminal));
                if pane.keep {
                    return Plan::Kept;
                }
                Plan::Run {
                    command: self.pane_command(pane, base),
                    size: self.size_of(rect.unwrap_or_default()),
                }
            }
            Shape::Split {
                vertical,
                share,
                first,
                second,
            } => {
                let first = self.plan(first, base, placed, terminals);
                let second = self.plan(second, base, placed, terminals);
                Plan::Split {
                    vertical: *vertical,
                    share: *share,
                    first: Box::new(first),
                    second: Box::new(second),
                }
            }
        }
    }

    /// Says what is wrong with the first preset that cannot be used; the log has them all.
    pub(super) fn tell_wrong_presets(&mut self) {
        if let Some((name, why)) = self.config.wrong_presets.first().cloned() {
            self.toast(gettext_f(
                "Preset “{name}” cannot be used: {why}",
                &[("name", &name), ("why", &why)],
            ));
        }
    }

    /// Asks for a name to save the tab's layout under.
    pub(super) fn ask_preset_name(&mut self) {
        let dialog = Dialog::asking(
            &gettext("Save Layout as Preset"),
            &gettext("Each pane keeps its directory and the program running in it."),
            [&gettext("Cancel"), &gettext("Save")],
            "",
        );
        self.confirm(dialog, super::confirm::Confirm::SavePreset);
    }

    /// Asks what the panes of the tab shown run, to save them under `name`.
    pub(super) fn save_preset(&mut self, name: String) {
        if name.is_empty() || self.saving.is_some() {
            return;
        }
        if self.config.presets.iter().any(|preset| preset.name == name) {
            self.toast(gettext_f(
                "There is already a preset “{name}”",
                &[("name", &name)],
            ));
            return;
        }
        let Some(tab) = self.tabs.get(self.selected) else {
            return;
        };
        let (root, focused) = (tab.root.clone(), tab.focused);
        let panes = root.panes();
        self.saving = Some(Saving {
            name,
            left: 2 * panes.len(),
            root,
            focused,
            programs: HashMap::new(),
            directories: HashMap::new(),
        });
        for terminal in panes {
            let id = self.server.ask(Question::Directory { terminal });
            self.asked.insert(id, Asked::SavedDirectory(terminal));
            let id = self.server.ask(Question::Foreground { terminal });
            self.asked.insert(id, Asked::SavedCommand(terminal));
        }
    }

    pub(super) fn saved_directory(&mut self, terminal: TerminalId, answer: Answer) {
        if let (Answer::Directory { path: Some(path) }, Some(saving)) = (answer, &mut self.saving) {
            saving
                .directories
                .insert(terminal, String::from_utf8_lossy(&path.0).into_owned());
        }
        self.saved_answer();
    }

    pub(super) fn saved_command(&mut self, terminal: TerminalId, answer: Answer) {
        if let (Answer::Command { command }, Some(saving)) = (answer, &mut self.saving)
            && !command.argv.is_empty()
            && let Ok(line) = shlex::try_join(command.argv.iter().map(String::as_str))
        {
            saving.programs.insert(terminal, line);
        }
        self.saved_answer();
    }

    /// Writes the preset once every pane has been heard about.
    fn saved_answer(&mut self) {
        let Some(saving) = &mut self.saving else {
            return;
        };
        saving.left -= 1;
        if saving.left > 0 {
            return;
        }
        let Some(saving) = self.saving.take() else {
            return;
        };
        let (layout, panes) = presets::saved(
            &saving.root,
            saving.focused,
            &saving.programs,
            &saving.directories,
        );
        let message = if config::add_preset(&saving.name, &layout, &panes) {
            gettext_f("Saved preset “{name}”", &[("name", &saving.name)])
        } else {
            gettext("Could not write the settings file")
        };
        self.toast(message);
    }
}

fn path_bytes(path: &Path) -> Bytes {
    Bytes(path.as_os_str().as_bytes().to_vec())
}

/// `directory` as a preset writes it: from the home directory after `~`, and from
/// `inherited` if it is relative.
fn resolved(directory: &str, inherited: Option<&Path>) -> PathBuf {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let under_home = match directory {
        "~" => Some(""),
        _ => directory.strip_prefix("~/"),
    };
    match (under_home, home) {
        (Some(rest), Some(home)) => home.join(rest),
        _ => inherited.map_or_else(|| PathBuf::from(directory), |from| from.join(directory)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_preset_s_directory_goes_from_home_or_where_the_pane_is() {
        let from = Path::new("/srv/app");
        assert_eq!(resolved("/tmp", Some(from)), Path::new("/tmp"));
        assert_eq!(resolved("logs", Some(from)), Path::new("/srv/app/logs"));
        assert_eq!(resolved("logs", None), Path::new("logs"));
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            assert_eq!(resolved("~/src", Some(from)), home.join("src"));
            assert_eq!(resolved("~", Some(from)), home);
        }
    }
}
