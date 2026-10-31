# Settings

Settings are in `~/.config/caret/config.toml` (under `$XDG_CONFIG_HOME` where that is
set). Preferences in the main menu, or Ctrl+,, opens the file with the program the
desktop has for it; where there is none yet, it writes one first with every setting at
its default and commented out. Open windows follow the file as it is saved; a value that
cannot be read is skipped with a warning on stderr, and the rest still apply.

    font = "Source Code Pro 12"
    color-scheme = "dark"

| Key | Values | Default |
|---|---|---|
| `font` | family and size, or empty | empty |
| `font-features` | OpenType features as in CSS, or empty | empty |
| `ligatures` | bool | true |
| `color-scheme` | `"system"`, `"light"`, `"dark"` | `"system"` |
| `light-scheme`, `dark-scheme` | `"caret"`, or the name of a scheme file | `"caret"` |
| `opacity` | 0 to 1 | 1 |
| `cursor-shape` | `"block"`, `"beam"`, `"underline"` | `"block"` |
| `scrollback-lines` | a number of lines | 10000 |
| `custom-command` | command line, or empty | empty |
| `scroll-on-output` | bool | false |
| `scroll-on-keystroke` | bool | true |
| `program-notifications` | bool | true |
| `program-clipboard` | bool | true |
| `audible-bell` | bool | true |
| `close-detaches` | bool | false |
| `title-bar` | bool | true |
| `server-side-decorations` | bool | true |
| `session-click` | `"switch"`, `"popup"` | `"switch"` |
| `session-list-click` | `"add"`, `"replace"` | `"add"` |
| `session-colors` | bool | true |
| `rounded-pane-ring` | bool | true |
| `detached-hint` | bool | true |
| `middle-click-detach` | bool | false |
| `tab-previews` | bool | false |

An empty `font` follows the desktop's monospace font, read from the settings portal
(`monospace-font-name` in `org.gnome.desktop.interface`). A family that is not installed
gives way to the first of Adwaita Mono, Source Code Pro, DejaVu Sans Mono, Noto Sans
Mono, Liberation Mono and Cascadia Mono that is, or else to any monospaced font. Caret
finds fonts in the usual font folders itself, so fontconfig's aliases and rules do not
apply to it.

`font-features` turns features of the font on and off, written as in CSS: `zero, ss01`
for a slashed zero and the first stylistic set, `calt=0` to turn one off. Which features
there are depends on the font. With `ligatures`, runs of up to twelve symbols that the
font draws joined, such as `->` and `!=`, are drawn that way; letters are never joined,
and neither is the symbol under a block cursor.

`color-scheme` follows the desktop's light or dark style with `"system"`. Each style
takes its colours from its scheme: `"caret"` is Caret's own, and another name is a file
in `~/.config/caret/schemes`, `"mine"` for `schemes/mine.toml`:

    foreground = "#d8d8d8"
    background = "#181818"
    palette = ["#181818", "#ab4642", "#a1b56c", "#f7ca88", "#7cafc2", "#ba8baf",
               "#86c1b9", "#d8d8d8", "#585858", "#ab4642", "#a1b56c", "#f7ca88",
               "#7cafc2", "#ba8baf", "#86c1b9", "#f8f8f8"]
    selection = "#383838"     # optional

Programs can change the cursor's shape and ask for it to blink or not; `cursor-shape` is
the shape they start with. A cursor that blinks does so for ten seconds after the last key,
unless the desktop has cursors not blink (`cursor-blink` in `org.gnome.desktop.interface`).

`custom-command` runs in new tabs in place of the shell. `close-detaches` makes closing a
window leave its programs running, to attach to again with `caret --attach`.
`server-side-decorations` off has Caret draw its own frame and header bar even where the
compositor offers to draw them. The `session-` settings, `middle-click-detach` and
`tab-previews` are about windows that show several sessions, described under
[sessions](usage.md#sessions). `rounded-pane-ring` rounds the corners of the ring control
mode draws around the focused pane.

## Profiles

A profile is a table of settings of its own, used by the windows opened with it, from
the main menu or with `caret --profile NAME`:

    [profiles.work]
    font = "Adwaita Mono 13"
    custom-command = "ssh work"

## Split presets

A split preset is a layout of panes under a name, offered by Presets in the terminal's
menu and the main menu, and by `caret --preset NAME` for a new window:

    [split-presets.dev]
    layout = "editor 60% | (logs / shell)"
    shortcut = "Ctrl+Alt+P"

    [split-presets.dev.panes.editor]
    command = "nvim"
    directory = "~/src/app"

    [split-presets.dev.panes.logs]
    command = "journalctl -f"

    [split-presets.dev.panes.shell]
    keep = true

`layout` draws the panes by name: `|` puts them side by side and `/` one under another,
with parentheses around a group where the two meet. A percent after a pane or group is
its part of its row; those without one share what is left.

A pane's table is optional. `command` is what it runs, the shell or `custom-command`
without one, and `directory` is where, starting with `~` for the home directory or
relative to the current pane; without one it starts where the current pane is.

A tab with one pane takes the layout, and its running program stays, moved to the pane
with `keep = true`, or else to the first without a command. A kept pane cannot have a
command. A tab with several panes is left as it is and the layout opens in a new tab,
where the kept pane starts a shell in its `directory`.

A preset that cannot be used is left out of the menu, with a warning in the log saying
why. Save Layout as Preset, under Presets in a tab with several panes, writes the tab's
layout here, with each pane's directory and the command line running in it.

## Keys

The `[keys]` table binds an action to other keys: one key, a list of them, or `[]` for
none. A key is written as modifiers and a key name joined by `+`, the names as xkbcommon
has them:

    [keys]
    copy = ["Ctrl+Insert", "Ctrl+Shift+C"]
    close-tab = "Ctrl+Shift+W"
    close-pane = []

The file Preferences writes lists every action with its keys. The actions are in
[keyboard shortcuts](keyboard-shortcuts.md).
