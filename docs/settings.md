# Settings

Everything is in the GSettings schema `io.github.sachesi.tangent`. Preferences (Ctrl+,)
exposes all but the window's size and state, which the window writes when it closes:

    gsettings set io.github.sachesi.tangent font 'JetBrains Mono 12'

Changes apply immediately to open windows.

| Key | Values | Default | In Preferences as |
|---|---|---|---|
| `font` | Pango font description, or empty | empty | Use the System Font, Font |
| `color-scheme` | `system`, `light`, `dark` | `system` | Style |
| `cursor-shape` | `block`, `beam`, `underline` | `block` | Cursor Shape |
| `custom-command` | command line, or empty | empty | Custom Command, Instead of the Shell |
| `scrollback-lines` | 0 to 1000000 | 10000 | Lines of History |
| `scroll-on-output` | bool | false | Scroll on Output |
| `scroll-on-keystroke` | bool | true | Scroll on Keystroke |
| `audible-bell` | bool | true | Terminal Bell |
| `title-bar` | bool | true | Title Bar |
| `window-size` | (width, height) | (0, 0) | |
| `window-maximized` | bool | false | |

An empty `font` follows the desktop's monospace font, as libadwaita reads it from the
settings portal (`monospace-font-name` in `org.gnome.desktop.interface`). A font that is
not monospaced is replaced by fontconfig's `Monospace` at the same size, since a
proportional font on the grid spreads its letters apart. A `window-size` of 0 fits 80
columns by 24 lines of the font.

Programs can change the cursor shape and ask for a blinking cursor; the setting is the
shape they start with. Blinking follows GTK's `gtk-cursor-blink`.
