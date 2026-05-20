# embedded-shell-rs (Cargo workspace)

Crates in this workspace:

| Crate | Kind | Purpose |
|---|---|---|
| [`embedded-shell`](crates/embedded-shell) | library | Async driver for Linux and U-Boot devices accessed over a serial line. `Shell` trait + concrete shells with deterministic exec framing. Documented in detail below. |
| [`embedded-shell-linux`](crates/embedded-shell-linux) | library | Thin async wrappers around common Linux userland CLI tools (`fs`, `iputils`, `systemd`, …), executed over any `LinuxShell`. Feature-gated per system package. |
| [`embedded-shell-transfer`](crates/embedded-shell-transfer) | library | File push and fetch between host and device, layered on a `LinuxShell`. Two transports behind Cargo features: `http` (fast, network-required) and `serial` (slow but works without network — the bootstrap path). |
| [`eshell`](crates/eshell) | binary | Command-line driver built on top of the three libraries. `eshell exec / push / pull / info / ping / reboot`. Useful as a daily-driver tool **and** as a reference application showing how to compose the libraries. |

## Building

```sh
cargo build --workspace
cargo build --workspace --all-features   # include opt-in features
```

The `embedded-shell-linux` and `embedded-shell-transfer` crates use Cargo
features (per system package, per transport — see each crate's README /
rustdoc). `--all-features` builds the full surface.

### Building just `eshell`

```sh
# Develop locally:
cargo build -p eshell                 # → target/debug/eshell
cargo build -p eshell --release       # → target/release/eshell

# Or install it to ~/.cargo/bin so it's on PATH everywhere:
cargo install --path crates/eshell
which eshell && eshell --help
```

Once installed, `eshell exec /dev/ttyUSB0 -- uname -a` works from any
directory. See [`crates/eshell/README.md`](crates/eshell/README.md) for
the full subcommand reference.

## Running tests

```sh
# All in-process tests, no hardware required.
cargo test --workspace --all-features
```

Per-crate, with finer control:

```sh
# embedded-shell — every state machine and framing path covered via
# tokio::io::duplex synthetic byte streams (no real serial port needed).
cargo test -p embedded-shell

# embedded-shell-linux — wrapper modules tested against SubprocessShell
# (fs against /tmp, iputils against 127.0.0.1).
cargo test -p embedded-shell-linux --all-features

# embedded-shell-transfer — both transports tested against SubprocessShell
# (HTTP via a loopback hyper server, serial via base64 + sh -c).
cargo test -p embedded-shell-transfer --all-features
```

See structured event output:

```sh
RUST_LOG=embedded_shell=debug,embedded_shell_transfer=debug \
  cargo test --workspace -- --nocapture
```

### Hardware-in-the-loop tests

Each crate (currently `embedded-shell` and `embedded-shell-transfer`)
ships `#[ignore]`-flagged tests in its `tests/hardware.rs` that drive a
real device. They're skipped by `cargo test` and run only when asked:

```sh
# Defaults to /dev/ttyUSB0 — override per the env vars below.
EMBEDDED_SHELL_LINUX_PORT=/dev/ttyUSB1 \
  cargo test --test hardware_linux -- --ignored --nocapture
```

The `embedded-shell` crate splits its hardware tests into two binaries
that are **independently runnable in any order**:

```sh
# Linux: login + run + reconnect + timeout + reboot.
cargo test -p embedded-shell --test hardware_linux -- --ignored --nocapture

# U-Boot: autoboot intercept + framed exec + multi-line output.
cargo test -p embedded-shell --test hardware_uboot -- --ignored --nocapture

# Run both back-to-back. The U-Boot binary's setup probes the device
# state and (if it's still in Linux) issues `reboot` to catch the
# autoboot countdown — no manual transition needed.
cargo test -p embedded-shell --test hardware_linux -- --ignored --nocapture \
    && cargo test -p embedded-shell --test hardware_uboot -- --ignored --nocapture
```

Each test's setup calls `common::open_at_linux()` or
`common::open_at_uboot()`. Those helpers try a direct open first; on
failure they assume the device is in the *other* state and transition
through it (Linux → reboot → catch autoboot, or U-Boot → `boot` → Linux
login). So you can run either binary regardless of what state the
device is in, and you can chain them in either order.

Other crates have a single hardware test binary, since they only drive
Linux:

```sh
# embedded-shell-linux — fs + iputils against the device.
cargo test -p embedded-shell-linux --test hardware --all-features \
  -- --ignored --nocapture

# embedded-shell-transfer — serial and HTTP push/fetch round-trips.
cargo test -p embedded-shell-transfer --test hardware \
  --features http,serial -- --ignored --nocapture
```

All three crates share **one implementation** of the state-aware
probe via the `test-utils` feature on the `embedded-shell` crate:

```rust
use embedded_shell::test_utils;

let shell = test_utils::open_at_linux(port, shell_prompt).await;  // probe + transition
let shell = test_utils::open_at_uboot(port, shell_prompt).await;  // probe + transition
```

Behind the scenes each helper tries a direct open first; on failure it
probes the *other* shell to disambiguate, and transitions via
`UBootSerialShell::boot_linux` or `LinuxSerialShell::reboot_no_reactivate`
as appropriate. If U-Boot reports a Linux login prompt during the
probe, the helper retries Linux with a longer timeout instead of
giving up. So every crate's hardware tests can be run regardless of
starting state, chained in any order, with no manual transitions.

To use the helpers in your own hardware tests, enable the feature in
dev-dependencies:

```toml
[dev-dependencies]
embedded-shell = { version = "0.1", features = ["test-utils"] }
```

The `embedded-shell` crate's own `hardware_linux` / `hardware_uboot`
binaries declare `required-features = ["test-utils"]`, so they're
skipped unless the feature is enabled — `cargo test --all-features` or
`cargo test --features test-utils` is the canonical invocation.

### Typical hardware-test timings

What to expect on a well-behaved embedded Linux board:

| Scenario | Time |
|---|---|
| Direct Linux open (device already at Linux shell or login) | 1–10 s |
| Direct U-Boot open (device at U-Boot prompt or autoboot countdown) | 5–15 s |
| Linux → U-Boot transition (Linux `reboot` + catch autoboot) | 60–120 s |
| U-Boot → Linux transition (U-Boot `boot` + kernel boot + login) | 30–60 s |
| Defensive retry (probe sequence + 120 s Linux retry) | 30–60 s |

The defensive retry is the most common "slow" path — it fires when the
device autoboots itself between two test invocations, leaving Linux in
a state that the 30 s direct probe doesn't quite cover.

Environment variables consumed by the hardware tests:

| Var | Default | Effect |
|---|---|---|
| `EMBEDDED_SHELL_LINUX_PORT` | `/dev/ttyUSB0` | Serial port for Linux-shell tests |
| `EMBEDDED_SHELL_UBOOT_PORT` | `/dev/ttyUSB0` | Serial port for U-Boot tests (shared with Linux on most embedded boards) |
| `EMBEDDED_SHELL_LINUX_SHELL_PROMPT` | crate default | Override the device's shell-prompt regex |
| `EMBEDDED_SHELL_LINUX_USERNAME` | crate default | Login username |
| `EMBEDDED_SHELL_LINUX_PASSWORD` | crate default | Login password — pass via env, never via command-line flags |
| `RUST_LOG` | unset | Subscriber filter; `embedded_shell=debug` is a good starting point |

The HTTP transfer hardware tests additionally require the device to
have a working network route back to the host's default-route interface
and `curl` installed device-side.

## Continuous integration

Pushes to `main` and pull requests are checked by
[`.github/workflows/ci.yml`](.github/workflows/ci.yml) on
`ubuntu-latest`:

| Step | Command |
|---|---|
| Format | `cargo fmt --all -- --check` |
| Build | `cargo build --workspace --all-features --all-targets` |
| Test | `cargo test --workspace --all-features` |
| Lint | `cargo clippy --workspace --all-targets --all-features -- -D warnings` |
| Docs | `RUSTDOCFLAGS='-D warnings' cargo doc --workspace --no-deps --all-features` |

Hardware tests stay out of CI two ways: they're `#[ignore]`d (so
`cargo test` skips them at runtime) and the `hardware_*` binaries
declare `required-features = ["test-utils"]` (so they're at least
compiled under `--all-features`, catching compile-time breakage even
when nothing is run). Combine those and CI passes deterministically
without any device attached, while still smoking out drift in the
test code itself.

Run-cancellation is enabled, so a fresh push to a branch supersedes
any in-flight run on the same branch.

### Lint

```sh
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

## Building and viewing the docs

Generate the rustdoc for every crate in the workspace:

```sh
cargo doc --workspace --no-deps --all-features
```

Output lands in `target/doc/`. The entry pages worth bookmarking:

- `target/doc/embedded_shell/index.html`
- `target/doc/embedded_shell_linux/index.html`
- `target/doc/embedded_shell_transfer/index.html`
- `target/doc/eshell/index.html`

### Open them locally

Easiest path — `cargo doc --open` opens the foundation crate's
landing page in your default browser:

```sh
cargo doc --workspace --no-deps --all-features --open
```

### Serve them over HTTP (reachable from other machines)

Useful for code review on a laptop, sharing with a colleague, or
browsing on a phone. Pick a port and bind to `0.0.0.0` so the listener
isn't restricted to loopback:

```sh
cargo doc --workspace --no-deps --all-features
python3 -m http.server --bind 0.0.0.0 --directory target/doc 8000
```

Now visit `http://<host-ip>:8000/embedded_shell/` (or any other crate
name) from anywhere on your local network.

**Caveats:** the server is plain HTTP with no auth, so only do this on
trusted networks. To stop sharing, `Ctrl-C` the `python3` process.

If Python isn't available, any single-binary static file server works
— for example `darkhttpd target/doc --port 8000` or
`miniserve target/doc --interfaces 0.0.0.0 --port 8000`.

---

# `embedded-shell`

Async driver for Linux and U-Boot devices accessed over a serial line.

## What's in the crate

- **`Shell` trait** — async `activate` / `deactivate` / `run` over any concrete shell.
- **`LinuxSerialShell`** — Linux login state machine, deterministic exec framing using `\x1f` (US) sentinels, host- and device-side timeouts (via `timeout(1)`), recovery on transient transport failures, optional reconnect.
- **`UBootSerialShell`** — U-Boot autoboot interrupt, `RETURNCODE=$?` framing, `reset` / `boot_linux` helpers, optional reconnect.
- **`SubprocessShell`** — same `Shell` trait, runs commands locally via `sh -c`. Useful for tests and host-side work.
- **`Command` builder** — argv-style with auto POSIX quoting (`Command::new("ls").arg("-la").arg("/tmp/with spaces")`). Use `sh -c` explicitly for pipes and redirects.
- **`ShellResult`** — typed stdout / stderr / exit code / duration with regex helpers.
- **Strict, typed errors** — `CommandFailed`, `CommandNotFound`, `Timeout`, `ReadTimeout`, `InvalidRegex`, `Initialization`, `Io`. No catch-all strings.

## Quick start

```rust
use embedded_shell::shell::{Command, LinuxSerialShell, Shell};

let mut shell = LinuxSerialShell::builder("/dev/ttyUSB0")
    .password("raspberry")           // omit for autologin / passwordless images
    .open()
    .await?;
shell.activate().await?;

let r = shell.run(&Command::new("uname").arg("-a")).await?;
println!("{}", r.stdout().unwrap_or(""));
```

For pipes and redirects, opt in explicitly via `sh -c`:

```rust
shell.run(&Command::new("sh").args(["-c", "dmesg | tail -20"])).await?;
```

## Custom prompts

If your device doesn't use the default `root@host:cwd#` shell prompt or `login: ` login prompt, override them:

```rust
let mut shell = LinuxSerialShell::builder("/dev/ttyUSB0")
    .username("dev")
    .shell_prompt(r"yocto-dev:[^#]*#")
    .login_prompt(r"please log in:")
    .open()
    .await?;
```

Patterns are validated at `.open()`-time. A bad regex surfaces as `ShellError::InvalidRegex { pattern, source }`.

## Reconnection

Disconnects (USB unplug, device reboot) surface as `ShellError::Io`. Reconnect is **explicit** via `reconnect()`, which closes the dead port, opens a fresh one with the same configuration, and re-runs the activate state machine in one shot:

```rust
match shell.run(&cmd).await {
    Err(ShellError::Io(_)) => {
        shell.reconnect().await?;
        shell.run(&cmd).await
    }
    other => other,
}
```

There is intentionally no auto-reconnect inside `run`. State on the device side is unknown after a disconnect, and a hidden retry would mask whether an in-flight command actually committed. The caller composes the policy.

## Logging

The library uses `tracing` and installs no subscriber. The recommended setup combines an `EnvFilter` (driven by `RUST_LOG`) with stderr output, optionally layered with `tracing-journald` for systemd-deployed services:

```rust
use tracing_subscriber::{EnvFilter, prelude::*};

tracing_subscriber::registry()
    .with(EnvFilter::from_default_env())
    .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
    // Linux + systemd: also write to journald
    // .with(tracing_journald::layer().ok())
    .init();
```

See `examples/init_logging.rs` for a full runnable example.

### Event schema

The crate emits a fixed catalogue of events, listed below. **Field names and event messages are considered part of the public API** — they will not change in a non-major version bump. New events and new fields on existing events are minor-version-compatible additions.

Structured fields (`port = "/dev/ttyUSB0"`, `elapsed = 4.2s`, etc.) are auto-promoted to journald fields:

```sh
journalctl _COMM=my-app PORT=/dev/ttyUSB0
journalctl _COMM=my-app -p info..   # info-level and above
```

#### `info` — lifecycle events you usually want unfiltered

| Message | Fields | Emitted when |
|---|---|---|
| `linux serial shell activated` | `port`, `username` | `LinuxSerialShell::activate` returns successfully |
| `u-boot serial shell activated` | `port` | `UBootSerialShell::activate` returns successfully |
| `device reboot complete` | `elapsed` | `LinuxSerialShell::reboot` returns |
| `device shutdown complete` | `elapsed` | `LinuxSerialShell::shutdown` returns |
| `u-boot reset complete` | `elapsed` | `UBootSerialShell::reset` returns |
| `u-boot handoff to linux complete` | — | `UBootSerialShell::boot_linux` returns |
| `reconnecting serial shell` | `port` | `LinuxSerialShell::reconnect` starts (before activate) |
| `reconnecting u-boot serial shell` | `port` | `UBootSerialShell::reconnect` starts (before activate) |

#### `warn` — recoverable issues

| Message | Fields | Emitted when |
|---|---|---|
| `shell exec failed, attempting recovery` | `error` | `LinuxSerialShell::run` hit a transport error; recovery starts |
| `shell unresponsive, re-activating` | — | The Ctrl-C probe also failed; falling back to full re-activate |

#### `debug` — operation-level

State-machine transitions and major actions: `probing linux serial shell`, `probing u-boot serial shell`, `rebooting device`, `shutting down device`, `u-boot reset`, `u-boot boot into linux`, `login prompt detected`, `shell prompt detected`, `u-boot prompt detected`, `autoboot banner detected, interrupting`, `device is at a Linux shell, rebooting to catch U-Boot`, `u-boot prompt detected, resetting to boot linux`, `executing` (`cmd` field, `SubprocessShell` only), `pushd` (`cwd` field).

Enable with `RUST_LOG=embedded_shell=debug`.

#### `trace` — byte-level

`tx` / `rx` events for every byte chunk over the serial line (`bytes` field, debug-formatted), plus `sending framed command` / `sending u-boot framed command` (`framed` field) when a command is dispatched, and reader-task lifecycle (`reader saw EOF`, `reader saw mpsc closed`, `reader error`).

Enable with `RUST_LOG=embedded_shell=trace`. Volumes get large fast — typically only useful when debugging framing or prompt-detection issues.

## Status

Foundation complete: `Shell` trait, `SubprocessShell`, `LinuxSerialShell`, `UBootSerialShell`. Transport-level integration is tested in-process via `tokio::io::duplex` — no real hardware needed for CI.

## License

MIT — see `LICENSE`.
