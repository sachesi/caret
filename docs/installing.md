# Installing

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
from [packaging/aur/PKGBUILD](../packaging/aur/PKGBUILD), which each release tag updates.

The same packages are attached to each [release](https://github.com/sachesi/caret/releases).

## What you need

To build: Rust 1.92 or newer, `just`, `glslc` for the shaders, `pkg-config`, and the
development packages of libwayland and libxkbcommon. On Fedora that is `glslc
wayland-devel libxkbcommon-devel just`; on Debian `glslc libwayland-dev libxkbcommon-dev
pkg-config just`. `just check` also wants `desktop-file-validate`, `appstreamcli` and gettext.

To run: a Wayland compositor, and the Vulkan loader, `libvulkan.so.1`, with a driver for
Vulkan 1.3. Mesa's software driver will do where there is no GPU. A session bus is used
where there is one, for the desktop's font and style from the settings portal, for
notifications and for screen readers.

## Build

    just build          # release
    just build-debug
    just check          # rustfmt, clippy, desktop and metainfo validation, catalogues
    just test
    just deny           # advisories, licences and sources of the dependencies
    just run            # debug build, uninstalled, as the separate io.github.sachesi.caret.Devel
    just run -e htop    # the same, running a command instead of the shell

The locale directory is compiled in (`CARET_LOCALEDIR`). `just build` sets it from the
prefix, `/usr/local/share/locale` by default; give the same prefix to `build` and
`install`. A plain `cargo build` uses that default unless `CARET_LOCALEDIR` is set.
`GLSLC` names another shader compiler than the `glslc` on the path.

## Install

    sudo just install
    just prefix=$HOME/.local build install
    DESTDIR=/tmp/stage just install

`install` copies what `just build` produced; it never builds. The default prefix is
`/usr/local`. It puts `caret` and `caret-server` in `bin`, the server's systemd user
units in `lib/systemd/user`, and the desktop entry, metainfo, icons, translations and
bash, zsh and fish completions under `share`, then refreshes the desktop and icon caches.
With `DESTDIR` set, those caches are left to the package manager.

The first window starts `caret-server` when nothing listens on its socket, and the server
ends once it keeps no terminal and no window is attached. To have systemd start it
instead, which keeps its terminals out of the window's process tree and resource limits:

    systemctl --user enable --now caret-server.socket

Either way, terminals do not outlast logging out. To have them do so, enable the socket as
above and let your user's services keep running without a login:

    loginctl enable-linger

`caret --attach` after logging in again then shows the windows that were detached. Programs
in them go on running while you are logged out, until they end or you close them. Nothing
outlasts a reboot, since the terminals live in the server's memory.

## Remove

    sudo just uninstall
    just prefix=$HOME/.local uninstall

Settings stay in `~/.config/caret` and the window's size in `~/.local/state/caret`;
removing those folders clears them.
