# Installing

## What you need

To build: Rust 1.92 or newer, `blueprint-compiler`, `just`, and the development packages
of GTK 4.22 and libadwaita 1.9. On Fedora that is `gtk4-devel libadwaita-devel
blueprint-compiler just`; on Debian `libgtk-4-dev libadwaita-1-dev blueprint-compiler
just`. `just check` also wants `desktop-file-validate` and `appstreamcli`.

To run: GTK 4.22, libadwaita 1.9, `libEGL.so.1` with a driver for OpenGL 3.3 or OpenGL ES
3.0, and a session bus. Without a GL context the tab says why and starts no
program.

## Build

    just build          # release
    just build-debug
    just check          # rustfmt, clippy, blueprint, schema, desktop and metainfo validation
    just run            # debug build, uninstalled
    just run -e htop    # the same, running a command instead of the shell

The locale directory is compiled in (`TANGENT_LOCALEDIR`, default
`/usr/local/share/locale`). Set it in the environment of `cargo build` if you install
somewhere else.

## Install

    sudo just install
    just prefix=$HOME/.local install
    DESTDIR=/tmp/stage just install

`install` copies what `just build` produced; it never builds. The default prefix is
`/usr/local`. It puts the binary in `bin`, and the desktop entry, metainfo, icons and
GSettings schema under `share`, then compiles the schema and refreshes the desktop and
icon caches. With `DESTDIR` set, those caches are left to the package manager.

## Remove

    sudo just uninstall
    just prefix=$HOME/.local uninstall

Settings stay in dconf; `dconf reset -f /io/github/sachesi/tangent/` clears them.
