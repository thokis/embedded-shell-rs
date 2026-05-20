use async_trait::async_trait;

use super::command::Command;
use super::error::ShellError;
use super::result::ShellResult;

/// Marker trait for shells that expose a Linux-style userland.
///
/// Higher-level wrappers built on top of [`Shell`] often need to assume
/// the device runs a familiar set of tools — `cat`, `ls`, `chmod`, `sh
/// -c`, `printf`, `base64`, `sha256sum`, and so on. Those wrappers
/// constrain themselves to `&mut dyn LinuxShell` instead of `&mut dyn
/// Shell`, so the compiler refuses calls that would target a shell
/// without a Linux-style userland — notably
/// [`UBootSerialShell`][crate::shell::UBootSerialShell], where commands
/// like `cat` and `sh` don't exist.
///
/// `LinuxShell` is an opt-in marker — implementing it is a promise that
/// the shell can run typical coreutils / busybox / toybox commands.
/// The crate's built-in [`SubprocessShell`][crate::shell::SubprocessShell]
/// and [`LinuxSerialShell`][crate::shell::LinuxSerialShell] implement it;
/// [`UBootSerialShell`][crate::shell::UBootSerialShell] does not. Custom
/// shells (SSH-backed, container-exec-backed, …) can opt in if they
/// satisfy the same contract.
///
/// # Why "Linux" and not "POSIX"
///
/// The wrappers built on this trait use a few non-POSIX extensions
/// (notably `sha256sum` and `timeout(1)`), so a strictly-POSIX device
/// — stock macOS, base FreeBSD — wouldn't actually run them. Naming
/// it `LinuxShell` is honest about that target: Linux distributions
/// and Linux-userland-compatible embedded stacks (busybox-based
/// OpenWrt / Buildroot / Yocto, toybox-based Android, …).
///
/// # Required device-side commands
///
/// The exact set depends on which higher-level wrapper is being used,
/// but a `LinuxShell` implementor should at minimum expose: `cat`,
/// `ls`, `chmod`, `mkdir`, `rm`, `rmdir`, `sh`, `printf`, `tr`,
/// `base64`, `sha256sum`, `timeout`.
pub trait LinuxShell: Shell {}

/// Async shell abstraction: bring it up, run commands, tear it down.
///
/// Every concrete shell — [`SubprocessShell`], [`LinuxSerialShell`],
/// [`UBootSerialShell`] — implements this trait. Higher-level wrappers
/// (e.g. the `embedded-shell-linux` crate's `fs::*` functions) take
/// `&mut dyn LinuxShell` (a sub-trait of `Shell`) when they need to
/// constrain themselves to Linux-userland devices, or `&mut dyn Shell`
/// when they work across all backends.
///
/// `&mut self` on every method serialises access by construction: the
/// borrow checker enforces single-caller exclusion without any explicit
/// lock. To share a shell across tasks, wrap it in
/// `tokio::sync::Mutex<Box<dyn Shell>>`.
///
/// # Lifecycle
///
/// 1. **Construct** the shell (via the type's builder).
/// 2. **[`activate`]** — opens the underlying transport (if any) and
///    drives the shell into a ready state. For serial shells this runs
///    the login state machine; for [`SubprocessShell`] it's a no-op.
///    Must be called before [`run`].
/// 3. **[`run`]** — execute one [`Command`] and get a [`ShellResult`].
///    Can be called repeatedly.
/// 4. **[`deactivate`]** — best-effort cleanup. The transport is closed
///    on serial shells; on [`SubprocessShell`] it's a no-op.
///
/// [`activate`]: Shell::activate
/// [`run`]: Shell::run
/// [`deactivate`]: Shell::deactivate
/// [`SubprocessShell`]: super::SubprocessShell
/// [`LinuxSerialShell`]: super::LinuxSerialShell
/// [`UBootSerialShell`]: super::UBootSerialShell
#[async_trait]
pub trait Shell: Send {
    /// Bring the shell into a ready state.
    ///
    /// For serial-backed shells this opens the prompt-detection /
    /// login state machine. For [`SubprocessShell`][super::SubprocessShell]
    /// it's a no-op that always returns `Ok(())`. Must be called before
    /// the first [`run`][Self::run].
    ///
    /// # Errors
    ///
    /// - [`ShellError::Initialization`] on login-timeout, invalid
    ///   configuration, or other startup failures.
    /// - [`ShellError::Io`] on transport-level failures.
    async fn activate(&mut self) -> Result<(), ShellError>;

    /// Best-effort teardown.
    ///
    /// Serial-backed shells close the underlying transport (releasing the
    /// OS file descriptor); [`SubprocessShell`][super::SubprocessShell]
    /// is a no-op. Idempotent — calling on an already-deactivated shell
    /// is harmless.
    ///
    /// # Errors
    ///
    /// - [`ShellError::Io`] if the transport reports an error during
    ///   close. Rare in practice.
    async fn deactivate(&mut self) -> Result<(), ShellError>;

    /// Execute one [`Command`] and return its [`ShellResult`].
    ///
    /// The exact wire-level framing depends on the concrete shell, but
    /// the contract is the same: stdout, stderr, and exit code are
    /// captured into the result, and non-zero exits map to
    /// [`ShellError::CommandFailed`] unless
    /// [`Command::allow_nonzero`][super::Command::allow_nonzero] is set.
    ///
    /// # Errors
    ///
    /// - [`ShellError::CommandFailed`] on non-zero exit (unless
    ///   `allow_nonzero` is set).
    /// - [`ShellError::CommandNotFound`] when the binary doesn't exist
    ///   on the device (exit code 127 on Linux).
    /// - [`ShellError::Timeout`] when device-side `timeout(1)` killed
    ///   the command (exit code 124 on Linux shells).
    /// - [`ShellError::ReadTimeout`] when the host-side read deadline
    ///   expires waiting for the framed response.
    /// - [`ShellError::Io`] on transport-level failures.
    async fn run(&mut self, command: &Command) -> Result<ShellResult, ShellError>;

    /// Close the transport and bring the shell back up.
    ///
    /// For serial-backed shells this re-opens the port with the same
    /// configuration and re-runs the activate state machine in one
    /// shot — useful after a USB unplug or device reboot.
    ///
    /// The default implementation returns
    /// [`ShellError::Initialization`] because most shells have nothing
    /// to reconnect to. [`SubprocessShell`][super::SubprocessShell]
    /// inherits this default. [`LinuxSerialShell`][super::LinuxSerialShell]
    /// and [`UBootSerialShell`][super::UBootSerialShell] override it.
    ///
    /// # Errors
    ///
    /// - [`ShellError::Initialization`] when the shell type has no
    ///   reconnect concept, or when re-activation fails.
    /// - [`ShellError::Io`] on transport-level failures.
    async fn reconnect(&mut self) -> Result<(), ShellError> {
        Err(ShellError::initialization(
            "this shell does not support reconnect",
        ))
    }
}
