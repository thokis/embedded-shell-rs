//! Raw-serial transport: base64-encoded payload over the shell line.
//!
//! Push and fetch implemented entirely through the shell — the host
//! base64-encodes the bytes locally, ships them as a single `sh -c`
//! command, and the device decodes them on the other side. Fetch is
//! the inverse: the device base64-encodes the file, the shell
//! captures the output, and the host decodes it.
//!
//! # When to use this
//!
//! When the device has no network connectivity yet (initial
//! provisioning, no DHCP / DNS / SSH configured) but you can still
//! reach it via serial. Typical example: pushing a network-config blob
//! to a freshly-flashed device before its first boot can reach the
//! network.
//!
//! # Size limits
//!
//! The push payload travels as a single shell command, bounded by the
//! device's `ARG_MAX` (typically 128 KiB on Linux, smaller on busybox).
//! After base64 expansion (×4/3) plus shell-command wrapping, the
//! practical raw-payload limit is **[`MAX_PUSH_BYTES`] (64 KiB)** by
//! default. Larger payloads return [`TransferError::PayloadTooLarge`]
//! — use [`crate::http`] for those.
//!
//! Fetch is bounded by the device-side shell's output capacity (the
//! transport's `console_buffer_cap`, default 1 MiB).
//!
//! # Performance
//!
//! At 115200 baud with base64's 4:3 expansion, effective throughput
//! is about **10 KB/s of raw payload**. A 32 KiB push takes around
//! 4–5 seconds end-to-end including framing overhead.

use std::path::Path;
use std::time::Instant;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use embedded_shell::shell::{Command, LinuxShell};
use sha2::{Digest, Sha256};
use tracing::info;

use crate::error::{Result, TransferError};

/// Maximum push payload size in raw bytes (before base64 expansion).
///
/// Conservative default chosen to fit comfortably in `ARG_MAX` on
/// busybox-based devices: 64 KiB raw → ~87 KiB base64 → ~88 KiB shell
/// command. Larger payloads should use [`crate::http`].
pub const MAX_PUSH_BYTES: usize = 64 * 1024;

/// Push `data` to `remote_path` on the device, verifying the SHA-256
/// digest after transfer.
///
/// The full payload travels in a single `sh -c` command:
///
/// ```text
/// printf '%s' '<base64-of-data>' | base64 -d > <remote_path>
/// ```
///
/// After the push, the host runs `sha256sum` on the device and
/// compares the result against the locally-computed digest. A
/// mismatch returns [`TransferError::ChecksumMismatch`].
///
/// # Errors
///
/// - [`TransferError::PayloadTooLarge`] when `data.len() >
///   MAX_PUSH_BYTES`.
/// - [`TransferError::Shell`] if the device-side decode, write, or
///   `sha256sum` fails.
/// - [`TransferError::ChecksumMismatch`] when the device's hash
///   doesn't match the local one.
///
/// # Example
///
/// ```ignore
/// use embedded_shell_transfer::serial;
///
/// let config = br#"{"key":"value"}"#;
/// serial::push(&mut shell, config, "/etc/app/config.json").await?;
/// ```
pub async fn push(
    shell: &mut dyn LinuxShell,
    data: &[u8],
    remote_path: impl AsRef<Path>,
) -> Result<()> {
    if data.len() > MAX_PUSH_BYTES {
        return Err(TransferError::PayloadTooLarge(format!(
            "{} bytes exceeds serial-transport limit of {} bytes; use the http transport for larger payloads",
            data.len(),
            MAX_PUSH_BYTES,
        )));
    }

    let remote = remote_path.as_ref().to_string_lossy().into_owned();
    let encoded = STANDARD.encode(data);
    let expected_hash = sha256_hex(data);

    info!(
        bytes = data.len(),
        wire_bytes = encoded.len(),
        path = %remote,
        "serial push starting",
    );
    let started = Instant::now();

    // Base64 alphabet contains no shell metacharacters, so single-quoting
    // is safe; the only concern is the remote path, which we quote
    // separately.
    let script = format!(
        "printf '%s' '{}' | base64 -d > {}",
        encoded,
        sh_single_quote(&remote),
    );
    shell.run(&Command::new("sh").args(["-c", &script])).await?;

    // Verify by re-hashing on the device.
    let sum = shell.run(&Command::new("sha256sum").arg(&remote)).await?;
    let actual_hash = parse_sha256_output(sum.stdout().unwrap_or(""))?;

    if actual_hash != expected_hash {
        return Err(TransferError::ChecksumMismatch {
            expected: expected_hash,
            actual: actual_hash,
        });
    }

    info!(
        bytes = data.len(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        path = %remote,
        "serial push verified",
    );

    Ok(())
}

/// Fetch the contents of `remote_path` from the device into a
/// `Vec<u8>`.
///
/// The device base64-encodes the file (`base64 <path> | tr -d '\\n\\r'`)
/// and the host decodes the result. Bounded by the transport's
/// console-buffer cap (default 1 MiB).
///
/// # Errors
///
/// - [`TransferError::Shell`] if `base64` or the file doesn't exist on
///   the device.
/// - [`TransferError::Io`] if the device's output isn't valid base64
///   (extremely rare; would indicate transport corruption or a
///   non-coreutils `base64` implementation).
///
/// # Example
///
/// ```ignore
/// use embedded_shell_transfer::serial;
///
/// let bytes = serial::fetch(&mut shell, "/etc/version").await?;
/// let text = String::from_utf8_lossy(&bytes);
/// println!("device version: {text}");
/// ```
pub async fn fetch(shell: &mut dyn LinuxShell, remote_path: impl AsRef<Path>) -> Result<Vec<u8>> {
    let remote = remote_path.as_ref().to_string_lossy().into_owned();

    info!(path = %remote, "serial fetch starting");
    let started = Instant::now();

    let script = format!("base64 {} | tr -d '\\n\\r'", sh_single_quote(&remote),);
    let result = shell.run(&Command::new("sh").args(["-c", &script])).await?;
    let encoded = result.stdout().unwrap_or("").trim();
    let decoded = STANDARD.decode(encoded).map_err(|e| {
        TransferError::Io(std::io::Error::other(format!(
            "invalid base64 from device: {e}"
        )))
    })?;

    info!(
        bytes = decoded.len(),
        wire_bytes = encoded.len(),
        elapsed_ms = started.elapsed().as_millis() as u64,
        path = %remote,
        "serial fetch complete",
    );

    Ok(decoded)
}

/// SHA-256 of `data` formatted as lowercase hex.
fn sha256_hex(data: &[u8]) -> String {
    let mut h = Sha256::new();
    h.update(data);
    let digest = h.finalize();
    let mut hex = String::with_capacity(64);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(hex, "{byte:02x}");
    }
    hex
}

/// Parse the first whitespace-separated token from `sha256sum <file>`
/// output, which is the hex digest.
fn parse_sha256_output(stdout: &str) -> Result<String> {
    stdout
        .split_whitespace()
        .next()
        .map(str::to_owned)
        .ok_or_else(|| TransferError::Io(std::io::Error::other("sha256sum produced no output")))
}

/// POSIX single-quote `s` for safe embedding in a shell command.
/// Wraps in single quotes and escapes any internal `'` as `'\''`.
fn sh_single_quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('\'');
    for c in s.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use embedded_shell::shell::SubprocessShell;

    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "embedded-shell-transfer-test-{}-{}",
            std::process::id(),
            name
        ));
        p
    }

    #[tokio::test]
    async fn push_writes_bytes_to_remote_path() {
        let mut shell = SubprocessShell::new();
        let path = temp_path("push-basic");

        push(&mut shell, b"hello world", &path).await.unwrap();
        let got = std::fs::read(&path).unwrap();
        assert_eq!(&got, b"hello world");

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn push_then_fetch_round_trips() {
        let mut shell = SubprocessShell::new();
        let path = temp_path("roundtrip");
        let original = b"the quick brown fox jumps over the lazy dog";

        push(&mut shell, original, &path).await.unwrap();
        let fetched = fetch(&mut shell, &path).await.unwrap();
        assert_eq!(&fetched, original);

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn push_preserves_all_byte_values() {
        let mut shell = SubprocessShell::new();
        let path = temp_path("binary");
        let binary: Vec<u8> = (0..=255u8).collect();

        push(&mut shell, &binary, &path).await.unwrap();
        let got = std::fs::read(&path).unwrap();
        assert_eq!(got, binary);

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn push_handles_payload_at_size_limit() {
        let mut shell = SubprocessShell::new();
        let path = temp_path("size-limit");
        let payload = vec![0xABu8; MAX_PUSH_BYTES];

        push(&mut shell, &payload, &path).await.unwrap();
        let got = std::fs::read(&path).unwrap();
        assert_eq!(got.len(), MAX_PUSH_BYTES);
        assert!(got.iter().all(|&b| b == 0xAB));

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn push_rejects_oversized_payload() {
        let mut shell = SubprocessShell::new();
        let huge = vec![0u8; MAX_PUSH_BYTES + 1];
        let err = push(&mut shell, &huge, "/tmp/should-not-be-touched")
            .await
            .unwrap_err();
        assert!(matches!(err, TransferError::PayloadTooLarge(_)));
    }

    #[tokio::test]
    async fn fetch_reads_an_existing_file() {
        let mut shell = SubprocessShell::new();
        let path = temp_path("fetch-existing");
        std::fs::write(&path, b"file contents on the device side").unwrap();

        let got = fetch(&mut shell, &path).await.unwrap();
        assert_eq!(&got, b"file contents on the device side");

        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn sh_single_quote_escapes_embedded_apostrophes() {
        assert_eq!(sh_single_quote("it's"), "'it'\\''s'");
        assert_eq!(sh_single_quote("/tmp/clean"), "'/tmp/clean'");
    }

    #[test]
    fn sha256_hex_matches_known_value() {
        // SHA-256("hello\n") = 5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03
        assert_eq!(
            sha256_hex(b"hello\n"),
            "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03",
        );
    }
}
