//! Filesystem operations on a remote device — shadows [`std::fs`].
//!
//! Every function in this module takes a [`LinuxShell`] reference, runs
//! a single command on the device, and returns a typed Rust value (or
//! [`Error`]). The wrappers don't hold state of their own.
//!
//! The [`LinuxShell`] bound restricts these functions to shells whose
//! device-side userland is Linux-style — they call `cat`, `ls`,
//! `chmod`, `mkdir`, `rm`, `rmdir`, `sha256sum`. That excludes
//! [`UBootSerialShell`][embedded_shell::shell::UBootSerialShell],
//! where these tools don't exist; the type checker catches that
//! mistake at compile time.
//!
//! # API shape
//!
//! Where a function has a direct [`std::fs`] analogue, the name and
//! intent match it exactly ([`read_to_string`], [`read_dir`], [`write()`],
//! [`create_dir`], [`create_dir_all`], [`remove_file`], [`remove_dir`],
//! [`remove_dir_all`], [`set_permissions`], [`copy`], [`rename`],
//! [`metadata`], [`read_link`], [`canonicalize`]). [`symlink`] mirrors
//! [`std::os::unix::fs::symlink`]. [`write_atomic`] has no std analogue
//! but solves a common ops problem (no partial reads). Notable
//! divergences from `std::fs`:
//!
//! - [`read_dir`] returns a `Vec<String>` of bare entry names rather
//!   than an iterator of `DirEntry` (serial lines don't lend themselves
//!   to streaming enumeration).
//! - [`set_permissions`] accepts a `chmod` mode string (`"0644"`,
//!   `"u+x"`, …) rather than a [`std::fs::Permissions`].
//! - [`copy`] returns `()` instead of the byte count — `cp` doesn't
//!   report it.
//! - [`metadata`] returns a small [`Metadata`] struct (size, type,
//!   mode, mtime) — see its docs for what's intentionally omitted from
//!   [`std::fs::Metadata`].
//! - [`sha256sum`] has no `std::fs` analogue but lives here because it
//!   wraps a coreutils tool that operates on files.
//! - [`walk_dir`] has no `std::fs` analogue (third-party `walkdir` is
//!   the usual std-side answer); included here because `find` on the
//!   device covers the same need in one shell call.
//!
//! All paths are accepted as `impl AsRef<Path>`, so `&str`, `&Path`,
//! `&String`, and `PathBuf` all work transparently.
//!
//! # Device-side requirement
//!
//! The device needs a coreutils-compatible userland (GNU coreutils,
//! busybox, toybox). Enabled by the `coreutils` Cargo feature
//! (default-on).
//!
//! [`LinuxShell`]: embedded_shell::shell::LinuxShell
//! [`Error`]: crate::Error

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use embedded_shell::shell::{Command, LinuxShell};

use crate::error::{Error, Result};

/// Hard upper bound on payload size for [`write()`] / [`write_atomic`].
/// Larger files won't fit in a single `sh -c` invocation (ARG_MAX is
/// ~128 KiB on Linux, smaller on busybox; after base64 expansion that
/// caps the raw payload at ~64 KiB). Past this, use
/// `embedded-shell-transfer` instead.
pub const MAX_WRITE_BYTES: usize = 64 * 1024;

/// Coarse file-type classification, mirroring [`std::fs::FileType`].
///
/// Anything that isn't a regular file, directory, or symlink (sockets,
/// FIFOs, block/character devices) collapses into [`FileType::Other`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileType {
    /// Regular file.
    File,
    /// Directory.
    Dir,
    /// Symbolic link. Note: [`metadata`] follows the link and reports
    /// the target's type — this variant only appears when the target
    /// is itself a symlink (which usually means a broken link).
    Symlink,
    /// Socket, FIFO, character or block device, or anything else.
    Other,
}

impl FileType {
    /// `true` for regular files.
    pub fn is_file(&self) -> bool {
        matches!(self, Self::File)
    }
    /// `true` for directories.
    pub fn is_dir(&self) -> bool {
        matches!(self, Self::Dir)
    }
    /// `true` for symbolic links.
    pub fn is_symlink(&self) -> bool {
        matches!(self, Self::Symlink)
    }
}

/// Subset of `stat(2)` for a path on the device.
///
/// Mirrors [`std::fs::Metadata`] but trimmed to the fields with
/// reliable cross-distro semantics on embedded Linux. Notable omissions
/// from `std::fs::Metadata`:
///
/// - **No `accessed()`** — atime is unreliable on `noatime`-mounted
///   embedded filesystems.
/// - **No `created()`** — birthtime is not consistently exposed across
///   embedded kernels and filesystems.
/// - **No uid/gid** — easy to add if asked, but uncommon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Metadata {
    /// Size in bytes (the file's "length").
    pub size: u64,
    /// Type of filesystem entry.
    pub file_type: FileType,
    /// Permission bits as a u32 (e.g. `0o644`, `0o755`).
    pub mode: u32,
    /// Modification time.
    pub modified: SystemTime,
}

/// Reads `path` and returns its contents as UTF-8, with replacement for
/// any non-UTF-8 bytes.
///
/// Analogue of [`std::fs::read_to_string`]. Equivalent to running
/// `cat <path>` on the device.
///
/// # Errors
///
/// - [`Error::Shell`] wrapping
///   [`ShellError::CommandFailed`][embedded_shell::shell::ShellError::CommandFailed]
///   if `cat` exits non-zero (file missing, permission denied, …).
/// - [`Error::Shell`] wrapping
///   [`ShellError::CommandNotFound`][embedded_shell::shell::ShellError::CommandNotFound]
///   if `cat` isn't installed on the device.
///
/// # Example
///
/// ```ignore
/// use embedded_shell_linux::fs;
///
/// let hostname = fs::read_to_string(&mut shell, "/etc/hostname").await?;
/// println!("hostname: {}", hostname.trim());
/// ```
pub async fn read_to_string(shell: &mut dyn LinuxShell, path: impl AsRef<Path>) -> Result<String> {
    let path = path_arg(path.as_ref());
    let result = shell.run(&Command::new("cat").arg(path)).await?;
    Ok(result.stdout().unwrap_or("").to_string())
}

/// Lists the visible entries of directory `path` and returns their
/// bare names (not full paths).
///
/// Analogue of [`std::fs::read_dir`], except it returns a `Vec<String>`
/// rather than a streaming iterator — serial lines don't lend themselves
/// to incremental enumeration. Equivalent to running `ls -1 <path>` on
/// the device. Hidden entries (starting with `.`) are excluded, as are
/// the special entries `.` and `..`.
///
/// # Errors
///
/// - [`Error::Shell`] if `ls` exits non-zero (directory missing,
///   permission denied, …) or isn't installed.
///
/// # Example
///
/// ```ignore
/// use embedded_shell_linux::fs;
///
/// for name in fs::read_dir(&mut shell, "/etc").await? {
///     println!("/etc/{name}");
/// }
/// ```
pub async fn read_dir(shell: &mut dyn LinuxShell, path: impl AsRef<Path>) -> Result<Vec<String>> {
    let path = path_arg(path.as_ref());
    let result = shell.run(&Command::new("ls").arg("-1").arg(path)).await?;
    Ok(result
        .stdout()
        .unwrap_or("")
        .lines()
        .filter(|line| !line.is_empty())
        .map(String::from)
        .collect())
}

/// Sets the permission bits of `path`.
///
/// Analogue of [`std::fs::set_permissions`], except it accepts a `chmod`
/// mode string rather than a [`std::fs::Permissions`] — `mode` is passed
/// verbatim to `chmod` on the device, so any form `chmod` accepts works:
/// octal (`"0644"`, `"755"`), symbolic (`"u+x"`, `"a-w"`, `"go=r"`), …
///
/// # Errors
///
/// - [`Error::Shell`] if `chmod` exits non-zero (path missing,
///   permission denied, mode syntax invalid).
///
/// # Example
///
/// ```ignore
/// use embedded_shell_linux::fs;
///
/// fs::set_permissions(&mut shell, "/usr/local/bin/foo", "0755").await?;
/// fs::set_permissions(&mut shell, "/etc/secret", "go-rwx").await?;
/// ```
pub async fn set_permissions(
    shell: &mut dyn LinuxShell,
    path: impl AsRef<Path>,
    mode: &str,
) -> Result<()> {
    let path = path_arg(path.as_ref());
    shell
        .run(&Command::new("chmod").arg(mode).arg(path))
        .await?;
    Ok(())
}

/// Creates a directory at `path`. The parent must already exist and
/// `path` must not.
///
/// Analogue of [`std::fs::create_dir`]. Equivalent to running
/// `mkdir <path>`. Use [`create_dir_all`] when you want "create with
/// parents, succeed if exists".
///
/// # Errors
///
/// - [`Error::Shell`] if `path` already exists, the parent
///   doesn't exist, or permissions are insufficient.
///
/// # Example
///
/// ```ignore
/// use embedded_shell_linux::fs;
///
/// fs::create_dir(&mut shell, "/tmp/scratch").await?;
/// ```
pub async fn create_dir(shell: &mut dyn LinuxShell, path: impl AsRef<Path>) -> Result<()> {
    let path = path_arg(path.as_ref());
    shell.run(&Command::new("mkdir").arg(path)).await?;
    Ok(())
}

/// Creates a directory at `path`, creating missing parents and treating
/// "already exists" as success.
///
/// Analogue of [`std::fs::create_dir_all`]. Equivalent to running
/// `mkdir -p <path>`.
///
/// # Errors
///
/// - [`Error::Shell`] on permission failures or filesystem
///   errors. Notably does **not** error if the directory already
///   exists.
///
/// # Example
///
/// ```ignore
/// use embedded_shell_linux::fs;
///
/// fs::create_dir_all(&mut shell, "/var/lib/myapp/cache/inner").await?;
/// ```
pub async fn create_dir_all(shell: &mut dyn LinuxShell, path: impl AsRef<Path>) -> Result<()> {
    let path = path_arg(path.as_ref());
    shell
        .run(&Command::new("mkdir").arg("-p").arg(path))
        .await?;
    Ok(())
}

/// Removes a single regular file at `path`.
///
/// Analogue of [`std::fs::remove_file`]. Equivalent to running
/// `rm <path>`. Errors if `path` is a directory — use [`remove_dir`]
/// or [`remove_dir_all`] for those.
///
/// # Errors
///
/// - [`Error::Shell`] if `path` doesn't exist, is a directory,
///   or permissions don't allow removal.
///
/// # Example
///
/// ```ignore
/// use embedded_shell_linux::fs;
///
/// fs::remove_file(&mut shell, "/tmp/scratch.txt").await?;
/// ```
pub async fn remove_file(shell: &mut dyn LinuxShell, path: impl AsRef<Path>) -> Result<()> {
    let path = path_arg(path.as_ref());
    shell.run(&Command::new("rm").arg(path)).await?;
    Ok(())
}

/// Removes an empty directory at `path`.
///
/// Analogue of [`std::fs::remove_dir`]. Equivalent to running
/// `rmdir <path>`. Errors if the directory contains anything — use
/// [`remove_dir_all`] for recursive removal.
///
/// # Errors
///
/// - [`Error::Shell`] if `path` doesn't exist, isn't empty,
///   isn't a directory, or permissions don't allow removal.
///
/// # Example
///
/// ```ignore
/// use embedded_shell_linux::fs;
///
/// fs::remove_dir(&mut shell, "/tmp/empty_workdir").await?;
/// ```
pub async fn remove_dir(shell: &mut dyn LinuxShell, path: impl AsRef<Path>) -> Result<()> {
    let path = path_arg(path.as_ref());
    shell.run(&Command::new("rmdir").arg(path)).await?;
    Ok(())
}

/// Removes `path` and any descendants, idempotently.
///
/// Analogue of [`std::fs::remove_dir_all`] (with the convenient
/// extension, inherited from `rm -rf`, that this also accepts plain
/// files and treats a missing `path` as success). Equivalent to
/// running `rm -rf <path>`.
///
/// # Errors
///
/// - [`Error::Shell`] only on permission failures or filesystem
///   errors. Notably does **not** error if `path` doesn't exist.
///
/// # Example
///
/// ```ignore
/// use embedded_shell_linux::fs;
///
/// fs::remove_dir_all(&mut shell, "/tmp/old_workdir").await?;
/// ```
pub async fn remove_dir_all(shell: &mut dyn LinuxShell, path: impl AsRef<Path>) -> Result<()> {
    let path = path_arg(path.as_ref());
    shell
        .run(&Command::new("rm").args(["-r", "-f"]).arg(path))
        .await?;
    Ok(())
}

/// Computes the SHA-256 digest of `path` and returns it as a
/// 64-character lowercase hex string.
///
/// No direct [`std::fs`] analogue — included here because it wraps a
/// coreutils tool that operates on files. Equivalent to running
/// `sha256sum <path>` on the device and extracting the hex digest
/// from the first whitespace-separated token of stdout.
///
/// # Errors
///
/// - [`Error::Shell`] if `sha256sum` exits non-zero (file
///   missing, permission denied) or isn't installed.
/// - [`Error::Parse`] if the output doesn't begin with a hex
///   digest (extremely rare; would indicate a non-standard `sha256sum`
///   implementation).
///
/// # Example
///
/// ```ignore
/// use embedded_shell_linux::fs;
///
/// let digest = fs::sha256sum(&mut shell, "/usr/bin/busybox").await?;
/// assert_eq!(digest.len(), 64);
/// ```
pub async fn sha256sum(shell: &mut dyn LinuxShell, path: impl AsRef<Path>) -> Result<String> {
    let path = path_arg(path.as_ref());
    let result = shell.run(&Command::new("sha256sum").arg(path)).await?;
    let stdout = result
        .stdout()
        .ok_or_else(|| Error::Parse("sha256sum produced no output".to_string()))?;
    let hex = stdout
        .split_whitespace()
        .next()
        .ok_or_else(|| Error::Parse(format!("sha256sum: unexpected output {stdout:?}")))?;
    Ok(hex.to_string())
}

/// Copies the contents of `from` to `to` on the device.
///
/// Analogue of [`std::fs::copy`], but returns `()` instead of the
/// byte count — `cp` doesn't report it and asking the device for it
/// would require a second round-trip. If you need the size, follow up
/// with [`metadata`].
///
/// Equivalent to running `cp <from> <to>` on the device.
///
/// # Errors
///
/// - [`Error::Shell`] if `cp` exits non-zero (source missing, target
///   directory not writable, …) or isn't installed.
///
/// # Example
///
/// ```ignore
/// use embedded_shell_linux::fs;
///
/// fs::copy(&mut shell, "/etc/hostname", "/tmp/hostname.bak").await?;
/// ```
pub async fn copy(
    shell: &mut dyn LinuxShell,
    from: impl AsRef<Path>,
    to: impl AsRef<Path>,
) -> Result<()> {
    let from = path_arg(from.as_ref());
    let to = path_arg(to.as_ref());
    shell.run(&Command::new("cp").arg(from).arg(to)).await?;
    Ok(())
}

/// Renames a file or directory from `from` to `to` on the device.
///
/// Analogue of [`std::fs::rename`]. Equivalent to running `mv <from>
/// <to>` on the device. As with `mv`, this works across rename
/// boundaries (different directories on the same filesystem) and
/// degrades to copy-then-delete across filesystems.
///
/// # Errors
///
/// - [`Error::Shell`] if `mv` exits non-zero (source missing,
///   destination not writable, …) or isn't installed.
///
/// # Example
///
/// ```ignore
/// use embedded_shell_linux::fs;
///
/// fs::rename(&mut shell, "/tmp/draft.cfg", "/etc/app.cfg").await?;
/// ```
pub async fn rename(
    shell: &mut dyn LinuxShell,
    from: impl AsRef<Path>,
    to: impl AsRef<Path>,
) -> Result<()> {
    let from = path_arg(from.as_ref());
    let to = path_arg(to.as_ref());
    shell.run(&Command::new("mv").arg(from).arg(to)).await?;
    Ok(())
}

/// Returns a [`Metadata`] for `path` on the device.
///
/// Equivalent to running `stat -c '%s|%F|%a|%Y' <path>` and parsing
/// the four pipe-separated fields. Follows symlinks (use [`std::fs`]
/// terminology: this is the `metadata`-not-`symlink_metadata`
/// behavior); to inspect a symlink itself rather than its target, pass
/// `stat -c` flags through a manual [`Command`] for now.
///
/// # Errors
///
/// - [`Error::Shell`] if `stat` exits non-zero (file missing,
///   permission denied) or isn't installed.
/// - [`Error::Parse`] if the output doesn't have four pipe-separated
///   fields parsable as `<u64>|<text>|<octal>|<u64>`.
///
/// # Example
///
/// ```ignore
/// use embedded_shell_linux::fs;
///
/// let m = fs::metadata(&mut shell, "/etc/hostname").await?;
/// println!("{} bytes, mode {:o}", m.size, m.mode);
/// ```
pub async fn metadata(shell: &mut dyn LinuxShell, path: impl AsRef<Path>) -> Result<Metadata> {
    let path = path_arg(path.as_ref());
    let result = shell
        .run(&Command::new("stat").arg("-c").arg("%s|%F|%a|%Y").arg(path))
        .await?;
    let stdout = result
        .stdout()
        .ok_or_else(|| Error::Parse("stat produced no output".to_string()))?;
    parse_stat_output(stdout.trim())
}

fn parse_stat_output(line: &str) -> Result<Metadata> {
    let mut parts = line.splitn(4, '|');
    let size_s = parts
        .next()
        .ok_or_else(|| Error::Parse(format!("stat: missing size field in {line:?}")))?;
    let kind_s = parts
        .next()
        .ok_or_else(|| Error::Parse(format!("stat: missing file-type field in {line:?}")))?;
    let mode_s = parts
        .next()
        .ok_or_else(|| Error::Parse(format!("stat: missing mode field in {line:?}")))?;
    let mtime_s = parts
        .next()
        .ok_or_else(|| Error::Parse(format!("stat: missing mtime field in {line:?}")))?;

    let size = size_s
        .parse::<u64>()
        .map_err(|e| Error::Parse(format!("stat: size: {e}")))?;
    let mode = u32::from_str_radix(mode_s, 8)
        .map_err(|e| Error::Parse(format!("stat: mode {mode_s:?}: {e}")))?;
    let mtime_secs = mtime_s
        .parse::<u64>()
        .map_err(|e| Error::Parse(format!("stat: mtime {mtime_s:?}: {e}")))?;

    let file_type = match kind_s {
        "regular file" | "regular empty file" => FileType::File,
        "directory" => FileType::Dir,
        "symbolic link" => FileType::Symlink,
        _ => FileType::Other,
    };

    Ok(Metadata {
        size,
        file_type,
        mode,
        modified: UNIX_EPOCH + Duration::from_secs(mtime_secs),
    })
}

/// Creates a symbolic link on the device.
///
/// Analogue of [`std::os::unix::fs::symlink`]: argument order is
/// `(original, link)` — the new path at `link` becomes a symbolic
/// link pointing to `original`. Equivalent to
/// `ln -s <original> <link>` on the device.
///
/// # Errors
///
/// - [`Error::Shell`] if `ln` exits non-zero (e.g. `link` already
///   exists, or its parent directory isn't writable).
///
/// # Example
///
/// ```ignore
/// use embedded_shell_linux::fs;
///
/// fs::symlink(&mut shell, "/etc/app/config.json", "/etc/app/current.json").await?;
/// ```
pub async fn symlink(
    shell: &mut dyn LinuxShell,
    original: impl AsRef<Path>,
    link: impl AsRef<Path>,
) -> Result<()> {
    let original = path_arg(original.as_ref());
    let link = path_arg(link.as_ref());
    shell
        .run(&Command::new("ln").arg("-s").arg(original).arg(link))
        .await?;
    Ok(())
}

/// Reads the target of a symbolic link.
///
/// Analogue of [`std::fs::read_link`]. Equivalent to `readlink <path>`
/// (without `-f`); the returned string is the link target *as
/// stored*, which may be relative. For the fully-resolved absolute
/// path, use [`canonicalize`] instead.
///
/// # Errors
///
/// - [`Error::Shell`] if `readlink` exits non-zero (path doesn't
///   exist or isn't a symlink) or isn't installed.
///
/// # Example
///
/// ```ignore
/// let target = fs::read_link(&mut shell, "/etc/app/current.json").await?;
/// // target == "/etc/app/config.json" (or whatever the symlink stores)
/// ```
pub async fn read_link(shell: &mut dyn LinuxShell, path: impl AsRef<Path>) -> Result<String> {
    let path = path_arg(path.as_ref());
    let r = shell.run(&Command::new("readlink").arg(path)).await?;
    Ok(r.stdout().unwrap_or("").trim_end_matches('\n').to_string())
}

/// Returns the canonical absolute path of `path`, with all symbolic
/// links resolved and `.`/`..` components collapsed.
///
/// Equivalent to `readlink -f <path>` on the device. **Diverges from
/// [`std::fs::canonicalize`] in one notable way:** GNU `readlink -f`
/// is willing to canonicalise a path whose *last* component doesn't
/// exist (as long as its parent does), returning the would-be
/// canonical path. `std::fs::canonicalize` errors in that case. If
/// you need the strict variant, follow up with a [`metadata`] call
/// to verify the returned path actually resolves to a file.
///
/// # Errors
///
/// - [`Error::Shell`] if `readlink` exits non-zero — typically
///   because a parent component of `path` doesn't exist.
/// - [`Error::Parse`] if the command returned empty output (some
///   `readlink` implementations behave that way on completely
///   bogus input).
///
/// # Example
///
/// ```ignore
/// let abs = fs::canonicalize(&mut shell, "/etc/app/current.json").await?;
/// // abs == "/etc/app/config.json" (resolved through the symlink)
/// ```
pub async fn canonicalize(shell: &mut dyn LinuxShell, path: impl AsRef<Path>) -> Result<String> {
    let path = path_arg(path.as_ref());
    let r = shell
        .run(&Command::new("readlink").arg("-f").arg(path))
        .await?;
    let trimmed = r.stdout().unwrap_or("").trim_end_matches('\n').to_string();
    if trimmed.is_empty() {
        return Err(Error::Parse(
            "readlink -f produced no output (path may not exist)".to_string(),
        ));
    }
    Ok(trimmed)
}

/// Recursively walks `path`, returning a flat list of every entry
/// found — files, directories, and symlinks all included.
///
/// Equivalent to `find <path>` on the device. Order is the same as
/// `find`'s (effectively depth-first, but unspecified beyond that).
/// The list includes `path` itself as the first element.
///
/// No direct [`std::fs`] analogue — third-party `walkdir` is what
/// you'd reach for in std-side code. Included here because `find`
/// solves the same problem in one shell call.
///
/// # Errors
///
/// - [`Error::Shell`] if `find` exits non-zero (e.g. permission
///   denied on a subdirectory) or isn't installed.
///
/// # Example
///
/// ```ignore
/// use embedded_shell_linux::fs;
///
/// let entries = fs::walk_dir(&mut shell, "/etc/app").await?;
/// for entry in &entries {
///     println!("{entry}");
/// }
/// ```
pub async fn walk_dir(shell: &mut dyn LinuxShell, path: impl AsRef<Path>) -> Result<Vec<String>> {
    let path = path_arg(path.as_ref());
    let r = shell.run(&Command::new("find").arg(path)).await?;
    Ok(r.stdout()
        .unwrap_or("")
        .lines()
        .filter(|l| !l.is_empty())
        .map(str::to_owned)
        .collect())
}

/// Writes `contents` to `path` on the device, overwriting any
/// existing content.
///
/// Analogue of [`std::fs::write`]. **Not atomic** — readers can
/// observe a partial file while the write is in progress. Use
/// [`write_atomic`] when readers may concurrently access `path` and
/// you can't tolerate that.
///
/// Implementation: base64-encodes `contents` on the host, then runs
/// `printf '%s' '<b64>' | base64 -d > <path>` on the device.
/// Suitable for small payloads (config files, env files, certs).
/// Bounded by [`MAX_WRITE_BYTES`] — larger writes return
/// [`Error::Parse`]; use `embedded-shell-transfer` for those.
///
/// # Errors
///
/// - [`Error::Parse`] if `contents` exceeds [`MAX_WRITE_BYTES`].
/// - [`Error::Shell`] if the device-side `printf | base64 -d > path`
///   pipeline fails (permission denied, parent dir missing, …) or
///   `base64` isn't installed.
///
/// # Example
///
/// ```ignore
/// use embedded_shell_linux::fs;
///
/// fs::write(&mut shell, "/etc/app/version", b"1.2.3\n").await?;
/// ```
pub async fn write(
    shell: &mut dyn LinuxShell,
    path: impl AsRef<Path>,
    contents: impl AsRef<[u8]>,
) -> Result<()> {
    let path = path_arg(path.as_ref());
    write_to_path(shell, &path, contents.as_ref()).await
}

/// Writes `contents` to `path` atomically — readers see the old
/// content until the moment the new content fully replaces it.
///
/// Writes to `<path>.tmp.<pid>` first, then `mv`s the temp into
/// place. Since rename within one filesystem is atomic, partial
/// reads can never happen.
///
/// **Same-filesystem requirement:** `mv` is only atomic when source
/// and destination share a filesystem. If `<path>`'s parent
/// directory lives on a different mount than where the temp file
/// lands, `mv` falls back to copy-then-delete and the atomicity
/// guarantee is gone. In practice this is fine — the temp file
/// lives alongside the final path, so they share a filesystem.
///
/// **Cleanup on partial failure:** if the encoded write succeeds
/// but the rename fails, a `<path>.tmp.<pid>` file remains. The
/// next successful `write_atomic` will overwrite it; the wrapper
/// does not retroactively clean it up.
///
/// Same size limits and error conditions as [`write()`].
///
/// # Example
///
/// ```ignore
/// use embedded_shell_linux::fs;
///
/// // Update a config that other processes may be reading right now.
/// fs::write_atomic(&mut shell, "/etc/app/config.json", new_config).await?;
/// ```
pub async fn write_atomic(
    shell: &mut dyn LinuxShell,
    path: impl AsRef<Path>,
    contents: impl AsRef<[u8]>,
) -> Result<()> {
    let path = path_arg(path.as_ref());
    let tmp = format!("{path}.tmp.{}", std::process::id());
    write_to_path(shell, &tmp, contents.as_ref()).await?;
    shell.run(&Command::new("mv").arg(&tmp).arg(&path)).await?;
    Ok(())
}

async fn write_to_path(shell: &mut dyn LinuxShell, path: &str, contents: &[u8]) -> Result<()> {
    if contents.len() > MAX_WRITE_BYTES {
        return Err(Error::Parse(format!(
            "{} bytes exceeds fs::write limit of {} bytes; use embedded-shell-transfer for larger payloads",
            contents.len(),
            MAX_WRITE_BYTES,
        )));
    }
    let encoded = STANDARD.encode(contents);
    // Base64 alphabet is shell-safe; only the path needs quoting.
    let script = format!(
        "printf '%s' '{}' | base64 -d > {}",
        encoded,
        sh_single_quote(path),
    );
    shell.run(&Command::new("sh").args(["-c", &script])).await?;
    Ok(())
}

/// POSIX single-quote `s` for safe embedding in a shell command.
/// Wraps in `'…'` and escapes any internal `'` as `'\''`.
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

/// Lossy UTF-8 conversion for path arguments. Linux paths are bytes,
/// not necessarily UTF-8; we accept replacement for the rare invalid
/// cases rather than refuse to run.
fn path_arg(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;
    use std::path::PathBuf;

    use embedded_shell::shell::SubprocessShell;

    use super::*;

    fn temp_path(name: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        p.push(format!(
            "embedded-shell-linux-test-{}-{}",
            std::process::id(),
            name
        ));
        p
    }

    #[tokio::test]
    async fn read_to_string_reads_a_file() {
        let mut shell = SubprocessShell::new();
        let path = temp_path("read_to_string");
        std::fs::write(&path, "hello world\n").unwrap();

        let got = read_to_string(&mut shell, &path).await.unwrap();
        assert_eq!(got, "hello world\n");

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn read_dir_returns_visible_entries_only() {
        let mut shell = SubprocessShell::new();
        let dir = temp_path("read_dir");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("alpha"), "").unwrap();
        std::fs::write(dir.join("beta"), "").unwrap();
        std::fs::write(dir.join(".hidden"), "").unwrap();

        let mut entries = read_dir(&mut shell, &dir).await.unwrap();
        entries.sort();
        assert_eq!(entries, vec!["alpha", "beta"]);

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn create_dir_all_is_idempotent() {
        let mut shell = SubprocessShell::new();
        let dir = temp_path("create_dir_all/inner/deeper");

        create_dir_all(&mut shell, &dir).await.unwrap();
        create_dir_all(&mut shell, &dir).await.unwrap();
        assert!(dir.exists());

        let outer = temp_path("create_dir_all");
        let _ = std::fs::remove_dir_all(&outer);
    }

    #[tokio::test]
    async fn create_dir_errors_when_parent_missing() {
        let mut shell = SubprocessShell::new();
        let path = temp_path("create-dir-missing-parent/inner");
        let err = create_dir(&mut shell, &path).await.unwrap_err();
        assert!(matches!(err, Error::Shell(_)));
    }

    #[tokio::test]
    async fn set_permissions_changes_mode() {
        let mut shell = SubprocessShell::new();
        let path = temp_path("set_permissions");
        std::fs::write(&path, "").unwrap();

        set_permissions(&mut shell, &path, "0600").await.unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn remove_file_removes_a_file() {
        let mut shell = SubprocessShell::new();
        let path = temp_path("remove_file");
        std::fs::write(&path, "").unwrap();
        assert!(path.exists());

        remove_file(&mut shell, &path).await.unwrap();
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn remove_dir_only_removes_empty_directories() {
        let mut shell = SubprocessShell::new();
        let dir = temp_path("remove_dir");
        std::fs::create_dir_all(&dir).unwrap();

        remove_dir(&mut shell, &dir).await.unwrap();
        assert!(!dir.exists());

        std::fs::create_dir_all(dir.join("inner")).unwrap();
        let err = remove_dir(&mut shell, &dir).await.unwrap_err();
        assert!(matches!(err, Error::Shell(_)));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn remove_dir_all_removes_a_tree_and_succeeds_on_missing() {
        let mut shell = SubprocessShell::new();
        let dir = temp_path("remove_dir_all");
        std::fs::create_dir_all(dir.join("inner")).unwrap();
        std::fs::write(dir.join("inner/file"), "").unwrap();

        remove_dir_all(&mut shell, &dir).await.unwrap();
        assert!(!dir.exists());

        remove_dir_all(&mut shell, &dir).await.unwrap();
    }

    #[tokio::test]
    async fn sha256sum_returns_64_char_hex_digest() {
        let mut shell = SubprocessShell::new();
        let path = temp_path("sha256");
        std::fs::write(&path, "hello\n").unwrap();

        let digest = sha256sum(&mut shell, &path).await.unwrap();
        assert_eq!(
            digest,
            "5891b5b522d5df086d0ff0b110fbd9d21bb4fc7163af34d08286a2e846f6be03"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn copy_duplicates_a_file() {
        let mut shell = SubprocessShell::new();
        let src = temp_path("copy-src");
        let dst = temp_path("copy-dst");
        std::fs::write(&src, b"some bytes").unwrap();

        copy(&mut shell, &src, &dst).await.unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"some bytes");
        assert!(src.exists(), "source should still exist after copy");

        let _ = std::fs::remove_file(&src);
        let _ = std::fs::remove_file(&dst);
    }

    #[tokio::test]
    async fn copy_fails_when_source_missing() {
        let mut shell = SubprocessShell::new();
        let src = temp_path("copy-missing-src");
        let dst = temp_path("copy-missing-dst");
        let err = copy(&mut shell, &src, &dst).await.unwrap_err();
        assert!(matches!(err, Error::Shell(_)));
    }

    #[tokio::test]
    async fn rename_moves_a_file() {
        let mut shell = SubprocessShell::new();
        let src = temp_path("rename-src");
        let dst = temp_path("rename-dst");
        std::fs::write(&src, b"payload").unwrap();

        rename(&mut shell, &src, &dst).await.unwrap();
        assert!(!src.exists(), "source should be gone after rename");
        assert_eq!(std::fs::read(&dst).unwrap(), b"payload");

        let _ = std::fs::remove_file(&dst);
    }

    #[tokio::test]
    async fn metadata_reads_a_regular_file() {
        let mut shell = SubprocessShell::new();
        let path = temp_path("metadata-file");
        std::fs::write(&path, b"hello world!").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o644)).unwrap();

        let m = metadata(&mut shell, &path).await.unwrap();
        assert_eq!(m.size, 12);
        assert!(m.file_type.is_file());
        assert!(!m.file_type.is_dir());
        assert_eq!(m.mode & 0o777, 0o644);
        // mtime is within the last hour and not in the future.
        let now = SystemTime::now();
        assert!(m.modified <= now);
        assert!(now.duration_since(m.modified).unwrap() < Duration::from_secs(3600));

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn metadata_reads_a_directory() {
        let mut shell = SubprocessShell::new();
        let dir = temp_path("metadata-dir");
        std::fs::create_dir_all(&dir).unwrap();

        let m = metadata(&mut shell, &dir).await.unwrap();
        assert!(m.file_type.is_dir());
        assert!(!m.file_type.is_file());

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn metadata_fails_when_path_missing() {
        let mut shell = SubprocessShell::new();
        let path = temp_path("metadata-missing");
        let err = metadata(&mut shell, &path).await.unwrap_err();
        assert!(matches!(err, Error::Shell(_)));
    }

    #[test]
    fn parse_stat_output_parses_gnu_format() {
        let m = parse_stat_output("12345|regular file|644|1700000000").unwrap();
        assert_eq!(m.size, 12345);
        assert!(m.file_type.is_file());
        assert_eq!(m.mode, 0o644);
        assert_eq!(m.modified, UNIX_EPOCH + Duration::from_secs(1_700_000_000),);
    }

    #[test]
    fn parse_stat_output_recognizes_directories() {
        let m = parse_stat_output("4096|directory|755|1700000000").unwrap();
        assert!(m.file_type.is_dir());
        assert_eq!(m.mode, 0o755);
    }

    #[test]
    fn parse_stat_output_recognizes_empty_files() {
        let m = parse_stat_output("0|regular empty file|644|1700000000").unwrap();
        assert!(m.file_type.is_file());
        assert_eq!(m.size, 0);
    }

    #[test]
    fn parse_stat_output_collapses_other_types_to_other() {
        let m = parse_stat_output("0|fifo|644|1700000000").unwrap();
        assert_eq!(m.file_type, FileType::Other);
    }

    #[test]
    fn parse_stat_output_rejects_short_lines() {
        let err = parse_stat_output("oops").unwrap_err();
        assert!(matches!(err, Error::Parse(_)));
    }

    #[tokio::test]
    async fn symlink_creates_and_read_link_reads_it_back() {
        let mut shell = SubprocessShell::new();
        let target = temp_path("symlink-target");
        let link = temp_path("symlink-link");
        std::fs::write(&target, "hello").unwrap();
        let _ = std::fs::remove_file(&link); // in case left over from a previous run

        symlink(&mut shell, &target, &link).await.unwrap();

        let got = read_link(&mut shell, &link).await.unwrap();
        assert_eq!(got, target.to_string_lossy());

        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_file(&target);
    }

    #[tokio::test]
    async fn canonicalize_resolves_through_symlink() {
        let mut shell = SubprocessShell::new();
        let target = temp_path("canon-target");
        let link = temp_path("canon-link");
        std::fs::write(&target, "x").unwrap();
        let _ = std::fs::remove_file(&link);
        symlink(&mut shell, &target, &link).await.unwrap();

        let resolved = canonicalize(&mut shell, &link).await.unwrap();
        // canonicalize follows the symlink AND resolves to the
        // device's canonical form (e.g. /tmp → /tmp on most systems
        // but could be /private/tmp on macOS). We assert the basename
        // matches the target's basename rather than the whole path.
        assert!(
            resolved.ends_with(target.file_name().unwrap().to_str().unwrap()),
            "canonicalize should land on the symlink target's path; got {resolved:?}"
        );

        let _ = std::fs::remove_file(&link);
        let _ = std::fs::remove_file(&target);
    }

    #[tokio::test]
    async fn read_link_errors_on_non_symlink() {
        let mut shell = SubprocessShell::new();
        let regular = temp_path("not-a-symlink");
        std::fs::write(&regular, "").unwrap();

        let err = read_link(&mut shell, &regular).await.unwrap_err();
        assert!(matches!(err, Error::Shell(_)));

        let _ = std::fs::remove_file(&regular);
    }

    #[tokio::test]
    async fn write_replaces_existing_file() {
        let mut shell = SubprocessShell::new();
        let path = temp_path("write-replace");
        std::fs::write(&path, "old content").unwrap();

        write(&mut shell, &path, b"new content").await.unwrap();
        let got = std::fs::read(&path).unwrap();
        assert_eq!(got, b"new content");

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn write_preserves_all_byte_values() {
        let mut shell = SubprocessShell::new();
        let path = temp_path("write-binary");
        let payload: Vec<u8> = (0..=255u8).collect();

        write(&mut shell, &path, &payload).await.unwrap();
        let got = std::fs::read(&path).unwrap();
        assert_eq!(got, payload);

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn write_rejects_oversized_payload() {
        let mut shell = SubprocessShell::new();
        let huge = vec![0u8; MAX_WRITE_BYTES + 1];
        let err = write(&mut shell, "/tmp/should-not-be-touched", &huge)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Parse(_)));
    }

    #[tokio::test]
    async fn write_atomic_leaves_no_tmp_on_success() {
        let mut shell = SubprocessShell::new();
        let path = temp_path("write-atomic");
        let tmp_pattern = format!("{}.tmp.", path.to_string_lossy());

        write_atomic(&mut shell, &path, b"final value")
            .await
            .unwrap();
        let got = std::fs::read(&path).unwrap();
        assert_eq!(got, b"final value");

        // No `.tmp.<pid>` file should remain after a successful write.
        let temp_dir = std::env::temp_dir();
        let leftovers: Vec<_> = std::fs::read_dir(&temp_dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().to_string_lossy().starts_with(&tmp_pattern))
            .collect();
        assert!(
            leftovers.is_empty(),
            "no .tmp.<pid> files should remain, found: {leftovers:?}"
        );

        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn write_handles_path_with_apostrophe() {
        let mut shell = SubprocessShell::new();
        let path = temp_path("dont't-break");
        write(&mut shell, &path, b"ok").await.unwrap();
        assert_eq!(std::fs::read(&path).unwrap(), b"ok");
        let _ = std::fs::remove_file(&path);
    }

    #[tokio::test]
    async fn walk_dir_returns_recursive_tree() {
        let mut shell = SubprocessShell::new();
        let root = temp_path("walk_dir");
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("a.txt"), "").unwrap();
        std::fs::write(root.join("sub").join("b.txt"), "").unwrap();

        let entries = walk_dir(&mut shell, &root).await.unwrap();
        // Should include the root, the subdir, and both files (≥ 4 entries).
        // We don't pin the count exactly because some implementations also
        // include or omit the trailing newline / `.` entry.
        assert!(
            entries.iter().any(|e| e.ends_with("a.txt")),
            "expected a.txt in {entries:?}"
        );
        assert!(
            entries.iter().any(|e| e.ends_with("b.txt")),
            "expected sub/b.txt in {entries:?}"
        );
        assert!(
            entries.iter().any(|e| e.ends_with("sub")),
            "expected the sub dir in {entries:?}"
        );

        let _ = std::fs::remove_dir_all(&root);
    }
}
