# `embedded-shell-linux`

Thin async wrappers around common Linux userland CLI tools, executed
over any [`LinuxShell`][LinuxShell] — the
[`SubprocessShell`][SubprocessShell] for local host, the
[`LinuxSerialShell`][LinuxSerialShell] for a serial-attached device.
Part of the [`embedded-shell-rs`](../..) workspace.

[LinuxShell]: ../embedded-shell/src/shell/traits.rs
[SubprocessShell]: ../embedded-shell/src/shell/subprocess.rs
[LinuxSerialShell]: ../embedded-shell/src/shell/linux.rs

## What's in the crate

Each module runs a single command on the device via the shell and
returns a typed Rust value. The [`LinuxShell`][LinuxShell] bound
restricts wrappers to shells whose device-side userland is Linux-style
— `UBootSerialShell` is rejected at the type level.

| Module | Feature | Wraps |
|---|---|---|
| `fs` | `coreutils` (default) | `cat`, `ls`, `chmod`, `mkdir`, `rm`, `cp`, `mv`, `ln -s`, `readlink`, `stat`, `find`, `sha256sum`. Mirrors `std::fs` where there's a direct analogue; adds `write_atomic`, `walk_dir`, `sha256sum`. |
| `iputils` | `iputils` (default) | `ping` → `PingStats`; `arping` → `ArpingStats` (with the responder's MAC). |
| `systemd` | `systemd` | `systemctl is-active`/`is-enabled`/`is-failed`/`start`/`stop`/`restart`/`reload`/`enable`/`disable`/`show` → `UnitStatus`; `list_units` for tabular listings. |
| `journalctl` | `systemd` | `journalctl -o json-seq` → `Vec<LogEntry>` with `tail(n)` and `tail_unit(unit, n)`. Uses RFC 7464 framing so multi-line messages parse cleanly. |
| `iproute2` | `iproute2` | `ip -j` JSON → `Vec<Link>` / `Vec<Address>` / `Vec<Route>`. |
| `networkmanager` | `networkmanager` | `nmcli -t` → `Vec<Connection>` and `Vec<Device>`. Read-only in v1. |
| `modemmanager` | `modemmanager` | `mmcli -J` → `Vec<u32>` indices and detailed `Modem` per index (state, IMEI, signal, operator, …). |

### Feature naming convention

Feature names reflect the **device-side dependency** — enabling
`coreutils` gives you the `fs` module (because the underlying tools
come from coreutils or busybox). The asymmetry is intentional: feature
names answer "what does the device need installed?"; module names
answer "what does the Rust API look like?"

Default-on (present on essentially every embedded Linux):

- `coreutils` — `fs`
- `iputils` — `iputils`

Opt-in (not universal on minimal embedded distros):

- `systemd` — `systemd` and `journalctl`
- `networkmanager` — `networkmanager`
- `modemmanager` — `modemmanager`
- `iproute2` — `iproute2`

## Quick start

```rust
use embedded_shell::shell::{LinuxSerialShell, Shell};
use embedded_shell_linux::{fs, iputils};

let mut shell = LinuxSerialShell::builder("/dev/ttyUSB0").open().await?;
shell.activate().await?;

// Read a file
let contents = fs::read(&mut shell, "/etc/os-release").await?;

// Ping a target from the device
let stats = iputils::ping(&mut shell, "8.8.8.8", 3).await?;
println!("loss = {}%", stats.loss_percent);
```

All wrappers take `&mut dyn LinuxShell` (or a concrete `LinuxShell`),
so calling code is the same against a local subprocess as it is
against a serial-attached device.

## Why read-only by default for `iproute2` / `networkmanager` / `modemmanager`

State-changing operations on the network stack of a device you're
*remoted into over the network* are self-defeating ("disconnect the
link you're using to talk to me"). For mutation, drop into
`shell.run(Command::new("ip").args([...]))` directly — the wrappers
exist for the read paths that cover 90% of day-to-day use.

## Stability

Pre-1.0 (0.x). Module names, function signatures, and the public types
returned by each wrapper are intended to remain stable across 0.x
releases. New modules and new functions on existing modules are
minor-version-compatible additions.

## License

MIT — see [`LICENSE`](../../LICENSE) in the workspace root.
