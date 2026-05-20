//! `eshell info` — one-page device summary (pretty or JSON).

use std::io::IsTerminal;
use std::process::ExitCode;

use anyhow::Result;
use embedded_shell::shell::Command;
use embedded_shell_linux::fs;
use serde::Serialize;

use crate::cli::InfoArgs;
use crate::shell::open_linux;

#[derive(Serialize)]
struct DeviceInfo {
    port: String,
    os: String,
    kernel: Kernel,
    hostname: String,
    uptime: Uptime,
    memory: Memory,
    root_fs: RootFs,
    ipv4: Vec<NetIface>,
}

#[derive(Serialize)]
struct Kernel {
    release: String,
    arch: String,
}

#[derive(Serialize)]
struct Uptime {
    pretty: String,
    load_1: f64,
    load_5: f64,
    load_15: f64,
}

#[derive(Serialize)]
struct Memory {
    total: String,
    used: String,
    available: Option<String>,
}

#[derive(Serialize)]
struct RootFs {
    size: String,
    used: String,
    avail: String,
    use_percent: String,
}

#[derive(Serialize)]
struct NetIface {
    iface: String,
    cidr: String,
}

pub async fn run(args: InfoArgs, port: Option<&str>, password: Option<&str>) -> Result<ExitCode> {
    let port_label = port.unwrap_or("local").to_string();
    let mut shell = open_linux(port, password).await?;

    let os_release = fs::read_to_string(&mut *shell, "/etc/os-release")
        .await
        .unwrap_or_default();

    // Three values in one round-trip: kernel release, machine arch, hostname.
    let sys_raw = shell
        .run(&Command::new("sh").args([
            "-c",
            "printf '%s\\n%s\\n%s\\n' \"$(uname -r)\" \"$(uname -m)\" \"$(hostname)\"",
        ]))
        .await?
        .stdout()
        .unwrap_or("")
        .to_string();

    let uptime_raw = first_line(shell.run(&Command::new("uptime")).await?.stdout());
    let memory_raw = first_line(
        shell
            .run(&Command::new("sh").args(["-c", "free -h 2>/dev/null | sed -n '2p'"]))
            .await?
            .stdout(),
    );
    let root_fs_raw = first_line(
        shell
            .run(&Command::new("sh").args(["-c", "df -h / 2>/dev/null | tail -1"]))
            .await?
            .stdout(),
    );
    // Tag each interface's address as `iface=cidr` device-side so the
    // host-side parse is trivial. Falls back to `hostname -I` (space-
    // separated addrs, no iface) on devices without `ip`.
    let ipv4_raw = shell
        .run(&Command::new("sh").args([
            "-c",
            "ip -4 -o addr show scope global 2>/dev/null | awk '{print $2 \"=\" $4}' \
             || hostname -I 2>/dev/null",
        ]))
        .await?
        .stdout()
        .unwrap_or("")
        .to_string();

    let (kernel, hostname) = parse_sys(&sys_raw);
    let info = DeviceInfo {
        port: port_label,
        os: pretty_name(&os_release),
        kernel,
        hostname,
        uptime: parse_uptime(&uptime_raw),
        memory: parse_memory(&memory_raw),
        root_fs: parse_root_fs(&root_fs_raw),
        ipv4: parse_ipv4(&ipv4_raw),
    };

    let _ = shell.deactivate().await;

    if args.json {
        serde_json::to_writer(std::io::stdout(), &info)?;
        println!();
    } else {
        let use_color = std::io::stdout().is_terminal();
        render_pretty(&info, use_color);
    }
    Ok(ExitCode::SUCCESS)
}

fn render_pretty(info: &DeviceInfo, use_color: bool) {
    // Pad the longest label (`Hostname` = 8 chars) with a 3-char gap
    // so values align in their columns within each section.
    const LABEL_PAD: usize = 11;

    println!();
    println!("{}", bold(&info.port, use_color));

    println!();
    println!("{}", bold("System", use_color));
    row("OS", &info.os, LABEL_PAD);
    row(
        "Kernel",
        &format!("{} ({})", info.kernel.release, info.kernel.arch),
        LABEL_PAD,
    );
    row("Hostname", &info.hostname, LABEL_PAD);
    row(
        "Uptime",
        &format!(
            "up {}   load {:.2} / {:.2} / {:.2}",
            info.uptime.pretty, info.uptime.load_1, info.uptime.load_5, info.uptime.load_15
        ),
        LABEL_PAD,
    );

    println!();
    println!("{}", bold("Storage", use_color));
    let memory_line = match &info.memory.available {
        Some(av) => format!(
            "{} used / {} total   ({} available)",
            info.memory.used, info.memory.total, av
        ),
        None => format!("{} used / {} total", info.memory.used, info.memory.total),
    };
    row("Memory", &memory_line, LABEL_PAD);
    row(
        "Root fs",
        &format!(
            "{} used / {} total   ({})",
            info.root_fs.used, info.root_fs.size, info.root_fs.use_percent
        ),
        LABEL_PAD,
    );

    println!();
    println!("{}", bold("Interfaces", use_color));
    if info.ipv4.is_empty() {
        println!("  (no IPv4 addresses)");
    } else {
        let iface_pad = info.ipv4.iter().map(|n| n.iface.len()).max().unwrap_or(0);
        for n in &info.ipv4 {
            println!(
                "  {:<iface_pad$}   {}",
                n.iface,
                n.cidr,
                iface_pad = iface_pad
            );
        }
    }
    println!();
}

/// Print one labelled row within a section: `  LABEL   value` with the
/// label left-aligned in a fixed-width column.
fn row(label: &str, value: &str, pad: usize) {
    println!("  {label:<pad$}{value}");
}

fn bold(s: &str, use_color: bool) -> String {
    if use_color {
        format!("\x1b[1m{s}\x1b[0m")
    } else {
        s.to_string()
    }
}

fn first_line(s: Option<&str>) -> String {
    s.unwrap_or("")
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .to_string()
}

fn pretty_name(os_release: &str) -> String {
    for line in os_release.lines() {
        if let Some(v) = line.strip_prefix("PRETTY_NAME=") {
            return v.trim_matches('"').to_string();
        }
    }
    "(unknown)".to_string()
}

fn parse_sys(raw: &str) -> (Kernel, String) {
    let mut lines = raw.lines();
    let release = lines.next().unwrap_or("").trim().to_string();
    let arch = lines.next().unwrap_or("").trim().to_string();
    let hostname = lines.next().unwrap_or("").trim().to_string();
    (Kernel { release, arch }, hostname)
}

fn parse_uptime(raw: &str) -> Uptime {
    // Real `uptime` output uses two spaces in a few places (eg. after
    // each comma and before `load average:`). Normalize whitespace
    // first so a single literal separator below matches reliably.
    // Example raw: "15:08:18 up 2 days,  1:42,  7 users,  load average: 0.09, 0.16, 0.12"
    let normalized: String = raw.split_whitespace().collect::<Vec<_>>().join(" ");
    let after_up = normalized
        .split_once(" up ")
        .map(|(_, r)| r.to_string())
        .unwrap_or(normalized);
    let (lhs, load_str) = after_up
        .split_once(", load average: ")
        .unwrap_or((after_up.as_str(), ""));
    let pretty = strip_trailing_users(lhs);
    let mut iter = load_str
        .split(',')
        .map(|p| p.trim().parse::<f64>().unwrap_or(0.0));
    Uptime {
        pretty,
        load_1: iter.next().unwrap_or(0.0),
        load_5: iter.next().unwrap_or(0.0),
        load_15: iter.next().unwrap_or(0.0),
    }
}

fn strip_trailing_users(s: &str) -> String {
    // "2 days,  1:42,  7 users" -> "2 days, 1:42"
    // "1 user"                  -> ""
    // "47 min"                  -> "47 min"
    let mut parts: Vec<&str> = s.split(',').map(str::trim).collect();
    if parts.last().is_some_and(|p| p.contains("user")) {
        parts.pop();
    }
    parts.join(", ")
}

fn parse_memory(raw: &str) -> Memory {
    // Example: "Mem:  62Gi  10Gi  6.5Gi  1.9Gi  48Gi  52Gi"
    //   cols:  0     1     2     3      4      5      6
    //          ^Mem  total used  free   shared buff/  available
    let cols: Vec<&str> = raw.split_whitespace().collect();
    Memory {
        total: cols.get(1).unwrap_or(&"?").to_string(),
        used: cols.get(2).unwrap_or(&"?").to_string(),
        available: cols.get(6).map(|s| s.to_string()),
    }
}

fn parse_root_fs(raw: &str) -> RootFs {
    // Example: "/dev/mapper/ubuntu--vg-ubuntu--lv  935G  638G  250G  72% /"
    let cols: Vec<&str> = raw.split_whitespace().collect();
    RootFs {
        size: cols.get(1).unwrap_or(&"?").to_string(),
        used: cols.get(2).unwrap_or(&"?").to_string(),
        avail: cols.get(3).unwrap_or(&"?").to_string(),
        use_percent: cols.get(4).unwrap_or(&"?").to_string(),
    }
}

fn parse_ipv4(raw: &str) -> Vec<NetIface> {
    let mut out = Vec::new();
    for line in raw.lines() {
        let line = line.trim();
        if line.is_empty() {
            continue;
        }
        if let Some((iface, cidr)) = line.split_once('=') {
            out.push(NetIface {
                iface: iface.trim().to_string(),
                cidr: cidr.trim().to_string(),
            });
        } else {
            // `hostname -I` fallback: just bare addresses, no interface.
            for addr in line.split_whitespace() {
                out.push(NetIface {
                    iface: "(?)".to_string(),
                    cidr: addr.to_string(),
                });
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pretty_name_extracts_quoted() {
        let osr = "NAME=\"Ubuntu\"\nPRETTY_NAME=\"Ubuntu 24.04.4 LTS\"\nID=ubuntu\n";
        assert_eq!(pretty_name(osr), "Ubuntu 24.04.4 LTS");
    }

    #[test]
    fn pretty_name_falls_back_when_absent() {
        assert_eq!(pretty_name("NAME=Foo\n"), "(unknown)");
    }

    #[test]
    fn parse_sys_splits_three_lines() {
        let (k, h) = parse_sys("6.17.0-29-generic\nx86_64\nlbdow141\n");
        assert_eq!(k.release, "6.17.0-29-generic");
        assert_eq!(k.arch, "x86_64");
        assert_eq!(h, "lbdow141");
    }

    #[test]
    fn parse_uptime_extracts_span_and_loads() {
        let raw = "15:08:18 up 2 days,  1:42,  7 users,  load average: 0.09, 0.16, 0.12";
        let u = parse_uptime(raw);
        assert_eq!(u.pretty, "2 days, 1:42");
        assert!((u.load_1 - 0.09).abs() < 1e-9);
        assert!((u.load_5 - 0.16).abs() < 1e-9);
        assert!((u.load_15 - 0.12).abs() < 1e-9);
    }

    #[test]
    fn parse_uptime_single_user_singular() {
        let raw = "13:25:28 up 1 min,  1 user,  load average: 0.10, 0.05, 0.01";
        let u = parse_uptime(raw);
        assert_eq!(u.pretty, "1 min");
        assert!((u.load_1 - 0.10).abs() < 1e-9);
    }

    #[test]
    fn parse_uptime_garbage_does_not_panic() {
        let u = parse_uptime("garbage line with no markers");
        assert!(u.load_1 == 0.0 && u.load_5 == 0.0 && u.load_15 == 0.0);
    }

    #[test]
    fn parse_memory_modern_columns() {
        let raw =
            "Mem:            62Gi        10Gi       6.5Gi       1.9Gi        48Gi        52Gi";
        let m = parse_memory(raw);
        assert_eq!(m.total, "62Gi");
        assert_eq!(m.used, "10Gi");
        assert_eq!(m.available.as_deref(), Some("52Gi"));
    }

    #[test]
    fn parse_memory_older_busybox_no_available() {
        let raw = "Mem:           512M       240M       272M         0";
        let m = parse_memory(raw);
        assert_eq!(m.total, "512M");
        assert_eq!(m.used, "240M");
        assert!(m.available.is_none());
    }

    #[test]
    fn parse_root_fs_columns() {
        let raw = "/dev/mapper/ubuntu--vg-ubuntu--lv  935G  638G  250G  72% /";
        let r = parse_root_fs(raw);
        assert_eq!(r.size, "935G");
        assert_eq!(r.used, "638G");
        assert_eq!(r.avail, "250G");
        assert_eq!(r.use_percent, "72%");
    }

    #[test]
    fn parse_ipv4_iface_cidr_pairs() {
        let raw = "enp109s0=192.168.10.66/24\ndocker0=172.17.0.1/16\n";
        let v = parse_ipv4(raw);
        assert_eq!(v.len(), 2);
        assert_eq!(v[0].iface, "enp109s0");
        assert_eq!(v[0].cidr, "192.168.10.66/24");
        assert_eq!(v[1].iface, "docker0");
    }

    #[test]
    fn parse_ipv4_hostname_dash_i_fallback() {
        let v = parse_ipv4("192.168.1.5 10.0.0.5\n");
        assert_eq!(v.len(), 2);
        assert!(v.iter().all(|n| n.iface == "(?)"));
    }

    #[test]
    fn parse_ipv4_empty_input() {
        assert!(parse_ipv4("").is_empty());
        assert!(parse_ipv4("\n\n").is_empty());
    }
}
