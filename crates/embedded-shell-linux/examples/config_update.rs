//! Atomic config-update pattern: write a small config to the device
//! using `fs::write_atomic`, then read it back to verify.
//!
//! Run with:
//! ```sh
//! cargo run --example config_update -- /dev/ttyUSB0
//! RUST_LOG=info cargo run --example config_update -- /dev/ttyUSB0
//! ```
//!
//! Writes to `/tmp/embedded-shell-example/config.json` — deliberately
//! a sandbox path. In a real provisioning flow you'd write to a path
//! like `/etc/myapp/config.json` then `systemctl restart myapp`. The
//! atomic-write guarantee means a reader concurrent with the update
//! sees either the old content or the new — never a torn write.

use std::time::Duration;

use embedded_shell::shell::{LinuxSerialShell, Shell};
use embedded_shell_linux::fs;
use tracing::info;
use tracing_subscriber::{EnvFilter, prelude::*};

const REMOTE_DIR: &str = "/tmp/embedded-shell-example";
const REMOTE_PATH: &str = "/tmp/embedded-shell-example/config.json";

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

    let new_config = br#"{"version":2,"feature_flags":["beta-routing","metrics"]}"#;

    info!(
        path = REMOTE_PATH,
        bytes = new_config.len(),
        "writing config"
    );
    fs::create_dir_all(&mut shell, REMOTE_DIR).await?;
    fs::write_atomic(&mut shell, REMOTE_PATH, new_config).await?;
    fs::set_permissions(&mut shell, REMOTE_PATH, "0600").await?;

    info!(path = REMOTE_PATH, "reading back to verify");
    let got = fs::read(&mut shell, REMOTE_PATH).await?;
    if got == new_config {
        println!(
            "✓ atomic write verified, {} bytes at {REMOTE_PATH}",
            got.len()
        );
    } else {
        println!(
            "✗ mismatch: wrote {} bytes, read back {}",
            new_config.len(),
            got.len()
        );
    }

    // In production you'd typically follow this with a service
    // restart, e.g.:
    //   systemd::restart(&mut shell, "myapp.service").await?;

    shell.deactivate().await?;
    Ok(())
}

fn init_tracing() {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();
}
