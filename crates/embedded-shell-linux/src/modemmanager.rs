//! Wrappers around `mmcli` for inspecting cellular modems managed
//! by ModemManager.
//!
//! Enabled by the opt-in `modemmanager` Cargo feature, which pulls
//! in `serde` and `serde_json` — the parser reads `mmcli -J` (JSON
//! output) rather than the human-readable default.
//!
//! # Device-side requirement
//!
//! `mmcli` on the device's `PATH`, talking to a running
//! ModemManager daemon (typically over D-Bus). Standard on Yocto
//! and Debian-based images that ship with NetworkManager — `nmcli`
//! and `mmcli` are sister tools.
//!
//! # Surface
//!
//! Read-only in v1:
//!
//! - [`list_modems`] — indices of every modem ModemManager knows
//!   about (one number per modem).
//! - [`modem`] — detailed [`Modem`] struct for one index.
//!
//! State-changing operations (enable/disable, connect/disconnect,
//! SIM-slot switching, SMS send/receive, …) aren't exposed yet.
//! mmcli's mutation surface is large and SIM-slot operations in
//! particular have observed side effects on connection state, so
//! they deserve dedicated thought when the use cases are concrete.
//! Drop into `shell.run(Command::new("mmcli").args([...]))` directly
//! when you need them.
//!
//! [`LinuxShell`]: embedded_shell::shell::LinuxShell

use embedded_shell::shell::{Command, LinuxShell};
use serde::Deserialize;

use crate::error::{Error, Result};

/// One cellular modem as ModemManager sees it.
///
/// Combines the most-used fields from `mmcli -m <index> -J`'s
/// `modem.generic` and `modem.3gpp` sections. Field semantics
/// match mmcli's own; values mmcli reports as `"--"` (its
/// "not available" sentinel) are surfaced as `None` here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Modem {
    /// Numeric index used by `mmcli -m <N>`.
    pub index: u32,
    /// Modem state: `disabled`, `enabled`, `searching`, `registered`,
    /// `connecting`, `connected`, `disconnecting`, `failed`, …
    pub state: String,
    /// Manufacturer string from the modem firmware ("Quectel",
    /// "Sierra Wireless", "Telit", …).
    pub manufacturer: Option<String>,
    /// Model name ("EG25-G", "EM7565", …).
    pub model: Option<String>,
    /// Firmware revision string.
    pub revision: Option<String>,
    /// IMEI (15-digit equipment identifier).
    pub imei: Option<String>,
    /// Currently-used radio access technologies (`"lte"`, `"5gnr"`,
    /// `"umts"`, `"gsm"`, …). Often one element; can be empty when
    /// the modem isn't registered.
    pub access_technologies: Vec<String>,
    /// Most-recent signal-quality measurement as a percentage
    /// (0–100). `None` when the modem hasn't reported one.
    pub signal_quality: Option<u8>,
    /// Mobile network operator's name (e.g. "T-Mobile").
    pub operator_name: Option<String>,
    /// Mobile network operator's MCC+MNC code (e.g. "26201" for
    /// T-Mobile DE).
    pub operator_code: Option<String>,
    /// Primary serial port the modem exposes (e.g. `ttyUSB2`,
    /// `wwan0`).
    pub primary_port: Option<String>,
}

impl Modem {
    /// `true` when the modem is in the `connected` state — i.e.
    /// actively carrying packet traffic.
    pub fn is_connected(&self) -> bool {
        self.state == "connected"
    }

    /// `true` when the modem is registered to a network. Includes
    /// `registered` *and* `connected` (a connected modem is by
    /// definition registered).
    pub fn is_registered(&self) -> bool {
        matches!(self.state.as_str(), "registered" | "connected")
    }
}

/// Lists indices of every modem ModemManager knows about.
///
/// Typically one element per physical modem. Returns an empty `Vec`
/// when ModemManager has no modems registered (e.g. the modem
/// hardware isn't connected yet).
///
/// # Errors
///
/// - [`Error::Shell`] if `mmcli` isn't installed or the daemon isn't
///   reachable.
/// - [`Error::Parse`] if the output isn't parseable as the documented
///   JSON shape.
///
/// # Example
///
/// ```ignore
/// use embedded_shell_linux::modemmanager;
///
/// for idx in modemmanager::list_modems(&mut shell).await? {
///     let m = modemmanager::modem(&mut shell, idx).await?;
///     println!("modem {idx}: {} {} ({})", m.manufacturer.as_deref().unwrap_or("?"), m.model.as_deref().unwrap_or("?"), m.state);
/// }
/// ```
pub async fn list_modems(shell: &mut dyn LinuxShell) -> Result<Vec<u32>> {
    let stdout = run_mmcli(shell, &["-L", "-J"]).await?;
    let raw: RawModemList = serde_json::from_str(&stdout)
        .map_err(|e| Error::Parse(format!("mmcli -L -J: {e}; got {stdout:?}")))?;
    let mut indices = Vec::with_capacity(raw.modem_list.len());
    for path in raw.modem_list {
        indices.push(parse_index_from_path(&path)?);
    }
    Ok(indices)
}

/// Returns detailed information for one modem.
///
/// `index` is a value from [`list_modems`] (or `0` for the first
/// modem — `mmcli -m 0` works even if the actual D-Bus path is
/// numbered higher).
///
/// # Errors
///
/// - [`Error::Shell`] if mmcli exits non-zero (most commonly:
///   "couldn't find modem" because the index is wrong, or
///   ModemManager isn't running).
/// - [`Error::Parse`] if the output isn't the expected JSON shape.
///
/// # Example
///
/// ```ignore
/// let m = embedded_shell_linux::modemmanager::modem(&mut shell, 0).await?;
/// if m.is_connected() {
///     println!("{} on {}: {}%", m.access_technologies.join("+"), m.operator_name.as_deref().unwrap_or("?"), m.signal_quality.unwrap_or(0));
/// }
/// ```
pub async fn modem(shell: &mut dyn LinuxShell, index: u32) -> Result<Modem> {
    let stdout = run_mmcli(shell, &["-m", &index.to_string(), "-J"]).await?;
    parse_modem(index, &stdout)
}

/// Details of the SIM card associated with a modem.
///
/// Returned by [`sim`]. mmcli's `--` "not available" placeholders are
/// normalised to `None`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sim {
    /// SIM index in mmcli's numbering.
    pub index: u32,
    /// ICCID (Integrated Circuit Card Identifier) — typically 19-20
    /// digits, uniquely identifies the SIM card hardware.
    pub iccid: Option<String>,
    /// IMSI (International Mobile Subscriber Identity) — identifies
    /// the subscriber on the network. Typically 15 digits.
    pub imsi: Option<String>,
    /// Mobile network operator's name as stored on the SIM
    /// (e.g. "Telekom.de").
    pub operator_name: Option<String>,
    /// Mobile network operator's MCC+MNC code (e.g. "26201").
    pub operator_code: Option<String>,
}

/// Returns SIM details for the modem at `modem_index`'s primary SIM.
///
/// Looks up the modem's `sim` path via `mmcli -m <index> -J`, then
/// queries the SIM directly via `mmcli -i <sim_index> -J`. Two
/// device-side calls per invocation.
///
/// # Errors
///
/// - [`Error::Shell`] if either mmcli call fails (no modem at that
///   index, daemon not running, no SIM inserted, …).
/// - [`Error::Parse`] if the JSON output isn't the expected shape.
///
/// # Example
///
/// ```ignore
/// use embedded_shell_linux::modemmanager;
///
/// let sim = modemmanager::sim(&mut shell, 0).await?;
/// println!("ICCID: {}", sim.iccid.as_deref().unwrap_or("?"));
/// ```
pub async fn sim(shell: &mut dyn LinuxShell, modem_index: u32) -> Result<Sim> {
    // First call: find the SIM's D-Bus path.
    let modem_json = run_mmcli(shell, &["-m", &modem_index.to_string(), "-J"]).await?;
    let sim_path = extract_sim_path(&modem_json)?;
    let sim_index = parse_index_from_path(&sim_path)?;

    // Second call: query that SIM.
    let sim_json = run_mmcli(shell, &["-i", &sim_index.to_string(), "-J"]).await?;
    parse_sim(sim_index, &sim_json)
}

fn extract_sim_path(modem_json: &str) -> Result<String> {
    let raw: RawModemRoot = serde_json::from_str(modem_json)
        .map_err(|e| Error::Parse(format!("modem json: {e}; got {modem_json:?}")))?;
    let path =
        raw.modem.generic.sim.ok_or_else(|| {
            Error::Parse("modem has no associated SIM (none inserted?)".to_string())
        })?;
    if path == "--" || path.is_empty() {
        return Err(Error::Parse(
            "modem has no associated SIM (none inserted?)".to_string(),
        ));
    }
    Ok(path)
}

fn parse_sim(index: u32, json: &str) -> Result<Sim> {
    let raw: RawSimRoot = serde_json::from_str(json)
        .map_err(|e| Error::Parse(format!("mmcli -i -J: {e}; got {json:?}")))?;
    let p = raw.sim.properties;
    Ok(Sim {
        index,
        iccid: unbar(p.iccid),
        imsi: unbar(p.imsi),
        operator_name: unbar(p.operator_name),
        operator_code: unbar(p.operator_code),
    })
}

async fn run_mmcli(shell: &mut dyn LinuxShell, args: &[&str]) -> Result<String> {
    let mut cmd = Command::new("mmcli");
    for a in args {
        cmd = cmd.arg(*a);
    }
    let r = shell.run(&cmd).await?;
    Ok(r.stdout().unwrap_or("").to_string())
}

fn parse_index_from_path(path: &str) -> Result<u32> {
    path.rsplit('/')
        .next()
        .and_then(|s| s.parse::<u32>().ok())
        .ok_or_else(|| Error::Parse(format!("can't extract modem index from path: {path:?}")))
}

fn parse_modem(index: u32, json: &str) -> Result<Modem> {
    let raw: RawModemRoot = serde_json::from_str(json)
        .map_err(|e| Error::Parse(format!("mmcli -m -J: {e}; got {json:?}")))?;
    let RawModem { generic, threegpp } = raw.modem;

    let signal_quality = generic
        .signal_quality
        .as_ref()
        .and_then(|sq| sq.value.parse::<u8>().ok());

    let access_technologies = generic
        .access_technologies
        .unwrap_or_default()
        .into_iter()
        .filter(|s| s != "--" && !s.is_empty())
        .collect();

    let (imei, operator_name, operator_code) = match threegpp {
        Some(t) => (t.imei, t.operator_name, t.operator_code),
        None => (None, None, None),
    };

    Ok(Modem {
        index,
        state: generic.state,
        manufacturer: unbar(generic.manufacturer),
        model: unbar(generic.model),
        revision: unbar(generic.revision),
        imei: unbar(imei),
        access_technologies,
        signal_quality,
        operator_name: unbar(operator_name),
        operator_code: unbar(operator_code),
        primary_port: unbar(generic.primary_port),
    })
}

/// mmcli's "not available" placeholder is the literal string `--`.
/// Map it (and empty strings) to None so callers can use Option
/// semantics without string-matching.
fn unbar(s: Option<String>) -> Option<String> {
    s.filter(|v| !v.is_empty() && v != "--")
}

// ---------- internal JSON shapes ----------

#[derive(Deserialize)]
struct RawModemList {
    #[serde(rename = "modem-list")]
    modem_list: Vec<String>,
}

#[derive(Deserialize)]
struct RawModemRoot {
    modem: RawModem,
}

#[derive(Deserialize)]
struct RawModem {
    generic: RawModemGeneric,
    #[serde(rename = "3gpp")]
    threegpp: Option<RawModem3gpp>,
}

#[derive(Deserialize)]
struct RawModemGeneric {
    state: String,
    manufacturer: Option<String>,
    model: Option<String>,
    revision: Option<String>,
    #[serde(rename = "access-technologies")]
    access_technologies: Option<Vec<String>>,
    #[serde(rename = "signal-quality")]
    signal_quality: Option<RawSignalQuality>,
    #[serde(rename = "primary-port")]
    primary_port: Option<String>,
    /// D-Bus path of the modem's primary SIM, e.g.
    /// `/org/freedesktop/ModemManager1/SIM/0`. mmcli emits the
    /// literal string `"--"` when no SIM is inserted.
    sim: Option<String>,
}

#[derive(Deserialize)]
struct RawSignalQuality {
    value: String,
}

#[derive(Deserialize)]
struct RawModem3gpp {
    imei: Option<String>,
    #[serde(rename = "operator-name")]
    operator_name: Option<String>,
    #[serde(rename = "operator-code")]
    operator_code: Option<String>,
}

#[derive(Deserialize)]
struct RawSimRoot {
    sim: RawSim,
}

#[derive(Deserialize)]
struct RawSim {
    properties: RawSimProperties,
}

#[derive(Deserialize)]
struct RawSimProperties {
    iccid: Option<String>,
    imsi: Option<String>,
    #[serde(rename = "operator-name")]
    operator_name: Option<String>,
    #[serde(rename = "operator-code")]
    operator_code: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIST_JSON: &str = r#"{
        "modem-list": [
            "/org/freedesktop/ModemManager1/Modem/0",
            "/org/freedesktop/ModemManager1/Modem/3"
        ]
    }"#;

    const LIST_EMPTY_JSON: &str = r#"{ "modem-list": [] }"#;

    // Real-world-shaped sample, lightly trimmed for readability.
    const MODEM_JSON: &str = r#"{
        "modem": {
            "3gpp": {
                "imei": "123456789012345",
                "operator-name": "Test Mobile",
                "operator-code": "26201",
                "registration-state": "home"
            },
            "generic": {
                "access-technologies": ["lte"],
                "manufacturer": "Quectel",
                "model": "EG25-G",
                "revision": "EG25GGBR07A08M2G",
                "state": "connected",
                "signal-quality": {"recent": "yes", "value": "78"},
                "primary-port": "ttyUSB2"
            }
        }
    }"#;

    const MODEM_DISCONNECTED_JSON: &str = r#"{
        "modem": {
            "generic": {
                "access-technologies": ["--"],
                "manufacturer": "Quectel",
                "model": "EG25-G",
                "revision": "EG25GGBR07A08M2G",
                "state": "registered",
                "signal-quality": {"recent": "no", "value": "0"},
                "primary-port": "ttyUSB2"
            }
        }
    }"#;

    const MODEM_BARE_JSON: &str = r#"{
        "modem": {
            "generic": {
                "manufacturer": "--",
                "model": "--",
                "revision": "--",
                "state": "disabled",
                "primary-port": "--"
            }
        }
    }"#;

    #[test]
    fn parses_modem_list() {
        let raw: RawModemList = serde_json::from_str(LIST_JSON).unwrap();
        let indices: Vec<u32> = raw
            .modem_list
            .iter()
            .map(|p| parse_index_from_path(p).unwrap())
            .collect();
        assert_eq!(indices, vec![0, 3]);
    }

    #[test]
    fn parses_empty_modem_list() {
        let raw: RawModemList = serde_json::from_str(LIST_EMPTY_JSON).unwrap();
        assert!(raw.modem_list.is_empty());
    }

    #[test]
    fn extracts_index_from_dbus_path() {
        assert_eq!(
            parse_index_from_path("/org/freedesktop/ModemManager1/Modem/0").unwrap(),
            0
        );
        assert_eq!(
            parse_index_from_path("/org/freedesktop/ModemManager1/Modem/42").unwrap(),
            42
        );
        assert!(parse_index_from_path("/foo/bar/baz").is_err());
    }

    #[test]
    fn parses_connected_modem() {
        let m = parse_modem(0, MODEM_JSON).unwrap();
        assert_eq!(m.index, 0);
        assert_eq!(m.state, "connected");
        assert!(m.is_connected());
        assert!(m.is_registered());
        assert_eq!(m.manufacturer.as_deref(), Some("Quectel"));
        assert_eq!(m.model.as_deref(), Some("EG25-G"));
        assert_eq!(m.imei.as_deref(), Some("123456789012345"));
        assert_eq!(m.access_technologies, vec!["lte"]);
        assert_eq!(m.signal_quality, Some(78));
        assert_eq!(m.operator_name.as_deref(), Some("Test Mobile"));
        assert_eq!(m.operator_code.as_deref(), Some("26201"));
        assert_eq!(m.primary_port.as_deref(), Some("ttyUSB2"));
    }

    #[test]
    fn parses_registered_but_not_connected_modem() {
        let m = parse_modem(0, MODEM_DISCONNECTED_JSON).unwrap();
        assert_eq!(m.state, "registered");
        assert!(!m.is_connected());
        assert!(m.is_registered());
        // `["--"]` access-technologies → filtered out → empty Vec.
        assert!(m.access_technologies.is_empty());
        // No 3gpp section → operator/IMEI are None.
        assert!(m.imei.is_none());
        assert!(m.operator_name.is_none());
    }

    #[test]
    fn double_dash_placeholders_become_none() {
        let m = parse_modem(7, MODEM_BARE_JSON).unwrap();
        assert_eq!(m.index, 7);
        assert_eq!(m.state, "disabled");
        assert!(!m.is_connected());
        assert!(!m.is_registered());
        assert!(m.manufacturer.is_none());
        assert!(m.model.is_none());
        assert!(m.revision.is_none());
        assert!(m.primary_port.is_none());
        assert!(m.signal_quality.is_none());
    }

    #[test]
    fn rejects_malformed_json() {
        let err = parse_modem(0, "not even json").unwrap_err();
        assert!(matches!(err, Error::Parse(_)));
    }

    const SIM_JSON: &str = r#"{
        "sim": {
            "dbus-path": "/org/freedesktop/ModemManager1/SIM/0",
            "properties": {
                "iccid": "8949011234567890123",
                "imsi": "262011234567890",
                "operator-name": "Test Mobile",
                "operator-code": "26201"
            }
        }
    }"#;

    const SIM_NO_IMSI_JSON: &str = r#"{
        "sim": {
            "properties": {
                "iccid": "8949011234567890123",
                "imsi": "--",
                "operator-name": "--",
                "operator-code": "--"
            }
        }
    }"#;

    const MODEM_WITH_SIM_PATH_JSON: &str = r#"{
        "modem": {
            "generic": {
                "state": "registered",
                "sim": "/org/freedesktop/ModemManager1/SIM/2"
            }
        }
    }"#;

    const MODEM_WITHOUT_SIM_JSON: &str = r#"{
        "modem": {
            "generic": {
                "state": "disabled",
                "sim": "--"
            }
        }
    }"#;

    #[test]
    fn parses_sim_properties() {
        let s = parse_sim(0, SIM_JSON).unwrap();
        assert_eq!(s.index, 0);
        assert_eq!(s.iccid.as_deref(), Some("8949011234567890123"));
        assert_eq!(s.imsi.as_deref(), Some("262011234567890"));
        assert_eq!(s.operator_name.as_deref(), Some("Test Mobile"));
        assert_eq!(s.operator_code.as_deref(), Some("26201"));
    }

    #[test]
    fn sim_double_dash_fields_become_none() {
        let s = parse_sim(0, SIM_NO_IMSI_JSON).unwrap();
        assert_eq!(s.iccid.as_deref(), Some("8949011234567890123"));
        assert!(s.imsi.is_none());
        assert!(s.operator_name.is_none());
        assert!(s.operator_code.is_none());
    }

    #[test]
    fn extracts_sim_path_from_modem_json() {
        let path = extract_sim_path(MODEM_WITH_SIM_PATH_JSON).unwrap();
        assert_eq!(path, "/org/freedesktop/ModemManager1/SIM/2");
    }

    #[test]
    fn errors_when_modem_has_no_sim() {
        let err = extract_sim_path(MODEM_WITHOUT_SIM_JSON).unwrap_err();
        assert!(matches!(err, Error::Parse(_)));
    }

    fn host_has_mmcli() -> bool {
        std::process::Command::new("sh")
            .args(["-c", "mmcli -L -J >/dev/null 2>&1"])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    #[tokio::test]
    async fn list_modems_via_subprocess_shell() {
        if !host_has_mmcli() {
            eprintln!("skipping: host has no working mmcli");
            return;
        }
        let mut shell = embedded_shell::shell::SubprocessShell::new();
        let indices = list_modems(&mut shell).await.unwrap();
        eprintln!("[test] {} modems on host", indices.len());
        // Host may legitimately have zero modems. The value of the
        // test is parsing succeeded.
    }
}
