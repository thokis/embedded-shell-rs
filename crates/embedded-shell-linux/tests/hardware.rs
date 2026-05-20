//! Hardware-in-the-loop tests for `embedded-shell-linux`.
//!
//! `#[ignore]`d by default — `cargo test` stays hardware-free. Run
//! explicitly:
//!
//! ```sh
//! cargo test -p embedded-shell-linux --test hardware --all-features \
//!     -- --ignored --nocapture
//! ```
//!
//! Add `RUST_LOG=embedded_shell=debug` to see the state machine.
//!
//! Port default: `/dev/ttyUSB0` (override via `EMBEDDED_SHELL_LINUX_PORT`).
//! Custom shell prompts: set `EMBEDDED_SHELL_LINUX_SHELL_PROMPT`.

use embedded_shell::shell::{Command, LinuxSerialShell, Shell};
use embedded_shell::test_utils;
use embedded_shell_linux::{fs, iputils};
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

const TEST_DIR: &str = "/tmp/embedded-shell-linux-hw-test";

#[cfg(feature = "coreutils")]
#[tokio::test]
#[ignore]
#[serial(linux_port)]
async fn fs_copy_rename_metadata_roundtrip() {
    init_logging();
    let mut shell = open_linux().await;

    // Set up a known starting state.
    let src = format!("{TEST_DIR}/source.txt");
    let dst_copy = format!("{TEST_DIR}/copy.txt");
    let dst_renamed = format!("{TEST_DIR}/renamed.txt");

    fs::create_dir_all(&mut shell, TEST_DIR)
        .await
        .expect("create_dir_all");
    shell
        .run(&Command::new("sh").args(["-c", &format!("printf 'twelve bytes' > {src}")]))
        .await
        .expect("seed source");
    fs::set_permissions(&mut shell, &src, "0644")
        .await
        .expect("chmod");

    // metadata reports the right size + type + mode on a regular file.
    let m = fs::metadata(&mut shell, &src).await.expect("metadata src");
    eprintln!("[hw] metadata({src}) = {m:?}");
    assert_eq!(m.size, 12, "source should be 12 bytes");
    assert!(m.file_type.is_file(), "source should be a regular file");
    assert_eq!(m.mode & 0o777, 0o644);

    // copy duplicates the file; source remains.
    fs::copy(&mut shell, &src, &dst_copy).await.expect("copy");
    let copied_bytes = fs::read_to_string(&mut shell, &dst_copy)
        .await
        .expect("read copy");
    assert_eq!(copied_bytes, "twelve bytes");
    let copy_meta = fs::metadata(&mut shell, &dst_copy)
        .await
        .expect("metadata copy");
    assert_eq!(copy_meta.size, 12);

    // rename moves the file; the old path is gone, the new one has the content.
    fs::rename(&mut shell, &dst_copy, &dst_renamed)
        .await
        .expect("rename");
    let renamed_bytes = fs::read_to_string(&mut shell, &dst_renamed)
        .await
        .expect("read renamed");
    assert_eq!(renamed_bytes, "twelve bytes");
    let renamed_missing = fs::metadata(&mut shell, &dst_copy).await;
    assert!(
        renamed_missing.is_err(),
        "old path should be gone after rename: {renamed_missing:?}"
    );

    // metadata on a directory reports Dir.
    let dir_meta = fs::metadata(&mut shell, TEST_DIR)
        .await
        .expect("metadata dir");
    assert!(
        dir_meta.file_type.is_dir(),
        "{TEST_DIR} should be a directory"
    );

    // Clean up.
    fs::remove_dir_all(&mut shell, TEST_DIR)
        .await
        .expect("cleanup");
    shell.deactivate().await.expect("deactivate");
}

#[cfg(feature = "iputils")]
#[tokio::test]
#[ignore]
#[serial(linux_port)]
async fn iputils_ping_reaches_device_loopback() {
    init_logging();
    let mut shell = open_linux().await;

    let stats = iputils::ping(&mut shell, "127.0.0.1", 3)
        .await
        .expect("ping 127.0.0.1");
    eprintln!("[hw] ping(127.0.0.1) = {stats:?}");
    assert!(stats.is_reachable(), "device should reach its own loopback");
    assert_eq!(stats.transmitted, 3);
    assert_eq!(stats.received, 3);
    assert_eq!(stats.loss_percent, 0.0);
    assert!(stats.rtt_avg_ms.is_some());

    shell.deactivate().await.expect("deactivate");
}

#[cfg(feature = "iputils")]
#[tokio::test]
#[ignore]
#[serial(linux_port)]
async fn iputils_ping_reports_total_loss_for_unroutable_target() {
    init_logging();
    let mut shell = open_linux().await;

    // 192.0.2.0/24 is reserved as TEST-NET-1 (RFC 5737) — guaranteed
    // not to be routed anywhere on the public internet.
    let stats = iputils::ping(&mut shell, "192.0.2.1", 2)
        .await
        .expect("ping unroutable");
    eprintln!("[hw] ping(192.0.2.1) = {stats:?}");
    assert!(!stats.is_reachable(), "TEST-NET-1 should not be reachable");
    assert_eq!(stats.received, 0);
    assert!(stats.rtt_avg_ms.is_none());

    shell.deactivate().await.expect("deactivate");
}

/// Auto-discovers the device's default gateway and arpings it.
///
/// Skipped if the device has no default route (no LAN connected) or
/// no `arping` binary — neither is a bug in the wrapper.
#[cfg(feature = "iputils")]
#[tokio::test]
#[ignore]
#[serial(linux_port)]
async fn iputils_arping_reaches_default_gateway() {
    init_logging();
    let mut shell = open_linux().await;

    // Discover the default gateway. Try iproute2 first (`ip route`),
    // fall back to net-tools (`route -n`). Either way we want the
    // gateway address in field-3-of-default-route format.
    let gw_result = shell
        .run(&Command::new("sh").args([
            "-c",
            "ip route show default 2>/dev/null | awk '/default/ {print $3; exit}' \
             || route -n 2>/dev/null | awk '/^0\\.0\\.0\\.0/ {print $2; exit}'",
        ]))
        .await;
    let gateway = match gw_result {
        Ok(r) => r.stdout().unwrap_or("").trim().to_string(),
        Err(e) => {
            eprintln!("[hw] could not look up default gateway, skipping: {e}");
            shell.deactivate().await.ok();
            return;
        }
    };
    if gateway.is_empty() {
        eprintln!("[hw] device has no default route, skipping arping test");
        shell.deactivate().await.expect("deactivate");
        return;
    }
    eprintln!("[hw] device default gateway = {gateway}");

    let stats = match iputils::arping(&mut shell, &gateway, 3).await {
        Ok(s) => s,
        Err(e) => {
            // arping needs CAP_NET_RAW; missing/non-suid → permission
            // denied. Not a wrapper bug.
            eprintln!("[hw] arping returned error (probably needs root), skipping: {e}");
            shell.deactivate().await.expect("deactivate");
            return;
        }
    };
    eprintln!("[hw] arping({gateway}) = {stats:?}");
    assert_eq!(stats.sent, 3, "should have sent 3 probes");
    assert!(stats.is_reachable(), "default gateway should answer ARP");
    assert!(
        stats.target_mac.is_some(),
        "should have learned the gateway's MAC"
    );

    shell.deactivate().await.expect("deactivate");
}
