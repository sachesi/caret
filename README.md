# Caret

Caret is a terminal for Wayland, written in Rust. Programs' output is parsed by a
terminal emulator of Caret's own, and Caret draws its window and the text itself with
Vulkan, one texel to each pixel of the screen, so it stays sharp at fractional scales.
It uses no toolkit and draws its own header bar, tabs, menus and dialogs; the compositor
draws the window's frame where it offers to.

It does not depend on any desktop session. You need a Wayland compositor and a Vulkan 1.3
driver.

It has tabs and split panes, and new tabs and windows start in the folder of the current
one. The programs run in a terminal server, so a window can be detached and attached
again later with its programs still running, and one window can show several sessions.
You can search the history, select a word or a line with a double or triple click, and
open an address with Ctrl and a click. Programs get mouse reporting, bracketed paste,
true colour and double, curly, dotted and dashed underlines. Box drawing and block
characters are drawn to fill their cells, so lines join without gaps. The font and the
light or dark style follow the desktop unless you choose others, and screen readers can
read what is on the screen.

## Packages

Fedora 44, 45 and Rawhide, from the Copr project
[sachesi/software](https://copr.fedorainfracloud.org/coprs/sachesi/software/):

    sudo dnf copr enable sachesi/software
    sudo dnf install caret

openSUSE Tumbleweed and Slowroll, from the OBS project
[home:sachesi:software](https://build.opensuse.org/project/show/home:sachesi:software); for
Slowroll the address has `openSUSE_Slowroll` in it, and on aarch64 `openSUSE_Factory_ARM`:

    sudo zypper addrepo https://download.opensuse.org/repositories/home:sachesi:software/openSUSE_Tumbleweed/home:sachesi:software.repo
    sudo zypper install caret

Debian testing and Ubuntu 26.04, from the same OBS project; for Ubuntu the addresses
have `xUbuntu_26.04` in place of `Debian_Testing`:

    sudo install -d /etc/apt/keyrings
    curl -fsSL https://download.opensuse.org/repositories/home:sachesi:software/Debian_Testing/Release.key | sudo gpg --dearmor -o /etc/apt/keyrings/sachesi-software.gpg
    echo 'deb [signed-by=/etc/apt/keyrings/sachesi-software.gpg] https://download.opensuse.org/repositories/home:sachesi:software/Debian_Testing/ /' | sudo tee /etc/apt/sources.list.d/sachesi-software.list
    sudo apt update
    sudo apt install caret

Debian 13 and Ubuntu 24.04 ship a Rust older than Caret needs.

Arch Linux: the AUR package [caret](https://aur.archlinux.org/packages/caret), built
from [packaging/aur/PKGBUILD](packaging/aur/PKGBUILD), which each release tag updates.

The same packages are attached to each [release](https://github.com/sachesi/caret/releases).

## Building and installing

    just build
    sudo just install        # or: just prefix=$HOME/.local build install

Build needs Rust 1.92, `just`, `glslc` and the development packages for libwayland and
libxkbcommon. Details, other prefixes and removal are in
[docs/installing.md](docs/installing.md).

## Starting it

    caret                  # your shell, in the current folder
    caret -w ~/src         # in another folder
    caret -e htop          # a command instead of the shell

Launchers that go through xdg-terminal-exec open Caret once
`io.github.sachesi.caret.desktop` is the first line of `~/.config/xdg-terminals.list`.

## Documentation

- [Installing](docs/installing.md)
- [Using Caret](docs/usage.md), including [keyboard shortcuts](docs/keyboard-shortcuts.md) and [settings](docs/settings.md)
- [Contributing](CONTRIBUTING.md), including where things are in the code, and [reporting a vulnerability](SECURITY.md)

Tabs are not restored between runs. The interface is available in English, Russian and
Ukrainian.

GPL-3.0-or-later.
