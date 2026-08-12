# Tangent build tasks. `build` needs cargo and blueprint-compiler.

set shell := ["bash", "-euo", "pipefail", "-c"]

app_id := "io.github.sachesi.tangent"
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

# Lints: rustfmt, clippy, blueprint and the schema.
check:
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings
    mkdir -p {{check_dir}}
    blueprint-compiler batch-compile {{check_dir}} data/ui data/ui/*.blp >/dev/null
    glib-compile-schemas --strict --dry-run data

# Unit tests.
test:
    cargo test
