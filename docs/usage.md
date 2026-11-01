# Using Caret

## Starting it

    caret                              # a window with your shell, in the current folder
    caret --working-directory ~/src    # in another folder; -w for short
    caret -e htop                      # running a command instead of the shell
    caret -- ssh host                  # the same
    caret --profile work               # with the settings of a profile
    caret --preset dev                 # its first tab laid out by a split preset

Everything after `-e` or `-x` is the command and its arguments, as launchers such as
xdg-terminal-exec expect. The shell is `$SHELL`, or the one in your passwd entry;
`custom-command` in the [settings](settings.md) runs something else in its place. Each
start opens a window of its own, and its program gets the environment of that start
rather than the terminal server's. Variables that belong to
another terminal, or to tmux or screen around the command line (`TMUX`, `VTE_VERSION`,
`KITTY_WINDOW_ID` and the like), are left out.

Launchers that go through xdg-terminal-exec open Caret once
`io.github.sachesi.caret.desktop` is the first line of `~/.config/xdg-terminals.list`.

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

## Completions

    caret completions --install

puts the completion script for your shell (bash, zsh, fish, elvish or nushell, taken from
`$SHELL` unless named) under your home, and says what else the shell needs to find it, as
zsh and others do. `caret completions SHELL` prints it instead, and `just install` puts
the bash, zsh and fish ones in place for everyone. Completions ask Caret as you type, so
sessions, profiles and presets complete by their names; nushell's cover only the commands
and options.

## Tabs

The tab bar shows once there are two tabs. A tab takes its title from the program, which
sets it with the usual escape sequence; until it does, the tab is named after the
program. Rename Tab in the tab's context menu gives it a name of your own, which stays
until you clear it. The same menu moves the tab to another session, or to a new one in the
window; in a session's list of tabs it opens on a right click on the tab's row. A tab whose program rings the bell while the tab is in the background is marked
until you look at it, and the bell sounds unless `audible-bell` is off. Tabs can be
dragged along the strip to reorder them. When more tabs are open than fit with room for
their titles, the strip shows them a stripful at a time, the one with the current tab, and a
bar at either end says there are more that way.

Where Caret draws the title bar itself, its window buttons are the ones the desktop's
`button-layout` in `org.gnome.desktop.wm.preferences` names, on the sides it puts them; with
no such setting there is a close button at the right. They show the `window-close-symbolic`
icons and the like of the desktop's icon theme, in the theme's own shapes, and plain ones
where the theme has none.

Ctrl+Shift+H, or `title-bar` in the settings, hides the title bar, and so does full
screen (F11, or Full Screen in the main menu). The context menu then has what the title
bar had: a new window, the title bar again, full screen, closing the window and the main
menu.

A program can ask to be told when Caret's colours go from dark to light or back (mode 2031),
so that it follows the theme while it runs. The text client passes on the same news from the
terminal it runs in, where that terminal gives it.

A shell that marks its prompts and commands (OSC 133, which fish does by itself) tells
Caret when a command starts and ends. A tab whose command is still running after a
second shows a ring before its title, and one whose command ends while you look
elsewhere is marked;
if the command took ten seconds or more and the window is not the one you are using, a
notification says it finished, and clicking it brings the tab back. Programs can also
send notifications themselves (OSC 9 and OSC 777), which show when their tab is out of
sight, and put text on the clipboard (OSC 52), though never read it;
`program-notifications` and `program-clipboard` turn them off.

Closing a tab, or a window, where something other than the shell is running in the
foreground asks first.

## Panes

Ctrl+Shift+E splits the pane with a new one to its right, Ctrl+Shift+O with one below,
and the terminal's menu has both. Alt and the arrows go to the pane that way, a click
goes to the one clicked, and dragging the gap between two panes moves it. Dragging from
where two gaps meet moves both at once. The lines along the pane the keys go to are drawn
in the session's colour, or in strong contrast with `active-pane-color` off. Ctrl+Shift+W closes the pane, and the tab with its last one.

Presets in the terminal's menu lays out a tab by a layout from the settings, with its
programs and directories, as described under [split presets](settings.md#split-presets).
Under it, Save Layout as Preset keeps the layout of the tab shown.

Control mode (Ctrl+Shift+Space) arranges panes from the keys: it rings the pane it acts
on and names its keys at the bottom of the window, as listed in
[keyboard shortcuts](keyboard-shortcuts.md). Escape or Enter leaves it.

## Sessions

The programs in the tabs run in Caret's terminal server, which keeps them while no
window shows them, until you log out unless you
[set it up to go on](installing.md#install). A window's tabs are a session. Detach Window
in the main menu closes the window and leaves its session running, with its history;
Open All Detached in the Sessions list, or
`caret --attach`, opens a window for each detached one, on the tab it showed and with its
profile. A window that crashes or is
killed is detached the same way. With `close-detaches`, closing a window detaches it
too; otherwise closing it ends its programs, as closing a tab does. A window with a program
running, with several sessions or with a named one asks first, and offers to detach instead.

Sessions… in the main menu (Ctrl+Shift+S) lists them by where they are: in this window, in
others, or detached, with New Session last. A click or Enter adds a session to the window
and shows it; `session-list-click = "replace"` has it take the place of the shown session,
which keeps running. Right-clicking a row, Right arrow, the Menu key or a long press offers
the rest: open it in a new window, replace or add, rename, detach and close, and with
`middle-click-detach` a middle click detaches one this window shows. With several
detached, the list can open them all or close them all. A new window says when sessions are
detached, with a button to list them, unless `detached-hint` is off. Right-clicking the +
button offers a new session and the sessions this window does not show.
`caret session` prints the same list, and `caret session attach` with a session's number
or name shows it: in a Caret window when run in one, or else in the terminal it runs in,
drawn with escape sequences, where control mode's D detaches back to the shell. A bar under
the panes there names the session and its tabs and the key for control mode, whose own keys
it then lists, on more lines when they do not fit. On a
Linux console it uses the console's sixteen colours, so a colour between them shows as
the nearest one. With no name, it takes the only detached session, if there is just one.

The rest of what the list does has a command too, for scripts and for a shell over ssh:

    caret session new --name build -- make     # no window; prints the session's number
    caret session rename build compile
    caret session detach compile               # every window showing it lets it go
    caret session close compile

`new` starts its first tab at 80 by 24 until something shows it. A program it cannot start
says so in that tab, and the session stays. `close` refuses while a program runs, and says
which, unless given `--force`; a shell waiting at its prompt does not count, but the shell
running this command is not waiting. Without a session, `rename`, `detach` and `close` act
on the one they run in.

`-j` gives any of these answers in JSON, with `"ok": true` and what was asked for, or
`"ok": false` and an `"error"`:

    caret session -j | jq -r '.sessions[].name'

The exit status is 1 when a command fails, and 2 when its line does not say enough, such
as no session named outside Caret.

A window that shows several sessions has one tab for each in the strip, with its name
or number, the title of the tab it shows and how many tabs it has. A click on another
session's tab shows the tab used last in it. A right click on a session's tab, or a click
on its arrow, lists its tabs, with New Tab, Rename, Detach and Close Session;
`session-click = "popup"` makes every click list them. Ctrl+Shift+L lists the shown
session's tabs from the keys, and typing narrows the list. Ctrl+Page Up and Page Down
and Alt and a number act on the shown session's tabs, and Ctrl+Shift+Page Up and Page
Down and Ctrl+Alt and a number on the sessions. Each session has a colour of its own on
its tab unless `session-colors` is off. The wheel over a session's tab goes through its
tabs.

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

On a touchscreen, a tap clicks, dragging a finger over the terminal scrolls it and a long
press opens the terminal's menu. On the header bar, the tabs and the gaps between panes a
finger does what the mouse's left button does.

Ctrl and a click opens a web, file, FTP, SSH, Gemini or mail address: one a program
marked as a link, or one in the text, found wherever it starts and even when it wraps
onto the next line, or when a program broke it into lines that each fill the width. The
address under the pointer is underlined. The pointer turns into a hand over one while
Ctrl is held, and the address shows at the bottom of the window. A link a program made
shows where it goes on hover without
Ctrl, since its text may say something else. The context menu of an address has Open
Link and Copy Link Address; a link that cannot be opened says why at the bottom of the
window.

## Scrolling and finding

The wheel and Shift+Page Up and Page Down scroll through the history, and Ctrl and the
wheel zooms. In a program that has the whole screen, a pager or an editor, the wheel sends
arrow keys instead, unless the program asks for the mouse itself. The wheel acts on the
pane under the pointer and leaves the keys where they are; with `scroll-under-pointer` off
it acts on the focused pane.

Ctrl+Shift+Up and Ctrl+Shift+Down scroll to the previous and next prompt, in a shell that
marks them.

Ctrl+Shift+F finds text in the history, upwards from the bottom. The search takes the text
literally, and ignores case unless the text has capitals in it.
