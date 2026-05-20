# `embedded-shell-linux` examples

Each example takes its arguments positionally (with sensible defaults).
All install a `tracing-subscriber` so events land on stderr — control
verbosity with `RUST_LOG`.

| Example | What it does |
|---|---|
| [`device_info`](device_info.rs) | Connects, reads `/etc/os-release` + uptime + memory + root-fs usage + IPv4, prints a one-page summary. Smallest reasonable end-to-end demo: builder → `activate` → `run` + `fs::read_to_string`. |
| [`ping_health_monitor`](ping_health_monitor.rs) | Pings a target every 5 s for ten iterations, emitting `info!`/`warn!` on transitions. Live tracing demo for `iputils::ping`. |
| [`services`](services.rs) | Lists active systemd services on the device; for each failed service, tails the last 3 journal entries. `systemd::list_units` + `journalctl::tail_unit`. |
| [`network`](network.rs) | Comprehensive network state — interfaces, addresses, default route, plus NM's active connections. `iproute2` (kernel view) + `networkmanager` (daemon view) side-by-side. |
| [`modem`](modem.rs) | Fleet-inventory style modem dump: every modem ModemManager knows about, plus its primary SIM (IMEI/ICCID/IMSI/operator). `modemmanager::list_modems` + `modem` + `sim`. |
| [`config_update`](config_update.rs) | Atomic config-update pattern: `fs::write_atomic` to a sandbox path, then `fs::read` to verify. The canonical provisioning move (real version follows with `systemd::restart`). |
| [`file_read`](file_read.rs) | Two ways to read: `fs::read_to_string` (text) vs `fs::read` (raw bytes). Pass the path as the second arg. |

## Running

```sh
# Default-feature examples (coreutils + iputils — always on):
cargo run --example device_info -- /dev/ttyUSB0
cargo run --example ping_health_monitor -- /dev/ttyUSB0 1.1.1.1
cargo run --example file_read -- /dev/ttyUSB0 /etc/hostname
cargo run --example config_update -- /dev/ttyUSB0

# Opt-in feature examples (need the matching Cargo feature enabled):
cargo run --example services --features systemd -- /dev/ttyUSB0
cargo run --example network --features iproute2,networkmanager -- /dev/ttyUSB0
cargo run --example modem --features modemmanager -- /dev/ttyUSB0
```

Or with `--all-features` to skip the per-example flag:

```sh
cargo run --example services --all-features -- /dev/ttyUSB0
```

`RUST_LOG=info` gives a useful default for the examples that emit
events; `embedded_shell=debug` adds the underlying shell state
machine.

## Devices that need a password

The examples assume a passwordless or autologin device. If yours needs
a password, edit the example to chain `.password("…")` onto
`LinuxSerialShell::builder(...)`.
