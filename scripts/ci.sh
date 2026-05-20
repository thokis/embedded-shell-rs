#!/usr/bin/env bash
# Mirrors .github/workflows/ci.yml — run before pushing to catch
# anything CI would catch, locally and faster.
#
# Usage:
#   scripts/ci.sh                   # full CI pipeline (fmt build test clippy doc)
#   scripts/ci.sh --fmt             # only the formatting check
#   scripts/ci.sh --build           # only the build step
#   scripts/ci.sh --test            # only the test step
#   scripts/ci.sh --clippy          # only the lint step
#   scripts/ci.sh --doc             # only the doc-build step
#
#   scripts/ci.sh --hardware [tgt]  # run hardware tests against a real device.
#                                   # Always #[ignore]d in CI; only this flag
#                                   # opts in. tgt is one of:
#                                   #   (omitted)     all four binaries
#                                   #   linux         embedded-shell hardware_linux
#                                   #   uboot         embedded-shell hardware_uboot
#                                   #   linux-crate   embedded-shell-linux
#                                   #   transfer      embedded-shell-transfer
#                                   # Honors EMBEDDED_SHELL_LINUX_PORT etc.

set -euo pipefail
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

# --- hardware targets ---

hw_linux() {
    step "hardware: embedded-shell hardware_linux"
    cargo test -p embedded-shell --features test-utils --test hardware_linux \
        -- --ignored --nocapture
}

hw_uboot() {
    step "hardware: embedded-shell hardware_uboot"
    cargo test -p embedded-shell --features test-utils --test hardware_uboot \
        -- --ignored --nocapture
}

hw_linux_crate() {
    step "hardware: embedded-shell-linux"
    cargo test -p embedded-shell-linux --test hardware --all-features \
        -- --ignored --nocapture
}

hw_transfer() {
    step "hardware: embedded-shell-transfer"
    cargo test -p embedded-shell-transfer --test hardware --features http,serial \
        -- --ignored --nocapture
}

case "${1:-all}" in
    --fmt)    run_fmt ;;
    --build)  run_build ;;
    --test)   run_test ;;
    --clippy) run_clippy ;;
    --doc)    run_doc ;;
    --hardware)
        case "${2:-}" in
            "")
                hw_linux
                hw_uboot
                hw_linux_crate
                hw_transfer
                ;;
            linux)        hw_linux ;;
            uboot)        hw_uboot ;;
            linux-crate)  hw_linux_crate ;;
            transfer)     hw_transfer ;;
            *)
                echo "unknown --hardware target: $2" >&2
                echo "expected one of: linux | uboot | linux-crate | transfer" >&2
                exit 2
                ;;
        esac
        printf '\n\033[1;32m✓ hardware tests passed\033[0m\n'
        ;;
    all|"")
        run_fmt
        run_build
        run_test
        run_clippy
        run_doc
        printf '\n\033[1;32m✓ all CI checks passed\033[0m\n'
        ;;
    -h|--help)
        sed -n '2,21p' "$0"
        ;;
    *)
        echo "unknown flag: $1" >&2
        echo "try: $0 --help" >&2
        exit 2
        ;;
esac
