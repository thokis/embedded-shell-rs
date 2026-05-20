# `embedded-shell` examples

Each example takes the serial port as its first positional argument
(default `/dev/ttyUSB0`). All install a `tracing-subscriber` so events
land on stderr — control verbosity with `RUST_LOG`.

| Example | What it does |
|---|---|
| [`init_logging`](init_logging.rs) | Reference setup for `tracing-subscriber` with `EnvFilter` and optional journald — start here if you just want to see what the log catalogue looks like. |
| [`reboot_uptime_delta`](reboot_uptime_delta.rs) | Reads `/proc/uptime`, calls `reboot()`, reads it again. Proves the reboot happened (uptime dropped) and demonstrates that after `reboot()` returns the shell is already re-activated. |

## Running

```sh
cargo run --example reboot_uptime_delta -- /dev/ttyUSB0
RUST_LOG=embedded_shell=info cargo run --example reboot_uptime_delta -- /dev/ttyUSB0
RUST_LOG=embedded_shell=debug cargo run --example reboot_uptime_delta
```

## Devices that need a password

The examples assume a passwordless or autologin device. If yours needs
a password, edit the example to chain `.password("…")` onto the
`LinuxSerialShell::builder(...)` call. The library never reads
credentials from environment variables on its own — that's the
consumer's choice.
