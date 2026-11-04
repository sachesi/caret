//! What the window keeps between runs that is no setting: the size it had and whether
//! it was maximized, so that the next window opens the same. It lives in
//! `$XDG_STATE_HOME/caret/window.toml`, apart from `config.toml`, which is the user's.

use std::path::{Path, PathBuf};

use toml_edit::{Array, DocumentMut, value};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct WindowState {
    /// The size it had last when neither maximized nor tiled, in logical pixels.
    pub size: Option<(u32, u32)>,
    pub maximized: bool,
}

impl WindowState {
    pub fn load() -> Self {
        path()
            .and_then(|path| std::fs::read_to_string(path).ok())
            .map(|text| Self::parse(&text))
            .unwrap_or_default()
    }

    fn parse(text: &str) -> Self {
        let Ok(document) = text.parse::<DocumentMut>() else {
            return Self::default();
        };
        let size = document
            .get("size")
            .and_then(|size| size.as_array())
            .and_then(|size| {
                let mut sides = size
                    .iter()
                    .filter_map(|side| u32::try_from(side.as_integer()?).ok());
                Some((sides.next()?, sides.next()?))
            })
            .filter(|&(width, height)| width > 0 && height > 0);
        let maximized = document
            .get("maximized")
            .and_then(toml_edit::Item::as_bool)
            .unwrap_or(false);
        Self { size, maximized }
    }

    fn document(self) -> DocumentMut {
        let mut document = DocumentMut::new();
        if let Some((width, height)) = self.size {
            document["size"] = value(Array::from_iter([i64::from(width), i64::from(height)]));
        }
        document["maximized"] = value(self.maximized);
        document
    }

    pub fn save(self) {
        let Some(path) = path() else {
            return;
        };
        // Written beside it and renamed over it, so that no reader sees half a file. Every
        // window closing at once saves, so each writes a file of its own first.
        let written = path.with_extension(format!("toml.{}.new", std::process::id()));
        let saved = path
            .parent()
            .map_or(Ok(()), std::fs::create_dir_all)
            .and_then(|()| std::fs::write(&written, self.document().to_string()))
            .and_then(|()| std::fs::rename(&written, &path));
        if let Err(error) = saved {
            tracing::warn!("writing {}: {error}", path.display());
        }
    }
}

fn path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_STATE_HOME")
        .filter(|base| Path::new(base).is_absolute())
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|home| home.join(".local/state")))?;
    Some(base.join("caret").join("window.toml"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_state_reads_back_as_it_was_written() {
        let state = WindowState {
            size: Some((900, 640)),
            maximized: true,
        };
        assert_eq!(WindowState::parse(&state.document().to_string()), state);
        assert_eq!(
            WindowState::parse("size = [0, 10]\n"),
            WindowState::default()
        );
        assert_eq!(WindowState::parse("not toml ["), WindowState::default());
    }
}
