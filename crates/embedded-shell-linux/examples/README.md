# `embedded-shell-linux` examples

Each example takes its arguments positionally (with sensible defaults).
All install a `tracing-subscriber` so events land on stderr — control
verbosity with `RUST_LOG`.

| Example | What it does |
|---|---|
| [`device_info`](device_info.rs) | Connects, reads `/etc/os-release` + uptime + memory + root-fs usage + IPv4 address, and pretty-prints a one-page summary. Smallest reasonable end-to-end demo: builder → `activate` → `run` + `fs::read_to_string`. |
| [`ping_health_monitor`](ping_health_monitor.rs) | Pings a target from the device every 5 s for ten iterations, emitting `info!`/`warn!` on transitions and `debug!` when nothing changed. Useful live tracing demo for `iputils::ping`. |

## Running

```sh
# device_info — defaults to /dev/ttyUSB0
cargo run --example device_info
cargo run --example device_info -- /dev/ttyUSB0

# ping_health_monitor — second arg is the ping target (default 8.8.8.8)
cargo run --example ping_health_monitor -- /dev/ttyUSB0 1.1.1.1
RUST_LOG=info cargo run --example ping_health_monitor -- /dev/ttyUSB0 1.1.1.1
```

## Features

Both examples build with the crate's default features (`coreutils +
iputils`). If you've disabled defaults, re-enable them on the command
line:

```sh
cargo run --example device_info --no-default-features --features coreutils,iputils
```

## Devices that need a password

The examples assume a passwordless or autologin device. If yours needs
a password, edit the example to chain `.password("…")` onto
`LinuxSerialShell::builder(...)`.
