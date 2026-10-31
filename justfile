# Caret build and install tasks.
#
# `build` needs cargo and glslc; `install` only copies what is already in
# target/release, so the two can run on different machines sharing this directory.
#
#   just build
#   sudo just install              (prefix /usr/local)
#   just prefix=$HOME/.local install

set shell := ["bash", "-euo", "pipefail", "-c"]

app_id := "io.github.sachesi.caret"
prefix := env("PREFIX", "/usr/local")
destdir := env("DESTDIR", "")
bindir := destdir + prefix + "/bin"
datadir := destdir + prefix + "/share"
unitdir := destdir + prefix + "/lib/systemd/user"
release := "target/release"
version := `sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml`

default:
    @just --list

# Release build that looks for its translations under the install prefix.
build:
    CARET_LOCALEDIR={{prefix}}/share/locale cargo build --release --locked

# Debug build.
build-debug:
    cargo build --locked

# Run a debug build uninstalled, as the separate {{app_id}}.Devel application, which starts
# the debug terminal server beside it: just run [-e COMMAND…]
run *args: build-debug
    target/debug/caret {{args}}

# Formatting, clippy, schema, desktop entry, metainfo and catalogues.
check:
    cargo fmt --check
    cargo clippy --workspace --all-targets --locked -- -D warnings
    glib-compile-schemas --strict --dry-run data
    desktop-file-validate data/{{app_id}}.desktop
    appstreamcli validate --no-net data/{{app_id}}.metainfo.xml
    for lang in $(cat po/LINGUAS); do msgfmt -c -o /dev/null po/$lang.po; done

# Regenerate po/caret.pot from the sources, desktop entry, metainfo and schema.
pot:
    xgettext --from-code=UTF-8 --package-name=caret --package-version={{version}} \
        --msgid-bugs-address=https://github.com/sachesi/caret/issues \
        --keyword=gettext_f --keyword=ngettext_f:1,2 \
        --add-comments=Translators --sort-by-file -o po/caret.pot $(find crates/window/src -name '*.rs' | sort)
    xgettext -j --from-code=UTF-8 --package-name=caret --package-version={{version}} --msgid-bugs-address=https://github.com/sachesi/caret/issues --language=Desktop --sort-by-file -o po/caret.pot data/{{app_id}}.desktop
    xgettext -j --from-code=UTF-8 --package-name=caret --package-version={{version}} --msgid-bugs-address=https://github.com/sachesi/caret/issues --sort-by-file -o po/caret.pot data/{{app_id}}.metainfo.xml data/{{app_id}}.gschema.xml

# Merge the current template into every po/<lang>.po.
po: pot
    for lang in $(cat po/LINGUAS); do msgmerge --update --backup=none --quiet po/$lang.po po/caret.pot; done
    for lang in $(cat po/LINGUAS); do msgfmt --statistics -o /dev/null po/$lang.po; done

# Unit tests.
test:
    cargo test --workspace --locked

# Throughput, frame times, key echo, idle wakeups and memory of the release build, in a
# headless session of its own: just bench [--runs N]
bench *args: build
    bench/bench.py {{args}}

# Advisories, licences and sources of the dependencies.
deny:
    cargo deny check

# Install the release build. Does not build: run `just build` first, with the same prefix.
install:
    @test -x {{release}}/caret || { echo "error: {{release}}/caret missing; run 'just build' first" >&2; exit 1; }
    @test -x {{release}}/caret-server || { echo "error: {{release}}/caret-server missing; run 'just build' first" >&2; exit 1; }
    install -Dm755 {{release}}/caret {{bindir}}/caret
    install -Dm755 {{release}}/caret-server {{bindir}}/caret-server
    mkdir -p {{unitdir}}
    install -Dm644 data/caret-server.socket {{unitdir}}/caret-server.socket
    sed 's|@bindir@|{{prefix}}/bin|' data/caret-server.service.in > {{unitdir}}/caret-server.service
    mkdir -p {{datadir}}/applications {{datadir}}/metainfo
    msgfmt --desktop --template=data/{{app_id}}.desktop -d po -o {{datadir}}/applications/{{app_id}}.desktop
    msgfmt --xml --template=data/{{app_id}}.metainfo.xml -d po -o {{datadir}}/metainfo/{{app_id}}.metainfo.xml
    install -Dm644 data/{{app_id}}.gschema.xml {{datadir}}/glib-2.0/schemas/{{app_id}}.gschema.xml
    install -Dm644 data/icons/hicolor/scalable/apps/{{app_id}}.svg {{datadir}}/icons/hicolor/scalable/apps/{{app_id}}.svg
    install -Dm644 data/icons/hicolor/symbolic/apps/{{app_id}}-symbolic.svg {{datadir}}/icons/hicolor/symbolic/apps/{{app_id}}-symbolic.svg
    # Shell completions, which ask caret itself what to offer as they go.
    install -d {{datadir}}/bash-completion/completions {{datadir}}/zsh/site-functions {{datadir}}/fish/vendor_completions.d
    {{release}}/caret completions bash > {{datadir}}/bash-completion/completions/caret
    {{release}}/caret completions zsh > {{datadir}}/zsh/site-functions/_caret
    {{release}}/caret completions fish > {{datadir}}/fish/vendor_completions.d/caret.fish
    for lang in $(cat po/LINGUAS); do install -d {{datadir}}/locale/$lang/LC_MESSAGES; msgfmt -o {{datadir}}/locale/$lang/LC_MESSAGES/caret.mo po/$lang.po; done
    # A staged install (DESTDIR) leaves the caches to the package manager's triggers.
    [ -n "{{destdir}}" ] || glib-compile-schemas {{datadir}}/glib-2.0/schemas
    [ -n "{{destdir}}" ] || update-desktop-database -q {{datadir}}/applications || true
    [ -n "{{destdir}}" ] || gtk4-update-icon-cache -qtf {{datadir}}/icons/hicolor || gtk-update-icon-cache -qtf {{datadir}}/icons/hicolor || true
    @echo "installed to {{prefix}}"

# Remove what install put under the prefix.
uninstall:
    rm -f {{bindir}}/caret {{bindir}}/caret-server
    rm -f {{datadir}}/bash-completion/completions/caret {{datadir}}/zsh/site-functions/_caret {{datadir}}/fish/vendor_completions.d/caret.fish
    rm -f {{unitdir}}/caret-server.socket {{unitdir}}/caret-server.service
    rm -f {{datadir}}/applications/{{app_id}}.desktop {{datadir}}/metainfo/{{app_id}}.metainfo.xml
    rm -f {{datadir}}/glib-2.0/schemas/{{app_id}}.gschema.xml
    rm -f {{datadir}}/icons/hicolor/scalable/apps/{{app_id}}.svg {{datadir}}/icons/hicolor/symbolic/apps/{{app_id}}-symbolic.svg
    for lang in $(cat po/LINGUAS); do rm -f {{datadir}}/locale/$lang/LC_MESSAGES/caret.mo; done
    glib-compile-schemas {{datadir}}/glib-2.0/schemas || true
    update-desktop-database -q {{datadir}}/applications || true
    # A cache that still lists the removed icons hides the same icons installed elsewhere.
    gtk4-update-icon-cache -qtf {{datadir}}/icons/hicolor || gtk-update-icon-cache -qtf {{datadir}}/icons/hicolor || true
