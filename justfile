set shell := ["bash", "-euo", "pipefail", "-c"]

root := justfile_directory()
dev_binary := root / "target" / "debug" / "opencode-memory"

# List development recipes.
default:
    @just --list

# Build the debug native daemon and stage its runtime library.
build-dev:
    bun run build:native

# Build and smoke-test the debug daemon in an isolated runtime directory.
test-daemon: build-dev
    OPENCODE_NATIVE_MEMORY_BIN="{{ dev_binary }}" bun run test:protocol

# Build and gracefully replace an idle shared daemon.
daemon-swap: build-dev
    bun scripts/dev-daemon.ts swap --binary "{{ dev_binary }}"

# Build and replace the shared daemon even when development clients keep it busy.
daemon-swap-force: build-dev
    bun scripts/dev-daemon.ts swap --binary "{{ dev_binary }}" --force

# Build, smoke-test, and force-swap the shared development daemon.
dev: test-daemon
    bun scripts/dev-daemon.ts swap --binary "{{ dev_binary }}" --force
