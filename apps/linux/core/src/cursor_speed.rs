//! What the inspector's cursor speed row offers for another computer
//! (`SetDeviceSpeed`): how fast the cursor moves there while this computer
//! controls it, in percent of how the mouse moves it here; as on the Mac.

use crate::snapshot::Snapshot;
use crate::text::{UiText, keys};
use crate::ui;

pub const NORMAL: u32 = 100;
pub const MIN: u32 = 50;
pub const MAX: u32 = 300;
pub const STEP: u32 = 10;
/// The speeds marked under the slider.
pub const TICKS: [u32; 4] = [50, 100, 200, 300];

/// The speed to show for `id`, or `None` where the row is not offered: this
/// computer, and an agent from before cursor speeds, which says 0.
pub fn shown(model: &Snapshot, id: &str) -> Option<u32> {
    if id == model.this_device_id() {
        return None;
    }
    model
        .device(id)
        .filter(|d| d.speed_percent != 0)
        .map(|d| d.speed_percent)
}

/// The speed a position of the slider stands for: the nearest step, in
/// range.
pub fn snapped(value: f64) -> u32 {
    let stepped = (value / f64::from(STEP)).round() * f64::from(STEP);
    // Clamped to the range first, so it fits.
    stepped.clamp(f64::from(MIN), f64::from(MAX)) as u32
}

/// The speed as shown beside the slider and read out, e.g. "150%".
pub fn label(percent: u32) -> String {
    format!("{percent}%")
}

/// Reset is offered unless the speed is the normal one.
pub fn shows_reset(percent: u32) -> bool {
    percent != NORMAL
}

/// What the row says under the slider, for the computer named `name`.
pub fn description(name: &str) -> UiText {
    ui!(keys::INSPECTOR_CURSOR_SPEED_HINT, name)
}

/// The speed being chosen on the slider, shown in place of the agent's
/// until the agent answers for it. The agent's value is what counts:
/// refused, the slider goes back to it. Each speed sent is numbered; until
/// the answer to the latest one comes, the slider shows the latest speed
/// sent, and a new choice is weighed against it, not against the agent's
/// older speed, so choosing the agent's speed again still goes. Answers to
/// earlier sends change nothing.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Draft {
    /// Chosen, and not sent yet (or still held).
    value: Option<u32>,
    editing: bool,
    /// The number of the latest send.
    sent: u64,
    /// The latest speed sent, until its answer comes.
    in_flight: Option<u32>,
}

impl Draft {
    pub fn value(&self) -> Option<u32> {
        self.value
    }

    /// The slider is held under the pointer.
    pub fn editing(&self) -> bool {
        self.editing
    }

    /// What a new choice is weighed against: the latest speed on its way,
    /// or else the agent's.
    fn baseline(&self, agent: u32) -> u32 {
        self.in_flight.unwrap_or(agent)
    }

    /// What the slider shows, the agent having `agent`.
    pub fn shown(&self, agent: u32) -> u32 {
        self.value.unwrap_or(self.baseline(agent))
    }

    /// The pointer pressed the slider.
    pub fn began(&mut self) {
        self.editing = true;
    }

    /// The slider moved to `position`.
    pub fn moved(&mut self, position: f64, agent: u32) {
        self.value = Some(snapped(position));
        self.settle(agent);
    }

    /// The pointer let go of the slider: the speed to send, if it differs
    /// from what the agent has or is getting.
    pub fn ended(&mut self, agent: u32) -> Option<u32> {
        self.editing = false;
        self.settle(agent);
        self.value
    }

    /// A speed chosen without the pointer (the keys, Reset), to send unless
    /// the slider is held.
    pub fn pending(&self, agent: u32) -> Option<u32> {
        self.value
            .filter(|&v| !self.editing && v != self.baseline(agent))
    }

    /// `percent` is sent: the number its answer comes back with.
    pub fn sending(&mut self, percent: u32) -> u64 {
        self.sent += 1;
        self.in_flight = Some(percent);
        if !self.editing && self.value == Some(percent) {
            self.value = None;
        }
        self.sent
    }

    /// The agent answered send `number`, either way. Only the latest one
    /// counts: the slider shows the agent's speed again, but for a speed
    /// chosen and not sent yet.
    pub fn answered(&mut self, number: u64) {
        if number == self.sent {
            self.in_flight = None;
        }
    }

    /// The agent has `agent`: a speed equal to what it has or is getting,
    /// no longer held, is no longer a draft.
    pub fn settle(&mut self, agent: u32) {
        if !self.editing && self.value == Some(self.baseline(agent)) {
            self.value = None;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::snapshot::Link;
    use iw_proto::app::v1::{ConnectionState, DeviceStatus, Status};

    fn model(devices: Vec<DeviceStatus>) -> Snapshot {
        Snapshot {
            link: Link::Connected,
            status: Status {
                this_device_id: "me".into(),
                devices,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn device(id: &str, paired: bool, speed_percent: u32) -> DeviceStatus {
        DeviceStatus {
            id: id.into(),
            paired,
            speed_percent,
            state: ConnectionState::Connected as i32,
            ..Default::default()
        }
    }

    #[test]
    fn offered_for_another_computer_whose_agent_knows_speeds() {
        let mut offline = device("laptop", true, 200);
        offline.state = ConnectionState::Offline as i32;
        let model = model(vec![
            device("mac", true, 150),
            offline,
            device("pc", false, 120),
            device("old", true, 0),
            device("me", true, 100),
        ]);
        assert_eq!(shown(&model, "mac"), Some(150));
        // Offline computers keep theirs: it is this computer's own choice.
        assert_eq!(shown(&model, "laptop"), Some(200));
        // From the configuration file too.
        assert_eq!(shown(&model, "pc"), Some(120));
        // An agent from before cursor speeds.
        assert_eq!(shown(&model, "old"), None);
        assert_eq!(shown(&model, "me"), None);
        assert_eq!(shown(&model, "ghost"), None);
    }

    #[test]
    fn snaps_to_steps_in_range_and_says_the_percent() {
        assert_eq!(snapped(154.0), 150);
        assert_eq!(snapped(155.0), 160);
        assert_eq!(snapped(12.0), 50);
        assert_eq!(snapped(999.0), 300);
        assert_eq!(label(150), "150%");
        assert_eq!(label(50), "50%");
        assert!(shows_reset(150));
        assert!(!shows_reset(100));
        assert_eq!(
            description("Mac mini").to_string(),
            "While the cursor is on Mac mini, mouse movement is multiplied by this. It \
             applies only when this computer controls it, and not to relative mouse for games."
        );
    }

    #[test]
    fn a_drag_sends_where_it_is_let_go_and_then_shows_the_agents() {
        let mut draft = Draft::default();
        assert_eq!(draft.shown(100), 100);
        draft.began();
        draft.moved(131.0, 100);
        assert_eq!(draft.shown(100), 130);
        // Nothing goes while the slider is held, however long it rests.
        assert_eq!(draft.pending(100), None);
        draft.moved(152.0, 100);
        assert_eq!(draft.ended(100), Some(150));
        let sent = draft.sending(150);
        assert_eq!(draft.shown(100), 150);
        draft.answered(sent);
        assert_eq!(draft.value(), None);
        assert_eq!(draft.shown(150), 150);
    }

    #[test]
    fn a_refused_speed_goes_back_to_the_agents() {
        let mut draft = Draft::default();
        draft.began();
        draft.moved(250.0, 100);
        assert_eq!(draft.ended(100), Some(250));
        let sent = draft.sending(250);
        draft.answered(sent);
        assert_eq!(draft.shown(100), 100);
    }

    #[test]
    fn the_agents_speed_chosen_again_while_another_is_on_its_way_still_goes() {
        let mut draft = Draft::default();
        draft.moved(110.0, 100);
        assert_eq!(draft.pending(100), Some(110));
        let first = draft.sending(110);
        // On its way, it is what shows.
        assert_eq!(draft.shown(100), 110);
        // Back to 100 before the agent answered: that is a change too.
        draft.moved(100.0, 100);
        assert_eq!(draft.pending(100), Some(100));
        let second = draft.sending(100);
        // The answer to the first leaves the second on its way.
        draft.answered(first);
        assert_eq!(draft.shown(110), 100);
        draft.answered(second);
        assert_eq!(draft.shown(100), 100);
        assert_eq!(draft.value(), None);
        // Choosing what is on its way again is no change.
        draft.moved(130.0, 100);
        draft.sending(130);
        draft.moved(130.0, 100);
        assert_eq!(draft.pending(100), None);
        // Held, the same goes on letting go.
        draft.began();
        draft.moved(100.0, 100);
        assert_eq!(draft.ended(100), Some(100));
    }

    #[test]
    fn only_the_answer_to_the_latest_send_counts() {
        let mut draft = Draft::default();
        let sends = [110, 120, 110].map(|percent| draft.sending(percent));
        // The answers to the first two change nothing: 110 stays.
        draft.answered(sends[0]);
        assert_eq!(draft.shown(100), 110);
        draft.answered(sends[1]);
        assert_eq!(draft.shown(100), 110);
        // The latest one's: the agent's speed shows, 110 if it took it, 100
        // if it refused.
        draft.answered(sends[2]);
        assert_eq!(draft.shown(110), 110);
        assert_eq!(draft.shown(100), 100);
    }

    #[test]
    fn letting_go_where_it_started_sends_nothing() {
        let mut draft = Draft::default();
        draft.began();
        draft.moved(180.0, 150);
        draft.moved(149.0, 150);
        assert_eq!(draft.ended(150), None);
        assert_eq!(draft.shown(200), 200);
    }

    #[test]
    fn the_keys_and_reset_send_without_the_slider_held() {
        let mut draft = Draft::default();
        draft.moved(110.0, 100);
        assert_eq!(draft.pending(100), Some(110));
        draft.moved(100.0, 100);
        assert_eq!(draft.pending(100), None);
        assert_eq!(draft.value(), None);
        // Reset, from 150.
        draft.moved(f64::from(NORMAL), 150);
        assert_eq!(draft.pending(150), Some(100));
        // An answer while a newer choice waits leaves that choice.
        let sent = draft.sending(130);
        draft.answered(sent);
        assert_eq!(draft.shown(150), 100);
        draft.settle(100);
        assert_eq!(draft.value(), None);
        // An answer while the slider is held leaves it.
        draft.moved(120.0, 100);
        let sent = draft.sending(120);
        draft.began();
        draft.moved(140.0, 100);
        draft.answered(sent);
        assert_eq!(draft.shown(100), 140);
    }
}
