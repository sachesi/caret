# Caret

Terminal for Wayland. Programs run in a background server, so they keep running after you
detach a window, and you can attach again from another window, over ssh or from a console.

> [!WARNING]
> Caret runs only on Wayland, with no X11 support, and needs a Vulkan 1.3 driver. Mesa's
> software driver works on a machine without a suitable GPU.

## Install

On Fedora, from [Copr](https://copr.fedorainfracloud.org/coprs/sachesi/software/):

    sudo dnf copr enable sachesi/software
    sudo dnf install caret

On Arch Linux, from the AUR: [caret](https://aur.archlinux.org/packages/caret).

[docs/installing.md](docs/installing.md) has the openSUSE, Debian and Ubuntu packages, and
how to build it yourself.

## Documentation

- [Installing](docs/installing.md)
- [Using Caret](docs/usage.md)
- [Keyboard shortcuts](docs/keyboard-shortcuts.md)
- [Settings](docs/settings.md)
- [Contributing](CONTRIBUTING.md) and [reporting a vulnerability](SECURITY.md)

Caret is free software under the GPL, version 3 or later.
