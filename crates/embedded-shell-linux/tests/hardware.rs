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
#[cfg(feature = "iproute2")]
use embedded_shell_linux::iproute2;
#[cfg(feature = "systemd")]
use embedded_shell_linux::journalctl;
#[cfg(feature = "modemmanager")]
use embedded_shell_linux::modemmanager;
#[cfg(feature = "networkmanager")]
use embedded_shell_linux::networkmanager;
#[cfg(feature = "systemd")]
use embedded_shell_linux::systemd;
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

/// Probes the device for systemd. If `systemctl --version` succeeds,
/// fetches the status of `systemd-journald.service` (universal on any
/// systemd-running Linux) and asserts the basics. Skips if the device
/// runs a non-systemd init (sysvinit, OpenRC, runit, busybox-init,
/// …) — that's a fact about the device image, not a wrapper bug.
#[cfg(feature = "systemd")]
#[tokio::test]
#[ignore]
#[serial(linux_port)]
async fn systemd_status_against_journald() {
    init_logging();
    let mut shell = open_linux().await;

    let probe = shell
        .run(
            &Command::new("sh")
                .args(["-c", "command -v systemctl >/dev/null 2>&1"])
                .allow_nonzero(),
        )
        .await
        .expect("probe for systemctl");
    if !probe.is_success() {
        eprintln!("[hw] device has no systemctl — skipping systemd tests");
        shell.deactivate().await.expect("deactivate");
        return;
    }

    let status = systemd::status(&mut shell, "systemd-journald.service")
        .await
        .expect("systemd-journald status");
    eprintln!("[hw] systemd-journald status: {status:?}");
    assert_eq!(
        status.load_state, "loaded",
        "journald should always be loaded on systemd"
    );
    assert!(
        !status.description.is_empty(),
        "journald should report a Description"
    );

    let active = systemd::is_active(&mut shell, "systemd-journald.service")
        .await
        .expect("is_active");
    assert!(active, "journald should be active on a systemd host");

    shell.deactivate().await.expect("deactivate");
}

/// Reads the tail of the journal from the device. Skips gracefully
/// if journalctl is missing or the current user can't read the
/// journal.
#[cfg(feature = "systemd")]
#[tokio::test]
#[ignore]
#[serial(linux_port)]
async fn journalctl_tail_against_device() {
    init_logging();
    let mut shell = open_linux().await;

    // Probe: does this device have journalctl and can we read it?
    let probe = shell
        .run(
            &Command::new("sh")
                .args(["-c", "journalctl -o json -n 1 >/dev/null 2>&1"])
                .allow_nonzero(),
        )
        .await
        .expect("probe journalctl");
    if !probe.is_success() {
        eprintln!("[hw] device has no readable journalctl — skipping");
        shell.deactivate().await.expect("deactivate");
        return;
    }

    let entries = journalctl::tail(&mut shell, 5)
        .await
        .expect("journalctl tail");
    eprintln!("[hw] {} journal entries", entries.len());
    for e in &entries {
        eprintln!(
            "[hw]   {:?} prio={:?} unit={:?}: {}",
            e.timestamp, e.priority, e.unit, e.message
        );
    }
    // Don't assert > 0 — a freshly-booted device with restrictive
    // journal permissions could legitimately return empty. The
    // value of the test is that the call parsed cleanly.

    shell.deactivate().await.expect("deactivate");
}

/// Inspects NetworkManager state on the device. Skips gracefully if
/// the device doesn't have nmcli or the daemon isn't running.
#[cfg(feature = "networkmanager")]
#[tokio::test]
#[ignore]
#[serial(linux_port)]
async fn networkmanager_inspects_device_connections() {
    init_logging();
    let mut shell = open_linux().await;

    let probe = shell
        .run(
            &Command::new("sh")
                .args(["-c", "nmcli -t -f NAME connection show >/dev/null 2>&1"])
                .allow_nonzero(),
        )
        .await
        .expect("probe nmcli");
    if !probe.is_success() {
        eprintln!("[hw] device has no working nmcli — skipping NM tests");
        shell.deactivate().await.expect("deactivate");
        return;
    }

    let conns = networkmanager::connections(&mut shell)
        .await
        .expect("connections");
    eprintln!("[hw] {} connections on device", conns.len());
    for c in &conns {
        eprintln!(
            "[hw]   {} ({}, {}) -> {:?}",
            c.name, c.uuid, c.kind, c.device
        );
    }

    let active = networkmanager::active_connections(&mut shell)
        .await
        .expect("active connections");
    eprintln!("[hw] {} active connections", active.len());
    // Every active connection should be bound to a device.
    for c in &active {
        assert!(
            c.is_active(),
            "connection in --active list should have a device: {c:?}"
        );
    }

    let devs = networkmanager::devices(&mut shell).await.expect("devices");
    eprintln!("[hw] {} devices on device", devs.len());
    for d in &devs {
        eprintln!(
            "[hw]   {} ({}) state={} -> {:?}",
            d.name, d.kind, d.state, d.connection
        );
    }
    assert!(
        devs.iter().any(|d| d.name == "lo"),
        "device should have a loopback in nmcli device status"
    );

    shell.deactivate().await.expect("deactivate");
}

/// Reads link / address / route state from the device. Skips if the
/// device's `ip` doesn't speak JSON (old busybox without
/// `CONFIG_FEATURE_IP_JSON`).
#[cfg(feature = "iproute2")]
#[tokio::test]
#[ignore]
#[serial(linux_port)]
async fn iproute2_inspects_device_network_state() {
    init_logging();
    let mut shell = open_linux().await;

    // Probe: does this device have `ip -j` support?
    let probe = shell
        .run(
            &Command::new("sh")
                .args(["-c", "ip -j link show >/dev/null 2>&1"])
                .allow_nonzero(),
        )
        .await
        .expect("probe ip -j");
    if !probe.is_success() {
        eprintln!("[hw] device has no `ip -j` JSON support — skipping iproute2 tests");
        shell.deactivate().await.expect("deactivate");
        return;
    }

    let links = iproute2::links(&mut shell).await.expect("links");
    eprintln!("[hw] {} links on device", links.len());
    for l in &links {
        eprintln!(
            "[hw]   {} idx={} state={} mtu={} mac={:?}",
            l.name, l.index, l.operstate, l.mtu, l.mac
        );
    }
    assert!(
        links.iter().any(|l| l.name == "lo"),
        "device should have a loopback interface"
    );

    let addrs = iproute2::addresses(&mut shell).await.expect("addresses");
    eprintln!("[hw] {} addresses on device", addrs.len());
    for a in &addrs {
        eprintln!(
            "[hw]   {} {}/{} on {} ({})",
            a.family, a.address, a.prefix_len, a.interface, a.scope
        );
    }
    assert!(
        addrs
            .iter()
            .any(|a| a.address == "127.0.0.1" && a.interface == "lo"),
        "device should have 127.0.0.1 on lo"
    );

    let routes = iproute2::routes(&mut shell).await.expect("routes");
    eprintln!("[hw] {} routes on device", routes.len());
    for r in &routes {
        eprintln!(
            "[hw]   {} via {:?} dev {} metric {:?}",
            r.destination, r.gateway, r.interface, r.metric
        );
    }

    shell.deactivate().await.expect("deactivate");
}

/// Inspects ModemManager state on the device. Skips gracefully if
/// mmcli isn't installed or the daemon isn't running. Useful even
/// when the device has zero modems — exercises the empty-list
/// parse path.
#[cfg(feature = "modemmanager")]
#[tokio::test]
#[ignore]
#[serial(linux_port)]
async fn modemmanager_inspects_device_modems() {
    init_logging();
    let mut shell = open_linux().await;

    let probe = shell
        .run(
            &Command::new("sh")
                .args(["-c", "mmcli -L -J >/dev/null 2>&1"])
                .allow_nonzero(),
        )
        .await
        .expect("probe mmcli");
    if !probe.is_success() {
        eprintln!("[hw] device has no working mmcli — skipping MM tests");
        shell.deactivate().await.expect("deactivate");
        return;
    }

    let indices = modemmanager::list_modems(&mut shell)
        .await
        .expect("list_modems");
    eprintln!("[hw] {} modem(s) on device", indices.len());

    for idx in indices {
        let m = modemmanager::modem(&mut shell, idx).await.expect("modem");
        eprintln!(
            "[hw]   modem {}: {} {} (rev {}) state={} tech={:?} signal={:?}% op={:?}",
            m.index,
            m.manufacturer.as_deref().unwrap_or("?"),
            m.model.as_deref().unwrap_or("?"),
            m.revision.as_deref().unwrap_or("?"),
            m.state,
            m.access_technologies,
            m.signal_quality,
            m.operator_name,
        );
        // Sanity check on the basics we expect from any real modem.
        assert!(!m.state.is_empty(), "modem state should not be empty");
    }

    shell.deactivate().await.expect("deactivate");
}
