//! Login and shell prompt detection helpers.
//!
//! Exposes the byte-level searchers used by the activate state machines
//! of [`LinuxSerialShell`] and [`UBootSerialShell`]. Most users won't
//! interact with this module directly — pass a custom pattern via
//! `LinuxSerialShellBuilder::shell_prompt` / `login_prompt` (or the
//! U-Boot equivalent) for the common case of overriding the default
//! prompt regex.
//!
//! # Operating on raw bytes
//!
//! Prompts are detected against the serial byte stream **before** any
//! UTF-8 decoding — partial multi-byte sequences mid-read would
//! otherwise risk losing a match. Each searcher takes `&[u8]` and
//! returns the **exclusive end offset** of the matched prompt
//! (`Some(end)`) or `None` for no match. The end-offset convention
//! lets a caller `buf[..end]` to take everything up to and including
//! the prompt, leaving the rest of the buffer for the next read.
//!
//! # The "Last login:" exclusion
//!
//! [`find_linux_login`] specifically rejects `Last login: ...` lines —
//! the banner OpenSSH prints on session start. Rust's `regex` crate
//! doesn't support look-around, so the exclusion is hand-coded rather
//! than expressed as `(?<!Last )login: `.
//!
//! [`LinuxSerialShell`]: super::LinuxSerialShell
//! [`UBootSerialShell`]: super::UBootSerialShell

use regex::bytes::Regex;
use std::sync::OnceLock;

/// Either the canned default detector for a prompt or a user-supplied regex.
/// Both encode the same end-offset contract that `SerialTransport::read_until`
/// (and the `activate` state machine) expect.
pub(crate) enum PromptDetector {
    Default(fn(&[u8]) -> Option<usize>),
    Custom(Regex),
}

impl PromptDetector {
    pub(crate) fn find(&self, buf: &[u8]) -> Option<usize> {
        match self {
            PromptDetector::Default(f) => f(buf),
            PromptDetector::Custom(re) => re.find(buf).map(|m| m.end()),
        }
    }

    /// Compile a user-supplied regex into a `Custom` detector. Returns
    /// [`crate::shell::ShellError::InvalidRegex`] on a bad pattern so callers
    /// can propagate the failure — patterns may come from config files or
    /// runtime arguments, so this is a recoverable error, not a panic.
    pub(crate) fn try_compile(pattern: &str) -> Result<Self, crate::shell::error::ShellError> {
        let re = Regex::new(pattern).map_err(|source| {
            crate::shell::error::ShellError::InvalidRegex {
                pattern: pattern.to_string(),
                source,
            }
        })?;
        Ok(PromptDetector::Custom(re))
    }
}

fn login_linux_inner() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"login: ").unwrap())
}

fn login_uboot() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"Hit any key to stop autoboot:").unwrap())
}

fn shell_linux() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"(root@.+:.+\#)").unwrap())
}

fn shell_uboot() -> &'static Regex {
    static RE: OnceLock<Regex> = OnceLock::new();
    RE.get_or_init(|| Regex::new(r"=>").unwrap())
}

/// Finds a Linux login prompt (`login: `) in `buf`, ignoring SSH-style
/// `Last login: ...` banner lines.
///
/// Returns the exclusive end offset of the match, or `None` if `buf`
/// doesn't contain a non-banner `login: `.
pub fn find_linux_login(buf: &[u8]) -> Option<usize> {
    login_linux_inner().find_iter(buf).find_map(|m| {
        let start = m.start();
        if start >= 5 && &buf[start - 5..start] == b"Last " {
            None
        } else {
            Some(m.end())
        }
    })
}

/// Finds a U-Boot autoboot banner (`Hit any key to stop autoboot:`) in
/// `buf`. Returns the exclusive end offset of the match, or `None`.
pub fn find_uboot_login(buf: &[u8]) -> Option<usize> {
    login_uboot().find(buf).map(|m| m.end())
}

/// Finds a Linux root shell prompt of the form `root@<host>:<cwd>#` in
/// `buf`. Returns the exclusive end offset of the match (the `#`
/// character), or `None`.
pub fn find_linux_shell(buf: &[u8]) -> Option<usize> {
    shell_linux().find(buf).map(|m| m.end())
}

/// Finds a U-Boot shell prompt (`=>`) in `buf`. Returns the exclusive
/// end offset of the match, or `None`.
pub fn find_uboot_shell(buf: &[u8]) -> Option<usize> {
    shell_uboot().find(buf).map(|m| m.end())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn linux_login_matches_plain_prompt() {
        let buf = b"device login: ";
        let end = find_linux_login(buf).unwrap();
        assert_eq!(&buf[..end], b"device login: ");
    }

    #[test]
    fn linux_login_ignores_last_login_line() {
        assert_eq!(find_linux_login(b"Last login: Tue ..."), None);
    }

    #[test]
    fn linux_login_finds_real_after_last_login() {
        let buf = b"Last login: Tue Jan 1\r\ndevice login: ";
        let end = find_linux_login(buf).unwrap();
        assert_eq!(end, buf.len());
        assert!(buf[..end].ends_with(b"login: "));
    }

    #[test]
    fn linux_login_handles_buffer_too_short_for_lookbehind() {
        let buf = b"login: ";
        assert_eq!(find_linux_login(buf), Some(buf.len()));
    }

    #[test]
    fn uboot_login_matches_autoboot_banner() {
        assert!(find_uboot_login(b"Hit any key to stop autoboot:  3").is_some());
    }

    #[test]
    fn linux_shell_matches_root_prompt() {
        let buf = b"root@device:~# ";
        let end = find_linux_shell(buf).unwrap();
        // The trailing space isn't captured by the regex; the # is the last
        // matched byte.
        assert_eq!(&buf[..end], b"root@device:~#");
    }

    #[test]
    fn linux_shell_rejects_non_root() {
        assert_eq!(find_linux_shell(b"user@host:~$ "), None);
    }

    #[test]
    fn uboot_shell_matches_arrow_prompt() {
        let buf = b"=> ";
        let end = find_uboot_shell(buf).unwrap();
        assert_eq!(&buf[..end], b"=>");
    }
}
