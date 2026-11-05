//! Dragging files from one device to another (docs/architecture.md §6.4,
//! §9): the drags this device's user began (`BeginDrag`), and those
//! other devices offer here. Pure state: the engine passes in what
//! happened and the time, and carries out the [`Effect`]s.
//!
//! The source is the controlling device, where the files are. As its
//! cursor enters another device with the drag going, it offers the drag
//! there (`DragOffer`), and takes the offer back (`DragCancel`) as the
//! cursor leaves still dragging. Released over a device holding the
//! offer, the drag waits for that device's answer: accepted, the items go
//! as a batch carrying the drag's id; declined, it ends. Released on the
//! source itself, it ends. The source decides: a target accepting an offer
//! it no longer holds is told to forget it.
//!
//! A target shows an offer to its app only while the source controls it,
//! which may be a moment after the offer arrives: the cursor comes on the
//! input stream, the offer on the control stream. With no app connected to
//! show it, the offer is declined at once.
//!
//! The files may also be on a device the controller controls. That device, the source, reports the drag
//! to its controller (`RemoteDrag`) instead of crossing with it itself;
//! the controller carries the cursor and the button, offers the drag on
//! the source's behalf (`DragOffer.source_device_id`), to its own app when
//! the cursor comes back to it, and tells the source where the files go
//! once a device accepts them (`DragAccept.device_id`). The source sends
//! only what its own app gave it for that drag, and only to its
//! controller or where its controller says.
//!
//! Every drag and offer lapses after [`DRAG_EXPIRY`].

use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use iw_core::layout::DeviceId;
use iw_proto::peer::v1::{
    DragAccept, DragCancel, DragDecline, DragItem, DragOffer, RemoteDrag, control,
};

use super::files::valid_transfer_id;
use super::listing::{ItemKind, MAX_ITEMS, Manifest};

/// How long a drag or an offer lasts at most.
pub(crate) const DRAG_EXPIRY: Duration = Duration::from_secs(10 * 60);
/// Offers held here at once; beyond this, the oldest is dropped.
const MAX_OFFERS: usize = 8;
/// The peer protocol version that knows drags of files on a controlled
/// device (`RemoteDrag`, `DragOffer.source_device_id`,
/// `DragAccept.device_id`).
pub(crate) const REMOTE_DRAG_SINCE: u32 = 4;
/// Devices a source names as linked to it, at most.
pub(crate) const MAX_LINKED: usize = 64;
/// How long the controller passes on a source's refusal to send a drag it
/// told the source to send elsewhere: the source refuses at once, or not.
const FORWARD_WINDOW: Duration = Duration::from_secs(30);

/// A drag another device offers here, as its app is told.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArrivedDrag {
    pub id: String,
    pub from: String,
    pub items: Vec<DraggedItem>,
    pub files: u32,
    pub bytes: u64,
    /// The files are on a device this one controls, and the cursor
    /// brought the drag back here: the button is held on this device's own
    /// mouse, and nothing controls it.
    pub at_controller: bool,
    /// The device the files are on, when it is not `from`, which controls
    /// this device and carries a drag of files on another device it
    /// controls.
    pub source: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DraggedItem {
    pub name: String,
    pub folder: bool,
    pub size: u64,
    pub files: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DragEnd {
    /// Dropped on the device and accepted there: the files go.
    Accepted,
    /// The device's user did not drop it there, or no app was there.
    Declined,
    /// Released where nothing takes it.
    Released,
    /// The source took the offer back.
    Withdrawn,
    Expired,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndedDrag {
    pub id: String,
    pub reason: DragEnd,
    /// The other device involved, if any.
    pub device: Option<String>,
}

/// What the apps hear about drags.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DragEvent {
    Arrived(ArrivedDrag),
    Ended(EndedDrag),
}

/// The drag going on from here, for the status.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Dragging {
    pub id: String,
    pub items: u32,
    pub files: u32,
    pub bytes: u64,
    /// The device the drag is offered to now.
    pub device: Option<String>,
}

/// Something the engine carries out.
#[derive(Debug)]
pub(crate) enum Effect {
    /// A control message to a device.
    Send(DeviceId, control::Msg),
    /// The drag's items go to the device as a batch.
    Transfer {
        device: DeviceId,
        manifest: Arc<Manifest>,
        drag: String,
    },
    /// Tells the apps.
    Event(DragEvent),
    /// No files come for this drag from that device.
    Forget { from: DeviceId, drag: String },
}

/// A drag that began here and is going on: the button is held.
struct Outgoing {
    id: String,
    manifest: Arc<Manifest>,
    offer: DragOffer,
    began: Instant,
    /// The device holding the offer, the cursor being there; for a drag
    /// reported to the controller, that device.
    over: Option<DeviceId>,
    /// The device controlling this one, which the drag was reported to
    /// (`RemoteDrag`): it carries the cursor, and says where the files go.
    controller: Option<DeviceId>,
    /// For a drag reported to the controller: the cursor left this device
    /// with it.
    crossed: bool,
}

/// Where a drag on a device this one controls is offered, the cursor
/// being there.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Over {
    /// To this device's own app.
    Here,
    Device(DeviceId),
}

/// A drag of files on a device this one controls (`RemoteDrag`), which the
/// cursor carries from here.
struct Remote {
    source: DeviceId,
    /// What is offered, naming the source.
    offer: DragOffer,
    /// The devices the source can send the files to.
    linked: HashSet<DeviceId>,
    began: Instant,
    over: Option<Over>,
    /// Released over the device holding the offer, waiting for its answer.
    dropped: bool,
}

/// A drag released over another device, waiting for its answer.
struct Dropped {
    id: String,
    manifest: Arc<Manifest>,
    device: DeviceId,
    at: Instant,
}

/// A drag another device offers here.
struct Offered {
    from: DeviceId,
    /// The device the files come from: `from`, or the device `from`
    /// controls and offers the drag of.
    source: DeviceId,
    offer: DragOffer,
    at: Instant,
    /// Its app was told.
    shown: bool,
}

/// Drags answered already that may still be called off, each kept a
/// while: at most `cap`, each until `lasts` after it was noted.
struct Kept<T> {
    items: Vec<(T, Instant)>,
    cap: usize,
    lasts: Duration,
}

impl<T> Kept<T> {
    fn new(cap: usize, lasts: Duration) -> Self {
        Self {
            items: Vec::new(),
            cap,
            lasts,
        }
    }

    fn note(&mut self, item: T, now: Instant) {
        if self.items.len() >= self.cap {
            self.items.remove(0);
        }
        self.items.push((item, now));
    }

    /// Takes out the first that `which` picks.
    fn take(&mut self, which: impl Fn(&T) -> bool) -> Option<T> {
        let at = self.items.iter().position(|(item, _)| which(item))?;
        Some(self.items.remove(at).0)
    }

    /// Takes out all that `which` picks.
    fn take_all(&mut self, which: impl Fn(&T) -> bool) -> Vec<T> {
        let (gone, kept): (Vec<_>, Vec<_>) = self.items.drain(..).partition(|(i, _)| which(i));
        self.items = kept;
        gone.into_iter().map(|(item, _)| item).collect()
    }

    /// Forgets those that lasted their while by `now`.
    fn lapse(&mut self, now: Instant) {
        let lasts = self.lasts;
        self.items
            .retain(|(_, at)| now.saturating_duration_since(*at) < lasts);
    }

    fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// A drag carried from here that another device accepted: its source was
/// told to send the files there, and may refuse at once.
struct Forwarded {
    source: DeviceId,
    id: String,
    target: DeviceId,
}

/// A drag accepted here, until its files start coming: they may still be
/// called off.
struct Accepted {
    from: DeviceId,
    source: DeviceId,
    id: String,
}

pub(crate) struct Drags {
    outgoing: Option<Outgoing>,
    dropped: Vec<Dropped>,
    offered: Vec<Offered>,
    remote: Option<Remote>,
    forwarded: Kept<Forwarded>,
    accepted: Kept<Accepted>,
}

impl Default for Drags {
    fn default() -> Self {
        Self {
            outgoing: None,
            dropped: Vec::new(),
            offered: Vec::new(),
            remote: None,
            forwarded: Kept::new(MAX_OFFERS, FORWARD_WINDOW),
            accepted: Kept::new(MAX_OFFERS, DRAG_EXPIRY),
        }
    }
}

/// No such drag is offered here.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct NotOffered;

fn ended(id: &str, reason: DragEnd, device: Option<&DeviceId>) -> Effect {
    Effect::Event(DragEvent::Ended(EndedDrag {
        id: id.to_owned(),
        reason,
        device: device.map(|d| d.to_string()),
    }))
}

fn cancel(device: &DeviceId, id: &str) -> Effect {
    Effect::Send(
        device.clone(),
        control::Msg::DragCancel(DragCancel {
            drag_id: id.to_owned(),
        }),
    )
}

fn decline(device: &DeviceId, id: &str) -> Effect {
    Effect::Send(
        device.clone(),
        control::Msg::DragDecline(DragDecline {
            drag_id: id.to_owned(),
        }),
    )
}

/// What a drag of `manifest` offers: the items that go, at most
/// [`MAX_ITEMS`] of them.
pub(crate) fn offer_of(id: &str, manifest: &Manifest) -> DragOffer {
    let items: Vec<DragItem> = manifest
        .items
        .iter()
        .filter(|item| item.sendable())
        .take(MAX_ITEMS)
        .map(|item| {
            let (files, size) = item.totals();
            DragItem {
                name: item.name.clone(),
                folder: matches!(item.kind, ItemKind::Folder { .. }),
                size,
                files: files as u32,
                unknown: false,
            }
        })
        .collect();
    let (files, bytes) = manifest.totals();
    DragOffer {
        drag_id: id.to_owned(),
        items,
        files: files as u32,
        bytes,
        source_device_id: String::new(),
    }
}

impl Drags {
    /// Whether a drag from here is going on.
    pub(crate) fn active(&self) -> bool {
        self.outgoing.is_some()
    }

    pub(crate) fn dragging(&self) -> Option<Dragging> {
        let drag = self.outgoing.as_ref()?;
        // Reported to the controller: on its way once the cursor left here.
        let device = match &drag.controller {
            Some(controller) => drag.crossed.then_some(controller),
            None => drag.over.as_ref(),
        };
        Some(Dragging {
            id: drag.id.clone(),
            items: drag.offer.items.len() as u32,
            files: drag.offer.files,
            bytes: drag.offer.bytes,
            device: device.map(|d| d.to_string()),
        })
    }

    /// A drag of `manifest` began here, in place of one going on.
    pub(crate) fn begin(&mut self, id: &str, manifest: Arc<Manifest>, now: Instant) -> Vec<Effect> {
        let effects = self.give_up(DragEnd::Released);
        self.outgoing = Some(Outgoing {
            id: id.to_owned(),
            offer: offer_of(id, &manifest),
            manifest,
            began: now,
            over: None,
            controller: None,
            crossed: false,
        });
        effects
    }

    /// A drag of `manifest` began here while `controller` controls this
    /// device: it is reported there, with the devices in `linked` this one
    /// can send the files to, and goes where `controller` says.
    pub(crate) fn begin_controlled(
        &mut self,
        id: &str,
        manifest: Arc<Manifest>,
        controller: &DeviceId,
        linked: Vec<String>,
        now: Instant,
    ) -> Vec<Effect> {
        let mut effects = self.give_up(DragEnd::Released);
        let offer = offer_of(id, &manifest);
        effects.push(Effect::Send(
            controller.clone(),
            control::Msg::RemoteDrag(RemoteDrag {
                drag_id: id.to_owned(),
                items: offer.items.clone(),
                files: offer.files,
                bytes: offer.bytes,
                linked_device_ids: linked.into_iter().take(MAX_LINKED).collect(),
            }),
        ));
        self.outgoing = Some(Outgoing {
            id: id.to_owned(),
            offer,
            manifest,
            began: now,
            over: Some(controller.clone()),
            controller: Some(controller.clone()),
            crossed: false,
        });
        effects
    }

    /// `controller` took the cursor off this device: the drag reported to
    /// it, if any, is on its way.
    pub(crate) fn left_source(&mut self, controller: &DeviceId) {
        if let Some(drag) = self.outgoing.as_mut()
            && drag.controller.as_ref() == Some(controller)
        {
            drag.crossed = true;
        }
    }

    /// Whether drag `id`, going on from here, was reported to `from`, which
    /// controls this device.
    fn outgoing_reported_to(&self, from: &DeviceId, id: &str) -> bool {
        self.outgoing
            .as_ref()
            .is_some_and(|d| d.id == id && d.controller.as_ref() == Some(from))
    }

    /// `controller` brought the cursor back to this device: the drag
    /// reported to it, if any, is here again.
    pub(crate) fn back_on_source(&mut self, controller: &DeviceId) {
        if let Some(drag) = self.outgoing.as_mut()
            && drag.controller.as_ref() == Some(controller)
        {
            drag.crossed = false;
        }
    }

    /// The controller ended drag `id`, reported to it from here: released
    /// back here, or nowhere. `None` if no such drag was reported to `from`.
    pub(crate) fn ended_by_controller(&mut self, from: &DeviceId, id: &str) -> Option<Vec<Effect>> {
        if !self.outgoing_reported_to(from, id) {
            return None;
        }
        self.outgoing = None;
        Some(vec![ended(id, DragEnd::Released, None)])
    }

    /// The controller says `target` accepted drag `id`, reported to it from
    /// here: the files go there. `None` if no such drag was reported to
    /// `from`.
    pub(crate) fn accepted_for(
        &mut self,
        from: &DeviceId,
        id: &str,
        target: &DeviceId,
    ) -> Option<Vec<Effect>> {
        if !self.outgoing_reported_to(from, id) {
            return None;
        }
        let drag = self.outgoing.take()?;
        Some(vec![
            Effect::Transfer {
                device: target.clone(),
                manifest: drag.manifest,
                drag: id.to_owned(),
            },
            ended(id, DragEnd::Accepted, Some(target)),
        ])
    }

    /// Ends the drag going on from here, if any, taking back its offer.
    fn give_up(&mut self, reason: DragEnd) -> Vec<Effect> {
        let Some(drag) = self.outgoing.take() else {
            return Vec::new();
        };
        let mut effects = Vec::new();
        if let Some(over) = &drag.over {
            effects.push(cancel(over, &drag.id));
        }
        effects.push(ended(&drag.id, reason, None));
        effects
    }

    /// The app says drag `id` ended without crossing. `None` if it is not
    /// going on.
    pub(crate) fn end(&mut self, id: &str) -> Option<Vec<Effect>> {
        if self.outgoing.as_ref()?.id != id {
            return None;
        }
        Some(self.give_up(DragEnd::Released))
    }

    /// The cursor entered `device`, dragging.
    pub(crate) fn entered(&mut self, device: &DeviceId) -> Vec<Effect> {
        let Some(drag) = self.outgoing.as_mut() else {
            return Vec::new();
        };
        if drag.over.as_ref() == Some(device) {
            return Vec::new();
        }
        let mut effects = Vec::new();
        if let Some(over) = drag.over.take() {
            effects.push(cancel(&over, &drag.id));
        }
        drag.over = Some(device.clone());
        effects.push(Effect::Send(
            device.clone(),
            control::Msg::DragOffer(drag.offer.clone()),
        ));
        effects
    }

    /// The cursor left `device`, still dragging: the offer there is off.
    pub(crate) fn left(&mut self, device: &DeviceId) -> Vec<Effect> {
        match self.outgoing.as_mut() {
            Some(drag) if drag.over.as_ref() == Some(device) => {
                drag.over = None;
                vec![cancel(device, &drag.id)]
            }
            _ => Vec::new(),
        }
    }

    /// The drag was released over `device`, `local` being this one.
    pub(crate) fn released(
        &mut self,
        device: &DeviceId,
        local: &DeviceId,
        now: Instant,
    ) -> Vec<Effect> {
        let holds = self
            .outgoing
            .as_ref()
            .is_some_and(|drag| drag.over.as_ref() == Some(device) && device != local);
        if !holds {
            return self.give_up(DragEnd::Released);
        }
        let Some(drag) = self.outgoing.take() else {
            return Vec::new();
        };
        self.dropped.push(Dropped {
            id: drag.id,
            manifest: drag.manifest,
            device: device.clone(),
            at: now,
        });
        Vec::new()
    }

    /// Takes drag `id` out, if it is offered to `from`: dropped there, or
    /// still going on with the cursor there.
    fn take_offered(&mut self, from: &DeviceId, id: &str) -> Option<Arc<Manifest>> {
        if let Some(at) = self
            .dropped
            .iter()
            .position(|d| d.id == id && d.device == *from)
        {
            Some(self.dropped.remove(at).manifest)
        } else if self
            .outgoing
            .as_ref()
            .is_some_and(|d| d.id == id && d.over.as_ref() == Some(from))
        {
            self.outgoing.take().map(|d| d.manifest)
        } else {
            None
        }
    }

    /// `from` accepted drag `id`, but files are off with it here (turned
    /// off after the offer went): the offer is taken back instead, so its
    /// promised files fail there, and the drag ends here.
    pub(crate) fn refused_to(&mut self, from: &DeviceId, id: &str) -> Vec<Effect> {
        match self.take_offered(from, id) {
            Some(_) => vec![cancel(from, id), ended(id, DragEnd::Withdrawn, Some(from))],
            None => vec![cancel(from, id)],
        }
    }

    /// `from` accepted drag `id`: its user dropped it there.
    pub(crate) fn accepted_by(&mut self, from: &DeviceId, id: &str) -> Vec<Effect> {
        match self.take_offered(from, id) {
            Some(manifest) => vec![
                Effect::Transfer {
                    device: from.clone(),
                    manifest,
                    drag: id.to_owned(),
                },
                ended(id, DragEnd::Accepted, Some(from)),
            ],
            // Not offered there any more: no files will come.
            None => vec![cancel(from, id)],
        }
    }

    /// `from` declined drag `id`.
    pub(crate) fn declined_by(&mut self, from: &DeviceId, id: &str) -> Vec<Effect> {
        // Reported to `from`, which controls this device: wherever it was
        // dropped, nobody took it.
        if self.outgoing_reported_to(from, id) {
            self.outgoing = None;
            return vec![ended(id, DragEnd::Declined, None)];
        }
        if let Some(at) = self
            .dropped
            .iter()
            .position(|d| d.id == id && d.device == *from)
        {
            self.dropped.remove(at);
            return vec![ended(id, DragEnd::Declined, Some(from))];
        }
        if let Some(drag) = self.outgoing.as_mut()
            && drag.id == id
            && drag.over.as_ref() == Some(from)
        {
            // Still dragging: it may yet be dropped elsewhere.
            drag.over = None;
        }
        Vec::new()
    }

    /// `from` offers a drag here, whose files are on `source` (`from`
    /// itself, or a device it controls); `controller` controls this device,
    /// and `has_app` says whether an app is there to show it.
    pub(crate) fn offered(
        &mut self,
        from: &DeviceId,
        source: &DeviceId,
        offer: DragOffer,
        controller: Option<&DeviceId>,
        has_app: bool,
        now: Instant,
    ) -> Vec<Effect> {
        if !valid_transfer_id(&offer.drag_id) || offer.items.len() > MAX_ITEMS {
            return Vec::new();
        }
        if !has_app {
            return vec![decline(from, &offer.drag_id)];
        }
        let shown = (controller == Some(from)).then_some(false);
        self.hold(from, source, offer, shown, now)
    }

    /// Holds `from`'s offer here, in place of its last (a device drags one
    /// thing at a time) and of the oldest beyond [`MAX_OFFERS`]; shown to
    /// the app at once if `shown` says so, with whether it came back to
    /// this device as its controller.
    fn hold(
        &mut self,
        from: &DeviceId,
        source: &DeviceId,
        offer: DragOffer,
        shown: Option<bool>,
        now: Instant,
    ) -> Vec<Effect> {
        let mut effects = self.drop_offers(|o| o.from == *from, DragEnd::Withdrawn);
        if self.offered.len() >= MAX_OFFERS {
            let oldest = self.offered.remove(0);
            if oldest.shown {
                effects.push(ended(
                    &oldest.offer.drag_id,
                    DragEnd::Withdrawn,
                    Some(&oldest.from),
                ));
            }
        }
        if let Some(at_controller) = shown {
            effects.push(arrived(from, source, &offer, at_controller));
        }
        self.offered.push(Offered {
            from: from.clone(),
            source: source.clone(),
            offer,
            at: now,
            shown: shown.is_some(),
        });
        effects
    }

    /// `from` took control of this device: its offer, if it beat the
    /// cursor here, is shown now.
    pub(crate) fn controller_entered(&mut self, from: &DeviceId) -> Vec<Effect> {
        let mut effects = Vec::new();
        for offered in self.offered.iter_mut().filter(|o| o.from == *from) {
            if !offered.shown {
                offered.shown = true;
                effects.push(arrived(
                    &offered.from,
                    &offered.source,
                    &offered.offer,
                    false,
                ));
            }
        }
        effects
    }

    /// `from` took back its offer of drag `id`; if this device's user
    /// accepted it already, its files are not coming after all, and the
    /// app hears so at once.
    pub(crate) fn withdrawn(&mut self, from: &DeviceId, id: &str) -> Vec<Effect> {
        let mut sources = vec![from.clone()];
        sources.extend(
            self.offered
                .iter()
                .filter(|o| o.from == *from && o.offer.drag_id == id)
                .map(|o| o.source.clone()),
        );
        let mut effects = self.drop_offers(
            |o| o.from == *from && o.offer.drag_id == id,
            DragEnd::Withdrawn,
        );
        if let Some(accepted) = self.accepted.take(|a| a.from == *from && a.id == id) {
            sources.push(accepted.source);
            effects.push(ended(id, DragEnd::Withdrawn, Some(from)));
        }
        let mut seen = HashSet::new();
        for source in sources {
            if seen.insert(source.clone()) {
                effects.push(Effect::Forget {
                    from: source,
                    drag: id.to_owned(),
                });
            }
        }
        effects
    }

    /// This device's user dropped drag `id` here. Returns the device the
    /// files come from, which expects them; the device that offered the
    /// drag is told.
    pub(crate) fn accept(
        &mut self,
        id: &str,
        now: Instant,
    ) -> Result<(DeviceId, Vec<Effect>), NotOffered> {
        let at = self
            .offered
            .iter()
            .position(|o| o.offer.drag_id == id)
            .ok_or(NotOffered)?;
        let offered = self.offered.remove(at);
        self.settled_here(&offered);
        let effect = Effect::Send(
            offered.from.clone(),
            control::Msg::DragAccept(DragAccept {
                drag_id: id.to_owned(),
                device_id: String::new(),
            }),
        );
        self.accepted.note(
            Accepted {
                from: offered.from.clone(),
                source: offered.source.clone(),
                id: id.to_owned(),
            },
            now,
        );
        Ok((offered.source, vec![effect]))
    }

    /// Forgets the drags accepted here whose files started coming (no
    /// longer `awaited` from their source): nothing calls them off now.
    pub(crate) fn forget_arriving(&mut self, awaited: impl Fn(&DeviceId, &str) -> bool) {
        self.accepted.take_all(|a| !awaited(&a.source, &a.id));
    }

    /// The offer of a drag on a device this one controls, shown here, was
    /// answered: its drag is no longer carried from here.
    fn settled_here(&mut self, offered: &Offered) {
        if self
            .remote
            .as_ref()
            .is_some_and(|r| r.source == offered.from && r.offer.drag_id == offered.offer.drag_id)
        {
            self.remote = None;
        }
    }

    /// This device's user did not drop drag `id` here.
    pub(crate) fn decline(&mut self, id: &str) -> Result<Vec<Effect>, NotOffered> {
        let at = self
            .offered
            .iter()
            .position(|o| o.offer.drag_id == id)
            .ok_or(NotOffered)?;
        let offered = self.offered.remove(at);
        self.settled_here(&offered);
        Ok(vec![decline(&offered.from, id)])
    }

    /// The session with `device` ended: its offers here are gone, the drag
    /// from here is no longer offered there (and ends, if it controlled
    /// this device), and a drag on it carried from here ends.
    pub(crate) fn peer_down(&mut self, device: &DeviceId) -> Vec<Effect> {
        let mut effects = Vec::new();
        if let Some(drag) = self.outgoing.as_mut()
            && drag.over.as_ref() == Some(device)
        {
            if drag.controller.as_ref() == Some(device) {
                let id = drag.id.clone();
                self.outgoing = None;
                effects.push(ended(&id, DragEnd::Released, None));
            } else {
                drag.over = None;
            }
        }
        if let Some(remote) = self.remote.as_ref() {
            if remote.source == *device {
                effects.extend(self.drop_remote());
            } else if remote.over == Some(Over::Device(device.clone())) {
                let dropped = remote.dropped;
                if let Some(remote) = self.remote.as_mut() {
                    remote.over = None;
                }
                if dropped {
                    // Dropped there, and nobody is left to answer.
                    effects.extend(self.end_remote());
                }
            }
        }
        // The target hears of its source going on its own session with it.
        self.forwarded
            .take_all(|f| f.source == *device || f.target == *device);
        // Files the device was to send here are not coming.
        for accepted in self.accepted.take_all(|a| a.source == *device) {
            effects.push(ended(
                &accepted.id,
                DragEnd::Withdrawn,
                Some(&accepted.from),
            ));
            effects.push(Effect::Forget {
                from: accepted.source,
                drag: accepted.id,
            });
        }
        effects.extend(self.drop_offers(|o| o.from == *device, DragEnd::Withdrawn));
        effects
    }

    /// `source`, which this device controls, reports a drag of files
    /// there, which the devices in `linked` can take. `None` if the report
    /// is malformed. A drag reported before it ends.
    pub(crate) fn remote_began(
        &mut self,
        source: &DeviceId,
        report: RemoteDrag,
        linked: HashSet<DeviceId>,
        now: Instant,
    ) -> Option<Vec<Effect>> {
        if !valid_transfer_id(&report.drag_id) || report.items.len() > MAX_ITEMS {
            return None;
        }
        let mut effects = self.end_remote();
        self.remote = Some(Remote {
            source: source.clone(),
            offer: DragOffer {
                drag_id: report.drag_id,
                items: report.items,
                files: report.files,
                bytes: report.bytes,
                source_device_id: source.to_string(),
            },
            linked,
            began: now,
            over: None,
            dropped: false,
        });
        effects.retain(|e| !matches!(e, Effect::Send(d, _) if d == source));
        Some(effects)
    }

    /// The device whose drag the cursor carries from here, if any.
    pub(crate) fn remote_source(&self) -> Option<&DeviceId> {
        self.remote.as_ref().map(|r| &r.source)
    }

    /// Whether the drag carried from here is offered to this device's own
    /// app now, the cursor having come back here with it.
    pub(crate) fn offered_here(&self) -> bool {
        self.remote
            .as_ref()
            .is_some_and(|r| r.over == Some(Over::Here))
    }

    /// Forgets the drag carried from here, if any, taking back its offer.
    fn drop_remote(&mut self) -> Vec<Effect> {
        let effects = self.leave_over();
        self.remote = None;
        effects
    }

    /// Ends the drag carried from here, if any, taking back its offer and
    /// telling its source.
    pub(crate) fn end_remote(&mut self) -> Vec<Effect> {
        let Some(remote) = self.remote.as_ref() else {
            return Vec::new();
        };
        let (source, id) = (remote.source.clone(), remote.offer.drag_id.clone());
        let mut effects = self.drop_remote();
        effects.push(cancel(&source, &id));
        effects
    }

    /// Takes back the offer of the drag carried from here, wherever it is
    /// held.
    fn leave_over(&mut self) -> Vec<Effect> {
        let Some(remote) = self.remote.as_mut() else {
            return Vec::new();
        };
        let (source, id) = (remote.source.clone(), remote.offer.drag_id.clone());
        match remote.over.take() {
            Some(Over::Here) => self.drop_offers(
                |o| o.from == source && o.offer.drag_id == id,
                DragEnd::Withdrawn,
            ),
            Some(Over::Device(device)) => vec![cancel(&device, &id)],
            None => Vec::new(),
        }
    }

    /// The cursor entered `device` with the drag carried from here, `local`
    /// being this device: the drag is offered there if it can take it
    /// (`offerable`: linked to the source, files not off with it, its
    /// protocol knows such drags), or here if an app is here (`has_app`).
    pub(crate) fn remote_entered(
        &mut self,
        device: &DeviceId,
        local: &DeviceId,
        offerable: bool,
        has_app: bool,
        now: Instant,
    ) -> Vec<Effect> {
        let Some(remote) = self.remote.as_ref() else {
            return Vec::new();
        };
        // Dropped already: only that device's answer, or the expiry, ends it.
        if remote.dropped {
            return Vec::new();
        }
        let over = if device == local {
            Over::Here
        } else {
            Over::Device(device.clone())
        };
        if remote.over.as_ref() == Some(&over) {
            return Vec::new();
        }
        let mut effects = self.leave_over();
        let Some(remote) = self.remote.as_mut() else {
            return effects;
        };
        match over {
            // Back where the files are: offered nowhere.
            Over::Device(device) if device == remote.source => {}
            Over::Here if has_app => {
                remote.over = Some(Over::Here);
                let offer = remote.offer.clone();
                let source = remote.source.clone();
                effects.extend(self.hold(&source, &source, offer, Some(true), now));
            }
            Over::Device(device) if offerable && remote.linked.contains(&device) => {
                effects.push(Effect::Send(
                    device.clone(),
                    control::Msg::DragOffer(remote.offer.clone()),
                ));
                remote.over = Some(Over::Device(device));
            }
            _ => {}
        }
        effects
    }

    /// The cursor left `device` with the drag carried from here. Dropped
    /// there already, the offer stands until that device answers.
    pub(crate) fn remote_left(&mut self, device: &DeviceId) -> Vec<Effect> {
        match &self.remote {
            Some(remote)
                if !remote.dropped && remote.over == Some(Over::Device(device.clone())) =>
            {
                self.leave_over()
            }
            _ => Vec::new(),
        }
    }

    /// The drag carried from here was released over `device`, `local` being
    /// this device. Released where it is offered, it waits for that
    /// device's answer (this device's app's, here); elsewhere it ends, and
    /// its source is told.
    pub(crate) fn remote_released(&mut self, device: &DeviceId, local: &DeviceId) -> Vec<Effect> {
        let Some(remote) = self.remote.as_mut() else {
            return Vec::new();
        };
        if remote.dropped {
            return Vec::new();
        }
        let held = match &remote.over {
            Some(Over::Here) => device == local,
            Some(Over::Device(over)) => over == device,
            None => false,
        };
        if !held {
            return self.end_remote();
        }
        if remote.over == Some(Over::Here) {
            // The offer shown here stands until this device's app answers.
            self.remote = None;
        } else {
            remote.dropped = true;
        }
        Vec::new()
    }

    /// `from` accepted drag `id`, carried from here on its source's
    /// behalf: the source sends the files there. `None` if it is no such
    /// drag.
    pub(crate) fn remote_accepted(
        &mut self,
        from: &DeviceId,
        id: &str,
        now: Instant,
    ) -> Option<Vec<Effect>> {
        let remote = self.remote.as_ref()?;
        if remote.offer.drag_id != id || remote.over != Some(Over::Device(from.clone())) {
            return None;
        }
        let source = remote.source.clone();
        self.remote = None;
        // The source may refuse to send them, which is passed on.
        self.forwarded.note(
            Forwarded {
                source: source.clone(),
                id: id.to_owned(),
                target: from.clone(),
            },
            now,
        );
        Some(vec![Effect::Send(
            source,
            control::Msg::DragAccept(DragAccept {
                drag_id: id.to_owned(),
                device_id: from.to_string(),
            }),
        )])
    }

    /// `from` declined drag `id`, carried from here on its source's
    /// behalf. Dropped there, it ends; still going on, it may yet be
    /// dropped elsewhere. `None` if it is no such drag.
    pub(crate) fn remote_declined(&mut self, from: &DeviceId, id: &str) -> Option<Vec<Effect>> {
        let remote = self.remote.as_mut()?;
        if remote.offer.drag_id != id || remote.over != Some(Over::Device(from.clone())) {
            return None;
        }
        if !remote.dropped {
            remote.over = None;
            return Some(Vec::new());
        }
        let source = remote.source.clone();
        self.remote = None;
        Some(vec![decline(&source, id)])
    }

    /// The source of drag `id`, carried from here, ended it there: before
    /// it was dropped, or refusing to send it where it was accepted, which
    /// is told at once. `None` if it is no such drag.
    pub(crate) fn remote_cancelled(&mut self, from: &DeviceId, id: &str) -> Option<Vec<Effect>> {
        if self
            .remote
            .as_ref()
            .is_some_and(|r| r.source == *from && r.offer.drag_id == id)
        {
            return Some(self.drop_remote());
        }
        let forwarded = self.forwarded.take(|f| f.source == *from && f.id == id)?;
        Some(vec![cancel(&forwarded.target, id)])
    }

    /// Ends whatever has lasted [`DRAG_EXPIRY`] by `now`.
    pub(crate) fn expire(&mut self, now: Instant) -> Vec<Effect> {
        let lapsed = |at: Instant| now.saturating_duration_since(at) >= DRAG_EXPIRY;
        let mut effects = Vec::new();
        if self.outgoing.as_ref().is_some_and(|d| lapsed(d.began)) {
            effects.extend(self.give_up(DragEnd::Expired));
        }
        if self.remote.as_ref().is_some_and(|r| lapsed(r.began)) {
            effects.extend(self.end_remote());
        }
        self.forwarded.lapse(now);
        self.accepted.lapse(now);
        let (gone, kept): (Vec<Dropped>, Vec<Dropped>) =
            self.dropped.drain(..).partition(|d| lapsed(d.at));
        self.dropped = kept;
        for dropped in gone {
            effects.push(ended(&dropped.id, DragEnd::Expired, Some(&dropped.device)));
        }
        let (gone, kept): (Vec<Offered>, Vec<Offered>) =
            self.offered.drain(..).partition(|o| lapsed(o.at));
        self.offered = kept;
        for offered in gone {
            effects.push(decline(&offered.from, &offered.offer.drag_id));
            if offered.shown {
                effects.push(ended(
                    &offered.offer.drag_id,
                    DragEnd::Expired,
                    Some(&offered.from),
                ));
            }
        }
        effects
    }

    /// Whether anything is waiting to lapse.
    pub(crate) fn pending(&self) -> bool {
        self.outgoing.is_some()
            || !self.dropped.is_empty()
            || !self.offered.is_empty()
            || self.remote.is_some()
            || !self.forwarded.is_empty()
            || !self.accepted.is_empty()
    }

    /// Drops the offers `which` picks, telling the app of those it showed.
    fn drop_offers(&mut self, which: impl Fn(&Offered) -> bool, reason: DragEnd) -> Vec<Effect> {
        let (gone, kept): (Vec<Offered>, Vec<Offered>) =
            self.offered.drain(..).partition(|o| which(o));
        self.offered = kept;
        gone.into_iter()
            .filter(|o| o.shown)
            .map(|o| ended(&o.offer.drag_id, reason, Some(&o.from)))
            .collect()
    }
}

/// The app hears that `from` offers `offer` here, its files on `source`.
fn arrived(from: &DeviceId, source: &DeviceId, offer: &DragOffer, at_controller: bool) -> Effect {
    Effect::Event(DragEvent::Arrived(ArrivedDrag {
        id: offer.drag_id.clone(),
        from: from.to_string(),
        at_controller,
        source: (source != from).then(|| source.to_string()),
        items: offer
            .items
            .iter()
            .map(|item| DraggedItem {
                name: item.name.clone(),
                folder: item.folder,
                size: item.size,
                files: item.files,
            })
            .collect(),
        files: offer.files,
        bytes: offer.bytes,
    }))
}

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::super::listing::{Entry, Item};
    use super::*;

    fn id(s: &str) -> DeviceId {
        DeviceId::new(s)
    }

    fn manifest() -> Arc<Manifest> {
        Arc::new(Manifest {
            items: vec![
                Item {
                    name: "Photos".to_owned(),
                    path: PathBuf::from("/u/Photos"),
                    kind: ItemKind::Folder {
                        entries: vec![
                            Entry {
                                relative: "Photos/a.jpg".to_owned(),
                                path: PathBuf::from("/u/Photos/a.jpg"),
                                size: 10,
                                folder: false,
                            },
                            Entry {
                                relative: "Photos/empty".to_owned(),
                                path: PathBuf::from("/u/Photos/empty"),
                                size: 0,
                                folder: true,
                            },
                        ],
                        failures: vec![],
                    },
                    detail: String::new(),
                },
                Item {
                    name: "link".to_owned(),
                    path: PathBuf::from("/u/link"),
                    kind: ItemKind::Failed("symlink"),
                    detail: String::new(),
                },
                Item {
                    name: "notes.txt".to_owned(),
                    path: PathBuf::from("/u/notes.txt"),
                    kind: ItemKind::File(5),
                    detail: String::new(),
                },
            ],
        })
    }

    /// The control messages among `effects`, by device.
    fn sent(effects: &[Effect]) -> Vec<(String, &'static str)> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Send(device, msg) => Some((
                    device.to_string(),
                    match msg {
                        control::Msg::DragOffer(_) => "offer",
                        control::Msg::DragAccept(_) => "accept",
                        control::Msg::DragDecline(_) => "decline",
                        control::Msg::DragCancel(_) => "cancel",
                        _ => "other",
                    },
                )),
                _ => None,
            })
            .collect()
    }

    fn events(effects: &[Effect]) -> Vec<DragEvent> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Event(event) => Some(event.clone()),
                _ => None,
            })
            .collect()
    }

    fn ended_as(reason: DragEnd, device: Option<&str>) -> DragEvent {
        DragEvent::Ended(EndedDrag {
            id: "d-1".to_owned(),
            reason,
            device: device.map(str::to_owned),
        })
    }

    #[test]
    fn offers_list_what_goes() {
        let offer = offer_of("d-1", &manifest());
        let names: Vec<(&str, bool, u64, u32)> = offer
            .items
            .iter()
            .map(|i| (i.name.as_str(), i.folder, i.size, i.files))
            .collect();
        assert_eq!(names, [("Photos", true, 10, 2), ("notes.txt", false, 5, 1)]);
        assert_eq!((offer.files, offer.bytes), (3, 15));
    }

    #[test]
    fn a_drag_follows_the_cursor_and_goes_where_it_is_accepted() {
        let now = Instant::now();
        let mut drags = Drags::default();
        drags.begin("d-1", manifest(), now);
        assert!(drags.active());
        assert_eq!(sent(&drags.entered(&id("win"))), [("win".into(), "offer")]);
        assert_eq!(drags.dragging().unwrap().device.as_deref(), Some("win"));
        // On to the mini: the PC's offer is taken back.
        assert_eq!(sent(&drags.left(&id("win"))), [("win".into(), "cancel")]);
        assert_eq!(
            sent(&drags.entered(&id("mini"))),
            [("mini".into(), "offer")]
        );
        assert!(drags.released(&id("mini"), &id("mac"), now).is_empty());
        assert!(!drags.active());
        // Only the device it was dropped on can take it.
        assert_eq!(
            sent(&drags.accepted_by(&id("win"), "d-1")),
            [("win".into(), "cancel")]
        );
        let accepted = drags.accepted_by(&id("mini"), "d-1");
        assert!(matches!(
            &accepted[0],
            Effect::Transfer { device, drag, .. } if *device == id("mini") && drag == "d-1"
        ));
        assert_eq!(
            events(&accepted),
            [ended_as(DragEnd::Accepted, Some("mini"))]
        );
        assert!(!drags.pending());
    }

    #[test]
    fn an_acceptance_after_files_were_turned_off_takes_the_offer_back() {
        let now = Instant::now();
        let mut drags = Drags::default();
        drags.begin("d-1", manifest(), now);
        drags.entered(&id("win"));
        drags.released(&id("win"), &id("mac"), now);
        let refused = drags.refused_to(&id("win"), "d-1");
        assert_eq!(sent(&refused), [("win".into(), "cancel")]);
        assert!(!refused.iter().any(|e| matches!(e, Effect::Transfer { .. })));
        assert_eq!(
            events(&refused),
            [ended_as(DragEnd::Withdrawn, Some("win"))]
        );
        assert!(!drags.pending());
        // Nothing left to take back: the device is only told.
        assert_eq!(
            sent(&drags.refused_to(&id("win"), "d-1")),
            [("win".into(), "cancel")]
        );
    }

    #[test]
    fn a_drag_released_here_or_declined_there_ends() {
        let now = Instant::now();
        let mut drags = Drags::default();
        drags.begin("d-1", manifest(), now);
        drags.entered(&id("win"));
        drags.left(&id("win"));
        // Back home and released: nothing takes it.
        let released = drags.released(&id("mac"), &id("mac"), now);
        assert_eq!(events(&released), [ended_as(DragEnd::Released, None)]);

        drags.begin("d-1", manifest(), now);
        drags.entered(&id("win"));
        drags.released(&id("win"), &id("mac"), now);
        assert_eq!(
            events(&drags.declined_by(&id("win"), "d-1")),
            [ended_as(DragEnd::Declined, Some("win"))]
        );

        // Ended by the app while still offered: taken back.
        drags.begin("d-1", manifest(), now);
        drags.entered(&id("win"));
        assert_eq!(drags.end("d-2").map(|e| e.len()), None);
        let ended = drags.end("d-1").unwrap();
        assert_eq!(sent(&ended), [("win".into(), "cancel")]);
        assert_eq!(events(&ended), [ended_as(DragEnd::Released, None)]);
    }

    #[test]
    fn offers_are_shown_while_their_source_controls_this_device() {
        let now = Instant::now();
        let mut drags = Drags::default();
        let offer = offer_of("d-1", &manifest());
        // Nobody to show it to.
        assert_eq!(
            sent(&drags.offered(
                &id("mac"),
                &id("mac"),
                offer.clone(),
                Some(&id("mac")),
                false,
                now
            )),
            [("mac".into(), "decline")]
        );
        // Ahead of the cursor: shown once it arrives.
        assert!(
            drags
                .offered(&id("mac"), &id("mac"), offer.clone(), None, true, now)
                .is_empty()
        );
        let shown = events(&drags.controller_entered(&id("mac")));
        assert!(
            matches!(&shown[..], [DragEvent::Arrived(a)] if a.id == "d-1" && a.from == "mac" && a.items.len() == 2)
        );
        assert!(drags.controller_entered(&id("mac")).is_empty());
        // Taken back by the source.
        assert_eq!(
            events(&drags.withdrawn(&id("mac"), "d-1")),
            [ended_as(DragEnd::Withdrawn, Some("mac"))]
        );
        assert_eq!(
            drags.accept("d-1", Instant::now()).map(|_| ()),
            Err(NotOffered)
        );
        // Dropped here.
        drags.offered(
            &id("mac"),
            &id("mac"),
            offer.clone(),
            Some(&id("mac")),
            true,
            now,
        );
        let (from, effects) = drags.accept("d-1", Instant::now()).unwrap();
        assert_eq!(from, id("mac"));
        assert_eq!(sent(&effects), [("mac".into(), "accept")]);
        // Or not.
        drags.offered(&id("mac"), &id("mac"), offer, Some(&id("mac")), true, now);
        assert_eq!(
            sent(&drags.decline("d-1").unwrap()),
            [("mac".into(), "decline")]
        );
        // Only the one accepted is kept, until it lapses, in case its
        // files are called off.
        assert!(drags.pending());
        assert!(drags.expire(Instant::now() + DRAG_EXPIRY).is_empty());
        assert!(!drags.pending());
    }

    #[test]
    fn drags_and_offers_lapse() {
        let now = Instant::now();
        let later = now + DRAG_EXPIRY;
        let mut drags = Drags::default();
        drags.begin("d-1", manifest(), now);
        drags.entered(&id("win"));
        drags.released(&id("win"), &id("mac"), now);
        drags.offered(
            &id("mini"),
            &id("mini"),
            offer_of("d-9", &manifest()),
            Some(&id("mini")),
            true,
            now,
        );
        assert!(drags.expire(now + Duration::from_secs(1)).is_empty());
        let lapsed = drags.expire(later);
        assert_eq!(sent(&lapsed), [("mini".into(), "decline")]);
        let reasons: Vec<DragEnd> = events(&lapsed)
            .into_iter()
            .filter_map(|e| match e {
                DragEvent::Ended(e) => Some(e.reason),
                _ => None,
            })
            .collect();
        assert_eq!(reasons, [DragEnd::Expired, DragEnd::Expired]);
        assert!(!drags.pending());
    }

    #[test]
    fn a_lost_device_takes_its_offers_along() {
        let now = Instant::now();
        let mut drags = Drags::default();
        drags.offered(
            &id("mac"),
            &id("mac"),
            offer_of("d-1", &manifest()),
            Some(&id("mac")),
            true,
            now,
        );
        assert_eq!(
            events(&drags.peer_down(&id("mac"))),
            [ended_as(DragEnd::Withdrawn, Some("mac"))]
        );
        assert!(!drags.pending());
    }

    fn transfers(effects: &[Effect]) -> Vec<String> {
        effects
            .iter()
            .filter_map(|e| match e {
                Effect::Transfer { device, drag, .. } => Some(format!("{drag} to {device}")),
                _ => None,
            })
            .collect()
    }

    /// The PC is controlled by the Mac, and its user drags there.
    fn reported_on_win() -> (Drags, Vec<Effect>) {
        let mut drags = Drags::default();
        let effects = drags.begin_controlled(
            "d-1",
            manifest(),
            &id("mac"),
            vec!["mini".to_owned()],
            Instant::now(),
        );
        (drags, effects)
    }

    #[test]
    fn a_drag_on_a_controlled_device_is_reported_and_goes_where_the_controller_says() {
        let (mut drags, effects) = reported_on_win();
        let [Effect::Send(to, control::Msg::RemoteDrag(report))] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        assert_eq!(*to, id("mac"));
        assert_eq!(report.drag_id, "d-1");
        assert_eq!(report.items.len(), 2);
        assert_eq!(report.linked_device_ids, ["mini"]);
        // On its way only once the cursor left with it.
        assert_eq!(drags.dragging().unwrap().device, None);
        drags.left_source(&id("mini"));
        assert_eq!(drags.dragging().unwrap().device, None);
        drags.left_source(&id("mac"));
        assert_eq!(drags.dragging().unwrap().device.as_deref(), Some("mac"));
        // Somebody else's word does not send it.
        assert!(
            drags
                .accepted_for(&id("mini"), "d-1", &id("mini"))
                .is_none()
        );
        let effects = drags.accepted_for(&id("mac"), "d-1", &id("mini")).unwrap();
        assert_eq!(transfers(&effects), ["d-1 to mini"]);
        assert!(!drags.active());

        // Dropped on the controller itself.
        let (mut drags, _) = reported_on_win();
        let effects = drags.accepted_by(&id("mac"), "d-1");
        assert_eq!(transfers(&effects), ["d-1 to mac"]);
    }

    #[test]
    fn a_drag_on_a_controlled_device_ends_as_its_controller_says() {
        // Declined where it was dropped.
        let (mut drags, _) = reported_on_win();
        assert_eq!(
            events(&drags.declined_by(&id("mac"), "d-1")),
            [ended_as(DragEnd::Declined, None)]
        );
        assert!(!drags.active());
        // Released back here, or nowhere.
        let (mut drags, _) = reported_on_win();
        assert!(drags.ended_by_controller(&id("mini"), "d-1").is_none());
        assert!(drags.ended_by_controller(&id("mac"), "d-2").is_none());
        assert_eq!(
            events(&drags.ended_by_controller(&id("mac"), "d-1").unwrap()),
            [ended_as(DragEnd::Released, None)]
        );
        // Ended here first: the controller is told.
        let (mut drags, _) = reported_on_win();
        assert_eq!(
            sent(&drags.end("d-1").unwrap()),
            [("mac".to_owned(), "cancel")]
        );
        // The controller gone.
        let (mut drags, _) = reported_on_win();
        assert_eq!(
            events(&drags.peer_down(&id("mac"))),
            [ended_as(DragEnd::Released, None)]
        );
        assert!(!drags.active());
    }

    fn report() -> RemoteDrag {
        let offer = offer_of("d-1", &manifest());
        RemoteDrag {
            drag_id: offer.drag_id,
            items: offer.items,
            files: offer.files,
            bytes: offer.bytes,
            linked_device_ids: vec![],
        }
    }

    /// The Mac controls the PC, whose user drags files there; the PC can
    /// send them to the mini.
    fn carrying() -> Drags {
        let mut drags = Drags::default();
        let linked = HashSet::from([id("mac"), id("mini")]);
        let effects = drags
            .remote_began(&id("win"), report(), linked, Instant::now())
            .unwrap();
        assert!(effects.is_empty());
        assert_eq!(drags.remote_source(), Some(&id("win")));
        drags
    }

    #[test]
    fn a_drag_carried_here_is_offered_to_this_devices_app() {
        let now = Instant::now();
        let mut drags = carrying();
        let effects = drags.remote_entered(&id("mac"), &id("mac"), false, true, now);
        let heard = events(&effects);
        let [DragEvent::Arrived(arrived)] = heard.as_slice() else {
            panic!("{effects:?}");
        };
        assert_eq!(arrived.from, "win");
        assert!(arrived.at_controller);
        assert!(drags.remote_released(&id("mac"), &id("mac")).is_empty());
        // Dropped: the PC sends the files here.
        let (source, effects) = drags.accept("d-1", Instant::now()).unwrap();
        assert_eq!(source, id("win"));
        assert_eq!(sent(&effects), [("win".to_owned(), "accept")]);
        assert!(drags.remote_source().is_none());

        // Taken away again before the drop: the app hears the offer ended.
        let mut drags = carrying();
        drags.remote_entered(&id("mac"), &id("mac"), false, true, now);
        let effects = drags.remote_entered(&id("win"), &id("mac"), true, true, now);
        assert_eq!(
            events(&effects),
            [DragEvent::Ended(EndedDrag {
                id: "d-1".to_owned(),
                reason: DragEnd::Withdrawn,
                device: Some("win".to_owned()),
            })]
        );
        assert!(drags.accept("d-1", Instant::now()).is_err());
        // No app here to take it: released here, it ends.
        let mut drags = carrying();
        assert!(
            drags
                .remote_entered(&id("mac"), &id("mac"), false, false, now)
                .is_empty()
        );
        assert_eq!(
            sent(&drags.remote_released(&id("mac"), &id("mac"))),
            [("win".to_owned(), "cancel")]
        );
        assert!(!drags.pending());
    }

    #[test]
    fn a_drag_carried_on_is_offered_for_its_source() {
        let now = Instant::now();
        let mut drags = carrying();
        let effects = drags.remote_entered(&id("mini"), &id("mac"), true, true, now);
        let [Effect::Send(to, control::Msg::DragOffer(offer))] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        assert_eq!(*to, id("mini"));
        assert_eq!(offer.source_device_id, "win");
        // Dropped there and accepted: the PC sends the files there.
        assert!(drags.remote_released(&id("mini"), &id("mac")).is_empty());
        assert!(
            drags
                .remote_accepted(&id("other"), "d-1", Instant::now())
                .is_none()
        );
        let effects = drags
            .remote_accepted(&id("mini"), "d-1", Instant::now())
            .unwrap();
        let [Effect::Send(to, control::Msg::DragAccept(accept))] = effects.as_slice() else {
            panic!("{effects:?}");
        };
        assert_eq!((to.as_str(), accept.device_id.as_str()), ("win", "mini"));
        assert!(drags.remote_source().is_none());

        // Declined there once dropped: the PC is told.
        let mut drags = carrying();
        drags.remote_entered(&id("mini"), &id("mac"), true, true, now);
        drags.remote_released(&id("mini"), &id("mac"));
        assert_eq!(
            sent(&drags.remote_declined(&id("mini"), "d-1").unwrap()),
            [("win".to_owned(), "decline")]
        );
        // Declined while still dragging: it may be dropped elsewhere.
        let mut drags = carrying();
        drags.remote_entered(&id("mini"), &id("mac"), true, true, now);
        assert!(
            drags
                .remote_declined(&id("mini"), "d-1")
                .unwrap()
                .is_empty()
        );
        assert!(drags.remote_source().is_some());
    }

    #[test]
    fn a_drag_carried_is_not_offered_where_its_files_cannot_go() {
        let now = Instant::now();
        // Not linked to the PC, or not taking files from here.
        let mut drags = carrying();
        assert!(
            drags
                .remote_entered(&id("far"), &id("mac"), true, true, now)
                .is_empty()
        );
        assert!(
            drags
                .remote_entered(&id("mini"), &id("mac"), false, true, now)
                .is_empty()
        );
        // Released there: nothing took it.
        assert_eq!(
            sent(&drags.remote_released(&id("mini"), &id("mac"))),
            [("win".to_owned(), "cancel")]
        );
    }

    #[test]
    fn a_drag_carried_back_to_its_source_and_released_ends_there() {
        let now = Instant::now();
        let mut drags = carrying();
        drags.remote_entered(&id("mini"), &id("mac"), true, true, now);
        // Leaving the mini takes the offer back there.
        assert_eq!(
            sent(&drags.remote_left(&id("mini"))),
            [("mini".to_owned(), "cancel")]
        );
        assert!(
            drags
                .remote_entered(&id("win"), &id("mac"), true, true, now)
                .is_empty()
        );
        assert_eq!(
            sent(&drags.remote_released(&id("win"), &id("mac"))),
            [("win".to_owned(), "cancel")]
        );
        assert!(!drags.pending());
    }

    #[test]
    fn a_drag_carried_ends_with_its_source() {
        let now = Instant::now();
        // Ended there: the offer elsewhere is taken back.
        let mut drags = carrying();
        drags.remote_entered(&id("mini"), &id("mac"), true, true, now);
        assert!(drags.remote_cancelled(&id("mini"), "d-1").is_none());
        assert_eq!(
            sent(&drags.remote_cancelled(&id("win"), "d-1").unwrap()),
            [("mini".to_owned(), "cancel")]
        );
        // The PC gone.
        let mut drags = carrying();
        drags.remote_entered(&id("mini"), &id("mac"), true, true, now);
        assert_eq!(
            sent(&drags.peer_down(&id("win"))),
            [("mini".to_owned(), "cancel")]
        );
        assert!(!drags.pending());
        // The mini gone once dropped there: the PC is told.
        let mut drags = carrying();
        drags.remote_entered(&id("mini"), &id("mac"), true, true, now);
        drags.remote_released(&id("mini"), &id("mac"));
        assert_eq!(
            sent(&drags.peer_down(&id("mini"))),
            [("win".to_owned(), "cancel")]
        );
        // Lapsed.
        let mut drags = carrying();
        assert_eq!(
            sent(&drags.expire(Instant::now() + DRAG_EXPIRY)),
            [("win".to_owned(), "cancel")]
        );
        assert!(!drags.pending());
    }

    #[test]
    fn a_drag_carried_and_dropped_waits_for_that_devices_answer() {
        let now = Instant::now();
        let mut drags = carrying();
        drags.remote_entered(&id("mini"), &id("mac"), true, true, now);
        drags.remote_released(&id("mini"), &id("mac"));
        // The cursor goes on: the drop stands, offered nowhere else.
        assert!(drags.remote_left(&id("mini")).is_empty());
        assert!(
            drags
                .remote_entered(&id("mac"), &id("mac"), false, true, now)
                .is_empty()
        );
        assert!(
            drags
                .remote_entered(&id("other"), &id("mac"), true, true, now)
                .is_empty()
        );
        assert!(drags.accept("d-1", Instant::now()).is_err());
        let effects = drags
            .remote_accepted(&id("mini"), "d-1", Instant::now())
            .unwrap();
        assert_eq!(sent(&effects), [("win".to_owned(), "accept")]);
    }

    #[test]
    fn a_source_refusing_to_send_is_passed_on_to_the_device_that_accepted() {
        let now = Instant::now();
        let mut drags = carrying();
        drags.remote_entered(&id("mini"), &id("mac"), true, true, now);
        drags.remote_released(&id("mini"), &id("mac"));
        drags
            .remote_accepted(&id("mini"), "d-1", Instant::now())
            .unwrap();
        assert!(drags.pending());
        // Only the source's word counts.
        assert!(drags.remote_cancelled(&id("mini"), "d-1").is_none());
        assert_eq!(
            sent(&drags.remote_cancelled(&id("win"), "d-1").unwrap()),
            [("mini".to_owned(), "cancel")]
        );
        assert!(!drags.pending());

        // The source gone instead: the target hears so on its own session
        // with it, files perhaps already there; nothing is sent from here.
        let mut drags = carrying();
        drags.remote_entered(&id("mini"), &id("mac"), true, true, now);
        drags.remote_released(&id("mini"), &id("mac"));
        drags.remote_accepted(&id("mini"), "d-1", now).unwrap();
        assert!(sent(&drags.peer_down(&id("win"))).is_empty());
        assert!(!drags.pending());

        // A refusal comes at once: later, the files are taken to be on
        // their way, and nothing is passed on.
        let mut drags = carrying();
        drags.remote_entered(&id("mini"), &id("mac"), true, true, now);
        drags.remote_released(&id("mini"), &id("mac"));
        drags.remote_accepted(&id("mini"), "d-1", now).unwrap();
        drags.expire(now + FORWARD_WINDOW);
        assert!(!drags.pending());
        assert!(drags.remote_cancelled(&id("win"), "d-1").is_none());
    }

    #[test]
    fn a_drag_accepted_here_whose_files_came_is_no_longer_called_off() {
        let now = Instant::now();
        let mut drags = Drags::default();
        drags.offered(
            &id("mac"),
            &id("win"),
            offer_of("d-1", &manifest()),
            Some(&id("mac")),
            true,
            now,
        );
        drags.accept("d-1", now).unwrap();
        // Still awaited: kept.
        drags.forget_arriving(|source, id| *source == DeviceId::new("win") && id == "d-1");
        assert!(drags.pending());
        // The files started coming: nothing to call off any more.
        drags.forget_arriving(|_, _| false);
        assert!(!drags.pending());
        assert!(events(&drags.peer_down(&id("win"))).is_empty());
        assert!(events(&drags.withdrawn(&id("mac"), "d-1")).is_empty());
    }

    #[test]
    fn a_drag_carried_here_is_held_like_any_offer() {
        let now = Instant::now();
        let mut drags = carrying();
        // Offers from as many devices as are kept already.
        for i in 0..MAX_OFFERS {
            drags.offered(
                &id(&format!("d{i}")),
                &id(&format!("d{i}")),
                offer_of(&format!("o-{i}"), &manifest()),
                Some(&id(&format!("d{i}"))),
                true,
                now,
            );
        }
        let heard = events(&drags.remote_entered(&id("mac"), &id("mac"), false, true, now));
        // The oldest goes, as for any offer.
        assert!(
            matches!(
                heard.as_slice(),
                [DragEvent::Ended(e), DragEvent::Arrived(a)] if e.id == "o-0" && a.id == "d-1"
            ),
            "{heard:?}"
        );
    }

    #[test]
    fn a_drag_brought_back_to_its_source_is_there_again() {
        let (mut drags, _) = reported_on_win();
        drags.left_source(&id("mac"));
        assert_eq!(drags.dragging().unwrap().device.as_deref(), Some("mac"));
        drags.back_on_source(&id("mini"));
        assert_eq!(drags.dragging().unwrap().device.as_deref(), Some("mac"));
        drags.back_on_source(&id("mac"));
        assert_eq!(drags.dragging().unwrap().device, None);
    }

    #[test]
    fn a_drag_accepted_here_and_called_off_is_told_to_the_app() {
        let now = Instant::now();
        let mut drags = Drags::default();
        drags.offered(
            &id("mac"),
            &id("win"),
            offer_of("d-1", &manifest()),
            Some(&id("mac")),
            true,
            now,
        );
        drags.accept("d-1", Instant::now()).unwrap();
        assert_eq!(
            events(&drags.withdrawn(&id("mac"), "d-1")),
            [ended_as(DragEnd::Withdrawn, Some("mac"))]
        );
        // Told once.
        assert!(events(&drags.withdrawn(&id("mac"), "d-1")).is_empty());
        // Or once its source is gone.
        drags.offered(
            &id("mac"),
            &id("win"),
            offer_of("d-2", &manifest()),
            Some(&id("mac")),
            true,
            now,
        );
        drags.accept("d-2", Instant::now()).unwrap();
        let heard = events(&drags.peer_down(&id("win")));
        assert!(
            matches!(heard.as_slice(), [DragEvent::Ended(e)] if e.id == "d-2"),
            "{heard:?}"
        );
    }

    #[test]
    fn an_earlier_offer_from_the_source_is_ended_when_its_drag_comes_back_here() {
        let now = Instant::now();
        let mut drags = carrying();
        // The PC, controlling this device a moment ago, had offered a drag.
        drags.offered(
            &id("win"),
            &id("win"),
            offer_of("d-0", &manifest()),
            Some(&id("win")),
            true,
            now,
        );
        let heard = events(&drags.remote_entered(&id("mac"), &id("mac"), false, true, now));
        assert!(
            matches!(
                heard.as_slice(),
                [DragEvent::Ended(e), DragEvent::Arrived(a)] if e.id == "d-0" && a.id == "d-1"
            ),
            "{heard:?}"
        );
    }

    #[test]
    fn an_offer_naming_its_source_brings_the_files_from_there() {
        let now = Instant::now();
        let mut drags = Drags::default();
        drags.offered(
            &id("mac"),
            &id("win"),
            offer_of("d-1", &manifest()),
            Some(&id("mac")),
            true,
            now,
        );
        let (source, effects) = drags.accept("d-1", Instant::now()).unwrap();
        assert_eq!(source, id("win"));
        // The answer goes to the device that offered it.
        assert_eq!(sent(&effects), [("mac".to_owned(), "accept")]);
        // Taken back: no files come from either.
        drags.offered(
            &id("mac"),
            &id("win"),
            offer_of("d-2", &manifest()),
            Some(&id("mac")),
            true,
            now,
        );
        let forgotten: Vec<String> = drags
            .withdrawn(&id("mac"), "d-2")
            .iter()
            .filter_map(|e| match e {
                Effect::Forget { from, .. } => Some(from.to_string()),
                _ => None,
            })
            .collect();
        assert_eq!(forgotten, ["mac", "win"]);
    }
}
