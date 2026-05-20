# `embedded-shell-transfer` examples

Each example takes the serial port as its first positional argument
(default `/dev/ttyUSB0`) and installs a `tracing-subscriber` so events
land on stderr — control verbosity with `RUST_LOG`.

| Example | What it does |
|---|---|
| [`push_and_verify`](push_and_verify.rs) | Pushes a small JSON blob via HTTP, applies `chmod 0600`, fetches it back, and byte-compares. End-to-end demo of `http::push` + `http::fetch` and the SHA-256 verification baked into both. |

## Running

```sh
cargo run --example push_and_verify --features http
cargo run --example push_and_verify --features http -- /dev/ttyUSB0

# See the per-call info events from the transfer crate:
RUST_LOG=embedded_shell_transfer=info cargo run \
    --example push_and_verify --features http -- /dev/ttyUSB0

# Add device-side debug for the underlying shell:
RUST_LOG=embedded_shell=debug,embedded_shell_transfer=info \
    cargo run --example push_and_verify --features http
```

## Why `--features http`

The crate has no transports compiled in by default — you must opt into
`http`, `serial`, or both. The example uses `http::*`, so the `http`
feature must be enabled. (You could swap the two `http::` calls for
`serial::` calls and run with `--features serial` instead.)

## Devices that need a password

The example assumes a passwordless or autologin device. If yours needs
a password, edit the source to chain `.password("…")` onto
`LinuxSerialShell::builder(...)`.

## Device-side requirements

- `wget` *or* `curl` for the push direction.
- `curl` for the fetch direction (busybox `wget` has no `POST`).
- A working IPv4 route from the device back to the host's default-route
  interface. If the device can't reach the host, see the `serial`
  transport for a network-free alternative.
