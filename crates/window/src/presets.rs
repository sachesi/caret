//! Split presets: layouts named in `config.toml`, each pane with what it runs and where.
//! Nothing here needs a window, which starts them from what `Preset` gives it.

use std::collections::HashMap;

use caret_protocol::TerminalId;
use caret_protocol::layout::{MOST_PANES, Node, WHOLE};
use toml_edit::{Item, TableLike};

use crate::shortcuts::Shortcut;

/// How deep a layout's parentheses may go, which reading them recurses into: a line of the
/// settings must not be able to run the window out of stack.
const DEEPEST_PARENTHESES: usize = 64;

#[derive(Debug, Clone, PartialEq)]
pub struct Preset {
    pub name: String,
    pub shape: Shape,
    pub shortcut: Option<Shortcut>,
    /// Every pane has a command, which leaves no pane for a running program: the layout
    /// always opens in a new tab.
    pub opens_tab: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    Pane(PresetPane),
    Split {
        vertical: bool,
        share: u16,
        first: Box<Shape>,
        second: Box<Shape>,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PresetPane {
    pub name: String,
    /// The program and its arguments; the shell when empty.
    pub command: Vec<String>,
    pub directory: Option<String>,
    /// The running program goes here.
    pub keep: bool,
}

impl Preset {
    /// The preset `name` as `table` of the settings file gives it, or what is wrong with it.
    pub fn read(name: &str, table: &dyn TableLike) -> Result<Self, String> {
        let mut layout = None;
        let mut shortcut = None;
        let mut settings = HashMap::new();
        for (key, item) in table.iter() {
            match key {
                "layout" => layout = Some(item.as_str().ok_or("`layout` is not text")?),
                "shortcut" => {
                    shortcut = Some(
                        item.as_str()
                            .and_then(Shortcut::parse)
                            .ok_or("`shortcut` is not a key such as \"Ctrl+Alt+1\"")?,
                    );
                }
                "panes" => {
                    let panes = item.as_table_like().ok_or("`panes` is not a table")?;
                    for (pane, item) in panes.iter() {
                        settings.insert(pane.to_owned(), pane_settings(pane, item)?);
                    }
                }
                _ => return Err(format!("it has no setting `{key}`")),
            }
        }
        let mut shape = parse_layout(layout.ok_or("it has no `layout`")?)?;
        let opens_tab = shape.settle(&settings)?;
        Ok(Self {
            name: name.to_owned(),
            shape,
            shortcut,
            opens_tab,
        })
    }

    /// The pane the running program goes to, or the one a new tab of the layout starts with.
    pub fn kept(&self) -> &PresetPane {
        self.shape
            .panes()
            .into_iter()
            .find(|pane| pane.keep)
            .expect("`settle` marks a pane to keep")
    }

    /// Each pane's rectangle in a square of side 1, as `[x, y, width, height]`.
    pub fn picture(&self) -> Vec<[f32; 4]> {
        let mut rects = Vec::new();
        self.shape.rects([0.0, 0.0, 1.0, 1.0], &mut rects);
        rects
    }
}

impl Shape {
    pub fn panes(&self) -> Vec<&PresetPane> {
        match self {
            Self::Pane(pane) => vec![pane],
            Self::Split { first, second, .. } => {
                let mut panes = first.panes();
                panes.extend(second.panes());
                panes
            }
        }
    }

    fn panes_mut(&mut self) -> Vec<&mut PresetPane> {
        match self {
            Self::Pane(pane) => vec![pane],
            Self::Split { first, second, .. } => {
                let mut panes = first.panes_mut();
                panes.extend(second.panes_mut());
                panes
            }
        }
    }

    fn rects(&self, [x, y, width, height]: [f32; 4], rects: &mut Vec<[f32; 4]>) {
        match self {
            Self::Pane(_) => rects.push([x, y, width, height]),
            Self::Split {
                vertical,
                share,
                first,
                second,
            } => {
                let part = f32::from(*share) / f32::from(WHOLE);
                if *vertical {
                    first.rects([x, y, width * part, height], rects);
                    second.rects([x + width * part, y, width * (1.0 - part), height], rects);
                } else {
                    first.rects([x, y, width, height * part], rects);
                    second.rects([x, y + height * part, width, height * (1.0 - part)], rects);
                }
            }
        }
    }

    /// Gives each pane its settings, and marks the one to keep where none is: the first
    /// that starts nothing of its own.
    /// Gives the panes their settings and marks the one to keep, and says whether the
    /// layout has to open in a new tab. A name used twice gives both panes its settings.
    fn settle(&mut self, settings: &HashMap<String, PresetPane>) -> Result<bool, String> {
        let mut panes = self.panes_mut();
        if panes.len() < 2 {
            return Err("its `layout` has only one pane".to_owned());
        }
        if let Some(unknown) = settings
            .keys()
            .find(|name| !panes.iter().any(|pane| pane.name == **name))
        {
            return Err(format!("its `layout` has no pane `{unknown}`"));
        }
        for pane in &mut panes {
            if let Some(set) = settings.get(&pane.name) {
                **pane = PresetPane {
                    name: std::mem::take(&mut pane.name),
                    ..set.clone()
                };
            }
        }
        if panes.iter().filter(|pane| pane.keep).count() > 1 {
            return Err("more than one pane has `keep`".to_owned());
        }
        if let Some(pane) = panes
            .iter()
            .find(|pane| pane.keep && !pane.command.is_empty())
        {
            return Err(format!(
                "pane `{}` has both `keep` and `command`, and a kept pane is already running",
                pane.name
            ));
        }
        if panes.iter().any(|pane| pane.keep) {
            return Ok(false);
        }
        let opens_tab = panes.iter().all(|pane| !pane.command.is_empty());
        if let Some(pane) = panes
            .iter_mut()
            .find(|pane| opens_tab || pane.command.is_empty())
        {
            pane.keep = true;
        }
        Ok(opens_tab)
    }

    /// The tree with made-up terminals in order, to find where the panes go.
    pub fn tree(&self) -> Node {
        let mut next = u32::MAX;
        self.tree_from(&mut next)
    }

    fn tree_from(&self, next: &mut u32) -> Node {
        match self {
            Self::Pane(_) => {
                let terminal = TerminalId(*next);
                *next -= 1;
                Node::Pane { terminal }
            }
            Self::Split {
                vertical,
                share,
                first,
                second,
            } => Node::Split {
                vertical: *vertical,
                share: *share,
                first: Box::new(first.tree_from(next)),
                second: Box::new(second.tree_from(next)),
            },
        }
    }
}

fn pane_settings(name: &str, item: &Item) -> Result<PresetPane, String> {
    let table = item
        .as_table_like()
        .ok_or_else(|| format!("pane `{name}` is not a table"))?;
    let mut pane = PresetPane::default();
    for (key, value) in table.iter() {
        match key {
            "command" => {
                let line = value
                    .as_str()
                    .ok_or_else(|| format!("pane `{name}`: `command` is not text"))?;
                pane.command = shlex::split(line)
                    .ok_or_else(|| format!("pane `{name}`: `command` has an open quote"))?;
            }
            "directory" => {
                let directory = value
                    .as_str()
                    .ok_or_else(|| format!("pane `{name}`: `directory` is not text"))?;
                pane.directory = Some(directory.to_owned());
            }
            "keep" => {
                pane.keep = value
                    .as_bool()
                    .ok_or_else(|| format!("pane `{name}`: `keep` is not true or false"))?;
            }
            _ => return Err(format!("pane `{name}` has no setting `{key}`")),
        }
    }
    Ok(pane)
}

#[derive(Debug, PartialEq)]
enum Token<'a> {
    Name(&'a str),
    Percent(u8),
    Side,
    Under,
    Open,
    Close,
}

fn tokens(layout: &str) -> Result<Vec<Token<'_>>, String> {
    let mut tokens = Vec::new();
    let mut rest = layout.trim_start();
    while let Some(first) = rest.chars().next() {
        let length = match first {
            '|' | '/' | '(' | ')' => {
                tokens.push(match first {
                    '|' => Token::Side,
                    '/' => Token::Under,
                    '(' => Token::Open,
                    _ => Token::Close,
                });
                1
            }
            _ => {
                let word = rest
                    .split(|c: char| c.is_whitespace() || "|/()".contains(c))
                    .next()
                    .unwrap_or(rest);
                tokens.push(match word.strip_suffix('%') {
                    Some(number) => Token::Percent(
                        number
                            .parse()
                            .ok()
                            .filter(|percent| (1..100).contains(percent))
                            .ok_or_else(|| format!("`{word}` is not from 1% to 99%"))?,
                    ),
                    None => Token::Name(word),
                });
                word.len()
            }
        };
        rest = rest[length..].trim_start();
    }
    Ok(tokens)
}

/// `layout` as a tree of panes without their settings, each named once.
fn parse_layout(layout: &str) -> Result<Shape, String> {
    let tokens = tokens(layout)?;
    let mut parser = Parser {
        tokens: &tokens,
        at: 0,
        panes: 0,
        depth: 0,
    };
    let shape = parser.group()?;
    if parser.at < parser.tokens.len() {
        return Err("`layout` has a `)` without its `(`".to_owned());
    }
    if parser.panes > MOST_PANES {
        return Err(format!("`layout` has more than {MOST_PANES} panes"));
    }
    Ok(shape)
}

struct Parser<'a> {
    tokens: &'a [Token<'a>],
    at: usize,
    panes: usize,
    /// How many parentheses are open, which the parser recurses into.
    depth: usize,
}

impl<'a> Parser<'a> {
    fn next(&mut self) -> Option<&'a Token<'a>> {
        let token = self.tokens.get(self.at);
        self.at += 1;
        token
    }

    /// Panes and groups all side by side or all under each other.
    fn group(&mut self) -> Result<Shape, String> {
        let mut parts = vec![self.part()?];
        let mut vertical = None;
        while let Some(token @ (Token::Side | Token::Under)) = self.tokens.get(self.at) {
            self.at += 1;
            let side = *token == Token::Side;
            if vertical.is_some_and(|vertical| vertical != side) {
                return Err("`layout` needs parentheses between `|` and `/`".to_owned());
            }
            vertical = Some(side);
            parts.push(self.part()?);
        }
        if let Some(vertical) = vertical {
            return joined(parts, vertical);
        }
        let (shape, percent) = parts.remove(0);
        if percent.is_some() {
            return Err("`layout` gives a size to what has all the room".to_owned());
        }
        Ok(shape)
    }

    /// A pane or a group in parentheses, and the size it asks for.
    fn part(&mut self) -> Result<(Shape, Option<u8>), String> {
        let shape = match self.next() {
            Some(Token::Name(name)) => {
                self.panes += 1;
                Shape::Pane(PresetPane {
                    name: (*name).to_owned(),
                    ..PresetPane::default()
                })
            }
            Some(Token::Open) => {
                if self.depth == DEEPEST_PARENTHESES {
                    return Err("`layout` nests its parentheses too deep".to_owned());
                }
                self.depth += 1;
                let shape = self.group()?;
                self.depth -= 1;
                if self.next() != Some(&Token::Close) {
                    return Err("`layout` has a `(` without its `)`".to_owned());
                }
                shape
            }
            _ => return Err("`layout` is missing a pane".to_owned()),
        };
        let percent = if let Some(&Token::Percent(percent)) = self.tokens.get(self.at) {
            self.at += 1;
            Some(percent)
        } else {
            None
        };
        Ok((shape, percent))
    }
}

/// `parts` in a row, those without a size sharing what the others leave.
fn joined(parts: Vec<(Shape, Option<u8>)>, vertical: bool) -> Result<Shape, String> {
    let asked: u32 = parts
        .iter()
        .filter_map(|(_, percent)| percent.map(u32::from))
        .sum();
    let unsized_parts = parts
        .iter()
        .filter(|(_, percent)| percent.is_none())
        .count() as u32;
    if asked > 100 || (unsized_parts > 0 && asked >= 100) || (unsized_parts == 0 && asked != 100) {
        return Err(format!("`layout` asks for {asked}% of the room in one row"));
    }
    let rest = if unsized_parts == 0 {
        0.0
    } else {
        f64::from(100 - asked) / f64::from(unsized_parts)
    };
    let sized: Vec<(Shape, f64)> = parts
        .into_iter()
        .map(|(shape, percent)| (shape, percent.map_or(rest, f64::from)))
        .collect();
    Ok(fold_row(sized, vertical))
}

/// Binary splits for a row of `parts`, each split's first side the row's next part.
fn fold_row(mut parts: Vec<(Shape, f64)>, vertical: bool) -> Shape {
    let (first, room) = parts.remove(0);
    if parts.is_empty() {
        return first;
    }
    let others: f64 = parts.iter().map(|(_, room)| room).sum();
    let share = (room / (room + others) * f64::from(WHOLE)).round() as u16;
    Shape::Split {
        vertical,
        share: share.clamp(1, WHOLE - 1),
        first: Box::new(first),
        second: Box::new(fold_row(parts, vertical)),
    }
}

/// A pane of a layout being saved.
#[derive(Debug, PartialEq, Eq)]
pub struct SavedPane {
    pub name: String,
    pub command: Option<String>,
    pub directory: Option<String>,
    pub keep: bool,
}

/// The `layout` line and the panes of `root`, as a preset saves them: each pane runs the
/// program `programs` has for it, and `focused` is kept if it has none. Panes have numbers
/// for names, as their programs do not tell them apart.
pub fn saved(
    root: &Node,
    focused: TerminalId,
    programs: &HashMap<TerminalId, String>,
    directories: &HashMap<TerminalId, String>,
) -> (String, Vec<SavedPane>) {
    let names: HashMap<TerminalId, String> = root
        .panes()
        .into_iter()
        .enumerate()
        .map(|(at, terminal)| (terminal, format!("pane{}", at + 1)))
        .collect();
    let panes = root
        .panes()
        .into_iter()
        .map(|terminal| SavedPane {
            name: names[&terminal].clone(),
            command: programs.get(&terminal).cloned(),
            directory: directories.get(&terminal).cloned(),
            keep: terminal == focused && !programs.contains_key(&terminal),
        })
        .collect();
    (layout_line(root, &names), panes)
}

fn layout_line(node: &Node, names: &HashMap<TerminalId, String>) -> String {
    match node {
        Node::Pane { terminal } => names[terminal].clone(),
        Node::Split {
            vertical,
            share,
            first,
            second,
        } => {
            let side = |side: &Node| match side {
                Node::Pane { .. } => layout_line(side, names),
                Node::Split { .. } => format!("({})", layout_line(side, names)),
            };
            let percent = (u32::from(*share) * 100 + u32::from(WHOLE) / 2) / u32::from(WHOLE);
            let percent = percent.clamp(1, 99);
            format!(
                "{} {percent}% {} {} {}%",
                side(first),
                if *vertical { '|' } else { '/' },
                side(second),
                100 - percent,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use toml_edit::DocumentMut;

    use super::*;

    fn read(text: &str) -> Result<Preset, String> {
        let document = text.parse::<DocumentMut>().unwrap();
        Preset::read("dev", document.as_table())
    }

    #[test]
    fn a_layout_past_what_a_tab_or_the_stack_takes_is_refused() {
        let nested = format!("{}a{}", "(".repeat(200_000), ")".repeat(200_000));
        assert!(parse_layout(&nested).is_err());
        let names: Vec<String> = (0..=MOST_PANES).map(|at| format!("p{at}")).collect();
        assert!(parse_layout(&names.join(" | ")).is_err());
        let names = &names[..MOST_PANES];
        assert!(parse_layout(&names.join(" | ")).is_ok());
    }

    fn names(preset: &Preset) -> Vec<&str> {
        preset
            .shape
            .panes()
            .iter()
            .map(|pane| pane.name.as_str())
            .collect()
    }

    #[test]
    fn a_layout_line_makes_splits_with_their_sizes() {
        let preset = read("layout = \"editor 60% | (logs / shell)\"").unwrap();
        assert_eq!(names(&preset), ["editor", "logs", "shell"]);
        let Shape::Split {
            vertical,
            share,
            second,
            ..
        } = &preset.shape
        else {
            panic!("{preset:?}");
        };
        assert!(*vertical);
        assert_eq!(*share, 6000);
        assert!(matches!(
            **second,
            Shape::Split {
                vertical: false,
                share: 5000,
                ..
            }
        ));
    }

    #[test]
    fn panes_without_a_size_share_what_the_others_leave() {
        let preset = read("layout = \"a | b | c\"").unwrap();
        let Shape::Split { share, second, .. } = &preset.shape else {
            panic!("{preset:?}");
        };
        assert_eq!(*share, 3333);
        assert!(matches!(**second, Shape::Split { share: 5000, .. }));

        let preset = read("layout = \"a 20% / b / c 20%\"").unwrap();
        let picture = preset.picture();
        assert!((picture[0][3] - 0.2).abs() < 0.001);
        assert!((picture[1][3] - 0.6).abs() < 0.001);
        assert!((picture[2][1] - 0.8).abs() < 0.001);
    }

    #[test]
    fn a_layout_that_cannot_be_made_says_why() {
        for (layout, why) in [
            ("a", "only one pane"),
            ("a | b / c", "parentheses"),
            ("(a | b", "without its `)`"),
            ("a | b)", "without its `(`"),
            ("a | ", "missing a pane"),
            ("a 100% | b", "not from 1% to 99%"),
            ("a 70% | b 40%", "110%"),
            ("a 60% | b 30%", "90%"),
            ("(a | b) 50%", "all the room"),
        ] {
            let error = read(&format!("layout = \"{layout}\"")).unwrap_err();
            assert!(error.contains(why), "{layout}: {error}");
        }
    }

    #[test]
    fn panes_take_their_settings_and_one_is_kept() {
        let preset = read(
            "layout = \"editor | logs\"\nshortcut = \"Ctrl+Alt+P\"\n\
             [panes.editor]\ncommand = \"nvim 'a b'\"\n\
             [panes.logs]\ndirectory = \"~/src\"\n",
        )
        .unwrap();
        let panes = preset.shape.panes();
        assert_eq!(panes[0].command, ["nvim", "a b"]);
        assert_eq!(panes[1].directory.as_deref(), Some("~/src"));
        assert_eq!(preset.kept().name, "logs");
        assert!(preset.shortcut.is_some());

        let marked = read("layout = \"a | b\"\n[panes.b]\nkeep = true\n").unwrap();
        assert_eq!(marked.kept().name, "b");
        let first = read("layout = \"a | b\"").unwrap();
        assert_eq!(first.kept().name, "a");
    }

    #[test]
    fn settings_that_contradict_each_other_say_why() {
        for (text, why) in [
            (
                "[panes.a]\nkeep = true\n[panes.b]\nkeep = true\n",
                "more than one",
            ),
            (
                "[panes.a]\nkeep = true\ncommand = \"top\"\n",
                "already running",
            ),
            ("[panes.c]\nkeep = true\n", "no pane `c`"),
            ("[panes.a]\nkep = true\n", "no setting `kep`"),
            ("[panes.a]\ncommand = \"'open\"\n", "open quote"),
        ] {
            let error = read(&format!("layout = \"a | b\"\n{text}")).unwrap_err();
            assert!(error.contains(why), "{text}: {error}");
        }
        assert!(read("").unwrap_err().contains("no `layout`"));
        assert!(
            read("layout = \"a | b\"\nsize = 3\n")
                .unwrap_err()
                .contains("`size`")
        );
    }

    #[test]
    fn a_name_used_twice_gives_both_panes_its_settings() {
        let preset = read("layout = \"a | b | a\"\n[panes.a]\ncommand = \"top\"\n").unwrap();
        let panes = preset.shape.panes();
        assert_eq!(panes[0].command, ["top"]);
        assert_eq!(panes[2].command, ["top"]);
        assert!(panes[1].keep);
        assert!(!preset.opens_tab);
        let error = read("layout = \"a | a\"\n[panes.a]\nkeep = true\n").unwrap_err();
        assert!(error.contains("more than one"), "{error}");
    }

    #[test]
    fn a_layout_with_a_command_in_every_pane_opens_a_tab_starting_with_the_first() {
        let preset = read("layout = \"a | a\"\n[panes.a]\ncommand = \"top\"\n").unwrap();
        assert!(preset.opens_tab);
        assert_eq!(preset.kept().command, ["top"]);
        assert_eq!(
            preset.shape.panes().iter().filter(|pane| pane.keep).count(),
            1
        );
        assert!(!read("layout = \"a | b\"").unwrap().opens_tab);
    }

    #[test]
    fn a_focused_pane_running_a_program_saves_it_and_keeps_nothing() {
        let (a, b) = (TerminalId(7), TerminalId(8));
        let root = Node::Split {
            vertical: true,
            share: 5000,
            first: Box::new(Node::Pane { terminal: a }),
            second: Box::new(Node::Pane { terminal: b }),
        };
        let programs = HashMap::from([(a, "nvim".to_owned()), (b, "top".to_owned())]);
        let (_, panes) = saved(&root, a, &programs, &HashMap::new());
        assert_eq!(panes[0].command.as_deref(), Some("nvim"));
        assert_eq!(panes[1].command.as_deref(), Some("top"));
        assert!(panes.iter().all(|pane| !pane.keep));
    }

    #[test]
    fn a_layout_saved_reads_back_the_same() {
        let (a, b, c) = (TerminalId(7), TerminalId(8), TerminalId(9));
        let root = Node::Split {
            vertical: true,
            share: 7000,
            first: Box::new(Node::Pane { terminal: a }),
            second: Box::new(Node::Split {
                vertical: false,
                share: 2500,
                first: Box::new(Node::Pane { terminal: b }),
                second: Box::new(Node::Pane { terminal: c }),
            }),
        };
        let programs = HashMap::from([(a, "nvim".to_owned()), (c, "top".to_owned())]);
        let directories = HashMap::from([(b, "/tmp".to_owned())]);
        let (layout, panes) = saved(&root, b, &programs, &directories);
        assert_eq!(layout, "pane1 70% | (pane2 25% / pane3 75%) 30%");
        assert!(panes[1].keep);
        assert_eq!(panes[0].command.as_deref(), Some("nvim"));
        assert_eq!(panes[1].command, None);
        assert_eq!(panes[1].directory.as_deref(), Some("/tmp"));
        assert_eq!(panes[2].command.as_deref(), Some("top"));

        let preset = read(&format!("layout = \"{layout}\"")).unwrap();
        let tree = preset.shape.tree();
        let shares = |node: &Node| match node {
            Node::Split { share, .. } => *share,
            Node::Pane { .. } => 0,
        };
        assert_eq!(shares(&tree), 7000);
        let Node::Split { second, .. } = &tree else {
            panic!("{tree:?}");
        };
        assert_eq!(shares(second), 2500);
        assert_eq!(tree.panes().len(), 3);
    }
}
