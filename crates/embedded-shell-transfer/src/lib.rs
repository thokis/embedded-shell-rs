//! File push and fetch on a remote device — multi-transport.
//!
//! Push (host → device) and fetch (device → host) operations layered on
//! top of any [`embedded_shell::shell::LinuxShell`]. The same conceptual
//! operation is offered over two genuinely different transports
//! (selected by Cargo feature), because the two have non-overlapping
//! operational niches:
//!
//! | Transport | Speed | When you'd use it |
//! |---|---|---|
//! | [`http`] | ~10 MB/s on 100 Mbit Ethernet | The common case once the device has network connectivity. Heavy deps (`hyper`, `local-ip-address`). |
//! | [`serial`] | ~10 KB/s effective at 115200 baud after base64 overhead | Bootstrap path: device has no network yet (initial provisioning, SSH not configured, recovery). No extra deps beyond `base64`. |
//!
//! # Why named functions, not a strategy enum
//!
//! [`http::push`] / [`serial::push`] (and their `fetch` counterparts)
//! are separate functions rather than a single `push(strategy)` with
//! an enum, because the transports take different ancillary arguments
//! and have different failure modes — hiding them behind a uniform
//! interface would either require irrelevant args or surprise the
//! caller with transport-specific errors that the type system didn't
//! warn about. Named functions make the choice — and its consequences
//! — visible at the call site.
//!
//! # Features
//!
//! - `http` (default) — enables [`http`] module
//! - `serial` — enables [`serial`] module
//!
//! At least one feature must be enabled or the crate is empty.
//!
//! # Event schema
//!
//! The crate emits a small fixed catalogue of [`tracing`] events.
//! **Event messages and field names are part of the public API** and
//! will not change in a non-major version bump.
//!
//! ## `info` — operation lifecycle
//!
//! | Message | Fields | Emitted when |
//! |---|---|---|
//! | `serial push starting` | `bytes`, `wire_bytes`, `path` | [`serial::push`] is about to ship the payload |
//! | `serial push verified` | `bytes`, `elapsed_ms`, `path` | [`serial::push`] finished including the SHA-256 verify |
//! | `serial fetch starting` | `path` | [`serial::fetch`] is about to run the device-side `base64` |
//! | `serial fetch complete` | `bytes`, `wire_bytes`, `elapsed_ms`, `path` | [`serial::fetch`] returned with decoded bytes |
//! | `http push starting` | `bytes`, `url`, `path` | [`http::push`] is about to serve the payload to the device |
//! | `http push verified` | `bytes`, `elapsed_ms`, `path` | [`http::push`] finished including the SHA-256 verify |
//! | `http fetch starting` | `url`, `path` | [`http::fetch`] is about to instruct the device to upload |
//! | `http fetch complete` | `bytes`, `elapsed_ms`, `path` | [`http::fetch`] received the body from the device |
//!
//! ## `debug`
//!
//! | Message | Emitted when |
//! |---|---|
//! | `wget not found on device, falling back to curl` | [`http::push`] is retrying with `curl` after a `CommandNotFound` from `wget` |
//!
//! The library installs no subscriber; the consumer's binary configures
//! output. See the `embedded-shell` crate's README for a recommended
//! `tracing-subscriber` setup.
//!
//! [`http`]: crate::http
//! [`serial`]: crate::serial

mod error;

pub use error::{Result, TransferError};

#[cfg(feature = "http")]
pub mod http;

#[cfg(feature = "serial")]
pub mod serial;
