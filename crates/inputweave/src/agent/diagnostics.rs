//! A plain-text report of what the agent knows, to attach to bug reports.
//! The agent only builds it; clients write it where the user chose, so on
//! Windows no file is written as SYSTEM. It holds nothing secret: no
//! private key, no pairing code, no file or clipboard contents, not even
//! the names of transferred files.

use std::fmt::Write as _;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use super::directory::Peer;
use super::files::TransferStatus;
use super::peers::HEARTBEAT;
use super::status::{Arrangement, Status};
use super::throttle::{PacingStatus, THRESHOLD};
use crate::offline_file::describe_setter;

/// The whole report stays under this, well inside an IPC frame.
pub(crate) const MAX_REPORT: usize = 1024 * 1024;
/// How much of the end of each log goes in.
const LOG_TAIL: u64 = 256 * 1024;

/// What is known when the agent starts and does not change.
#[derive(Debug)]
pub(crate) struct Fixed {
    pub(crate) fingerprint: String,
    pub(crate) listen: String,
    /// The effective configuration, after defaults were filled in.
    pub(crate) config: String,
    pub(crate) devices_file: Option<PathBuf>,
    pub(crate) log_files: Vec<PathBuf>,
    /// The agent's systemd user unit, whose journal goes in (Linux).
    pub(crate) journal_unit: Option<String>,
    /// The SHA-256 of the agent's executable; empty when unknown.
    pub(crate) agent_digest: String,
}

/// The agent's state at the moment of the report.
pub(crate) struct Snapshot {
    pub(crate) status: Status,
    pub(crate) arrangement: Arrangement,
    pub(crate) peers: Vec<Peer>,
    pub(crate) transfers: Vec<TransferStatus>,
    /// How fast files may go to each device.
    pub(crate) pacing: Vec<PacingStatus>,
    pub(crate) receive_folder: Option<PathBuf>,
    /// The group's document of the devices taken offline for now.
    pub(crate) offline: crate::offline_file::OfflineDoc,
}

/// Builds the report. Reads the logs, so call it off the async runtime.
pub(crate) fn report(fixed: &Fixed, snapshot: &Snapshot, now: SystemTime) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "InputWeave diagnostics");
    let _ = writeln!(out, "Generated: {}", utc(now));
    let _ = writeln!(
        out,
        "Agent: inputweave {} (IPC {}, peer protocol {}-{})",
        env!("CARGO_PKG_VERSION"),
        iw_proto::IPC_VERSION,
        iw_proto::PEER_PROTOCOL_MIN,
        iw_proto::PEER_PROTOCOL_MAX,
    );
    let _ = writeln!(
        out,
        "Agent SHA-256: {}",
        if fixed.agent_digest.is_empty() {
            "unknown"
        } else {
            &fixed.agent_digest
        }
    );
    let _ = writeln!(
        out,
        "System: {} {}",
        std::env::consts::OS,
        std::env::consts::ARCH
    );
    let _ = writeln!(out, "Fingerprint: {}", fixed.fingerprint);
    let _ = writeln!(out, "Listening on: {}", fixed.listen);
    if let Some(path) = &fixed.devices_file {
        let _ = writeln!(out, "Devices file: {}", path.display());
    }
    let _ = writeln!(
        out,
        "Receive folder: {}",
        snapshot
            .receive_folder
            .as_ref()
            .map_or("none".into(), |p| p.display().to_string())
    );

    let _ = writeln!(
        out,
        "Screen: {}",
        if snapshot.status.screen_locked {
            "locked"
        } else {
            "unlocked"
        }
    );
    let _ = writeln!(
        out,
        "Lock screen input: {}",
        if snapshot.status.lock_screen_input {
            "yes"
        } else {
            "no"
        }
    );
    let _ = writeln!(
        out,
        "Input from other devices: {}",
        if snapshot.status.input_available {
            "reaches this device"
        } else {
            "cannot reach this device now"
        }
    );
    if !snapshot.status.permissions.is_empty() {
        let states: Vec<String> = snapshot
            .status
            .permissions
            .iter()
            .map(iw_platform::PermissionState::describe)
            .collect();
        let _ = writeln!(out, "Permissions: {}", states.join(", "));
    }
    if let Some(session) = &snapshot.status.session {
        let session = crate::convert::session_to_proto(session);
        let _ = writeln!(out, "Session: {}", crate::ctl::format_session(&session));
        for backend in &session.backends {
            let _ = writeln!(out, "  {}", crate::ctl::format_backend(backend));
        }
    }
    if snapshot.status.edges_only {
        let _ = writeln!(
            out,
            "Leaving this device: only across the edges of its screens (no jumps)"
        );
    }
    let (firewall, port) = snapshot.status.firewall;
    if let Some(firewall) = crate::convert::firewall_to_proto(firewall, port) {
        let _ = writeln!(out, "Firewall: {}", crate::ctl::format_firewall(&firewall));
    }
    let folders = &snapshot.status.folder_access;
    if !folders.folders.is_empty() {
        let states: Vec<String> = folders
            .folders
            .iter()
            .map(|(folder, access)| format!("{} {}", folder.name(), access.name()))
            .collect();
        let _ = writeln!(
            out,
            "Protected folders: {}; asked {}",
            states.join(", "),
            if folders.asked { "yes" } else { "no" }
        );
    }
    let relative = &snapshot.status;
    let _ = writeln!(
        out,
        "Relative mouse: {}; automatic {}; an app here holds the pointer: {}",
        match &relative.relative {
            Some(mode) if mode.automatic => format!("on for {} (automatic)", mode.device),
            Some(mode) => format!("on for {}", mode.device),
            None => "off".to_owned(),
        },
        if relative.relative_auto { "on" } else { "off" },
        if relative.pointer_captured {
            "yes"
        } else {
            "no"
        }
    );

    // Every device the group lists offline, those not trusted here too:
    // they can be brought back from here by id.
    let listed: Vec<String> = snapshot
        .offline
        .offline()
        .map(|(id, e)| {
            let by = describe_setter(
                &snapshot.status.this_device,
                e.by.as_deref().unwrap_or_default(),
                &e.by_name,
            );
            format!("{id} ({by})")
        })
        .collect();
    let _ = writeln!(
        out,
        "Offline for now: {}",
        if listed.is_empty() {
            "none".to_owned()
        } else {
            listed.join(", ")
        }
    );

    // The profile and the network by their ids and kinds only: never the
    // names, which say where the user is.
    let status = &snapshot.status;
    let in_use = status.profile_in_use();
    let _ = writeln!(
        out,
        "Profile: {}{}{}; {} profiles, {} networks listed; Wi-Fi names {}",
        status.profile,
        if status.profile_chosen {
            " (chosen)"
        } else {
            " (automatic)"
        },
        match in_use {
            Some(p) if p.paused => ", paused".to_owned(),
            Some(p) => format!(
                ", clipboard {}, files {}, not connecting {}",
                if p.clipboard { "on" } else { "off" },
                if p.files { "on" } else { "off" },
                if p.off.is_empty() {
                    "none".to_owned()
                } else {
                    p.off.iter().cloned().collect::<Vec<_>>().join(", ")
                }
            ),
            None => String::new(),
        },
        status.profiles.len(),
        status
            .profiles
            .iter()
            .map(|p| p.networks.len())
            .sum::<usize>(),
        if status.wifi_names { "on" } else { "off" }
    );
    let _ = writeln!(
        out,
        "Network: {}",
        match &status.network {
            Some(n) => format!(
                "{} ({:?}{}){}",
                n.id,
                n.kind,
                if n.wifi_name.is_some() {
                    ", Wi-Fi name known"
                } else {
                    ""
                },
                match status
                    .profiles
                    .iter()
                    .find(|p| p.networks.iter().any(|l| l.id == n.id))
                {
                    Some(p) => format!(", listed in {}", p.id),
                    None => ", not listed".to_owned(),
                }
            ),
            None => "none".to_owned(),
        }
    );

    section(&mut out, "Connections");
    if snapshot.status.devices.is_empty() {
        let _ = writeln!(out, "none");
    }
    for d in &snapshot.status.devices {
        let state = if d.connected {
            let locked = match (d.screen_locked, d.lock_screen_input) {
                (true, true) => ", screen locked",
                (true, false) => ", screen locked, no input",
                (false, _) => "",
            };
            let held = if d.pointer_captured {
                ", an app holds the pointer"
            } else {
                ""
            };
            let unavailable = if d.input_unavailable {
                ", takes no input now"
            } else {
                ""
            };
            let paused = if d.sharing_paused {
                ", sharing paused"
            } else {
                ""
            };
            format!(
                "connected, round trip {} ms{locked}{held}{unavailable}{paused}",
                d.rtt_ms
            )
        } else if let Some((by, name)) = &d.offline_by {
            let by = describe_setter(&snapshot.status.this_device, by, name);
            format!("offline for now, {by}")
        } else if d.not_in_profile {
            "not in this profile".to_owned()
        } else {
            "offline".to_owned()
        };
        let unaware = if d.offline_unaware {
            "; knows nothing of devices taken offline for now"
        } else {
            ""
        };
        let _ = writeln!(out, "{} ({}): {state}{unaware}", d.id, d.name);
    }
    section(&mut out, "Clipboard offers");
    let lazy = &snapshot.status.lazy_clipboard;
    let _ = writeln!(
        out,
        "offered {}, taken {}, pulls answered {}, pulled {} ({} bytes), failed {}, superseded {}, last: {}",
        lazy.offered,
        lazy.received,
        lazy.served,
        lazy.pulled,
        lazy.bytes_pulled,
        lazy.failed,
        lazy.superseded,
        lazy.last.as_deref().unwrap_or("none")
    );
    section(&mut out, "Transfer pacing");
    pacing(&mut out, &snapshot.pacing);
    section(&mut out, "Status");
    let _ = writeln!(out, "{:#?}", snapshot.status);
    section(&mut out, "Trusted devices");
    let _ = writeln!(out, "{:#?}", snapshot.peers);
    section(&mut out, "Arrangement");
    let _ = writeln!(out, "{:#?}", snapshot.arrangement);
    section(&mut out, "Transfers");
    if snapshot.transfers.is_empty() {
        let _ = writeln!(out, "none");
    }
    for t in &snapshot.transfers {
        // Ids and counts only: no names or paths.
        let batch = t.batch.as_ref().map_or(String::new(), |b| {
            let drag = if b.drag.is_some() { ", dragged" } else { "" };
            format!(
                ", batch {} item {}/{} ({} files, {} bytes{drag})",
                b.id,
                b.index + 1,
                b.items,
                b.files,
                b.bytes
            )
        });
        let folder = t.folder.as_ref().map_or(String::new(), |f| {
            format!(
                ", folder: {} of {} files done, {} failed",
                f.done, f.entries, f.failed
            )
        });
        let _ = writeln!(
            out,
            "{} {:?} {} {}: {}/{} bytes, {:?}{batch}{folder}",
            t.id,
            t.direction,
            t.device,
            utc(t.started),
            t.done,
            t.size,
            t.state
        );
    }
    section(&mut out, "Configuration");
    let _ = writeln!(out, "{}", fixed.config);

    for log in &fixed.log_files {
        section(&mut out, &format!("Log {}", log.display()));
        match tail(log, LOG_TAIL) {
            Ok(text) => out.push_str(&text),
            Err(e) => {
                let _ = writeln!(out, "(cannot read: {e})");
            }
        }
    }
    if let Some(unit) = &fixed.journal_unit {
        section(&mut out, &format!("Journal {unit}"));
        if std::env::var_os("INVOCATION_ID").is_none() {
            let _ = writeln!(
                out,
                "(this agent does not run under systemd: its own log went to where it was started)"
            );
        }
        out.push_str(&journal(unit));
        // GLib, GTK and the portals may name files in the app's: not here.
        let _ = writeln!(
            out,
            "\nThe app's journal is left out, as it may name files: \
             `journalctl --user -u {}` shows it.",
            crate::paths::APP_UNIT
        );
    }
    truncate(&mut out, MAX_REPORT);
    out
}

/// How many lines of each unit's journal go in.
const JOURNAL_LINES: u32 = 500;
/// How long `journalctl` may take.
const JOURNAL_WAIT: Duration = Duration::from_secs(5);

/// The end of `unit`'s journal, from the user's systemd, as `journalctl
/// --user` prints it; or why it cannot be had.
fn journal(unit: &str) -> String {
    let lines = JOURNAL_LINES.to_string();
    let args = [
        "--user",
        "--unit",
        unit,
        "--lines",
        &lines,
        "--no-pager",
        "--output",
        "short-iso",
    ];
    match iw_platform::system::run_for("journalctl", &args, JOURNAL_WAIT) {
        Ok(ran) if ran.text.trim().is_empty() => "(nothing in the journal)\n".to_owned(),
        Ok(ran) if !ran.whole => format!(
            "{}\n(cut short: journalctl did not finish in time)\n",
            ran.text
        ),
        Ok(ran) => ran.text,
        Err(why) => format!("(cannot read the journal: {why})\n"),
    }
}

/// How fast files may go to each device, and the round trips that decide
/// it: what to watch while a large file is sent (docs/architecture.md
/// §9).
fn pacing(out: &mut String, pacing: &[PacingStatus]) {
    let _ = writeln!(
        out,
        "Files slow down while the round trip is more than {} over its idle baseline; \
         sampled every {}.",
        millis(THRESHOLD),
        millis(HEARTBEAT)
    );
    if pacing.is_empty() {
        let _ = writeln!(out, "no device connected yet");
    }
    for p in pacing {
        let rate = match p.rate {
            Some(rate) => format!("at most {:.1} MB/s", rate as f64 / 1e6),
            None => "unlimited".to_owned(),
        };
        let _ = writeln!(
            out,
            "{}: baseline {}, latest {}, files {rate}, {} sending, slowed down {} times",
            p.device,
            p.baseline.map_or("unknown".to_owned(), millis),
            p.rtt.map_or("unknown".to_owned(), millis),
            p.sending,
            p.slowdowns
        );
        let _ = writeln!(out, "  idle: {}", spread(&p.idle));
        let _ = writeln!(out, "  sending: {}", spread(&p.busy));
        let recent: Vec<String> = p
            .recent
            .iter()
            .map(|(rtt, busy)| {
                let ms = rtt.as_secs_f64() * 1000.0;
                if *busy {
                    format!("{ms:.1}*")
                } else {
                    format!("{ms:.1}")
                }
            })
            .collect();
        let _ = writeln!(
            out,
            "  latest (ms, oldest first, * while sending): {}",
            recent.join(" ")
        );
    }
}

/// "240 samples, p50 1.2 ms, p99 2.0 ms, max 2.4 ms".
fn spread(samples: &[Duration]) -> String {
    if samples.is_empty() {
        return "no samples".to_owned();
    }
    let mut sorted = samples.to_vec();
    sorted.sort();
    format!(
        "{} samples, p50 {}, p99 {}, max {}",
        sorted.len(),
        millis(percentile(&sorted, 50)),
        millis(percentile(&sorted, 99)),
        millis(sorted[sorted.len() - 1])
    )
}

/// The nearest-rank percentile `p` of `sorted`, which is not empty.
fn percentile(sorted: &[Duration], p: usize) -> Duration {
    let rank = (sorted.len() * p).div_ceil(100).max(1);
    sorted[rank.min(sorted.len()) - 1]
}

/// "1.2 ms".
fn millis(d: Duration) -> String {
    format!("{:.1} ms", d.as_secs_f64() * 1000.0)
}

fn section(out: &mut String, title: &str) {
    let _ = write!(out, "\n== {title} ==\n");
}

/// The last `max` bytes of a file, from the first whole line in them.
fn tail(path: &Path, max: u64) -> std::io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(max);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(max).read_to_end(&mut bytes)?;
    let mut text = String::from_utf8_lossy(&bytes).into_owned();
    if start > 0
        && let Some(newline) = text.find('\n')
    {
        text.drain(..=newline);
    }
    Ok(text)
}

fn truncate(text: &mut String, max: usize) {
    if text.len() <= max {
        return;
    }
    let marker = "\n(cut off)\n";
    let mut end = max - marker.len();
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str(marker);
}

/// "2026-10-21 15:04:05 UTC".
pub(crate) fn utc(time: SystemTime) -> String {
    let secs = time.duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs());
    let (days, rest) = (secs / 86_400, secs % 86_400);
    let (year, month, day) = civil(days as i64);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02} UTC",
        rest / 3600,
        rest % 3600 / 60,
        rest % 60
    )
}

/// The calendar date `days` after 1970-01-01, after Howard Hinnant's
/// `civil_from_days`.
fn civil(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = yoe + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn dates_are_utc_calendar_dates() {
        assert_eq!(utc(UNIX_EPOCH), "1970-01-01 00:00:00 UTC");
        let t = UNIX_EPOCH + Duration::from_secs(1_792_595_045);
        assert_eq!(utc(t), "2026-10-21 15:04:05 UTC");
        let leap = UNIX_EPOCH + Duration::from_secs(951_782_400);
        assert_eq!(utc(leap), "2000-02-29 00:00:00 UTC");
    }

    #[test]
    fn tails_start_at_a_whole_line() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.log");
        std::fs::write(&path, "first line\nsecond line\nthird\n").unwrap();
        assert_eq!(
            tail(&path, 1000).unwrap(),
            "first line\nsecond line\nthird\n"
        );
        assert_eq!(tail(&path, 15).unwrap(), "third\n");
        assert!(tail(&dir.path().join("missing.log"), 10).is_err());
    }

    #[test]
    fn the_report_says_who_took_devices_offline_as_ctl_does() {
        use super::super::directory::Peer;
        use super::super::folder_access::FolderAccess;
        use super::super::status::DeviceState;
        use crate::config::Config;
        use crate::offline_file::{Entry, OfflineDoc};
        use iw_core::hotkey::Hotkeys;

        let mut status = Status::initial(
            &Config::default(),
            &[],
            true,
            false,
            Hotkeys::default(),
            false,
            FolderAccess::default(),
        );
        let peer = Peer {
            id: "x".to_owned(),
            name: "x".to_owned(),
            reported_name: "x".to_owned(),
            fingerprint: iw_net::Fingerprint::from_bytes([1; 32]),
            os: iw_proto::peer::v1::Os::Linux,
            origin: super::super::directory::Origin::Paired,
            address: None,
            last_address: None,
            swap_gui_and_control: false,
            side: crate::store::Side::Right,
            clipboard_off: false,
            files_off: false,
            cursor_speed: iw_core::speed::CursorSpeed::NORMAL,
            wake: None,
        };
        let shown = |id: &str, by: Option<(&str, &str)>| DeviceState {
            id: id.to_owned(),
            name: id.to_owned(),
            offline_for_now: by.is_some(),
            offline_by: by.map(|(id, name)| (id.to_owned(), name.to_owned())),
            ..DeviceState::of(&peer)
        };
        status.devices = vec![
            shown("a", Some(("", ""))),
            shown("b", Some(("", "Studio PC"))),
        ];
        let entry = |by: Option<&str>, name: &str| Entry {
            version: 1,
            author: String::new(),
            offline: true,
            by: by.map(str::to_owned),
            by_name: name.to_owned(),
        };
        let offline = OfflineDoc {
            devices: [
                ("a".to_owned(), entry(None, "")),
                ("b".to_owned(), entry(None, "Studio PC")),
                ("ghost".to_owned(), entry(Some("zed"), "")),
            ]
            .into(),
        };
        let fixed = Fixed {
            fingerprint: String::new(),
            listen: String::new(),
            config: String::new(),
            devices_file: None,
            log_files: Vec::new(),
            journal_unit: None,
            agent_digest: String::new(),
        };
        let snapshot = Snapshot {
            status,
            arrangement: Arrangement::default(),
            peers: Vec::new(),
            transfers: Vec::new(),
            pacing: Vec::new(),
            receive_folder: None,
            offline,
        };
        let out = report(&fixed, &snapshot, UNIX_EPOCH);
        for expected in [
            "Offline for now: a (set by another computer), b (set by Studio PC), ghost (set by zed)\n",
            "a (a): offline for now, set by another computer\n",
            "b (b): offline for now, set by Studio PC\n",
        ] {
            assert!(out.contains(expected), "no {expected:?} in\n{out}");
        }
        assert!(!out.contains("set by \n") && !out.contains("()"), "{out}");
    }

    #[test]
    fn the_report_never_holds_the_license_key_or_whose_it_is() {
        use super::super::folder_access::FolderAccess;
        use super::super::license::{LicenseState, LicenseStatus, LicenseUse, LicensedDevice};
        use crate::config::Config;
        use iw_core::hotkey::Hotkeys;

        const KEY: &str = "INPUTWEAVE-SECRET-KEY-0123-7E42";
        const ACTIVATION: &str = "act-5f0e9c1d-private";
        const EMAIL: &str = "buyer@example.com";
        let status = Status {
            license: Some(LicenseStatus {
                key: KEY.to_owned(),
                state: LicenseState::Licensed,
                activation_id: ACTIVATION.to_owned(),
                customer_email: EMAIL.to_owned(),
                validated_at: 1_793_800_000,
                source_device_id: "win".to_owned(),
                source_device_name: "Office PC".to_owned(),
                devices: vec![LicensedDevice {
                    id: "win".to_owned(),
                    name: "Office PC".to_owned(),
                    licensed_use: LicenseUse::Licensed,
                    activation_id: "act-of-the-office-pc".to_owned(),
                    legacy: false,
                    can_take_back: false,
                    key_out_of_reach: false,
                    legacy_refused: false,
                    waiting_to_apply: false,
                }],
                to_free: Vec::new(),
            }),
            ..Status::initial(
                &Config::default(),
                &[],
                true,
                false,
                Hotkeys::default(),
                false,
                FolderAccess::default(),
            )
        };
        let fixed = Fixed {
            fingerprint: String::new(),
            listen: String::new(),
            config: String::new(),
            devices_file: None,
            log_files: Vec::new(),
            journal_unit: None,
            agent_digest: String::new(),
        };
        let snapshot = Snapshot {
            status,
            arrangement: Arrangement::default(),
            peers: Vec::new(),
            transfers: Vec::new(),
            pacing: Vec::new(),
            receive_folder: None,
            offline: Default::default(),
        };
        let out = report(&fixed, &snapshot, UNIX_EPOCH);
        for secret in [KEY, ACTIVATION, EMAIL, "SECRET-KEY", "act-of-the-office-pc"] {
            assert!(!out.contains(secret), "{secret:?} in\n{out}");
        }
        // What helps find a problem is still there.
        for expected in ["INPUTWEAVE-…7E42", "Licensed", "Office PC", "use: Licensed"] {
            assert!(out.contains(expected), "no {expected:?} in\n{out}");
        }
    }

    #[test]
    fn long_reports_are_cut_on_a_character_boundary() {
        let mut text = "é".repeat(100);
        truncate(&mut text, 51);
        assert!(text.len() <= 51);
        assert!(text.ends_with("(cut off)\n"));
    }

    #[test]
    fn pacing_shows_the_rate_and_how_round_trips_spread() {
        let ms = |n: u64| Duration::from_millis(n);
        let mut out = String::new();
        pacing(
            &mut out,
            &[PacingStatus {
                device: "win".to_owned(),
                baseline: Some(Duration::from_micros(1_200)),
                rtt: Some(ms(9)),
                rate: Some(12_500_000),
                sending: 2,
                slowdowns: 3,
                idle: vec![ms(1), ms(2)],
                busy: (1..=100).map(ms).collect(),
                recent: vec![(ms(2), false), (ms(9), true)],
            }],
        );
        for expected in [
            "more than 15.0 ms over its idle baseline; sampled every 250.0 ms",
            "win: baseline 1.2 ms, latest 9.0 ms, files at most 12.5 MB/s, 2 sending, \
             slowed down 3 times",
            "idle: 2 samples, p50 1.0 ms, p99 2.0 ms, max 2.0 ms",
            "sending: 100 samples, p50 50.0 ms, p99 99.0 ms, max 100.0 ms",
            "* while sending): 2.0 9.0*",
        ] {
            assert!(out.contains(expected), "no {expected:?} in\n{out}");
        }
    }
}
