//! Two ways to read a remote file: `read_to_string` for text,
//! `read` for raw bytes. Pass the path as the second argument.
//!
//! Run with:
//! ```sh
//! cargo run --example file_read -- /dev/ttyUSB0 /etc/hostname
//! cargo run --example file_read -- /dev/ttyUSB0 /etc/machine-id
//! cargo run --example file_read -- /dev/ttyUSB0 /sys/class/net/eth0/address
//! ```
//!
//! Demonstrates [`fs::read_to_string`] (UTF-8 with lossy
//! replacement) and [`fs::read`] (binary-safe via base64 round-trip
//! through the shell). Use the latter when you can't be sure the
//! file is UTF-8 — kernel sysfs values, binary blobs, etc.

use std::time::Duration;

use embedded_shell::shell::{LinuxSerialShell, Shell};
use embedded_shell_linux::fs;
use tracing_subscriber::{EnvFilter, prelude::*};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    init_tracing();
    let mut args = std::env::args().skip(1);
    let port = args.next().unwrap_or_else(|| "/dev/ttyUSB0".to_string());
    let path = args.next().unwrap_or_else(|| "/etc/hostname".to_string());

    let mut shell = LinuxSerialShell::builder(&port)
        .login_timeout(Duration::from_secs(30))
        .open()
        .await?;
    shell.activate().await?;

    println!("==== {path} ====");

    // Text path: useful for config files, /etc/hostname, /proc/* —
    // anything you know is UTF-8.
    let text = fs::read_to_string(&mut shell, &path).await?;
    println!("\nAs text ({} chars):", text.len());
    println!("{text}");

    // Bytes path: safe regardless of content. Use this for binary
    // files, anything you're hashing, or anywhere a single byte
    // matters.
    let bytes = fs::read(&mut shell, &path).await?;
    println!("As bytes ({} bytes):", bytes.len());
    print!("hex:");
    for (i, b) in bytes.iter().enumerate() {
        if i > 0 && i % 16 == 0 {
            print!("\n    ");
        }
        print!(" {b:02x}");
    }
    println!();

    shell.deactivate().await?;
    Ok(())
}

fn init_tracing() {
    tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .init();
}
