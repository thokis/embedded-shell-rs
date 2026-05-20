use embedded_shell::shell::ShellError;

/// Errors that file-transfer operations in this crate can return.
///
/// Wraps [`ShellError`] for failures that happened while orchestrating
/// the transfer via the shell, and adds transfer-specific variants for
/// failures that have no shell analogue (HTTP errors, checksum
/// mismatches, host-IP discovery failures).
#[derive(Debug, thiserror::Error)]
pub enum TransferError {
    /// A shell command issued as part of the transfer failed.
    /// Carries the underlying [`ShellError`] verbatim.
    #[error(transparent)]
    Shell(#[from] ShellError),

    /// Local I/O error on the host side (writing a temp file, opening
    /// a socket, etc.).
    #[error(transparent)]
    Io(#[from] std::io::Error),

    /// SHA-256 verification failed after a transfer. The bytes that
    /// arrived on the device (or came back from the device) don't
    /// match what was sent.
    #[error("sha256 mismatch: expected {expected}, got {actual}")]
    ChecksumMismatch {
        /// The hash computed locally over the bytes the host had.
        expected: String,
        /// The hash computed device-side over what arrived.
        actual: String,
    },

    /// Generic HTTP-transport error. Carries a human-readable message;
    /// the detail isn't structured because the failure modes from
    /// `hyper` are many and a typed enum per case would not earn its
    /// complexity here.
    #[cfg(feature = "http")]
    #[error("HTTP transfer failed: {0}")]
    Http(String),

    /// The host couldn't determine a local IP visible to the device.
    /// On multi-interface or container-NAT setups, ensure the host's
    /// default route points at the device-facing network.
    #[cfg(feature = "http")]
    #[error("could not determine host IP visible to device")]
    NoHostIp,

    /// The device has no HTTP downloader installed. `push` tries
    /// `wget` first and then `curl`; if neither is present this
    /// variant is returned. Install at least one device-side, or use
    /// [`crate::serial`] for transfers that don't require the network.
    #[cfg(feature = "http")]
    #[error("device has neither wget nor curl installed; HTTP push requires at least one")]
    NoDownloader,

    /// The transfer payload is too large for the selected transport.
    /// The serial transport, in particular, is bounded by what fits in
    /// a single shell command line (~`ARG_MAX`, typically 128 KiB on
    /// Linux); larger payloads should use the HTTP transport.
    #[error("payload too large for transport: {0}")]
    PayloadTooLarge(String),
}

/// Convenience alias used throughout the crate.
pub type Result<T> = std::result::Result<T, TransferError>;
