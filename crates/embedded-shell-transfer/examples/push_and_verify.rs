//! Pushes a small config blob to the device over HTTP, sets its
//! permissions, fetches it back, and byte-compares. End-to-end
//! demonstration of the transfer crate.
//!
//! Run with:
//! ```sh
//! cargo run --example push_and_verify --features http -- /dev/ttyUSB0
//! RUST_LOG=embedded_shell_transfer=info cargo run \
//!     --example push_and_verify --features http -- /dev/ttyUSB0
//! ```
//!
//! Demonstrates [`http::push`] + [`http::fetch`] and uses an inline
//! `chmod` instead of pulling in `embedded-shell-linux`, so the
//! example stays a single-crate dep.

use std::time::Duration;

use embedded_shell::shell::{Command, LinuxSerialShell, Shell};
use embedded_shell_transfer::http;
use tracing::info;
use tracing_subscriber::{EnvFilter, prelude::*};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();

    let port = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "/dev/ttyUSB0".to_string());

    let mut shell = LinuxSerialShell::builder(&port)
        .login_timeout(Duration::from_secs(30))
        .open()
        .await?;
    shell.activate().await?;

    let payload: &[u8] = b"{\"name\":\"example\",\"flags\":[\"a\",\"b\"]}\n";
    let remote = "/tmp/embedded-shell-example-config.json";

    info!(bytes = payload.len(), path = remote, "pushing payload");
    http::push(&mut shell, payload, remote).await?;

    info!(path = remote, "applying mode 0600");
    shell
        .run(&Command::new("chmod").arg("0600").arg(remote))
        .await?;

    info!(path = remote, "fetching back for verification");
    let fetched = http::fetch(&mut shell, remote).await?;

    if fetched == payload {
        println!("✓ byte-equal round-trip ({} bytes)", payload.len());
    } else {
        println!(
            "✗ mismatch: pushed {} bytes, fetched {} bytes",
            payload.len(),
            fetched.len()
        );
    }

    // Tidy up.
    shell.run(&Command::new("rm").arg("-f").arg(remote)).await?;
    shell.deactivate().await?;
    Ok(())
}

fn init_tracing() {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();
}
