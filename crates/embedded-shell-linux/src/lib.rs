//! Thin async wrappers around common Linux userland CLI tools,
//! executed over any [`embedded_shell::shell::LinuxShell`].
//!
//! Each wrapper runs a single command on the device via the shell and
//! returns a typed Rust value. The [`LinuxShell`] bound restricts the
//! wrappers to shells whose device-side userland is Linux-style —
//! excluding [`UBootSerialShell`][embedded_shell::shell::UBootSerialShell]
//! (which has no `cat`, `ls`, `chmod`, `sh -c`, etc.) at the type level.
//!
//! # Naming
//!
//! Where a concept has a direct analogue in Rust's standard library, the
//! module and function names shadow std — for example [`fs`] mirrors
//! [`std::fs`]. Where there is no std analogue (`systemd`, `nmcli`,
//! `mmcli`, …), the module is named after the system package it wraps.
//!
//! Cargo feature names reflect the **device-side dependency**:
//! enabling the `coreutils` feature gives you the [`fs`] module
//! (because the underlying tools come from coreutils or busybox). The
//! asymmetry is intentional — feature names answer "what does the
//! device need installed?", module names answer "what does the Rust API
//! look like?".
//!
//! # Features
//!
//! Default-on (present on essentially every embedded Linux):
//!
//! - `coreutils` — enables the [`fs`] module
//! - `iputils` — enables the `iputils` module (`ping`, `arping`)
//!
//! Opt-in (not universal on minimal embedded distros):
//!
//! - `systemd` — enables the [`systemd`] module (`systemctl`) and the
//!   [`journalctl`] module (reading the systemd journal)
//! - `networkmanager` — enables the `networkmanager` module (`nmcli`)
//! - `modemmanager` — enables the `modemmanager` module (`mmcli`)
//! - `iproute2` — enables the `iproute2` module (`ip`, `ss`, `route`)
//!
//! [`LinuxShell`]: embedded_shell::shell::LinuxShell

mod error;

pub use error::{Error, Result};

#[cfg(feature = "coreutils")]
pub mod fs;

#[cfg(feature = "iputils")]
pub mod iputils;

#[cfg(feature = "systemd")]
pub mod systemd;

#[cfg(feature = "systemd")]
pub mod journalctl;

#[cfg(feature = "networkmanager")]
pub mod networkmanager;

#[cfg(feature = "modemmanager")]
pub mod modemmanager;

#[cfg(feature = "iproute2")]
pub mod iproute2;
