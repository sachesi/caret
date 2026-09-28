# Using Tangent

## Starting it

    tangent                              # a window with your shell, in the current folder
    tangent --working-directory ~/src    # in another folder; -w for short
    tangent -e htop                      # running a command instead of the shell
    tangent -- ssh host                  # the same, the way GOption spells it

Everything after `-e` or `-x` is the command and its arguments, as launchers such as
xdg-terminal-exec expect. The shell is `$SHELL`, or the one in your passwd entry. Each
start opens a window in the Tangent that is already running, and its program gets the
environment of that start rather than the running Tangent's. Variables that belong to
another terminal, or to tmux or screen around the command line (`TMUX`, `VTE_VERSION`,
`KITTY_WINDOW_ID` and the like), are left out.

A tab closes when its program exits, and the window goes with its last tab. A command
named on the command line that fails stays on screen instead, with its output and how it
ended under it, until you close the tab.

A new tab or window starts in the folder the current tab's program is in, read from
`/proc`, so a shell that has changed directory passes it on without needing to tell the
terminal.

## Tabs

The tab bar shows once there are two tabs. A tab takes its title from the program, which
sets it with the usual escape sequence; until it does, the tab is named after the
program. A tab whose program rings the bell while the tab is in the background is marked
until you look at it, and the bell sounds unless Terminal Bell is off in Preferences.

Ctrl+Shift+H, or Title Bar in Preferences, hides the title bar, and so does full screen
(F11, or Full Screen in the main menu). The context menu then
has what the title bar had: a new window, all tabs, the title bar again, Preferences,
Keyboard Shortcuts, About, Full Screen and closing the window. Super and a drag moves the window.

Closing a tab, or a window, where something other than the shell is running in the
foreground asks first.

## Selecting, copying and pasting

Dragging selects, a double click selects a word and a triple click a line, and Shift and a
click extends the selection. What is selected is also the primary selection, which a
middle click pastes. Ctrl+Shift+C and Ctrl+Shift+V copy and paste with the clipboard,
and the context menu has both. A program that asks for mouse events gets them; hold Shift
to select anyway.

Pasting into a program that asks for bracketed paste marks the text as pasted, so a
shell does not run the lines as they arrive. Files dropped on the terminal are typed as
their paths, quoted for the shell.

## Links

Ctrl and a click opens an address: one a program marked as a link, or a web, file, FTP,
SSH, Gemini or mail address in the text, found wherever it starts and even when it wraps
onto the next line. The pointer turns into a hand over one while Ctrl is held, and the
address shows as a tooltip. A link a program made shows where it goes on hover without
Ctrl, since its text may say something else. The context menu of an address has Open
Link and Copy Link Address; a link that cannot be opened says why at the bottom of the
window.

## Scrolling and finding

The wheel and Shift+Page Up and Page Down scroll through the history, and Ctrl and the
wheel zooms. In a program that has the whole screen, a pager or an editor, the wheel sends
arrow keys instead, unless the program asks for the mouse itself.

Ctrl+Shift+F finds text in the history, upwards from the bottom. The search takes the text
literally, and ignores case unless the text has capitals in it.
