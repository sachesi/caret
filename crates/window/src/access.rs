//! What screen readers are told of the window: its title, its tabs, the terminal's rows
//! as text with the cursor as the caret and the selection, and the menu or dialog open,
//! focused where the keys are.

use accesskit::{
    Action, Node, NodeId, Rect, Role, TextPosition, TextSelection, TreeId, TreeInfo, TreeUpdate,
};

/// The nodes' ids: the window, its parts, and the parts' items from their own bases.
const WINDOW: NodeId = NodeId(0);
const TERMINAL: NodeId = NodeId(1);
const TABS: NodeId = NodeId(2);
const MENU: NodeId = NodeId(3);
const DIALOG: NodeId = NodeId(4);
const FIELD: NodeId = NodeId(5);
const RUNS: u64 = 1 << 32;
const TAB_ITEMS: u64 = 2 << 32;
const MENU_ITEMS: u64 = 3 << 32;
const BUTTONS: u64 = 4 << 32;

/// The characters a text run holds at most: its word starts are counted in bytes.
const RUN: usize = 255;

/// A character of a row: its text, which may carry marks, the column it starts in and
/// the columns it takes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Character {
    pub text: String,
    pub column: u16,
    pub cells: u16,
}

/// The terminal as it is drawn: its rows, the cursor and the selection in cells, where
/// it is in the window and how big a cell is, in logical pixels.
#[derive(Debug, Default, PartialEq)]
pub struct Terminal {
    pub rows: Vec<Vec<Character>>,
    pub cursor: Option<(u16, u16)>,
    /// The first and the last cell selected, as line and column.
    pub selection: Option<((u16, u16), (u16, u16))>,
    pub bounds: (f64, f64, f64, f64),
    pub cell: (f64, f64),
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Menu {
    /// Each item's label and whether it can be chosen.
    pub items: Vec<(String, bool)>,
    pub focused: Option<usize>,
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct Dialog {
    pub heading: String,
    pub body: String,
    /// The line of text it asks for.
    pub field: Option<String>,
    pub buttons: Vec<String>,
    /// The button the keys press; none while the field has them.
    pub focused: Option<usize>,
}

/// Everything told, gathered from the window when it changes.
#[derive(Debug, Default, PartialEq)]
pub struct Scene {
    pub title: String,
    /// Each tab's label and whether it is the one shown.
    pub tabs: Vec<(String, bool)>,
    pub terminal: Terminal,
    pub menu: Option<Menu>,
    pub dialog: Option<Dialog>,
}

/// What a node a screen reader clicks stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    Tab(usize),
    MenuItem(usize),
    Button(usize),
}

pub fn target(node: NodeId) -> Option<Target> {
    let index = usize::try_from(node.0 & 0xFFFF_FFFF).ok()?;
    match node.0 & !0xFFFF_FFFF {
        TAB_ITEMS => Some(Target::Tab(index)),
        MENU_ITEMS => Some(Target::MenuItem(index)),
        BUTTONS => Some(Target::Button(index)),
        _ => None,
    }
}

/// The whole tree for `scene`.
pub fn tree(scene: &Scene) -> TreeUpdate {
    let mut nodes = Vec::new();
    let mut window = Node::new(Role::Window);
    window.set_label(scene.title.clone());
    let mut children = Vec::new();
    if scene.tabs.len() > 1 {
        children.push(TABS);
        let list = tab_list(&scene.tabs, &mut nodes);
        nodes.push((TABS, list));
    }
    children.push(TERMINAL);
    let (terminal, caret) = terminal(&scene.terminal, &mut nodes);
    nodes.push((TERMINAL, terminal));
    let mut focus = if caret { TERMINAL } else { WINDOW };
    if let Some(menu) = &scene.menu {
        children.push(MENU);
        let node = menu_node(menu, &mut nodes);
        nodes.push((MENU, node));
        focus = menu
            .focused
            .map_or(MENU, |index| NodeId(MENU_ITEMS + index as u64));
    }
    if let Some(dialog) = &scene.dialog {
        children.push(DIALOG);
        let node = dialog_node(dialog, &mut nodes);
        nodes.push((DIALOG, node));
        focus = match (dialog.focused, &dialog.field) {
            (Some(index), _) => NodeId(BUTTONS + index as u64),
            (None, Some(_)) => FIELD,
            (None, None) => DIALOG,
        };
    }
    window.set_children(children);
    nodes.push((WINDOW, window));
    let mut info = TreeInfo::new(WINDOW);
    info.toolkit_name = Some("Caret".to_owned());
    info.toolkit_version = Some(env!("CARGO_PKG_VERSION").to_owned());
    TreeUpdate {
        nodes,
        tree: Some(info),
        tree_id: TreeId::ROOT,
        focus,
    }
}

fn tab_list(tabs: &[(String, bool)], nodes: &mut Vec<(NodeId, Node)>) -> Node {
    let mut list = Node::new(Role::TabList);
    let mut children = Vec::new();
    for (index, (label, shown)) in tabs.iter().enumerate() {
        let id = NodeId(TAB_ITEMS + index as u64);
        let mut tab = Node::new(Role::Tab);
        tab.set_label(label.clone());
        tab.set_selected(*shown);
        tab.add_action(Action::Click);
        children.push(id);
        nodes.push((id, tab));
    }
    list.set_children(children);
    list
}

fn menu_node(menu: &Menu, nodes: &mut Vec<(NodeId, Node)>) -> Node {
    let mut node = Node::new(Role::Menu);
    let mut children = Vec::new();
    for (index, (label, enabled)) in menu.items.iter().enumerate() {
        let id = NodeId(MENU_ITEMS + index as u64);
        let mut item = Node::new(Role::MenuItem);
        item.set_label(label.clone());
        if *enabled {
            item.add_action(Action::Click);
        } else {
            item.set_disabled();
        }
        children.push(id);
        nodes.push((id, item));
    }
    node.set_children(children);
    node
}

fn dialog_node(dialog: &Dialog, nodes: &mut Vec<(NodeId, Node)>) -> Node {
    let mut node = Node::new(Role::Dialog);
    node.set_label(dialog.heading.clone());
    node.set_description(dialog.body.clone());
    let mut children = Vec::new();
    if let Some(text) = &dialog.field {
        let mut field = Node::new(Role::TextInput);
        field.set_label(dialog.heading.clone());
        field.set_value(text.clone());
        children.push(FIELD);
        nodes.push((FIELD, field));
    }
    for (index, label) in dialog.buttons.iter().enumerate() {
        let id = NodeId(BUTTONS + index as u64);
        let mut button = Node::new(Role::Button);
        button.set_label(label.clone());
        button.add_action(Action::Click);
        children.push(id);
        nodes.push((id, button));
    }
    node.set_children(children);
    node
}

/// Where a cell's character is in the text: the run and the character in it.
struct Place {
    run: NodeId,
    index: usize,
}

/// The terminal's node, its text runs added to `nodes`, and whether it has a caret.
fn terminal(terminal: &Terminal, nodes: &mut Vec<(NodeId, Node)>) -> (Node, bool) {
    let mut node = Node::new(Role::Terminal);
    let (x, y, width, height) = terminal.bounds;
    node.set_bounds(Rect::new(x, y, x + width, y + height));
    let cell_height = terminal.cell.1;
    let mut children = Vec::new();
    let mut runs: Vec<Vec<(u16, NodeId)>> = Vec::new();
    let last = terminal.rows.len().saturating_sub(1);
    for (line, row) in terminal.rows.iter().enumerate() {
        // The caret goes on the cell the cursor is on, which may be past the text.
        let reach = terminal
            .cursor
            .filter(|&(cursor_line, _)| usize::from(cursor_line) == line)
            .map_or(0, |(_, column)| column + 1);
        let row = filled(row, reach);
        let chunks: Vec<&[Character]> = if row.is_empty() {
            vec![&[]]
        } else {
            row.chunks(RUN).collect()
        };
        let mut starts = Vec::new();
        for (part, chunk) in chunks.iter().enumerate() {
            let id = NodeId(RUNS + (line as u64) * 64 + part as u64);
            let ends_line = part == chunks.len() - 1 && line < last;
            let top = y + line as f64 * cell_height;
            nodes.push((id, run(chunk, ends_line, (x, top), terminal.cell)));
            children.push(id);
            starts.push((chunk.first().map_or(0, |character| character.column), id));
        }
        runs.push(starts);
    }
    node.set_children(children);
    let place = |(line, column): (u16, u16)| -> Option<Place> {
        let row = terminal.rows.get(usize::from(line))?;
        let starts = runs.get(usize::from(line))?;
        let row = filled(row, column + 1);
        let at = row
            .iter()
            .position(|character| character.column + character.cells > column)
            .unwrap_or(row.len());
        let part = at / RUN;
        let run = starts.get(part).or(starts.last())?.1;
        Some(Place {
            run,
            index: at - part * RUN,
        })
    };
    let position = |place: Place| TextPosition {
        node: place.run,
        character_index: place.index,
    };
    let selection = match (terminal.selection, terminal.cursor) {
        (Some((first, last)), _) => place(first).zip(place((last.0, last.1 + 1))),
        (None, Some(cursor)) => place(cursor).zip(place(cursor)),
        (None, None) => None,
    };
    let caret = selection.is_some();
    if let Some((anchor, focus)) = selection {
        node.set_text_selection(TextSelection {
            anchor: position(anchor),
            focus: position(focus),
        });
    }
    (node, caret)
}

/// `row` with a blank for each cell between its characters and up to `columns`, which
/// read as the spaces they show.
fn filled(row: &[Character], columns: u16) -> Vec<Character> {
    let mut filled = Vec::with_capacity(row.len());
    let mut end = 0;
    let blank = |column| Character {
        text: " ".to_owned(),
        column,
        cells: 1,
    };
    for character in row {
        filled.extend((end..character.column).map(blank));
        end = character.column + character.cells;
        filled.push(character.clone());
    }
    filled.extend((end..columns).map(blank));
    filled
}

/// A run of `characters`, ending with the line's break if `ends_line`, its top left
/// corner at `origin`.
fn run(characters: &[Character], ends_line: bool, origin: (f64, f64), cell: (f64, f64)) -> Node {
    let (cell_width, cell_height) = cell;
    let mut node = Node::new(Role::TextRun);
    let mut value = String::new();
    let mut lengths = Vec::new();
    let mut positions = Vec::new();
    let mut widths = Vec::new();
    let mut words = Vec::new();
    let start = characters.first().map_or(0, |character| character.column);
    let mut previous_blank = true;
    for (index, character) in characters.iter().enumerate() {
        value.push_str(&character.text);
        lengths.push(u8::try_from(character.text.len()).unwrap_or(u8::MAX));
        positions.push(f32::from(character.column - start) * cell_width as f32);
        widths.push(f32::from(character.cells) * cell_width as f32);
        let blank = character.text.trim().is_empty();
        // A word starts where text follows blanks; its trailing blanks are its own.
        if previous_blank && !blank && index > 0 {
            words.push(u8::try_from(index).unwrap_or(u8::MAX));
        }
        previous_blank = blank;
    }
    let width = widths.iter().sum::<f32>();
    if ends_line {
        value.push('\n');
        lengths.push(1);
        positions.push(width);
        widths.push(0.0);
    }
    let (x, y) = (origin.0 + f64::from(start) * cell_width, origin.1);
    node.set_bounds(Rect::new(x, y, x + f64::from(width), y + cell_height));
    node.set_value(value);
    node.set_character_lengths(lengths);
    node.set_character_positions(positions);
    node.set_character_widths(widths);
    node.set_word_starts(words);
    node
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(text: &str) -> Vec<Character> {
        text.chars()
            .enumerate()
            .filter(|(_, c)| *c != ' ')
            .map(|(column, c)| Character {
                text: c.to_string(),
                column: column as u16,
                cells: 1,
            })
            .collect()
    }

    fn node(update: &TreeUpdate, id: NodeId) -> &Node {
        &update
            .nodes
            .iter()
            .find(|(known, _)| *known == id)
            .expect("the node is in the tree")
            .1
    }

    fn scene(rows: &[&str], cursor: (u16, u16)) -> Scene {
        Scene {
            title: "zsh".to_owned(),
            terminal: Terminal {
                rows: rows.iter().map(|text| row(text)).collect(),
                cursor: Some(cursor),
                cell: (8.0, 16.0),
                ..Terminal::default()
            },
            ..Scene::default()
        }
    }

    #[test]
    fn each_row_is_a_line_of_text_with_the_caret_at_the_cursor() {
        let update = tree(&scene(&["$ ls  -l", "a"], (1, 3)));
        assert_eq!(update.focus, TERMINAL);
        let first = node(&update, NodeId(RUNS));
        // The gap between letters reads as the spaces it shows.
        assert_eq!(first.value(), Some("$ ls  -l\n"));
        assert_eq!(first.word_starts(), &[2, 6]);
        let second = node(&update, NodeId(RUNS + 64));
        assert_eq!(second.value(), Some("a   "));
        let selection = node(&update, TERMINAL).text_selection().expect("a caret");
        assert_eq!(selection.focus.node, NodeId(RUNS + 64));
        assert_eq!(selection.focus.character_index, 3);
        assert_eq!(selection.anchor, selection.focus);
    }

    #[test]
    fn a_selection_runs_from_its_first_cell_to_after_its_last() {
        let mut scene = scene(&["one two"], (0, 0));
        scene.terminal.selection = Some(((0, 4), (0, 6)));
        let update = tree(&scene);
        let selection = node(&update, TERMINAL)
            .text_selection()
            .expect("a selection");
        assert_eq!(selection.anchor.character_index, 4);
        assert_eq!(selection.focus.character_index, 7);
    }

    #[test]
    fn a_menu_or_dialog_takes_the_focus_where_its_keys_are() {
        let mut scene = scene(&["x"], (0, 0));
        scene.menu = Some(Menu {
            items: vec![("Copy".to_owned(), false), ("Paste".to_owned(), true)],
            focused: Some(1),
        });
        assert_eq!(tree(&scene).focus, NodeId(MENU_ITEMS + 1));
        scene.dialog = Some(Dialog {
            heading: "Rename Tab".to_owned(),
            field: Some("build".to_owned()),
            buttons: vec!["Cancel".to_owned(), "Rename".to_owned()],
            ..Dialog::default()
        });
        let update = tree(&scene);
        assert_eq!(update.focus, FIELD);
        assert_eq!(node(&update, FIELD).value(), Some("build"));
    }

    #[test]
    fn clicked_nodes_name_what_they_stand_for() {
        let mut scene = scene(&["x"], (0, 0));
        scene.tabs = vec![("a".to_owned(), false), ("b".to_owned(), true)];
        let update = tree(&scene);
        let tab = NodeId(TAB_ITEMS + 1);
        assert!(node(&update, tab).supports_action(Action::Click));
        assert_eq!(target(tab), Some(Target::Tab(1)));
        assert_eq!(target(NodeId(BUTTONS)), Some(Target::Button(0)));
        assert_eq!(target(NodeId(RUNS + 64)), None);
        assert_eq!(target(TERMINAL), None);
    }

    #[test]
    fn a_row_wider_than_a_run_takes_several() {
        let wide = "x".repeat(300);
        let update = tree(&scene(&[&wide], (0, 299)));
        assert_eq!(node(&update, NodeId(RUNS)).value().map(str::len), Some(RUN));
        let selection = node(&update, TERMINAL).text_selection().expect("a caret");
        assert_eq!(selection.focus.node, NodeId(RUNS + 1));
        assert_eq!(selection.focus.character_index, 299 - RUN);
    }
}
