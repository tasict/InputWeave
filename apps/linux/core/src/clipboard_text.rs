//! The line about the last clipboard that moved (the Windows app's
//! `ClipboardText`).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::snapshot::Snapshot;
use crate::text::{UiText, keys};
use crate::ui;

/// For how long after it moved the line shows.
pub const SHOWN_FOR: Duration = Duration::from_secs(600);

pub fn line(model: &Snapshot, now: SystemTime) -> Option<UiText> {
    let status = &model.status;
    if !model.connected() || !status.clipboard_sync {
        return None;
    }
    let activity = status.last_clipboard.as_ref()?;
    let at = UNIX_EPOCH + Duration::from_millis(activity.at_unix_ms.min(u64::MAX / 2));
    let elapsed = now.duration_since(at).unwrap_or_default();
    if elapsed >= SHOWN_FOR {
        return None;
    }
    match activity.skipped.as_str() {
        "sensitive" => return Some(ui!(keys::CLIPBOARD_SENSITIVE)),
        "too_large" => return Some(ui!(keys::CLIPBOARD_TOO_LARGE)),
        "" => {}
        _ => return None,
    }
    let when = ago(elapsed, AgoStyle::Short);
    let kind = kind(&activity.kinds);
    Some(if activity.to_device_id == model.this_device_id() {
        ui!(
            keys::CLIPBOARD_SYNCED_FROM,
            model.name_of(&activity.from_device_id),
            when,
            kind
        )
    } else {
        ui!(
            keys::CLIPBOARD_SENT_TO,
            model.name_of(&activity.to_device_id),
            when,
            kind
        )
    })
}

/// How [`ago`] says it: as the clipboard line, "2 min ago"; or in full,
/// "2 minutes ago", as for a computer offline since then.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AgoStyle {
    Short,
    Full,
}

/// The time since something: "just now" under a minute, then whole
/// minutes, under a day whole hours, after that whole days (each counted
/// down to the unit, "1 hour ago" until two hours have passed).
pub fn ago(elapsed: Duration, style: AgoStyle) -> UiText {
    const MINUTE: u64 = 60;
    const HOUR: u64 = 60 * MINUTE;
    const DAY: u64 = 24 * HOUR;
    let seconds = elapsed.as_secs();
    let (count, one, many) = if seconds < MINUTE {
        return ui!(keys::RELATIVE_JUST_NOW);
    } else if seconds < HOUR {
        if style == AgoStyle::Short {
            return ui!(keys::RELATIVE_MINUTES_AGO, (seconds / MINUTE).to_string());
        }
        (
            seconds / MINUTE,
            keys::RELATIVE_MINUTES_ONE,
            keys::RELATIVE_MINUTES_MANY,
        )
    } else if seconds < DAY {
        (
            seconds / HOUR,
            keys::RELATIVE_HOURS_ONE,
            keys::RELATIVE_HOURS_MANY,
        )
    } else {
        (
            seconds / DAY,
            keys::RELATIVE_DAYS_ONE,
            keys::RELATIVE_DAYS_MANY,
        )
    };
    if count == 1 {
        ui!(one)
    } else {
        ui!(many, count.to_string())
    }
}

pub fn kind(kinds: &[String]) -> UiText {
    if kinds.iter().any(|k| k == "image") {
        ui!(keys::CLIPBOARD_KIND_IMAGE)
    } else if kinds.iter().any(|k| k == "rich_text") {
        ui!(keys::CLIPBOARD_KIND_RICH_TEXT)
    } else {
        ui!(keys::CLIPBOARD_KIND_TEXT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::Link;
    use iw_proto::app::v1::{ClipboardActivity, ConnectionState, DeviceStatus, Status};

    #[test]
    fn the_last_clipboard_for_ten_minutes() {
        let now = UNIX_EPOCH + Duration::from_secs(10_000);
        let mut model = Snapshot {
            link: Link::Connected,
            status: Status {
                this_device_id: "me".into(),
                clipboard_sync: true,
                devices: vec![DeviceStatus {
                    id: "mac".into(),
                    name: "Mac".into(),
                    state: ConnectionState::Connected as i32,
                    ..Default::default()
                }],
                last_clipboard: Some(ClipboardActivity {
                    at_unix_ms: 9_880_000,
                    from_device_id: "mac".into(),
                    to_device_id: "me".into(),
                    kinds: vec!["text".into(), "image".into()],
                    skipped: String::new(),
                }),
                ..Default::default()
            },
            ..Default::default()
        };
        assert_eq!(
            line(&model, now).unwrap().to_string(),
            "Clipboard: synced from Mac 2 min ago (image)"
        );
        model.status.last_clipboard.as_mut().unwrap().skipped = "sensitive".into();
        assert_eq!(
            line(&model, now).unwrap().to_string(),
            "The clipboard came from a password manager, so it was not synced."
        );
        assert_eq!(line(&model, now + Duration::from_secs(600)), None);
        model.status.clipboard_sync = false;
        assert_eq!(line(&model, now), None);
    }

    #[test]
    fn how_long_ago_by_whole_units() {
        use crate::text::{Catalog, Language};
        let zh = Catalog::for_language(Language::ChineseTaiwan);
        let said = |seconds: u64, style| {
            let text = ago(Duration::from_secs(seconds), style);
            (text.to_string(), zh.render(&text))
        };
        let full = |seconds| said(seconds, AgoStyle::Full);
        let pair = |en: &str, zh: &str| (en.to_owned(), zh.to_owned());
        assert_eq!(full(0), pair("just now", "剛剛"));
        assert_eq!(full(59), pair("just now", "剛剛"));
        assert_eq!(full(60), pair("1 minute ago", "1 分鐘前"));
        assert_eq!(full(119), pair("1 minute ago", "1 分鐘前"));
        assert_eq!(full(120), pair("2 minutes ago", "2 分鐘前"));
        assert_eq!(full(3599), pair("59 minutes ago", "59 分鐘前"));
        assert_eq!(full(3600), pair("1 hour ago", "1 小時前"));
        assert_eq!(full(7199), pair("1 hour ago", "1 小時前"));
        assert_eq!(full(7200), pair("2 hours ago", "2 小時前"));
        assert_eq!(full(86_399), pair("23 hours ago", "23 小時前"));
        assert_eq!(full(86_400), pair("1 day ago", "1 天前"));
        assert_eq!(full(172_799), pair("1 day ago", "1 天前"));
        assert_eq!(full(172_800), pair("2 days ago", "2 天前"));
        assert_eq!(full(40 * 86_400), pair("40 days ago", "40 天前"));
        // The clipboard line keeps its own short minutes.
        let short = |seconds| said(seconds, AgoStyle::Short);
        assert_eq!(short(59), pair("just now", "剛剛"));
        assert_eq!(short(60), pair("1 min ago", "1 分鐘前"));
        assert_eq!(short(599), pair("9 min ago", "9 分鐘前"));
        assert_eq!(short(3600), pair("1 hour ago", "1 小時前"));
    }
}
