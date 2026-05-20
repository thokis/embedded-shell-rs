//! Test-local conveniences for the hardware-in-the-loop binaries.
//!
//! The heavy lifting — state-aware probes, transition logic — lives in
//! [`embedded_shell::test_utils`] (gated by the `test-utils` feature,
//! which is required by the `hardware_*` test binaries via
//! `[[test]] required-features` in `Cargo.toml`).
//!
//! This module just owns env-var reading (port + optional prompt
//! override) and `tracing-subscriber` setup, then forwards to the
//! library helpers.

// hardware_linux uses linux helpers; hardware_uboot uses uboot ones —
// each binary sees half of this module as "unused".
#![allow(dead_code)]

use embedded_shell::shell::{LinuxSerialShell, UBootSerialShell};
use embedded_shell::test_utils;
use tracing_subscriber::{EnvFilter, prelude::*};

pub fn init_logging() {
    let _ = tracing_subscriber::registry()
        .with(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("warn")))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr))
        .try_init();
}

pub fn linux_port() -> String {
    std::env::var("EMBEDDED_SHELL_LINUX_PORT").unwrap_or_else(|_| "/dev/ttyUSB0".to_string())
}

pub fn uboot_port() -> String {
    std::env::var("EMBEDDED_SHELL_UBOOT_PORT").unwrap_or_else(|_| "/dev/ttyUSB0".to_string())
}

fn shell_prompt() -> Option<String> {
    std::env::var("EMBEDDED_SHELL_LINUX_SHELL_PROMPT").ok()
}

pub async fn open_at_linux() -> LinuxSerialShell {
    test_utils::open_at_linux(&linux_port(), shell_prompt().as_deref()).await
}

pub async fn open_at_uboot() -> UBootSerialShell {
    test_utils::open_at_uboot(&uboot_port(), shell_prompt().as_deref()).await
}
