# Using Caret

## Starting it

    caret                              # a window with your shell, in the current folder
    caret --working-directory ~/src    # in another folder; -w for short
    caret -e htop                      # running a command instead of the shell
    caret -- ssh host                  # the same, the way GOption spells it

Everything after `-e` or `-x` is the command and its arguments, as launchers such as
xdg-terminal-exec expect. The shell is `$SHELL`, or the one in your passwd entry; Custom Command in Preferences
runs something else in its place. Each
start opens a window in the Caret that is already running, and its program gets the
environment of that start rather than the running Caret's. Variables that belong to
another terminal, or to tmux or screen around the command line (`TMUX`, `VTE_VERSION`,
`KITTY_WINDOW_ID` and the like), are left out.

A tab closes when its program exits, and the window goes with its last tab. A command
named on the command line that fails stays on screen instead, with its output and how it
ended under it, until you close the tab.

A new tab or window starts in the folder the current tab's shell is in. A shell that
reports its folder (OSC 7, which fish does by itself and bash and zsh do with
`/etc/profile.d/vte.sh` or a line in their prompt) is taken at its word, except while a command it marked is running, since that output
may come from a file or another machine; otherwise it is
read from `/proc`, which is right unless the shell runs in a container or on another
machine. A new tab from a shell inside a toolbox or distrobox container, which says so
when it enters one, enters the same container.

## Tabs

The tab bar shows once there are two tabs. A tab takes its title from the program, which
sets it with the usual escape sequence; until it does, the tab is named after the
program. Rename Tab in the tab's context menu gives it a name of your own, which stays
until you clear it. A tab whose program rings the bell while the tab is in the background is marked
until you look at it, and the bell sounds unless Terminal Bell is off in Preferences.

Ctrl+Shift+H, or Title Bar in Preferences, hides the title bar, and so does full screen
(F11, or Full Screen in the main menu). The context menu then
has what the title bar had: a new window, all tabs, the title bar again, Preferences,
Keyboard Shortcuts, About, Full Screen and closing the window. Super and a drag moves the window.

A shell that marks its prompts and commands (OSC 133, which fish does by itself) tells
Caret when a command starts and ends. A tab whose command is still running after a
second shows a spinner, and one whose command ends while you look elsewhere is marked;
if the command took ten seconds or more and the window is not the one you are using, a
notification says it finished, and clicking it brings the tab back. Programs can also
send notifications themselves (OSC 9 and OSC 777), which show when their tab is out of
sight, and put text on the clipboard (OSC 52), though never read it; both can be turned
off under Programs in Preferences.

Closing a tab, or a window, where something other than the shell is running in the
foreground asks first.

## Selecting, copying and pasting

Dragging selects, a double click selects a word and a triple click a line, and Shift and a
click extends the selection. What is selected is also the primary selection, which a
middle click pastes. Ctrl+Shift+C and Ctrl+Shift+V copy and paste with the clipboard,
and the context menu has both. Ctrl+Shift+A selects all the text, the history too, and
Ctrl+Shift+D deselects. A program that asks for mouse events gets them; hold Shift
to select anyway.

Pasting into a program that asks for bracketed paste marks the text as pasted, so a
shell does not run the lines as they arrive. Pasting several lines into one that does not asks first,
since each line would run as it arrives. Files dropped on the terminal are typed as
their paths, quoted for the shell.

## Links

Ctrl and a click opens a web, file, FTP, SSH, Gemini or mail address: one a program
marked as a link, or one in the text, found wherever it starts and even when it wraps
onto the next line, or when a program broke it into lines that each fill the width. The
address under the pointer is underlined. The pointer turns into a hand over one while
Ctrl is held, and the address shows as a tooltip. A link a program made shows where it goes on hover without
Ctrl, since its text may say something else. The context menu of an address has Open
Link and Copy Link Address; a link that cannot be opened says why at the bottom of the
window.

## Scrolling and finding

The wheel and Shift+Page Up and Page Down scroll through the history, and Ctrl and the
wheel zooms. In a program that has the whole screen, a pager or an editor, the wheel sends
arrow keys instead, unless the program asks for the mouse itself.

Ctrl+Shift+Up and Ctrl+Shift+Down scroll to the previous and next prompt, in a shell that
marks them.

Ctrl+Shift+F finds text in the history, upwards from the bottom. The search takes the text
literally, and ignores case unless the text has capitals in it.
