//! `eshell devices` — list serial ports + USB descriptors.
//!
//! Linux-only. Scans `/dev/ttyUSB*` and `/dev/ttyACM*`, walks `sysfs`
//! to attach the USB vendor/product/manufacturer/serial-number from
//! the parent USB device, and checks `/proc/*/fd/*` for any process
//! holding the device file open.

use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use anyhow::Result;
use serde::Serialize;

use crate::cli::DevicesArgs;

#[derive(Serialize)]
struct Device {
    path: String,
    driver: Option<String>,
    vendor: Option<String>,
    product: Option<String>,
    manufacturer: Option<String>,
    serial: Option<String>,
    claimed_by: Option<Claimer>,
}

#[derive(Serialize)]
struct Claimer {
    pid: u32,
    comm: String,
}

pub fn run(args: DevicesArgs) -> Result<ExitCode> {
    if !cfg!(target_os = "linux") {
        eprintln!("eshell devices: only Linux is supported (sysfs walk is Linux-specific)");
        return Ok(ExitCode::from(2));
    }

    let mut devices = scan_devices()?;
    devices.sort_by(|a, b| a.path.cmp(&b.path));

    if args.json {
        serde_json::to_writer(std::io::stdout(), &devices)?;
        println!();
    } else {
        let use_color = std::io::stdout().is_terminal();
        render_pretty(&devices, use_color);
    }
    Ok(ExitCode::SUCCESS)
}

fn scan_devices() -> Result<Vec<Device>> {
    let mut out = Vec::new();
    let Ok(entries) = fs::read_dir("/dev") else {
        return Ok(out);
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if !(name.starts_with("ttyUSB") || name.starts_with("ttyACM")) {
            continue;
        }
        let path = entry.path();
        let usb = sysfs_usb_descriptors(&name);
        let driver = sysfs_driver(&name);
        let claimed_by = find_claimer(&path);
        out.push(Device {
            path: path.to_string_lossy().into_owned(),
            driver,
            vendor: usb.vendor,
            product: usb.product,
            manufacturer: usb.manufacturer,
            serial: usb.serial,
            claimed_by,
        });
    }
    Ok(out)
}

#[derive(Default)]
struct UsbDescriptors {
    vendor: Option<String>,
    product: Option<String>,
    manufacturer: Option<String>,
    serial: Option<String>,
}

/// Walk up from `/sys/class/tty/<name>/device` until we find a
/// directory containing `idVendor` (the USB device level), then read
/// the descriptor files there.
fn sysfs_usb_descriptors(tty_name: &str) -> UsbDescriptors {
    let sym = PathBuf::from(format!("/sys/class/tty/{tty_name}/device"));
    let Ok(mut p) = fs::canonicalize(&sym) else {
        return UsbDescriptors::default();
    };
    while p.parent().is_some() {
        if p.join("idVendor").exists() {
            return UsbDescriptors {
                vendor: read_trimmed(&p.join("idVendor")),
                product: read_trimmed(&p.join("product"))
                    .or_else(|| read_trimmed(&p.join("idProduct"))),
                manufacturer: read_trimmed(&p.join("manufacturer")),
                serial: read_trimmed(&p.join("serial")),
            };
        }
        let Some(parent) = p.parent() else { break };
        p = parent.to_path_buf();
    }
    UsbDescriptors::default()
}

fn sysfs_driver(tty_name: &str) -> Option<String> {
    let link = PathBuf::from(format!("/sys/class/tty/{tty_name}/device/driver"));
    let target = fs::read_link(&link).ok()?;
    target.file_name().map(|s| s.to_string_lossy().into_owned())
}

fn read_trimmed(p: &Path) -> Option<String> {
    fs::read_to_string(p)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Scan `/proc/<pid>/fd/*` to find a process that has `dev_path`
/// open. Returns the first match (a device is typically held by one
/// process at a time). Slow on systems with many processes but
/// acceptable for a one-shot interactive command.
fn find_claimer(dev_path: &Path) -> Option<Claimer> {
    let target = fs::canonicalize(dev_path).ok()?;
    for proc_entry in fs::read_dir("/proc").ok()?.flatten() {
        let name = proc_entry.file_name();
        let Some(pid) = name.to_string_lossy().parse::<u32>().ok() else {
            continue;
        };
        let fd_dir = proc_entry.path().join("fd");
        let Ok(fds) = fs::read_dir(&fd_dir) else {
            continue;
        };
        for fd_entry in fds.flatten() {
            // /proc/PID/fd/N is a symlink to the open file. If we
            // can't read it (other user's process, no permission),
            // skip silently.
            let Ok(resolved) = fs::read_link(fd_entry.path()) else {
                continue;
            };
            if resolved == target {
                let comm = fs::read_to_string(proc_entry.path().join("comm"))
                    .ok()
                    .map(|s| s.trim().to_string())
                    .unwrap_or_else(|| "?".to_string());
                return Some(Claimer { pid, comm });
            }
        }
    }
    None
}

fn render_pretty(devices: &[Device], use_color: bool) {
    if devices.is_empty() {
        println!("(no /dev/ttyUSB* or /dev/ttyACM* devices found)");
        return;
    }

    let path_w = devices.iter().map(|d| d.path.len()).max().unwrap_or(0);
    let driver_w = devices
        .iter()
        .map(|d| d.driver.as_deref().unwrap_or("-").len())
        .max()
        .unwrap_or(0);
    let desc_w = devices.iter().map(|d| describe(d).len()).max().unwrap_or(0);

    println!();
    println!(
        "  {}  {}  {}  {}",
        bold(&format!("{:<path_w$}", "Path"), use_color),
        bold(&format!("{:<driver_w$}", "Driver"), use_color),
        bold(&format!("{:<desc_w$}", "Description"), use_color),
        bold("Status", use_color),
    );
    for d in devices {
        let driver = d.driver.as_deref().unwrap_or("-");
        let desc = describe(d);
        let status = match &d.claimed_by {
            Some(c) => colored_status(
                &format!("claimed by {}({})", c.comm, c.pid),
                "31",
                use_color,
            ),
            None => colored_status("free", "32", use_color),
        };
        println!(
            "  {path:<path_w$}  {driver:<driver_w$}  {desc:<desc_w$}  {status}",
            path = d.path,
        );
    }
    println!();
}

fn describe(d: &Device) -> String {
    let mut parts = Vec::new();
    if let Some(m) = &d.manufacturer {
        parts.push(m.clone());
    }
    if let Some(p) = &d.product {
        parts.push(p.clone());
    }
    let head = parts.join(" ");
    match (&d.vendor, &d.serial) {
        (_, Some(s)) if !head.is_empty() => format!("{head} (s/n {s})"),
        (_, Some(s)) => format!("s/n {s}"),
        (Some(v), None) if !head.is_empty() => format!("{head} [{v}]"),
        (Some(v), None) => v.clone(),
        (None, None) if head.is_empty() => "(no USB descriptors)".to_string(),
        (None, None) => head,
    }
}

fn bold(s: &str, use_color: bool) -> String {
    if use_color {
        format!("\x1b[1m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

fn colored_status(s: &str, code: &str, use_color: bool) -> String {
    if use_color {
        format!("\x1b[{code}m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dev_with(manufacturer: Option<&str>, product: Option<&str>, serial: Option<&str>) -> Device {
        Device {
            path: "/dev/ttyUSB0".into(),
            driver: None,
            vendor: None,
            product: product.map(String::from),
            manufacturer: manufacturer.map(String::from),
            serial: serial.map(String::from),
            claimed_by: None,
        }
    }

    #[test]
    fn describe_with_manufacturer_product_and_serial() {
        let d = dev_with(Some("FTDI"), Some("FT232R USB UART"), Some("A50285BI"));
        assert_eq!(describe(&d), "FTDI FT232R USB UART (s/n A50285BI)");
    }

    #[test]
    fn describe_with_only_product() {
        let d = dev_with(None, Some("EC25"), None);
        assert_eq!(describe(&d), "EC25");
    }

    #[test]
    fn describe_with_no_descriptors_at_all() {
        let d = dev_with(None, None, None);
        assert_eq!(describe(&d), "(no USB descriptors)");
    }

    #[test]
    fn describe_serial_without_head() {
        let d = dev_with(None, None, Some("XYZ123"));
        assert_eq!(describe(&d), "s/n XYZ123");
    }
}
