//! Screen readers: the window tells one what it shows while one listens, and does what
//! one asks of its tabs, menus and dialogs.

use accesskit::{
    Action, ActionHandler, ActionRequest, ActivationHandler, DeactivationHandler, TreeUpdate,
};
use accesskit_unix::Adapter;
use smithay_client_toolkit::reexports::calloop::channel::{Channel, Sender};

use super::App;
use crate::access::{self, Scene, Target};

/// What a screen reader did, heard on the accessibility bus's thread.
pub(super) enum Heard {
    Listening(bool),
    Asked(ActionRequest),
}

/// Passes what is heard to the loop, where the window's state is.
#[derive(Clone)]
struct Handlers(Sender<Heard>);

impl ActivationHandler for Handlers {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        let _ = self.0.send(Heard::Listening(true));
        None
    }
}

impl DeactivationHandler for Handlers {
    fn deactivate_accessibility(&mut self) {
        let _ = self.0.send(Heard::Listening(false));
    }
}

impl ActionHandler for Handlers {
    fn do_action(&mut self, request: ActionRequest) {
        let _ = self.0.send(Heard::Asked(request));
    }
}

pub(super) fn adapter() -> (Adapter, Channel<Heard>) {
    let (sender, heard) = smithay_client_toolkit::reexports::calloop::channel::channel();
    let handlers = Handlers(sender);
    (
        Adapter::new(handlers.clone(), handlers.clone(), handlers),
        heard,
    )
}

impl App {
    pub(super) fn heard(&mut self, heard: Heard) {
        match heard {
            Heard::Listening(listening) => {
                self.listened = listening;
                if listening {
                    self.tell_reader();
                }
            }
            Heard::Asked(request) if request.action == Action::Click => {
                match access::target(request.target_node) {
                    Some(Target::Tab(index)) => self.show(index),
                    Some(Target::MenuItem(index)) => self.choose(index),
                    Some(Target::Button(index)) => self.respond(index == 1),
                    None => {}
                }
            }
            Heard::Asked(_) => {}
        }
    }

    /// Tells a listening screen reader what the window shows now.
    pub(super) fn tell_reader(&mut self) {
        if !self.listened {
            return;
        }
        let scene = self.scene();
        self.reader.update_if_active(|| access::tree(&scene));
    }

    fn scene(&self) -> Scene {
        let factor = self.factor();
        let margin = f64::from(self.chrome.margin());
        let bounds = self
            .terminal()
            .and_then(|terminal| self.placed().rect(terminal))
            .map_or((0.0, 0.0, 0.0, 0.0), |rect| {
                (
                    f64::from(rect.x) / factor - margin,
                    f64::from(rect.y) / factor - margin,
                    f64::from(rect.width) / factor,
                    f64::from(rect.height) / factor,
                )
            });
        let cell = self.fonts.cell();
        let terminal = access::Terminal {
            rows: (0..self.grid.lines)
                .map(|line| self.grid.characters(line))
                .collect(),
            cursor: self
                .grid
                .cursor
                .map(|cursor| (cursor.point.line, cursor.point.column)),
            selection: self.grid.selection(),
            bounds,
            cell: (
                f64::from(cell.width) / factor,
                f64::from(cell.height) / factor,
            ),
        };
        let menu = self.chrome.menu.as_ref().map(|menu| access::Menu {
            items: menu
                .items()
                .map(|item| (item.label.clone(), item.enabled))
                .collect(),
            focused: menu.focused,
        });
        let list = self
            .chrome
            .shortcut_list
            .as_ref()
            .map(|list| access::Dialog {
                heading: list.heading.clone(),
                body: list.text(),
                ..access::Dialog::default()
            });
        let dialog = self
            .chrome
            .dialog
            .as_ref()
            .map(|dialog| access::Dialog {
                heading: dialog.heading.clone(),
                body: dialog.body.clone(),
                field: dialog.field.as_ref().map(|field| field.text.clone()),
                buttons: dialog.responses.clone(),
                focused: dialog.focused,
            })
            .or(list);
        Scene {
            title: self.chrome.title.clone(),
            tabs: self
                .chrome
                .tabs
                .iter()
                .enumerate()
                .map(|(index, tab)| (tab.title.clone(), index == self.chrome.selected))
                .collect(),
            terminal,
            menu,
            dialog,
        }
    }
}
