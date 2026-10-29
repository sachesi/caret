# Contributing

Bugs and ideas go to the [issue tracker](https://github.com/sachesi/caret/issues);
security problems do not, see [SECURITY.md](SECURITY.md).

Before a change goes in:

- `just check` and `just test` pass. CI runs both on Fedora 44, with `cargo deny check` and
  zizmor over the workflows, for every push and pull request.
- Commit messages follow [Conventional Commits](https://www.conventionalcommits.org/):
  `fix:`, `feat:`, `perf:`, `docs:` and so on, with a subject that says what changed for
  someone using Caret.
- Every string the user sees goes through `gettext`. `just po` updates the catalogues in
  `po/`, and a change that adds strings brings their translations along where it can.
- Behaviour described in `docs/` changes with the code that implements it.

## Where things are

    build.rs                  runs blueprint-compiler and bundles the GResource, with the
                              metainfo the About dialog reads
    data/ui/*.blp             the window and the shortcuts dialog
    src/main.rs               early setup: translations, resources, GTK's renderer
    src/application.rs        GtkApplication subclass, command line, app actions, accels
    src/window.rs             the window: tabs, find bar, win.* actions, closing
    src/preferences.rs        the preferences dialog
    src/settings.rs           the GSettings object, the font and the colour scheme
    src/terminal_view.rs      one terminal: the widget, its GL context, sizing, scrolling
                              and clipboard; in terminal_view/ its input, drawing, find
                              and accessible text
    src/connection.rs         the connection to the terminal server, started if none
                              listens, and what it sends routed to windows and tabs
    src/frame.rs              the screen as the server sent it, turned into quads
    src/renderer.rs           OpenGL: shaders, the glyph atlas, the textures GTK composites
    src/fonts.rs              cell metrics and glyphs rasterised by Pango and cairo
    src/box_drawing.rs        box drawing and block characters drawn to fill their cells
    src/palette.rs            the default colours, sent to the server
    crates/protocol           what clients and the server send each other, and how it
                              is written on the socket; in encode.rs, keys, mouse
                              events and pastes as bytes for the program
    crates/server             caret-server: terminals, their screens and history, the
                              sessions windows attach to, links and search

Widgets are GObject subclasses; the window has a composite template from the Blueprint
file. Actions use the usual prefixes: `app.`, `win.`, and `term.` on the terminal for its
context menu.

Terminals live in `caret-server`, so that a window can detach and attach again later,
or crash, without its programs ending. The server keeps sessions, each a list of tabs a
window shows; one window attaches to one session. It parses what each program writes
with alacritty_terminal and sends a client the rows that changed when the client asks for
a frame, already resolved to colours and glyphs, so a client never sees the terminal
state itself. A client sends input, sizes, scrolling and selection as requests, and asks
questions (the selected text, the link under a point, a search) whose answers come back
in order with everything else on the connection.

The server runs one loop that owns every session and terminal; each client has a reader
and a writer thread, and each terminal alacritty's thread that reads its pty. A search
runs on a thread of its own, holding the terminal lock's lease so no output moves the
lines under it, and the lock itself a few thousand lines at a time. The server ends once
it keeps nothing and no client is connected. systemd starts it for its socket where its
units are installed; otherwise the first window starts it.

The protocol's version is exchanged first on each connection, and the server speaks
every version from its oldest to its newest. A tag of a message or field is never
reused.

## Drawing

A frame is one instanced draw of quads: backgrounds, the selection and cursor, glyphs
from the atlas, and lines for underlines and strikeout. It goes into a texture the widget
owns, as large as the widget in the screen's pixels, and GTK composites that texture one
texel to a pixel, placed on the pixel grid. The widget makes its own `GdkGLContext` rather
than being a `GtkGLArea`, whose buffer is sized by the integer scale and would be
resampled on a fractional one. Caret asks GTK for its GL renderer, which takes that
texture as it is, unless `GSK_RENDERER` already names one: the Vulkan renderer has to
import it as a dmabuf, and where the driver refuses Mesa's implicit modifier it copies
every frame through the CPU instead.

Glyphs are rasterised at the screen's scale, with the desktop's hinting, and packed into
an atlas texture that grows when it fills. A frame rasterises new glyphs itself for a few
milliseconds and hands the rest to a thread with its own copy of the fonts; those cells
fill in over the next frames. Box drawing and block characters are drawn
by `box_drawing.rs` to the cell's exact size, so lines join across cells.

## Running

    just run [-e COMMAND…]   # debug build, as io.github.sachesi.caret.Devel
    just check               # fmt, clippy -D warnings, blueprint, validators, catalogues
    just test                # the unit tests
    just deny                # advisories, licences and sources of the dependencies
    just bench               # how fast the release build is, in a headless session

Debug builds are a separate application, `io.github.sachesi.caret.Devel`, with stripes in
the header bar and settings of their own under `/io/github/sachesi/caret/Devel/`, so they
run beside an installed Caret. A second `just run` opens a window in the first;
`GSETTINGS_BACKEND=memory` keeps experiments out of the development settings. Warnings go to stderr; `RUST_LOG=debug` shows more.

`just bench` needs sway and wdotool. It runs the release build in a headless session with
settings of its own and prints, as the median of three runs, how fast output goes
through, how long frames take, how soon a typed key is echoed on screen, how often an
idle window wakes up and how much memory it holds. The numbers and the raw timings stay
in `target/bench`. Any build writes the same timings to the file `CARET_TIMINGS` names.

## A few things to know before changing them

User-visible strings go through `gettext`, called by that name so xgettext finds it, and
not inside a macro such as `glib::clone!`, whose body xgettext skips. Values go in through
named placeholders with `gettext_f` and `ngettext_f` from `src/i18n.rs`, `{program}`
rather than `%s`, so a translation can put them in its own order. Counts use `ngettext`
even where English would not need it, because the plural rules of other languages do. `just pot`
regenerates the template from the Rust sources, the Blueprint files, the desktop entry,
the metainfo and the schema, and `just po` merges it into every `po/<lang>.po`. A new
language is a new line in `po/LINGUAS` plus the `.po` file. `install` compiles the
catalogues and merges the desktop and metainfo translations with `msgfmt`.

Shortcuts of the window take Ctrl and Shift; Ctrl with a single key belongs to the program
in the terminal. Application accelerators are handled before the terminal sees the key,
and only for actions the window can reach.
