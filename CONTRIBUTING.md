# Contributing

Bugs and ideas go to the [issue tracker](https://github.com/sachesi/tangent/issues);
security problems do not, see [SECURITY.md](SECURITY.md).

Before a change goes in:

- `just check` and `just test` pass. CI runs both on Fedora 44, with `cargo deny check`, for
  every push and pull request.
- Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/):
  `fix:`, `feat:`, `perf:`, `docs:` and so on, with a subject that says what changed for
  someone using Tangent.
- Every string the user sees goes through `gettext`. `just po` updates the catalogues in
  `po/`, and a change that adds strings brings their translations along where it can.
- Behaviour described in `docs/` changes with the code that implements it.

## Where things are

    build.rs                  runs blueprint-compiler and bundles the GResource
    data/ui/*.blp             the window and the shortcuts dialog
    src/bin/tangent.rs        the executable, a thin main
    src/application.rs        GtkApplication subclass, command line, app actions, accels
    src/window.rs             the window: tabs, find bar, win.* actions, closing
    src/preferences.rs        the preferences dialog
    src/settings.rs           the GSettings object, the font and the colour scheme
    src/terminal_view.rs      one terminal: the widget, its GL context, sizing, scrolling,
                              drawing, clipboard and find; in terminal_view/ its input
    src/session.rs            the terminal state, the pty and the thread that reads it
    src/encode.rs             keys, mouse events, pastes and focus as bytes for the program
    src/frame.rs              a screen captured from the terminal, turned into quads
    src/renderer.rs           OpenGL: shaders, the glyph atlas, the textures GTK composites
    src/fonts.rs              cell metrics and glyphs rasterised by Pango and cairo
    src/box_drawing.rs        box drawing and block characters drawn to fill their cells
    src/palette.rs            the default colours and the 256-colour palette
    src/links.rs              addresses in the text

Widgets are GObject subclasses; the window has a composite template from the Blueprint
file. Actions use the usual prefixes: `app.`, `win.`, and `term.` on the terminal for its
context menu.

The terminal state is alacritty_terminal's `Term`, behind a lock that its reader thread
and the main context share. The thread parses what the program writes and wakes the main
context through a channel; the next frame takes the lock long enough to copy what is on
screen, and draws from that copy. A search runs on a thread of its own, holding the
lock's lease so no output moves the lines under it, and the lock itself a few thousand
lines at a time; the main context takes the lock without the lease, so it never waits
for more than one of those steps.

## Drawing

A frame is one instanced draw of quads: backgrounds, the selection and cursor, glyphs
from the atlas, and lines for underlines and strikeout. It goes into a texture the widget
owns, as large as the widget in the screen's pixels, and GTK composites that texture one
texel to a pixel, placed on the pixel grid. The widget makes its own `GdkGLContext` rather
than being a `GtkGLArea`, whose buffer is sized by the integer scale and would be
resampled on a fractional one.

Glyphs are rasterised at the screen's scale, with the desktop's hinting, and packed into
an atlas texture that grows when it fills. Box drawing and block characters are drawn
by `box_drawing.rs` to the cell's exact size, so lines join across cells.

## Running

    just run [-e COMMAND…]   # debug build with the schema compiled into target/schemas
    just check               # fmt, clippy -D warnings, blueprint, validators, catalogues
    just test                # the unit tests
    cargo deny check         # advisories, licences and sources of the dependencies

A second `just run` opens a window in the first; `GSETTINGS_BACKEND=memory` keeps
experiments out of your settings.

## A few things to know before changing them

User-visible strings go through `gettext` with `%s`-style placeholders and
`str::replace`; there is no printf from Rust. Counts use `ngettext` even where English
would not need it, because the plural rules of other languages do. `just pot`
regenerates the template from the Rust sources, the Blueprint files, the desktop entry,
the metainfo and the schema, and `just po` merges it into every `po/<lang>.po`. A new
language is a new line in `po/LINGUAS` plus the `.po` file. `install` compiles the
catalogues and merges the desktop and metainfo translations with `msgfmt`.

Shortcuts of the window take Ctrl and Shift; Ctrl with a single key belongs to the program
in the terminal. Application accelerators are handled before the terminal sees the key,
and only for actions the window can reach.
