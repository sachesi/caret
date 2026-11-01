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

    crates/window             caret, the window
      main.rs, cli.rs, cli/   start-up, and the command line with `caret session` and
                              `caret completions`
      app.rs                  a window: Wayland events, keys, and the frames it hands to
                              the render thread
      app/                    its parts: tabs and their panes, control mode, sessions,
                              menus, dialogs, find, the pointer, the clipboard, input
                              methods, bells and notifications, screen readers
      chrome.rs, chrome/      what the window draws around the terminal, as quads: the
                              header bar, the tab strip, the find bar, menus, dialogs,
                              toasts and the list of shortcuts
      gpu.rs                  the Vulkan device and the swapchain
      render.rs               the render thread
      renderer.rs             the pipelines, the atlas texture, the draws of cells and quads
      shaders/                GLSL, compiled to SPIR-V by build.rs with glslc
      grid.rs                 the screen as the server sent it, as cells for the GPU
      atlas.rs, fonts.rs      fonts found, shaped and rasterised, and where glyphs sit in
                              the atlas
      box_drawing.rs          box drawing and block characters drawn to fill their cells
      color_glyph.rs          colour glyphs of the COLRv1 table, painted on the CPU
      panes.rs                where a tab's panes go, and the gaps between them
      config.rs               config.toml, profiles and keys
      shortcuts.rs, keys.rs   actions and their keys; keys as the terminal encodes them
      desktop.rs              the style and the font from the settings portal
      access.rs               what screen readers are told
      text.rs                 a session drawn in another terminal, for `caret session attach`
                              outside a window
    crates/protocol           what clients and the server send each other, and how it
                              is written on the socket; in encode.rs, keys, mouse
                              events and pastes as bytes for the program
    crates/server             caret-server: terminals, their screens and history, the
                              sessions windows attach to, links and search
    crates/term               caret-term, the terminal emulator

A window is one event loop (calloop) on the main thread, which takes the Wayland events
and the server's messages. Its frames are drawn on a render thread with a Wayland event
queue of its own, so a slow frame never holds up the keys. The settings portal and
notifications talk D-Bus through zbus on threads of their own, and screen readers through
AccessKit.

Terminals live in `caret-server`, so that a window can detach and attach again later,
or crash, without its programs ending. The server keeps sessions, each a list of tabs a
window shows; one window attaches to one session. It parses what each program writes
with Caret's own emulator, the `caret-term` crate, and sends a client the rows that
changed when the client asks for a frame, already resolved to colours and glyphs, so a
client never sees the terminal state itself. A client sends input, sizes, scrolling and selection as requests, and asks
questions (the selected text, the link under a point, a search) whose answers come back
in order with everything else on the connection.

The server runs one loop that owns every session and tab; each client has a reader and a
writer thread. Each terminal has a thread of its own that reads and writes its pty, keeps
the emulator, and keeps every client's view of it: where the client scrolled to, what it
selected, the link under its pointer, a search going on. That thread sends clients their
frames and answers their questions about what they see, so one client can read back
through the history while another follows the output; the loop hears from it only of
the title, bells, the clipboard, what the shell reports, and the program's end. A search
goes a few thousand lines at a time between reads of the program's output. The server
ends once it keeps nothing and no client is connected. systemd starts it for its socket where its
units are installed; otherwise the first window starts it.

The protocol's version is exchanged first on each connection, and the server speaks
every version from its oldest to its newest. A tag of a message or field is never
reused.

## Drawing

Each frame is drawn with Vulkan 1.3 into the window's swapchain, at the screen's scale:
at a fractional scale the buffer is as large as the window in the screen's pixels and the
viewporter maps it onto the window, so a texel lands on each pixel. The cells of each pane
shown are kept on the GPU as a buffer of rows, of which a frame copies only those that
changed; shaders turn them into backgrounds, glyphs from the atlas, the cursor, the
selection and lines for underlines and strikeout. The frame, the bars, menus and dialogs
are instanced quads over them: rounded rectangles, shadows, outlines and glyphs. An open
menu is an xdg_popup, so the compositor can keep it on the screen; it changes seldom, and
its quads are painted on the CPU into shared memory rather than given a swapchain.

Fonts are found by scanning the font folders (fontdb), runs are shaped with harfrust,
and glyphs are rasterised by swash at the screen's scale into an atlas texture that grows
when it fills. Box drawing and block characters are drawn by `box_drawing.rs` to the
cell's exact size, so lines join across cells. Colour emoji in the COLRv1 table, which
swash does not draw, are painted by `color_glyph.rs` from skrifa's description of them.

## Running

    just run [-e COMMAND…]   # debug build, as io.github.sachesi.caret.Devel
    just check               # fmt, clippy -D warnings, validators, catalogues
    just test                # the tests, which run the server and `caret session` too
    just deny                # advisories, licences and sources of the dependencies
    just bench               # how fast the release build is, in a headless session

Debug builds are a separate application, `io.github.sachesi.caret.Devel`, with a terminal
server of their own, so they run beside an installed Caret. They read the same
`config.toml`, unless `XDG_CONFIG_HOME` points elsewhere. A second `just run` opens a
window of its own on the first one's server. Warnings go to
stderr; `RUST_LOG=debug` shows more.

`just bench` needs sway and wdotool. It runs the release build in a headless session with
settings of its own and prints, as the median of three runs, how fast output goes
through, how long frames take, how soon a typed key is echoed on screen, how often an
idle window wakes up and how much memory it holds. The numbers and the raw timings stay
in `target/bench`. Any build writes the same timings to the file `CARET_TIMINGS` names.

## A few things to know before changing them

Output passes through about seven threads on its way to the screen, and on a machine with
every CPU taken each waits its turn. So the window and the server ask the scheduler for short
turns when they start, which their threads inherit and which takes no privilege or more than a
fair share of the CPU. Programs must not inherit them, or a build would take the advantage
back: anything Caret starts other than itself goes through `start_with_usual_turns`, or
`ask_for_usual_turns` where it already has code between fork and exec.

User-visible strings go through `gettext`, called by that name so xgettext finds it, and
not inside a macro, whose body xgettext skips. Values go in through named placeholders
with `gettext_f` and `ngettext_f` from `crates/window/src/i18n.rs`, `{program}` rather
than `%s`, so a translation can put them in its own order. Counts use `ngettext` even
where English would not need it, because the plural rules of other languages do. `just
pot` regenerates the template from the window's sources, the desktop entry and the
metainfo, and `just po` merges it into every `po/<lang>.po`. A new language is a new
line in `po/LINGUAS` plus the `.po` file. `install` compiles the catalogues and merges the
desktop and metainfo translations with `msgfmt`.

The server goes by `caret`, in its process name and the first word of its command line, as
programs in a terminal have it for a parent and system information tools take the parent's
name for the terminal's. So `pgrep -f 'caret --socket'` finds a server a window or systemd
started, and neither `pgrep -x` nor `-f` with `caret-server` does.

Shortcuts of the window take Ctrl and Shift; Ctrl with a single key belongs to the program
in the terminal. The window matches its shortcuts before the terminal sees the key, and an
action that has nothing to do at the time, such as going to a pane where there is none,
lets the key through to the program.
