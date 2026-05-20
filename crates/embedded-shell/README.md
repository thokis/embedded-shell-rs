# `embedded-shell`

Async driver for Linux and U-Boot devices accessed over a serial line.
The foundation of the [`embedded-shell-rs`](../..) workspace — every
other crate is built on top of this `Shell` trait.

## What's in the crate

- **`Shell` trait** — async `activate` / `deactivate` / `run` over any
  concrete shell.
- **`LinuxSerialShell`** — Linux login state machine, deterministic
  exec framing using `\x1f` (US) sentinels, host- and device-side
  timeouts (via `timeout(1)`), recovery on transient transport
  failures, optional reconnect.
- **`UBootSerialShell`** — U-Boot autoboot interrupt, `RETURNCODE=$?`
  framing, `reset` / `boot_linux` helpers, optional reconnect.
- **`SubprocessShell`** — same `Shell` trait, runs commands locally
  via `sh -c`. Useful for tests and host-side work.
- **`Command` builder** — argv-style with auto POSIX quoting
  (`Command::new("ls").arg("-la").arg("/tmp/with spaces")`). Use
  `sh -c` explicitly for pipes and redirects.
- **`ShellResult`** — typed stdout / stderr / exit code / duration
  with regex helpers.
- **Strict, typed errors** — `CommandFailed`, `CommandNotFound`,
  `Timeout`, `ReadTimeout`, `InvalidRegex`, `Initialization`, `Io`.
  No catch-all strings.

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

If your device doesn't use the default `root@host:cwd#` shell prompt
or `login: ` login prompt, override them:

```rust
let mut shell = LinuxSerialShell::builder("/dev/ttyUSB0")
    .username("dev")
    .shell_prompt(r"yocto-dev:[^#]*#")
    .login_prompt(r"please log in:")
    .open()
    .await?;
```

Patterns are validated at `.open()`-time. A bad regex surfaces as
`ShellError::InvalidRegex { pattern, source }`.

## Reconnection

Disconnects (USB unplug, device reboot) surface as `ShellError::Io`.
Reconnect is **explicit** via `reconnect()`, which closes the dead
port, opens a fresh one with the same configuration, and re-runs the
activate state machine in one shot:

```rust
match shell.run(&cmd).await {
    Err(ShellError::Io(_)) => {
        shell.reconnect().await?;
        shell.run(&cmd).await
    }
    other => other,
}
```

There is intentionally no auto-reconnect inside `run`. State on the
device side is unknown after a disconnect, and a hidden retry would
mask whether an in-flight command actually committed. The caller
composes the policy.

## Logging

The library uses `tracing` and installs no subscriber. The recommended
setup combines an `EnvFilter` (driven by `RUST_LOG`) with stderr
output, optionally layered with `tracing-journald` for systemd-deployed
services:

```rust
use tracing_subscriber::{EnvFilter, prelude::*};

tracing_subscriber::registry()
    .with(EnvFilter::from_default_env())
    .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
    // Linux + systemd: also write to journald
    // .with(tracing_journald::layer().ok())
    .init();
```

See [`examples/init_logging.rs`](examples/init_logging.rs) for a full
runnable example.

### Event schema

The crate emits a fixed catalogue of events, listed below. **Field
names and event messages are considered part of the public API** —
they will not change in a non-major version bump. New events and new
fields on existing events are minor-version-compatible additions.

Structured fields (`port = "/dev/ttyUSB0"`, `elapsed = 4.2s`, etc.)
are auto-promoted to journald fields:

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

State-machine transitions and major actions: `probing linux serial
shell`, `probing u-boot serial shell`, `rebooting device`, `shutting
down device`, `u-boot reset`, `u-boot boot into linux`, `login prompt
detected`, `shell prompt detected`, `u-boot prompt detected`,
`autoboot banner detected, interrupting`, `device is at a Linux
shell, rebooting to catch U-Boot`, `u-boot prompt detected, resetting
to boot linux`, `executing` (`cmd` field, `SubprocessShell` only),
`pushd` (`cwd` field).

Enable with `RUST_LOG=embedded_shell=debug`.

#### `trace` — byte-level

`tx` / `rx` events for every byte chunk over the serial line (`bytes`
field, debug-formatted), plus `sending framed command` / `sending
u-boot framed command` (`framed` field) when a command is dispatched,
and reader-task lifecycle (`reader saw EOF`, `reader saw mpsc closed`,
`reader error`).

Enable with `RUST_LOG=embedded_shell=trace`. Volumes get large fast —
typically only useful when debugging framing or prompt-detection
issues.

## `test-utils` feature

Enable with:

```toml
[dev-dependencies]
embedded-shell = { version = "0.1", features = ["test-utils"] }
```

Exposes `embedded_shell::test_utils::open_at_linux(port, shell_prompt)`
and `open_at_uboot(...)` — state-aware probes that try a direct open
first and transition through the other shell if needed. Used by every
hardware-in-the-loop test in this workspace. See the
[workspace README](../../README.md#hardware-in-the-loop-tests) for the
chained-test workflow this enables.

## Stability

Pre-1.0 (0.x). The following are intended to remain stable across 0.x
releases; everything else may change:

- `Shell` trait method signatures
- `ShellError` variant names
- Event messages and field names listed above
- `Command` builder method names (`new`, `arg`, `args`, `timeout`,
  `cwd`, `allow_nonzero`)

A 1.0 release will commit the full public API.

## Status

Foundation complete: `Shell` trait, `SubprocessShell`,
`LinuxSerialShell`, `UBootSerialShell`. Transport-level integration is
tested in-process via `tokio::io::duplex` — no real hardware needed
for CI.

## License

MIT — see [`LICENSE`](../../LICENSE) in the workspace root.
