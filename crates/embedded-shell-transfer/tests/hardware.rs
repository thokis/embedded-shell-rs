//! Hardware-in-the-loop integration tests for the file-transfer
//! crate.
//!
//! `#[ignore]`d by default so plain `cargo test` stays hardware-free.
//! Run explicitly:
//!
//! ```sh
//! cargo test --test hardware --features serial,http -- --ignored --nocapture
//! ```
//!
//! The HTTP tests additionally require the device to have a network
//! route back to the host's default-route interface.
//!
//! Add `RUST_LOG=embedded_shell=debug` to see the state machine.
//!
//! Port default: `/dev/ttyUSB0` (override via `EMBEDDED_SHELL_LINUX_PORT`).
//! Custom shell prompts: set `EMBEDDED_SHELL_LINUX_SHELL_PROMPT`.

use embedded_shell::shell::{LinuxSerialShell, Shell};
use embedded_shell::test_utils;
#[cfg(feature = "http")]
use embedded_shell_transfer::http;
#[cfg(feature = "serial")]
use embedded_shell_transfer::serial;
use serial_test::serial;
use tracing_subscriber::{EnvFilter, prelude::*};

fn init_logging() {
    let _ = tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .try_init();
}

fn linux_port() -> String {
    std::env::var("EMBEDDED_SHELL_LINUX_PORT").unwrap_or_else(|_| "/dev/ttyUSB0".to_string())
}

fn shell_prompt() -> Option<String> {
    std::env::var("EMBEDDED_SHELL_LINUX_SHELL_PROMPT").ok()
}

/// Opens a Linux shell using the state-aware probe from
/// [`embedded_shell::test_utils::open_at_linux`]. Transitions through
/// U-Boot if the device is currently in the bootloader.
async fn open_linux() -> LinuxSerialShell {
    test_utils::open_at_linux(&linux_port(), shell_prompt().as_deref()).await
}

const TEST_PATH: &str = "/tmp/embedded-shell-transfer-hw-test";

#[cfg(feature = "serial")]
#[tokio::test]
#[ignore]
#[serial(linux_port)]
async fn serial_push_then_fetch_roundtrips_a_small_payload() {
    init_logging();
    let mut shell = open_linux().await;

    let original: &[u8] = b"hello from the host over a serial line!\n";
    serial::push(&mut shell, original, TEST_PATH)
        .await
        .expect("push");
    eprintln!("[hw] pushed {} bytes to {TEST_PATH}", original.len());

    let fetched = serial::fetch(&mut shell, TEST_PATH).await.expect("fetch");
    eprintln!("[hw] fetched {} bytes", fetched.len());
    assert_eq!(&fetched, original);

    // Clean up.
    let _ = shell
        .run(
            &embedded_shell::shell::Command::new("rm")
                .arg("-f")
                .arg(TEST_PATH),
        )
        .await;

    shell.deactivate().await.expect("deactivate");
}

#[cfg(feature = "serial")]
#[tokio::test]
#[ignore]
#[serial(linux_port)]
async fn serial_push_handles_binary_bytes() {
    init_logging();
    let mut shell = open_linux().await;

    // All 256 byte values — verifies the base64 path doesn't mangle anything.
    let original: Vec<u8> = (0..=255u8).collect();
    serial::push(&mut shell, &original, TEST_PATH)
        .await
        .expect("push");

    let fetched = serial::fetch(&mut shell, TEST_PATH).await.expect("fetch");
    assert_eq!(fetched, original);

    let _ = shell
        .run(
            &embedded_shell::shell::Command::new("rm")
                .arg("-f")
                .arg(TEST_PATH),
        )
        .await;

    shell.deactivate().await.expect("deactivate");
}

#[cfg(feature = "serial")]
#[tokio::test]
#[ignore]
#[serial(linux_port)]
async fn serial_push_rejects_oversized_payload() {
    init_logging();
    let mut shell = open_linux().await;

    let huge = vec![0u8; embedded_shell_transfer::serial::MAX_PUSH_BYTES + 1];
    let err = serial::push(&mut shell, &huge, TEST_PATH)
        .await
        .expect_err("oversized push should fail");
    assert!(matches!(
        err,
        embedded_shell_transfer::TransferError::PayloadTooLarge(_)
    ));

    shell.deactivate().await.expect("deactivate");
}

#[cfg(feature = "http")]
#[tokio::test]
#[ignore]
#[serial(linux_port)]
async fn http_push_then_fetch_roundtrips_a_small_payload() {
    init_logging();
    let mut shell = open_linux().await;

    let original: &[u8] = b"hello from the host over HTTP!\n";
    http::push(&mut shell, original, TEST_PATH)
        .await
        .expect("push");
    eprintln!("[hw] pushed {} bytes to {TEST_PATH}", original.len());

    let fetched = http::fetch(&mut shell, TEST_PATH).await.expect("fetch");
    eprintln!("[hw] fetched {} bytes", fetched.len());
    assert_eq!(&fetched, original);

    let _ = shell
        .run(
            &embedded_shell::shell::Command::new("rm")
                .arg("-f")
                .arg(TEST_PATH),
        )
        .await;

    shell.deactivate().await.expect("deactivate");
}

#[cfg(feature = "http")]
#[tokio::test]
#[ignore]
#[serial(linux_port)]
async fn http_push_carries_a_large_payload() {
    init_logging();
    let mut shell = open_linux().await;

    // 1 MiB — comfortably past serial's 64 KiB cap; smoke-tests that
    // the HTTP transport can move real-world-size payloads.
    let payload: Vec<u8> = (0..1024 * 1024).map(|i| (i % 251) as u8).collect();
    http::push(&mut shell, &payload, TEST_PATH)
        .await
        .expect("push");
    eprintln!("[hw] pushed {} bytes to {TEST_PATH}", payload.len());

    let fetched = http::fetch(&mut shell, TEST_PATH).await.expect("fetch");
    assert_eq!(fetched.len(), payload.len());
    assert_eq!(fetched, payload);

    let _ = shell
        .run(
            &embedded_shell::shell::Command::new("rm")
                .arg("-f")
                .arg(TEST_PATH),
        )
        .await;

    shell.deactivate().await.expect("deactivate");
}
