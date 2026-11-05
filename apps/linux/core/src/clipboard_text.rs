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
    let when = ago(elapsed);
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

pub fn ago(elapsed: Duration) -> UiText {
    let minutes = elapsed.as_secs() / 60;
    if minutes < 1 {
        ui!(keys::RELATIVE_JUST_NOW)
    } else {
        ui!(keys::RELATIVE_MINUTES_AGO, minutes.to_string())
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
}
