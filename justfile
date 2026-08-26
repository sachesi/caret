# Tangent build and install tasks.
#
# `build` needs cargo and blueprint-compiler; `install` only copies what is already in
# target/release, so the two can run on different machines sharing this directory.
#
#   just build
#   sudo just install              (prefix /usr/local)
#   just prefix=$HOME/.local install

set shell := ["bash", "-euo", "pipefail", "-c"]

app_id := "io.github.sachesi.tangent"
prefix := env("PREFIX", "/usr/local")
destdir := env("DESTDIR", "")
bindir := destdir + prefix + "/bin"
datadir := destdir + prefix + "/share"
release := "target/release"
schema_dir := "target/schemas"
check_dir := "target/check"

default:
    @just --list

# Release build.
build:
    cargo build --release

# Debug build.
build-debug:
    cargo build

# Compile the GSettings schema into target/schemas for running uninstalled.
schemas:
    mkdir -p {{schema_dir}}
    cp data/{{app_id}}.gschema.xml {{schema_dir}}/
    glib-compile-schemas {{schema_dir}}

# Run the debug build uninstalled: just run [-e COMMAND…]
run *args: build-debug schemas
    GSETTINGS_SCHEMA_DIR={{schema_dir}} target/debug/tangent {{args}}

# Lints: rustfmt, clippy, blueprint, desktop file and metainfo validation.
check:
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    mkdir -p {{check_dir}}
    blueprint-compiler batch-compile {{check_dir}} data/ui data/ui/*.blp >/dev/null
    glib-compile-schemas --strict --dry-run data
    desktop-file-validate data/{{app_id}}.desktop
    appstreamcli validate --no-net data/{{app_id}}.metainfo.xml

# Unit tests.
test:
    cargo test

# Install the release build. Does not build: run `just build` first.
install:
    @test -x {{release}}/tangent || { echo "error: {{release}}/tangent missing; run 'just build' first" >&2; exit 1; }
    install -Dm755 {{release}}/tangent {{bindir}}/tangent
    install -Dm644 data/{{app_id}}.desktop {{datadir}}/applications/{{app_id}}.desktop
    install -Dm644 data/{{app_id}}.metainfo.xml {{datadir}}/metainfo/{{app_id}}.metainfo.xml
    install -Dm644 data/{{app_id}}.gschema.xml {{datadir}}/glib-2.0/schemas/{{app_id}}.gschema.xml
    install -Dm644 data/icons/hicolor/scalable/apps/{{app_id}}.svg {{datadir}}/icons/hicolor/scalable/apps/{{app_id}}.svg
    install -Dm644 data/icons/hicolor/symbolic/apps/{{app_id}}-symbolic.svg {{datadir}}/icons/hicolor/symbolic/apps/{{app_id}}-symbolic.svg
    # A staged install (DESTDIR) leaves the caches to the package manager's triggers.
    [ -n "{{destdir}}" ] || glib-compile-schemas {{datadir}}/glib-2.0/schemas
    [ -n "{{destdir}}" ] || update-desktop-database -q {{datadir}}/applications || true
    [ -n "{{destdir}}" ] || gtk4-update-icon-cache -qtf {{datadir}}/icons/hicolor || gtk-update-icon-cache -qtf {{datadir}}/icons/hicolor || true
    @echo "installed to {{prefix}}"

uninstall:
    rm -f {{bindir}}/tangent
    rm -f {{datadir}}/applications/{{app_id}}.desktop {{datadir}}/metainfo/{{app_id}}.metainfo.xml
    rm -f {{datadir}}/glib-2.0/schemas/{{app_id}}.gschema.xml
    rm -f {{datadir}}/icons/hicolor/scalable/apps/{{app_id}}.svg {{datadir}}/icons/hicolor/symbolic/apps/{{app_id}}-symbolic.svg
    glib-compile-schemas {{datadir}}/glib-2.0/schemas || true
    update-desktop-database -q {{datadir}}/applications || true
    # A cache that still lists the removed icons hides the same icons installed elsewhere.
    gtk4-update-icon-cache -qtf {{datadir}}/icons/hicolor || gtk-update-icon-cache -qtf {{datadir}}/icons/hicolor || true
