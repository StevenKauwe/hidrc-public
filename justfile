# hidrc tasks. Run `just` to list them.

bin := "target/release/hidrc"

default:
    @just --list

# Build the release binary
build:
    cargo build --release

# Run all tests (read-only: never changes your settings)
test:
    cargo test

# Lint and format check
check:
    cargo fmt --check
    cargo clippy --all-targets -- -D warnings

# Write this Mac's current settings to config.toml (refuses to overwrite)
capture: build
    @test ! -e config.toml || (echo "config.toml exists; use: just capture-to <file>" && exit 1)
    {{bin}} capture > config.toml
    @echo "wrote config.toml"

# Write current settings to another file, e.g. to diff against config.toml
capture-to file: build
    {{bin}} capture > {{file}}

# Show what apply would change
plan: build
    {{bin}} plan

# Apply config.toml now (restarts the agent if installed)
apply: build
    {{bin}} apply

# Build, apply, and run the agent at login
install: build
    {{bin}} apply
    {{bin}} install

uninstall:
    {{bin}} uninstall

# Devices, permission and agent state
status: build
    {{bin}} status

# Measure whether the mouse is really linear and 1:1 (needs Input Monitoring)
measure seconds="20": build
    {{bin}} measure {{seconds}}

# Follow the agent's log
logs:
    tail -f ~/Library/Logs/hidrc.log

# Run the agent in the foreground (stop the installed one first)
run: build
    {{bin}} run
