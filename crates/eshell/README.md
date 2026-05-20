# `eshell` — command-line driver for `embedded-shell-rs`

A small CLI that talks to embedded Linux devices over a serial line.
Use it like `ssh`/`scp` but for serial-attached devices: run a command,
push/pull a file, get a one-page health summary, ping a target,
reboot.

## Install

```sh
cargo install --path crates/eshell
```

## Subcommands

| Command | What it does |
|---|---|
| `eshell exec PORT -- argv…` | Run one command on the device, mirror stdout/stderr/exit code. `--json` for structured output. |
| `eshell push PORT --src ... --dst ... [--mode 0644] [--via http\|serial]` | Push a local file to the device. Default is HTTP-first with serial fallback. |
| `eshell pull PORT --src ... --dst ... [--via http\|serial]` | Fetch a remote file. Same fallback policy as `push`. |
| `eshell info PORT [--json]` | Pretty-prints OS, kernel, uptime, memory, root-fs usage, IPv4. |
| `eshell ping PORT TARGET [--count N] [--json]` | Ping `TARGET` from the device. Exits 1 on total loss so scripts can branch. |
| `eshell reboot PORT` | Reboot the device, wait for it to come back. Reports how long that took. |
| `eshell service PORT UNIT <action>` | Systemd unit control. `<action>` is one of `status` / `start` / `stop` / `restart` / `reload` / `enable` / `disable`. `status` returns structured info (and exits 3 if not active, mirroring `systemctl is-active`); `--json` for the status flavor. |
| `eshell services PORT [--pattern P] [--failed-only] [--json]` | Tabular listing of systemd units. Default shows every active unit; `--pattern '*.service'` to filter, `--failed-only` to show just the broken ones. |
| `eshell journal PORT [--unit U] [-n N] [--since EXPR] [--json]` | Tail the systemd journal. Filters compose: `--unit foo --since "1 hour ago"` gives that unit's last hour. Default is the last 50 entries from everything. `--json` emits JSONL. |
| `eshell modem PORT [INDEX] [--no-sim] [--json]` | Modem + primary-SIM details from ModemManager. `INDEX` defaults to `0`. `--no-sim` skips the SIM lookup (faster, doesn't error if no SIM is inserted). |
| `eshell network PORT [--json]` | Comprehensive network state: kernel-view (`ip -j` links/addresses/routes) and NM-view (`nmcli` connections) side by side. Gracefully degrades to NM-only on devices whose `ip` lacks JSON support. |

`PORT` is always the device's serial port (e.g. `/dev/ttyUSB0`).

## Transport fallback for push/pull

By default `push` and `pull` try HTTP first because it's ~1000× faster.
If HTTP fails for an HTTP-specific reason — no usable host IP, no
`wget` or `curl` on the device, hyper-layer error — the CLI falls back
to the serial transport automatically and logs a `WARN` event saying
so.

Failures rooted in the device or the file (permission denied, source
missing, checksum mismatch) **do not** trigger the fallback — the same
failure would happen on either transport, and masking it would just
take longer.

Force a specific transport with `--via http` or `--via serial`.

**Caveat for serial:** push payloads are capped at 64 KiB (the limit
of a single shell command line). Large payloads with HTTP failing will
fail loudly.

## Passwords

The CLI accepts a login password via:

```sh
eshell --password 'secret' info /dev/ttyUSB0
# or
ESHELL_PASSWORD=secret eshell info /dev/ttyUSB0
```

The env-var path is preferred in scripts so the password doesn't end
up in shell history. `--password` is a global flag — it works on every
subcommand.

## Tracing

`eshell` installs a `tracing-subscriber` filtering on `RUST_LOG`
(default `warn`). To see the library's lifecycle events:

```sh
RUST_LOG=embedded_shell=info,embedded_shell_transfer=info eshell push /dev/ttyUSB0 \
    --src ./config.json --dst /etc/app.cfg --mode 0644
```

`embedded_shell=debug` adds operation-level events; `=trace` adds
byte-level RX/TX of the serial line.

## Examples

```sh
# Run something on the device
eshell exec /dev/ttyUSB0 -- uname -a
eshell exec /dev/ttyUSB0 -- sh -c 'systemctl is-active sshd'

# Push a config and chmod it
eshell push /dev/ttyUSB0 --src ./app.cfg --dst /etc/app.cfg --mode 0644

# Pull a log
eshell pull /dev/ttyUSB0 --src /var/log/messages --dst .

# Health snapshot, as JSON for piping to jq
eshell info /dev/ttyUSB0 --json | jq .

# Smoke-test connectivity, scripted
if eshell ping /dev/ttyUSB0 8.8.8.8 --count 2 >/dev/null; then
    echo "online"
fi

# Reboot and verify it comes back
eshell reboot /dev/ttyUSB0

# Check a service, then restart it
eshell service /dev/ttyUSB0 sshd.service status
eshell service /dev/ttyUSB0 sshd.service restart

# Last hour of logs from one unit, JSONL for pipeline use
eshell journal /dev/ttyUSB0 --unit sshd.service --since "1 hour ago" --json | jq .

# Modem inventory — pipeline-friendly JSON
eshell modem /dev/ttyUSB0 --json | jq '{model, signal_quality, operator: .operator_name}'

# Failed services on this device
eshell services /dev/ttyUSB0 --failed-only

# Network state in one call
eshell network /dev/ttyUSB0
```

## Exit codes

| Code | Meaning |
|---|---|
| `0` | Success |
| `1` | Generic CLI failure (couldn't open shell, bad arguments, etc.) |
| `1` from `ping` | Total packet loss — the device couldn't reach the target |
| `<device's exit code>` from `exec` | The CLI mirrors the device's exit code (clamped to 0–255) |
