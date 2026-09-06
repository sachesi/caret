# Tangent

Tangent is a terminal written in Rust with GTK 4 and libadwaita. Programs' output is
parsed by [alacritty_terminal](https://crates.io/crates/alacritty_terminal), and Tangent
draws the text itself with OpenGL, one texel to each pixel of the screen, so it stays
sharp at fractional scales.

It runs on Wayland and X11 and does not depend on any desktop session. You need OpenGL
3.3 or OpenGL ES 3.0 through EGL, GTK 4.22 and libadwaita 1.9.

It has tabs, with an overview, and new tabs and windows start in the folder of the current
one. You can search the history, select a word or a line with a double or triple click,
and open an address with Ctrl and a click. Programs get mouse reporting, bracketed paste,
true colour and double, curly, dotted and dashed underlines. Box drawing and block
characters are drawn to fill their cells, so lines join without gaps. The font and the
light or dark style follow the desktop unless you choose others.

## Building and installing

    just build
    sudo just install        # or: just prefix=$HOME/.local install

Build needs Rust 1.92, `blueprint-compiler`, `just` and the development packages for GTK
and libadwaita. Details, other prefixes and removal are in
[docs/installing.md](docs/installing.md).

## Starting it

    tangent                  # your shell, in the current folder
    tangent -w ~/src         # in another folder
    tangent -e htop          # a command instead of the shell

Launchers that go through xdg-terminal-exec open Tangent once
`io.github.sachesi.tangent.desktop` is the first line of `~/.config/xdg-terminals.list`.

## Documentation

- [Installing](docs/installing.md)
- [Using Tangent](docs/usage.md), including [keyboard shortcuts](docs/keyboard-shortcuts.md) and [settings](docs/settings.md)
- [Contributing](CONTRIBUTING.md), including where things are in the code, and [reporting a vulnerability](SECURITY.md)

Tabs are not restored between runs. The interface is available in English, Russian and
Ukrainian.

GPL-3.0-or-later.
