//! Icons from the desktop's icon theme, found as the icon theme specification says: in the
//! theme's folders, in the themes it inherits from, under each place themes are kept.

use std::collections::HashSet;
use std::fs;
use std::path::PathBuf;

use crate::fonts::Raster;

/// The places icon themes are kept, the first winning.
fn theme_roots() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let data_home = std::env::var_os("XDG_DATA_HOME")
        .map(PathBuf::from)
        .or_else(|| Some(home.as_ref()?.join(".local/share")));
    let data_dirs = std::env::var("XDG_DATA_DIRS")
        .ok()
        .filter(|dirs| !dirs.is_empty())
        .unwrap_or_else(|| "/usr/local/share:/usr/share".to_owned());
    home.map(|home| home.join(".icons"))
        .into_iter()
        .chain(data_home.map(|data| data.join("icons")))
        .chain(
            data_dirs
                .split(':')
                .map(|dir| PathBuf::from(dir).join("icons")),
        )
        .collect()
}

/// What a theme's `index.theme` says about finding icons.
#[derive(Debug, Default, PartialEq, Eq)]
struct Index {
    inherits: Vec<String>,
    /// The theme's folders, those of icons drawn nearest the size asked for first.
    folders: Vec<String>,
}

fn parse_index(text: &str, size: u32) -> Index {
    let list = |value: &str| -> Vec<String> {
        value
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .map(str::to_owned)
            .collect()
    };
    let mut index = Index::default();
    let mut sizes = Vec::new();
    let mut section = "";
    for line in text.lines().map(str::trim) {
        if let Some(name) = line
            .strip_prefix('[')
            .and_then(|line| line.strip_suffix(']'))
        {
            section = name;
        } else if let Some((key, value)) = line.split_once('=') {
            match (section, key.trim()) {
                ("Icon Theme", "Inherits") => index.inherits = list(value),
                ("Icon Theme", "Directories") => index.folders = list(value),
                (folder, "Size") => {
                    if let Ok(drawn) = value.trim().parse::<u32>() {
                        sizes.push((folder.to_owned(), drawn));
                    }
                }
                _ => {}
            }
        }
    }
    let distance = |folder: &String| {
        sizes
            .iter()
            .find(|(name, _)| name == folder)
            .map_or(u32::MAX, |(_, drawn)| drawn.abs_diff(size))
    };
    index.folders.sort_by_key(distance);
    index
}

/// The file with the SVG icon `name` in `theme` or the themes it inherits from.
fn find(theme: &str, name: &str, size: u32, roots: &[PathBuf]) -> Option<PathBuf> {
    let file = format!("{name}.svg");
    let mut seen = HashSet::new();
    let mut themes = vec![theme.to_owned()];
    while let Some(theme) = themes.pop() {
        if !seen.insert(theme.clone()) {
            continue;
        }
        let dirs: Vec<PathBuf> = roots.iter().map(|root| root.join(&theme)).collect();
        let Some(text) = dirs
            .iter()
            .find_map(|dir| fs::read_to_string(dir.join("index.theme")).ok())
        else {
            continue;
        };
        let index = parse_index(&text, size);
        for folder in &index.folders {
            for dir in &dirs {
                let path = dir.join(folder).join(&file);
                if path.is_file() {
                    return Some(path);
                }
            }
        }
        themes.extend(index.inherits.into_iter().rev());
    }
    None
}

/// `svg` as a `size` pixels square mask: symbolic icons take the colour of the text around
/// them, whatever colour they are drawn in.
fn mask(svg: &[u8], size: u32) -> Option<Raster> {
    let tree = resvg::usvg::Tree::from_data(svg, &resvg::usvg::Options::default()).ok()?;
    let mut pixmap = resvg::tiny_skia::Pixmap::new(size, size)?;
    let drawn = tree.size();
    let scale = size as f32 / drawn.width().max(drawn.height());
    let transform = resvg::tiny_skia::Transform::from_scale(scale, scale);
    resvg::render(&tree, transform, &mut pixmap.as_mut());
    Some(Raster {
        width: size,
        height: size,
        left: 0,
        top: 0,
        pixels: pixmap
            .pixels()
            .iter()
            .flat_map(|pixel| [pixel.alpha(); 4])
            .collect(),
        color: false,
    })
}

/// The symbolic icon `name` of `theme`, `size` pixels square, if the theme has one.
pub fn symbolic(theme: &str, name: &str, size: u32) -> Option<Raster> {
    let path = find(theme, name, size, &theme_roots())?;
    match fs::read(&path) {
        Ok(svg) => mask(&svg, size).or_else(|| {
            tracing::warn!("drawing the icon {}: not an SVG image", path.display());
            None
        }),
        Err(error) => {
            tracing::warn!("reading the icon {}: {error}", path.display());
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const INDEX: &str = "[Icon Theme]\nName=Test\nInherits=base, hicolor\n\
        Directories=big/actions,small/actions,scalable\n\n\
        [big/actions]\nSize=48\n\n[small/actions]\nSize=16\n\n[scalable]\nSize=64\n";

    #[test]
    fn the_folders_nearest_the_size_come_first() {
        let index = parse_index(INDEX, 16);
        assert_eq!(index.inherits, ["base", "hicolor"]);
        assert_eq!(index.folders, ["small/actions", "big/actions", "scalable"]);
    }

    /// Themes in a folder that goes when the test ends.
    struct Themes(PathBuf);

    impl Drop for Themes {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    impl Themes {
        fn new(test: &str) -> Self {
            let root =
                std::env::temp_dir().join(format!("caret-icons-{}-{test}", std::process::id()));
            let write = |path: &str, text: &str| {
                let path = root.join(path);
                fs::create_dir_all(path.parent().expect("a file in a folder")).expect("a folder");
                fs::write(path, text).expect("a file");
            };
            write("own/index.theme", INDEX);
            write("own/big/actions/both-symbolic.svg", "");
            write("own/small/actions/both-symbolic.svg", "");
            write(
                "base/index.theme",
                "[Icon Theme]\nInherits=own\nDirectories=ui\n[ui]\nSize=16\n",
            );
            write("base/ui/inherited-symbolic.svg", "");
            Self(root)
        }

        fn find(&self, theme: &str, name: &str) -> Option<PathBuf> {
            find(theme, name, 16, std::slice::from_ref(&self.0))
        }
    }

    #[test]
    fn an_icon_comes_from_the_theme_at_the_size_nearest() {
        let themes = Themes::new("nearest");
        assert_eq!(
            themes.find("own", "both-symbolic"),
            Some(themes.0.join("own/small/actions/both-symbolic.svg")),
        );
    }

    #[test]
    fn an_icon_the_theme_lacks_comes_from_the_theme_it_inherits_from() {
        let themes = Themes::new("inherited");
        assert_eq!(
            themes.find("own", "inherited-symbolic"),
            Some(themes.0.join("base/ui/inherited-symbolic.svg")),
        );
    }

    #[test]
    fn themes_inheriting_in_a_circle_end_the_search() {
        let themes = Themes::new("circle");
        assert_eq!(themes.find("own", "missing-symbolic"), None);
        assert_eq!(themes.find("no-such-theme", "both-symbolic"), None);
    }

    #[test]
    fn an_icon_is_a_mask_of_what_it_paints_in_any_colour() {
        let svg = br##"<svg xmlns="http://www.w3.org/2000/svg" width="16" height="16">
            <rect x="0" y="0" width="8" height="16" fill="#2e3436"/></svg>"##;
        let raster = mask(svg, 32).expect("the icon draws");
        assert_eq!((raster.width, raster.height, raster.color), (32, 32, false));
        let pixel = |x: usize| &raster.pixels[x * 4..x * 4 + 4];
        assert_eq!(pixel(2), [255; 4]);
        assert_eq!(pixel(24), [0; 4]);
    }

    #[test]
    fn what_is_not_an_image_draws_nothing() {
        assert_eq!(mask(b"not an image", 16), None);
    }
}
