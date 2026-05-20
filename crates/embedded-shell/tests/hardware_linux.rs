//! Hardware-in-the-loop tests against the **Linux** shell.
//!
//! `#[ignore]`d by default — `cargo test` stays hardware-free. Run
//! explicitly:
//!
//! ```sh
//! cargo test --test hardware_linux -- --ignored --nocapture
//! ```
//!
//! Add `RUST_LOG=embedded_shell=debug` (or `trace`) to see the state
//! machine and byte-level events.
//!
//! Each test's setup uses [`common::open_at_linux`], which probes the
//! device first and transitions through U-Boot if necessary — so this
//! binary can be run regardless of what state the device is in. The
//! U-Boot test binary ([`hardware_uboot`](../hardware_uboot.rs)) is the
//! mirror image.
//!
//! Port default: `/dev/ttyUSB0` (override via `EMBEDDED_SHELL_LINUX_PORT`).
//! Custom shell prompts: set `EMBEDDED_SHELL_LINUX_SHELL_PROMPT`.

mod common;

use std::time::Duration;

use embedded_shell::shell::{Command, LinuxSerialShell, Shell, ShellError};
use serial_test::serial;

use crate::common::{init_logging, linux_port, open_at_linux};

// ---------- smoke ----------

#[tokio::test]
#[ignore]
#[serial(linux_port)]
async fn linux_smoke() {
    init_logging();
    let mut shell = open_at_linux().await;

    let r = shell
        .run(&Command::new("uname").arg("-a"))
        .await
        .expect("uname -a");
    let stdout = r.stdout().unwrap_or("").to_owned();
    eprintln!("[hw] uname -a -> exit={}, stdout={stdout:?}", r.exit_code());
    assert_eq!(r.exit_code(), 0);
    assert!(stdout.starts_with("Linux"));

    shell.deactivate().await.expect("deactivate Linux shell");
}

// ---------- error variant mapping ----------

#[tokio::test]
#[ignore]
#[serial(linux_port)]
async fn linux_command_exit_codes() {
    init_logging();
    let mut shell = open_at_linux().await;

    let r = shell.run(&Command::new("true")).await.expect("true");
    assert_eq!(r.exit_code(), 0);

    let err = shell
        .run(&Command::new("false"))
        .await
        .expect_err("false should produce CommandFailed");
    match err {
        ShellError::CommandFailed(result) => assert_eq!(result.exit_code(), 1),
        other => panic!("expected CommandFailed, got {other:?}"),
    }

    let err = shell
        .run(&Command::new("xyz-nonexistent-binary-abc123"))
        .await
        .expect_err("nonexistent binary should produce CommandNotFound");
    match err {
        ShellError::CommandNotFound { command, result } => {
            eprintln!(
                "[hw] not-found: command={command:?}, exit={}",
                result.exit_code()
            );
            assert_eq!(command, "xyz-nonexistent-binary-abc123");
            assert_eq!(result.exit_code(), 127);
        }
        other => panic!("expected CommandNotFound, got {other:?}"),
    }

    let r = shell
        .run(&Command::new("false").allow_nonzero())
        .await
        .expect("false with allow_nonzero should be Ok");
    assert_eq!(r.exit_code(), 1);

    shell.deactivate().await.expect("deactivate");
}

// ---------- command features ----------

#[tokio::test]
#[ignore]
#[serial(linux_port)]
async fn linux_command_features() {
    init_logging();
    let mut shell = open_at_linux().await;

    let r = shell
        .run(&Command::new("sh").args(["-c", "echo one two three | wc -w"]))
        .await
        .expect("pipe via sh -c");
    eprintln!("[hw] pipe stdout={:?}", r.stdout());
    assert_eq!(r.exit_code(), 0);
    assert_eq!(r.stdout().unwrap_or("").trim(), "3");

    let r = shell
        .run(&Command::new("sh").args(["-c", "echo oops 1>&2"]))
        .await
        .expect("stderr via sh -c");
    eprintln!(
        "[hw] stderr stdout={:?} stderr={:?}",
        r.stdout(),
        r.stderr()
    );
    assert_eq!(r.exit_code(), 0);
    assert!(r.stdout().is_none() || r.stdout().unwrap().is_empty());
    assert_eq!(r.stderr().unwrap_or("").trim(), "oops");

    let r = shell
        .run(&Command::new("pwd").cwd("/tmp"))
        .await
        .expect("pwd with cwd=/tmp");
    eprintln!("[hw] pwd in /tmp -> stdout={:?}", r.stdout());
    assert_eq!(r.exit_code(), 0);
    assert_eq!(r.stdout().unwrap_or("").trim(), "/tmp");

    let r = shell
        .run(&Command::new("echo").arg("hello world from rust"))
        .await
        .expect("echo with quoted arg");
    eprintln!("[hw] echo quoted -> stdout={:?}", r.stdout());
    assert_eq!(r.exit_code(), 0);
    assert_eq!(r.stdout().unwrap_or("").trim(), "hello world from rust");

    shell.deactivate().await.expect("deactivate");
}

// ---------- timeout ----------

#[tokio::test]
#[ignore]
#[serial(linux_port)]
async fn linux_timeout() {
    init_logging();
    let mut shell = open_at_linux().await;

    let started = std::time::Instant::now();
    let err = shell
        .run(
            &Command::new("sleep")
                .arg("10")
                .timeout(Duration::from_secs(2)),
        )
        .await
        .expect_err("sleep 10 with timeout=2s should produce Timeout");
    let wall = started.elapsed();
    eprintln!("[hw] sleep 10 with 2s timeout took {wall:?}");

    match err {
        ShellError::Timeout { duration, result } => {
            assert_eq!(duration, Duration::from_secs(2));
            assert_eq!(result.exit_code(), 124, "timeout(1) returns exit 124");
        }
        other => panic!("expected Timeout, got {other:?}"),
    }

    assert!(
        wall < Duration::from_secs(6),
        "timeout wall-clock too high: {wall:?}"
    );

    shell.deactivate().await.expect("deactivate");
}

// ---------- argv quoting against a real bash ----------
//
// Hardware-only because the value of this test is the composition of
// `Command::posix_quote` (unit-tested) with how real bash interprets the
// resulting wire string. A quoting bug would let shell metacharacters in
// argv arguments execute as separate commands on the device.
#[tokio::test]
#[ignore]
#[serial(linux_port)]
async fn linux_argv_quoting_against_real_bash() {
    init_logging();
    let mut shell = open_at_linux().await;

    let cases: &[(&str, &str)] = &[
        ("a;b", "a;b"),
        ("a|b", "a|b"),
        ("a&b", "a&b"),
        ("a&&b", "a&&b"),
        ("$HOME", "$HOME"),
        ("$(whoami)", "$(whoami)"),
        ("`whoami`", "`whoami`"),
        ("it's quoted", "it's quoted"),
        ("space  here", "space  here"),
    ];

    for (raw, expected) in cases {
        let r = shell
            .run(&Command::new("printf").args(["%s", raw]))
            .await
            .unwrap_or_else(|e| panic!("printf {raw:?}: {e}"));
        let got = r.stdout().unwrap_or("");
        eprintln!("[hw] printf {raw:?} -> stdout={got:?}");
        assert_eq!(got, *expected, "argv quoting failed for {raw:?}");
    }

    shell.deactivate().await.expect("deactivate");
}

// ---------- reconnect() happy path ----------

#[tokio::test]
#[ignore]
#[serial(linux_port)]
async fn linux_reconnect_happy_path() {
    init_logging();
    let mut shell = open_at_linux().await;

    let r = shell.run(&Command::new("true")).await.expect("true #1");
    assert_eq!(r.exit_code(), 0);

    eprintln!("[hw] calling reconnect()...");
    let started = std::time::Instant::now();
    shell.reconnect().await.expect("reconnect");
    eprintln!("[hw] reconnect took {:?}", started.elapsed());

    let r = shell.run(&Command::new("true")).await.expect("true #2");
    assert_eq!(r.exit_code(), 0);

    let r = shell
        .run(&Command::new("uname").arg("-a"))
        .await
        .expect("uname after reconnect");
    assert!(r.stdout().unwrap_or("").starts_with("Linux"));

    shell.deactivate().await.expect("deactivate");
}

// ---------- sustained output ----------

#[tokio::test]
#[ignore]
#[serial(linux_port)]
async fn linux_long_output() {
    init_logging();
    let mut shell = open_at_linux().await;

    let r = shell
        .run(
            &Command::new("seq")
                .args(["1", "200"])
                .timeout(Duration::from_secs(10)),
        )
        .await
        .expect("seq 1 200");
    let stdout = r.stdout().unwrap_or("");
    let lines: Vec<&str> = stdout.lines().collect();
    eprintln!(
        "[hw] seq 1 200 -> exit={}, lines={}",
        r.exit_code(),
        lines.len()
    );
    assert_eq!(r.exit_code(), 0);
    assert_eq!(lines.len(), 200);
    assert_eq!(lines.first().copied(), Some("1"));
    assert_eq!(lines.last().copied(), Some("200"));

    shell.deactivate().await.expect("deactivate");
}

// ---------- reboot (actually reboots the device, ~30-60s) ----------
//
// High value because it's the only test that exercises the full
// post-reboot activate path: kernel boot log → fresh login prompt →
// re-authenticate → fresh shell.
#[tokio::test]
#[ignore]
#[serial(linux_port)]
async fn linux_reboot() {
    init_logging();
    // We deliberately bypass open_at_linux() here because reboot needs a
    // longer login_timeout than the helper's default to absorb the boot
    // log between issuing `reboot` and the device coming back up.
    let port = linux_port();
    eprintln!("[hw] connecting to Linux device at {port} (reboot test, 240s login timeout)");
    // 240 s is generous — the post-reboot activate has to absorb the
    // full boot log up to the login prompt. 120 s was on the edge on
    // boards that take their time coming back from a kernel-issued
    // reboot.
    let mut shell = LinuxSerialShell::builder(&port)
        .login_timeout(Duration::from_secs(240))
        .open()
        .await
        .unwrap_or_else(|e| panic!("open: {e}"));
    if let Err(e) = shell.activate().await {
        let buf = shell.console_buffer();
        panic!("activate: {e}\n--- buffer ---\n{buf}");
    }

    let r = shell
        .run(&Command::new("cut").args(["-d", " ", "-f", "1", "/proc/uptime"]))
        .await
        .expect("uptime #1");
    let uptime_before: f64 = r.stdout().unwrap_or("0").trim().parse().unwrap_or(0.0);
    eprintln!("[hw] uptime before reboot: {uptime_before}s");

    eprintln!("[hw] rebooting device (this takes 30-60s)...");
    let elapsed = shell.reboot().await.expect("reboot");
    eprintln!("[hw] reboot completed in {elapsed:?}");

    let buffer = shell.console_buffer();
    eprintln!(
        "[hw] ===== console buffer during reboot ({} bytes) =====",
        buffer.len()
    );
    eprintln!("{buffer}");
    eprintln!("[hw] ===== end console buffer =====");

    let r = shell
        .run(&Command::new("cut").args(["-d", " ", "-f", "1", "/proc/uptime"]))
        .await
        .expect("uptime #2");
    let uptime_after: f64 = r.stdout().unwrap_or("0").trim().parse().unwrap_or(f64::MAX);
    eprintln!("[hw] uptime after reboot: {uptime_after}s");
    assert!(
        uptime_after < uptime_before,
        "uptime after reboot ({uptime_after}) should be less than before ({uptime_before})"
    );

    shell.deactivate().await.expect("deactivate");
}
