//! Example: initialising logging for an application that uses
//! `embedded-shell`.
//!
//! The library emits structured `tracing` events at `trace` (byte-level RX/TX),
//! `debug` (operation-level), `info` (lifecycle: activated / rebooted /
//! shutdown / reopened), and `warn` (recovery attempts). It installs no
//! subscribers — the choice of where events land belongs to the binary using
//! the library.
//!
//! Run with:
//! ```sh
//! RUST_LOG=embedded_shell=debug cargo run --example init_logging
//! ```
//!
//! ## Adding journald
//!
//! For systemd-deployed services on Linux, layer in `tracing-journald`:
//!
//! ```ignore
//! // Cargo.toml:  tracing-journald = "0.3"
//!
//! tracing_subscriber::registry()
//!     .with(EnvFilter::from_default_env())
//!     .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
//!     .with(tracing_journald::layer().ok())   // <-- adds journald output
//!     .init();
//! ```
//!
//! Structured fields like `port = "/dev/ttyUSB0"` are auto-promoted to
//! journald fields, so you can query with:
//! ```sh
//! journalctl _COMM=my-app PORT=/dev/ttyUSB0
//! ```

use tracing_subscriber::{EnvFilter, prelude::*};

#[tokio::main]
async fn main() {
    tracing_subscriber::registry()
        .with(EnvFilter::from_default_env())
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();

    tracing::info!("logging initialised");
    tracing::debug!(example = true, "this is what a structured event looks like");
    tracing::trace!(bytes = ?b"hello", "byte-level events default off without RUST_LOG=trace");
}
