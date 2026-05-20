#!/usr/bin/env bash
# Mirrors .github/workflows/ci.yml — run before pushing to catch
# anything CI would catch, locally and faster.
#
# Usage:
#   scripts/ci.sh         # run every step, abort on first failure
#   scripts/ci.sh --fmt   # only the formatting check
#   scripts/ci.sh --test  # only the test step (skips fmt/clippy/doc)
#
# Hardware tests stay #[ignore]d — this script never touches a device.

set -euo pipefail

# Always run from the workspace root so paths don't depend on the
# caller's cwd.
cd "$(dirname "$0")/.."

step() {
    printf '\n\033[1;34m▶ %s\033[0m\n' "$1"
}

run_fmt() {
    step "cargo fmt --check"
    cargo fmt --all -- --check
}

run_build() {
    step "cargo build --workspace --all-features --all-targets"
    cargo build --workspace --all-features --all-targets
}

run_test() {
    step "cargo test --workspace --all-features"
    cargo test --workspace --all-features
}

run_clippy() {
    step "cargo clippy --all-targets --all-features -- -D warnings"
    cargo clippy --workspace --all-targets --all-features -- -D warnings
}

run_doc() {
    step "cargo doc --workspace --no-deps --all-features (RUSTDOCFLAGS=-D warnings)"
    RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --all-features
}

case "${1:-all}" in
    --fmt)    run_fmt ;;
    --build)  run_build ;;
    --test)   run_test ;;
    --clippy) run_clippy ;;
    --doc)    run_doc ;;
    all|"")
        run_fmt
        run_build
        run_test
        run_clippy
        run_doc
        printf '\n\033[1;32m✓ all CI checks passed\033[0m\n'
        ;;
    -h|--help)
        sed -n '2,9p' "$0"
        ;;
    *)
        echo "unknown flag: $1" >&2
        echo "try: $0 --help" >&2
        exit 2
        ;;
esac
