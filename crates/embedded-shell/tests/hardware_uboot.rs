//! Hardware-in-the-loop tests against the **U-Boot** shell.
//!
//! `#[ignore]`d by default — `cargo test` stays hardware-free. Run
//! explicitly:
//!
//! ```sh
//! cargo test --test hardware_uboot -- --ignored --nocapture
//! ```
//!
//! Add `RUST_LOG=embedded_shell=debug` (or `trace`) to see the state
//! machine and byte-level events.
//!
//! Each test's setup uses [`common::open_at_uboot`], which probes the
//! device first and (if it's currently in Linux) issues `reboot` and
//! catches the autoboot countdown — so this binary can be run
//! regardless of what state the device is in. The Linux test binary
//! ([`hardware_linux`](../hardware_linux.rs)) is the mirror image.
//!
//! Port default: `/dev/ttyUSB0` (override via `EMBEDDED_SHELL_UBOOT_PORT`).

mod common;

use std::time::Duration;

use embedded_shell::shell::{Command, Shell, ShellError};
use serial_test::serial;

use crate::common::{init_logging, open_at_uboot};

// ---------- smoke ----------

#[tokio::test]
#[ignore]
#[serial(uboot_port)]
async fn uboot_smoke() {
    init_logging();
    let mut shell = open_at_uboot().await;

    let r = shell.run(&Command::new("version")).await.expect("version");
    let stdout = r.stdout().unwrap_or("").to_owned();
    eprintln!("[hw] version -> exit={}, stdout={stdout:?}", r.exit_code());
    assert_eq!(r.exit_code(), 0);
    assert!(stdout.contains("U-Boot"));

    shell.deactivate().await.expect("deactivate U-Boot shell");
}

// ---------- error variant mapping ----------

#[tokio::test]
#[ignore]
#[serial(uboot_port)]
async fn uboot_command_exit_codes() {
    init_logging();
    let mut shell = open_at_uboot().await;

    let r = shell.run(&Command::new("version")).await.expect("version");
    assert_eq!(r.exit_code(), 0);

    // Unknown command — U-Boot prints "Unknown command 'X' - try 'help'"
    // and returns non-zero. Should map to CommandFailed.
    let err = shell
        .run(&Command::new("xyz_definitely_not_a_real_uboot_command"))
        .await
        .expect_err("bogus u-boot command should fail");
    match err {
        ShellError::CommandFailed(result) => {
            eprintln!(
                "[hw] u-boot bogus -> exit={}, stdout={:?}",
                result.exit_code(),
                result.stdout()
            );
            assert!(result.exit_code() != 0);
        }
        other => panic!("expected CommandFailed, got {other:?}"),
    }

    shell.deactivate().await.expect("deactivate");
}

// ---------- multi-line output ----------

#[tokio::test]
#[ignore]
#[serial(uboot_port)]
async fn uboot_help_returns_multiline_output() {
    init_logging();
    let mut shell = open_at_uboot().await;

    let r = shell
        .run(&Command::new("help").timeout(Duration::from_secs(10)))
        .await
        .expect("help");
    let stdout = r.stdout().unwrap_or("");
    eprintln!(
        "[hw] help -> exit={}, lines={}",
        r.exit_code(),
        stdout.lines().count()
    );
    assert_eq!(r.exit_code(), 0);
    assert!(
        stdout.lines().count() > 3,
        "expected multi-line help output, got {stdout:?}"
    );

    shell.deactivate().await.expect("deactivate");
}
