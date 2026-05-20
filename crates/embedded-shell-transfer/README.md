# `embedded-shell-transfer`

File push (host → device) and fetch (device → host) layered on any
[`LinuxShell`][LinuxShell] — two genuinely different transports, picked
by Cargo feature, because their operational niches don't overlap. Part
of the [`embedded-shell-rs`](../..) workspace.

[LinuxShell]: ../embedded-shell/src/shell/traits.rs

## Transports

| Transport | Speed | When to use it |
|---|---|---|
| `http` (default) | ~10 MB/s on 100 Mbit Ethernet | Common case once the device has network connectivity. Heavier deps (`hyper`, `local-ip-address`). |
| `serial` | ~10 KB/s effective at 115200 baud after base64 overhead | Bootstrap path: device has no network yet (initial provisioning, SSH not configured, recovery). Only adds `base64`. |

```toml
[dependencies]
embedded-shell-transfer = { version = "0.1", features = ["http", "serial"] }
```

At least one feature must be enabled or the crate is empty. The
`eshell` CLI in [`embedded-shell-cli`](../embedded-shell-cli) enables
both and uses HTTP-first with serial fallback.

## API shape

```rust
use embedded_shell_transfer::{http, serial};

http::push(&mut shell, &local_path, "/etc/app.cfg").await?;
http::fetch(&mut shell, "/var/log/messages", &local_path).await?;

serial::push(&mut shell, &local_path, "/etc/app.cfg").await?;
serial::fetch(&mut shell, "/var/log/messages", &local_path).await?;
```

`http::push` / `serial::push` (and their `fetch` counterparts) are
separate functions rather than one `push(strategy)` with an enum,
because each transport takes different ancillary arguments and has
different failure modes. Named functions make the choice — and its
consequences — visible at the call site.

## How each transport works

**HTTP** spins up a short-lived `hyper` server on the host bound to the
host's default-route interface. The host then drives the device via the
shell to `curl` (or `wget`, with auto-fallback) the URL. Both pushes
and fetches are verified end-to-end with SHA-256.

**Serial** base64-encodes the payload host-side and ships it as a
single shell command line to the device's `base64 -d > file`. Fetches
go the other way — device-side `base64 < file` whose output is captured
through the framed exec protocol. SHA-256 verified as well.

**Caveat for serial:** push payloads are capped at 64 KiB — the upper
limit of a single shell command line. Larger files need HTTP.

## Event schema

The crate emits a small fixed catalogue of [`tracing`] events. **Event
messages and field names are part of the public API.**

### `info` — operation lifecycle

| Message | Fields | Emitted when |
|---|---|---|
| `serial push starting` | `bytes`, `wire_bytes`, `path` | `serial::push` is about to ship the payload |
| `serial push verified` | `bytes`, `elapsed_ms`, `path` | `serial::push` finished including the SHA-256 verify |
| `serial fetch starting` | `path` | `serial::fetch` is about to run the device-side `base64` |
| `serial fetch complete` | `bytes`, `wire_bytes`, `elapsed_ms`, `path` | `serial::fetch` returned with decoded bytes |
| `http push starting` | `bytes`, `url`, `path` | `http::push` is about to serve the payload to the device |
| `http push verified` | `bytes`, `elapsed_ms`, `path` | `http::push` finished including the SHA-256 verify |
| `http fetch starting` | `url`, `path` | `http::fetch` is about to instruct the device to upload |
| `http fetch complete` | `bytes`, `elapsed_ms`, `path` | `http::fetch` received the body from the device |

### `debug`

| Message | Emitted when |
|---|---|
| `wget not found on device, falling back to curl` | `http::push` is retrying with `curl` after a `CommandNotFound` from `wget` |

The crate installs no subscriber; the consumer's binary configures
output. See the [`embedded-shell` README](../embedded-shell/README.md#logging)
for a recommended `tracing-subscriber` setup.

## Stability

Pre-1.0 (0.x). The following are intended to remain stable across 0.x
releases:

- `http::push` / `http::fetch` / `serial::push` / `serial::fetch` signatures
- `TransferError` variant names
- Event messages and field names listed above

A 1.0 release will commit the full public API.

## License

MIT — see [`LICENSE`](../../LICENSE) in the workspace root.
