# Keyboard shortcuts

Ctrl+? shows this list inside the application, with the keys as they are bound.

Shortcuts of the window take Ctrl and Shift together, so that Ctrl and a letter reach the
programs in the terminal. Each action can be bound to other keys in the `[keys]` table of
[the settings](settings.md#keys), by the name in the last column.

| Keys | | Name |
|---|---|---|
| Ctrl+Shift+N | New window | `new-window` |
| Ctrl+Shift+Q | Close the window | `close-window` |
| Ctrl+Shift+T | New tab | `new-tab` |
| Ctrl+Shift+Alt+T | New session in this window | `new-session` |
| | Close the tab | `close-tab` |
| Ctrl+Page Down, Ctrl+Tab | Next tab | `next-tab` |
| Ctrl+Page Up, Ctrl+Shift+Tab | Previous tab | `previous-tab` |
| Alt+1 … Alt+9 | Go to that tab | `tab-1` … `tab-9` |
| F11 | Full screen | `fullscreen` |
| Ctrl+Shift+H | Show or hide the title bar | `title-bar` |
| Ctrl+Shift+E | Split the pane, the new one to the right | `split-right` |
| Ctrl+Shift+O | Split the pane, the new one below | `split-down` |
| Ctrl+Shift+W | Close the pane, and the tab with its last pane | `close-pane` |
| Alt+Left, Right, Up, Down | Go to the pane that way | `focus-left` … `focus-down` |
| Ctrl+Shift+Space | Control mode, to arrange panes | `control-mode` |
| Ctrl+Shift+S | List the sessions | `sessions` |
| Ctrl+Shift+L | List the tabs of the session shown | `session-tabs` |
| Ctrl+Shift+Page Down, Ctrl+Shift+Page Up | Next, previous session | `next-session`, `previous-session` |
| Ctrl+Alt+1 … Ctrl+Alt+9 | Go to that session | `session-1` … `session-9` |
| Ctrl+Shift+C, Ctrl+Shift+V | Copy, paste | `copy`, `paste` |
| Ctrl+Shift+A, Ctrl+Shift+D | Select all text, including the history, and deselect | `select-all`, `deselect` |
| Ctrl+Shift+F | Find in the history | `find` |
| Shift+Page Up, Shift+Page Down | Scroll a page | `page-up`, `page-down` |
| Ctrl+Shift+Up, Ctrl+Shift+Down | Scroll to the previous, next prompt, where the shell marks them | `previous-prompt`, `next-prompt` |
| Ctrl++, Ctrl+-, Ctrl+0 | Zoom in, out, back to the font's size | `zoom-in`, `zoom-out`, `zoom-reset` |
| Menu | The terminal's menu, at the cursor | `terminal-menu` |
| Ctrl+, | Preferences | `preferences` |
| Ctrl+? | This list | `shortcuts` |

With a layout that does not type Latin letters, Ctrl and a key count as the Latin letter
the key types in the first layout that has one, so Ctrl+C interrupts and the shortcuts
work in any layout.

In control mode, a bar at the bottom names the keys. The arrows, or H, J, K and L, go to
the pane that way; with Shift they swap the pane with that one, and with Ctrl they move
the divider. V and S split the pane to the right and below, X closes it, Z has it take
the whole tab and back, T moves it to a tab of its own, and D detaches the session. Any
other key leaves the mode.

In the find bar, Enter finds the next match upwards and Shift+Enter downwards, and the
arrows beside the field go either way. Escape closes it.
