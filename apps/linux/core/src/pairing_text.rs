//! Pairing's words and the code's look (the Windows app's `PairingText`).

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use iw_proto::app::v1::{PairingPhase, PairingState};

use crate::text::{UiText, keys};
use crate::ui;

/// Why pairing failed, from `PairingState.error`.
pub fn failure(state: &PairingState) -> UiText {
    let name = || {
        if state.peer_name.is_empty() {
            crate::text::Arg::Text(ui!(keys::PAIRING_THE_OTHER_COMPUTER))
        } else {
            crate::text::Arg::Plain(state.peer_name.clone())
        }
    };
    match state.error.as_str() {
        "too_many_attempts" => ui!(keys::PAIRING_TOO_MANY_ATTEMPTS),
        "expired" => ui!(keys::PAIRING_EXPIRED),
        "rejected" => UiText::with(keys::PAIRING_REJECTED, [name()]),
        "not_allowed" => UiText::with(keys::PAIRING_NOT_ALLOWED, [name()]),
        "busy" => UiText::with(keys::PAIRING_BUSY, [name()]),
        "id_conflict" => UiText::with(keys::PAIRING_ID_CONFLICT, [name()]),
        "incompatible" => UiText::with(keys::PAIRING_INCOMPATIBLE, [name()]),
        "unreachable" => UiText::with(keys::PAIRING_UNREACHABLE, [name()]),
        "cancelled" => ui!(keys::PAIRING_CANCELLED),
        _ => ui!(keys::PAIRING_DID_NOT_WORK),
    }
}

/// Why StartPairing was refused, from the refusal's code.
pub fn start_failure(code: &str) -> UiText {
    match code {
        "busy" => ui!(keys::PAIRING_START_BUSY),
        "invalid_address" => ui!(keys::PAIRING_START_INVALID_ADDRESS),
        "unreachable" | "unknown_device" => ui!(keys::PAIRING_START_UNREACHABLE),
        _ => ui!(keys::PAIRING_START_FAILED),
    }
}

pub const CODE_LENGTH: usize = 6;

/// The digits typed, at most six.
pub fn clean(typed: &str) -> String {
    typed
        .chars()
        .filter(char::is_ascii_digit)
        .take(CODE_LENGTH)
        .collect()
}

/// The code's six boxes after what is now in box `at` was typed or pasted
/// there: a digit goes into the box and the focus to the next; more than
/// one (pasted, or typed into a box that had one) carry on into the boxes
/// after it; a whole code fills every box, wherever it went. Anything but
/// digits is dropped. Returns the boxes and the box to focus.
pub fn spread(
    boxes: &[String; CODE_LENGTH],
    at: usize,
    now: &str,
) -> ([String; CODE_LENGTH], usize) {
    let mut out = boxes.clone();
    let at = at.min(CODE_LENGTH - 1);
    let digits = clean(now);
    let count = digits.chars().count();
    if count == 0 {
        out[at].clear();
        return (out, at);
    }
    let start = if count == CODE_LENGTH { 0 } else { at };
    let mut last = start;
    for (i, digit) in digits.chars().enumerate() {
        let index = start + i;
        if index >= CODE_LENGTH {
            break;
        }
        out[index] = digit.to_string();
        last = index;
    }
    (out, (last + 1).min(CODE_LENGTH - 1))
}

/// "482 913".
pub fn grouped(code: &str) -> String {
    if code.len() == CODE_LENGTH && code.is_ascii() {
        format!("{} {}", &code[..3], &code[3..])
    } else {
        code.to_owned()
    }
}

/// "4 8 2 9 1 3", digit by digit for screen readers.
pub fn spoken(code: &str) -> String {
    code.chars().map(String::from).collect::<Vec<_>>().join(" ")
}

/// "4:59" until the code expires.
pub fn countdown(expires_at_unix_ms: u64, now: SystemTime) -> String {
    let deadline = UNIX_EPOCH + Duration::from_millis(expires_at_unix_ms);
    let left = deadline.duration_since(now).unwrap_or_default();
    let seconds = left.as_secs() + u64::from(left.subsec_nanos() > 0);
    format!("{}:{:02}", seconds / 60, seconds % 60)
}

/// Whether this computer shows a code for another one to type.
pub fn shows_code(state: &PairingState) -> bool {
    state.phase() == PairingPhase::ShowCode && !state.code.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_name_the_other_computer() {
        let mut state = PairingState {
            error: "rejected".into(),
            peer_name: "Mac mini".into(),
            ..Default::default()
        };
        assert_eq!(
            failure(&state).to_string(),
            "Mac mini declined the pairing."
        );
        state.peer_name.clear();
        assert_eq!(
            failure(&state).to_string(),
            "The other computer declined the pairing."
        );
        state.error = "weird".into();
        assert_eq!(
            failure(&state).to_string(),
            "Pairing did not work. Start again."
        );
        assert_eq!(
            start_failure("unknown_device").to_string(),
            "That computer cannot be reached."
        );
    }

    fn boxes(digits: &str) -> [String; CODE_LENGTH] {
        let mut out: [String; CODE_LENGTH] = Default::default();
        for (i, c) in digits.chars().enumerate() {
            if c != '_' {
                out[i] = c.to_string();
            }
        }
        out
    }

    #[test]
    fn digits_spread_over_the_boxes() {
        // One digit, and the focus moves on.
        assert_eq!(spread(&boxes("______"), 0, "4"), (boxes("4_____"), 1));
        // Not a digit: the box empties.
        assert_eq!(spread(&boxes("4_____"), 1, "x"), (boxes("4_____"), 1));
        // A whole code, pasted into any box, fills them all.
        assert_eq!(spread(&boxes("______"), 0, "482913"), (boxes("482913"), 5));
        assert_eq!(spread(&boxes("4_____"), 3, "482 913"), (boxes("482913"), 5));
        // Part of one carries on from the box it went into.
        assert_eq!(spread(&boxes("48____"), 2, "29"), (boxes("4829__"), 4));
        // Typing into a full box pushes the new digit on.
        assert_eq!(spread(&boxes("4_____"), 0, "48"), (boxes("48____"), 2));
        // Past the last box is dropped.
        assert_eq!(spread(&boxes("4829__"), 4, "1357"), (boxes("482913"), 5));
        // Cleared.
        assert_eq!(spread(&boxes("482___"), 2, ""), (boxes("48____"), 2));
    }

    #[test]
    fn codes_look_and_count_down() {
        assert_eq!(clean("48-29 13x7"), "482913");
        assert_eq!(grouped("482913"), "482 913");
        assert_eq!(grouped("4829"), "4829");
        assert_eq!(spoken("482"), "4 8 2");
        let now = UNIX_EPOCH + Duration::from_millis(1_000_000);
        assert_eq!(countdown(1_000_000 + 299_500, now), "5:00");
        assert_eq!(countdown(1_000_000 + 61_000, now), "1:01");
        assert_eq!(countdown(500, now), "0:00");
        let showing = PairingState {
            phase: PairingPhase::ShowCode as i32,
            code: "123456".into(),
            ..Default::default()
        };
        assert!(shows_code(&showing));
    }
}
