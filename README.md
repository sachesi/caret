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
light or dark style follow the desktop unless you choose others, and screen readers can
read what is on the screen.

## Packages

Fedora 44, 45 and Rawhide, from the Copr project
[sachesi/software](https://copr.fedorainfracloud.org/coprs/sachesi/software/):

    sudo dnf copr enable sachesi/software
    sudo dnf install tangent

openSUSE Tumbleweed and Slowroll, from the OBS project
[home:sachesi:software](https://build.opensuse.org/project/show/home:sachesi:software); for
Slowroll the address has `openSUSE_Slowroll` in it, and on aarch64 `openSUSE_Factory_ARM`:

    sudo zypper addrepo https://download.opensuse.org/repositories/home:sachesi:software/openSUSE_Tumbleweed/home:sachesi:software.repo
    sudo zypper install tangent

Debian testing and Ubuntu 26.04, from the same OBS project; for Ubuntu the addresses
have `xUbuntu_26.04` in place of `Debian_Testing`:

    sudo install -d /etc/apt/keyrings
    curl -fsSL https://download.opensuse.org/repositories/home:sachesi:software/Debian_Testing/Release.key | sudo gpg --dearmor -o /etc/apt/keyrings/sachesi-software.gpg
    echo 'deb [signed-by=/etc/apt/keyrings/sachesi-software.gpg] https://download.opensuse.org/repositories/home:sachesi:software/Debian_Testing/ /' | sudo tee /etc/apt/sources.list.d/sachesi-software.list
    sudo apt update
    sudo apt install tangent

Debian 13 and Ubuntu 24.04 ship a GTK and libadwaita older than Tangent needs.

Arch Linux: the AUR package [tangent](https://aur.archlinux.org/packages/tangent), built
from [packaging/aur/PKGBUILD](packaging/aur/PKGBUILD), which each release tag updates.

The same packages are attached to each [release](https://github.com/sachesi/tangent/releases).

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
