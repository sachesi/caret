# Settings

Everything is in the GSettings schema `io.github.sachesi.caret`. Preferences (Ctrl+,)
exposes all but the window's size and state, which the window writes when it closes:

    gsettings set io.github.sachesi.caret font 'JetBrains Mono 12'

Changes apply immediately to open windows.

| Key | Values | Default | In Preferences as |
|---|---|---|---|
| `font` | Pango font description, or empty | empty | Use the System Font, Font |
| `font-features` | OpenType features as in CSS, or empty | empty | Font Features |
| `ligatures` | bool | true | Ligatures |
| `color-scheme` | `system`, `light`, `dark` | `system` | Style |
| `cursor-shape` | `block`, `beam`, `underline` | `block` | Cursor Shape |
| `custom-command` | command line, or empty | empty | Custom Command, Instead of the Shell |
| `scrollback-lines` | 0 to 1000000 | 10000 | Lines of History |
| `scroll-on-output` | bool | false | Scroll on Output |
| `scroll-on-keystroke` | bool | true | Scroll on Keystroke |
| `program-notifications` | bool | true | Notifications |
| `program-clipboard` | bool | true | Copying |
| `audible-bell` | bool | true | Terminal Bell |
| `title-bar` | bool | true | Title Bar |
| `window-size` | (width, height) | (0, 0) | |
| `window-maximized` | bool | false | |

An empty `font` follows the desktop's monospace font, as libadwaita reads it from the
settings portal (`monospace-font-name` in `org.gnome.desktop.interface`). A font that is
not monospaced is replaced by fontconfig's `Monospace` at the same size, since a
proportional font on the grid spreads its letters apart.

`font-features` turns features of the font on and off, written as in CSS: `zero, ss01`
for a slashed zero and the first stylistic set, `calt=0` to turn one off. Which features
there are depends on the font. With `ligatures`, runs of up to twelve symbols that the
font draws joined, such as `->` and `!=`, are drawn that way; letters are never joined,
and neither is the symbol under a block cursor.

A `window-size` of 0 fits 80
columns by 24 lines of the font.

Programs can change the cursor shape and ask for a blinking cursor; the setting is the
shape they start with. Blinking follows GTK's `gtk-cursor-blink`.
