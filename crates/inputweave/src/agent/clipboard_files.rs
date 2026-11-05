//! Files copied on one device and pasted on another (docs/architecture.md
//! §9). The device they were copied on offers them as they are at the
//! top (`top_level`; on Windows the broker looks at them as the user,
//! `top_level_listed`), so moving the cursor never walks a large folder; the
//! device pasting them fetches them straight from it, the whole of them
//! listed only then, into a folder of their own (`Staging`), as a batch
//! that goes nowhere else, and an app pasting gets their paths there once
//! the batch has arrived (`await_files`); the app copies them on from
//! there.

use std::fmt::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use iw_core::layout::DeviceId;
use iw_proto::peer::v1::DragItem;
use tokio::sync::watch;
use tracing::{debug, info, warn};

use super::files::{Arrival, Files, kept_name};
use super::lazy_clipboard::FilesWait;
use super::listing::{Kind, Lister, MAX_ITEMS};
use super::private::private_dir;

/// Folders older than this are removed when the agent starts: left by an
/// agent that stopped before it could.
const STALE: Duration = Duration::from_secs(24 * 60 * 60);

/// How long pasted files may come without anything of them moving before
/// they are given up, once the first of them came.
pub(crate) const STALL: Duration = Duration::from_secs(2 * 60);

/// How long a device pasting files waits on the origin.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Patience {
    /// Once the first file came: how long nothing of them may move.
    pub(crate) stall: Duration,
    /// Before that: how long the origin may say nothing, listing them or
    /// waiting with them behind other files it sends this device. It says
    /// they still come every [`Patience::note_every`] while they wait.
    pub(crate) start: Duration,
    /// While this device checks one of them, writing it out and hashing
    /// it, which takes most of an hour for the largest file on a slow
    /// disk: how long that may take, so a broker that hangs without going
    /// away does not keep the paste waiting for ever.
    pub(crate) checking: Duration,
}

impl Patience {
    pub(crate) fn new(stall: Duration) -> Self {
        Self {
            stall,
            start: stall * 5,
            checking: stall * 30,
        }
    }

    /// How often an origin whose files wait their turn says they still
    /// come.
    pub(crate) fn note_every(&self) -> Duration {
        self.stall / 4
    }
}

impl Default for Patience {
    fn default() -> Self {
        Self::new(STALL)
    }
}

/// Where pasted files are fetched into, one folder per offer, and how long
/// a folder stays once its promise left the clipboard: an app may still be
/// copying from it.
#[derive(Clone, Debug)]
pub(crate) struct Staging {
    pub(crate) dir: PathBuf,
    pub(crate) grace: Duration,
}

impl Staging {
    /// The folder for the files of offer `offer_id` from `origin`: both in
    /// hex, so no id another device picks names anything else, and no two
    /// devices' offers share one.
    pub(crate) fn folder(&self, origin: &DeviceId, offer_id: u64) -> PathBuf {
        let mut name = String::with_capacity(origin.as_str().len() * 2 + 17);
        for b in origin.as_str().bytes() {
            let _ = write!(name, "{b:02x}");
        }
        let _ = write!(name, "-{offer_id:016x}");
        self.dir.join(name)
    }

    /// Makes the folder for offer `offer_id` from `origin`, which only this
    /// user can open. It reads the disk: call it off the async runtime.
    pub(crate) fn make(&self, origin: &DeviceId, offer_id: u64) -> std::io::Result<PathBuf> {
        let folder = self.folder(origin, offer_id);
        private_dir(&self.dir)?;
        private_dir(&folder)?;
        Ok(folder)
    }

    /// Removes the folder of offer `offer_id` from `origin` once the grace
    /// time passed.
    pub(crate) fn remove_later(&self, origin: &DeviceId, offer_id: u64) {
        let (folder, grace) = (self.folder(origin, offer_id), self.grace);
        tokio::spawn(async move {
            tokio::time::sleep(grace).await;
            let removed = tokio::task::spawn_blocking(move || remove(&folder)).await;
            if let Ok(Err(e)) = removed {
                warn!(error = %e, "cannot remove the folder pasted files were fetched into");
            }
        });
    }

    /// Removes folders left from earlier runs, older than a day.
    pub(crate) fn clean_old(&self) {
        let dir = self.dir.clone();
        tokio::task::spawn_blocking(move || {
            let Ok(entries) = std::fs::read_dir(&dir) else {
                return;
            };
            let now = SystemTime::now();
            for entry in entries.flatten() {
                let old = entry
                    .metadata()
                    .and_then(|m| m.modified())
                    .is_ok_and(|at| now.duration_since(at).is_ok_and(|age| age > STALE));
                if old && let Err(e) = remove(&entry.path()) {
                    warn!(error = %e, "cannot remove an old folder of pasted files");
                }
            }
        });
    }
}

/// Removes `path`, a folder, without following links out of it.
fn remove(path: &Path) -> std::io::Result<()> {
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.is_dir() => std::fs::remove_dir_all(path),
        Ok(_) => std::fs::remove_file(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e),
    }
}

/// The files and folders at `paths` as they are offered, with their paths,
/// at most a batch's items: a file with its size, a folder with nothing
/// counted yet, as its files are listed only when they are pasted. Links,
/// what is neither a file nor a folder, and what a receiver would not
/// write under its name ([`kept_name`]) are left out, as they would fail
/// or come under another name then. It looks at each only, so it is quick,
/// but still reads the disk: call it off the async runtime.
pub(crate) fn top_level(paths: &[PathBuf]) -> Vec<(PathBuf, DragItem)> {
    paths
        .iter()
        .filter_map(|path| {
            let kind = match std::fs::symlink_metadata(path) {
                Ok(meta) if meta.file_type().is_symlink() => Kind::Link,
                Ok(meta) if meta.is_file() => Kind::File(meta.len()),
                Ok(meta) if meta.is_dir() => Kind::Folder,
                Ok(_) => Kind::Other,
                Err(_) => Kind::Missing,
            };
            offered(path, &kind)
        })
        .take(MAX_ITEMS)
        .collect()
}

/// How long looking at copied files through the broker may take as the
/// clipboard moves ([`top_level_listed`]); what is not answered by then is
/// offered by name only, so the rest of the clipboard is never held up
/// longer.
pub(crate) const LIST_BUDGET: Duration = Duration::from_secs(3);

/// Paths asked about at once.
const LISTED_AT_ONCE: usize = 8;

/// [`top_level`], with `lister` looking at the paths, at most
/// [`LISTED_AT_ONCE`] at a time and for at most `budget` in all: on
/// Windows the broker, as the user, as the agent never opens what the user
/// copied. What it could not tell, cannot read, or did not answer in time
/// is offered by name only ([`DragItem::unknown`]), and what is sent when
/// it is pasted says what it is; once the lister is gone, nothing more is
/// asked.
pub(crate) async fn top_level_listed<L>(
    lister: L,
    paths: Vec<PathBuf>,
    budget: Duration,
) -> Vec<(PathBuf, DragItem)>
where
    L: Lister + Clone + Send + Sync + 'static,
{
    let paths: Vec<PathBuf> = paths.into_iter().take(MAX_ITEMS).collect();
    let mut kinds: Vec<Option<Kind>> = vec![None; paths.len()];
    let deadline = tokio::time::Instant::now() + budget;
    let mut asking = tokio::task::JoinSet::new();
    let (mut next, mut gone) = (0, false);
    loop {
        while !gone && asking.len() < LISTED_AT_ONCE && next < paths.len() {
            let (lister, path, index) = (lister.clone(), paths[next].clone(), next);
            asking.spawn(async move { (index, lister.list(&path, 0).await) });
            next += 1;
        }
        let Ok(Some(joined)) = tokio::time::timeout_at(deadline, asking.join_next()).await else {
            break;
        };
        let Ok((index, listed)) = joined else {
            continue;
        };
        match listed {
            Ok(listing) => kinds[index] = listing.kind,
            Err(e) if e.kind() == std::io::ErrorKind::NotConnected => gone = true,
            // By its kind only: what it says may name the user's files.
            Err(e) => debug!(kind = ?e.kind(), "cannot look at a copied file; offering it by name"),
        }
    }
    asking.abort_all();
    let unanswered = kinds.iter().filter(|k| k.is_none()).count();
    if unanswered > 0 {
        debug!(
            unanswered,
            "offering copied files not looked at by name only"
        );
    }
    paths
        .iter()
        .zip(kinds)
        .filter_map(|(path, kind)| offered(path, &kind.unwrap_or(Kind::Unreadable)))
        .collect()
}

/// The item a copied file or folder at `path`, of `kind`, is offered as,
/// unless it is left out. One that could not be read is offered by name
/// only.
fn offered(path: &Path, kind: &Kind) -> Option<(PathBuf, DragItem)> {
    let name = path.file_name()?.to_str()?.to_owned();
    if !kept_name(&name) {
        // Without the name, which the agent's log never has.
        info!(
            bytes = name.len(),
            "leaving out a copied file whose name other devices would not keep"
        );
        return None;
    }
    let item = match kind {
        Kind::File(size) => DragItem {
            name,
            folder: false,
            size: *size,
            files: 1,
            unknown: false,
        },
        Kind::Folder => DragItem {
            name,
            folder: true,
            size: 0,
            files: 0,
            unknown: false,
        },
        Kind::Unreadable => DragItem {
            name,
            folder: false,
            size: 0,
            files: 1,
            unknown: true,
        },
        Kind::Link | Kind::Other | Kind::Missing => {
            debug!(
                bytes = path.as_os_str().len(),
                "leaving out a copied link, or what is neither a file nor a folder"
            );
            return None;
        }
    };
    Some((path.to_owned(), item))
}

/// Where each item promised here (`promised`, by its place in the offer)
/// is once the items the origin sends (`items`) have all ended: those that
/// arrived; `None` while any is still coming. Once the batch was declined
/// here as a whole, items none of whose files came are not waited for.
pub(crate) fn arrived(
    arrival: &Arrival,
    items: &[u32],
    promised: &[u32],
) -> Option<Vec<Option<PathBuf>>> {
    let ended = |index: &u32| {
        arrival.ended.contains_key(index) || (arrival.refused && !arrival.listed.contains(index))
    };
    items.iter().all(ended).then(|| {
        promised
            .iter()
            .map(|index| arrival.ended.get(index).cloned().flatten())
            .collect()
    })
}

/// Of the items an origin says it sends, those promised here (`promised`,
/// by their place in the offer), if what it says is sound: each item
/// once.
pub(crate) fn sent_items(items: &[u32], promised: &[u32]) -> Option<Vec<u32>> {
    let mut sent = items.to_vec();
    sent.sort_unstable();
    sent.dedup();
    if sent.len() != items.len() {
        return None;
    }
    sent.retain(|index| promised.contains(index));
    Some(sent)
}

/// Waits for the files of offer `offer_id` from `origin` promised here
/// (`promised`, by their place in the offer) to arrive: once the origin
/// said which of them it sends (`expected`), until each of them ended
/// (`arrival`). Tells `wait` where they are, or that none came: when the
/// origin refused or went away, or the promise ended (`expected` dropped),
/// or the origin was not heard of for as long as `patience` allows
/// (longer while this device checks one of them), when what is still
/// coming is cancelled. Returns where they are when they arrived after an
/// app gave up waiting for them.
pub(crate) async fn await_files(
    files: Arc<Files>,
    (origin, offer_id): (DeviceId, u64),
    promised: Vec<u32>,
    (mut arrival, mut expected): (watch::Receiver<Arrival>, watch::Receiver<Option<Vec<u32>>>),
    wait: Arc<FilesWait>,
    patience: Patience,
) -> Option<Vec<Option<PathBuf>>> {
    loop {
        // Both seen, so `changed` waits for what comes next.
        let (paths, stalled) = {
            let arrival = arrival.borrow_and_update();
            let items = expected.borrow_and_update();
            let paths = items
                .as_ref()
                .and_then(|items| arrived(&arrival, items, &promised));
            let allowed = if arrival.checking > 0 {
                patience.checking
            } else if arrival.started {
                patience.stall
            } else {
                patience.start
            };
            (paths, arrival.progressed + allowed)
        };
        if let Some(paths) = paths {
            debug!(%origin, arrived = paths.iter().flatten().count(), "pasted files arrived");
            let late = wait.arrived(Some(paths.clone()));
            return late.then_some(paths);
        }
        tokio::select! {
            changed = arrival.changed() => if changed.is_err() {
                break;
            },
            changed = expected.changed() => if changed.is_err() {
                break;
            },
            () = tokio::time::sleep_until(tokio::time::Instant::from_std(stalled)) => {
                debug!(%origin, "nothing of the pasted files moved for a while");
                break;
            }
        }
    }
    files.cancel_clipboard(&origin, offer_id);
    wait.arrived(None);
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What the fake lister says of a path.
    #[derive(Clone)]
    enum Says {
        Kind(Kind),
        Fails(std::io::ErrorKind),
        Never,
    }

    /// A lister that says what it is told of each path, and counts how many
    /// it is asked about at once and in all.
    #[derive(Clone, Default)]
    struct Told {
        says: Arc<std::collections::HashMap<PathBuf, Says>>,
        asked: Arc<std::sync::atomic::AtomicUsize>,
        at_once: Arc<std::sync::atomic::AtomicUsize>,
        most_at_once: Arc<std::sync::atomic::AtomicUsize>,
    }

    impl Lister for Told {
        async fn list(
            &self,
            path: &Path,
            _max: usize,
        ) -> std::io::Result<super::super::listing::Listing> {
            use std::sync::atomic::Ordering::SeqCst;

            self.asked.fetch_add(1, SeqCst);
            let now = self.at_once.fetch_add(1, SeqCst) + 1;
            self.most_at_once.fetch_max(now, SeqCst);
            tokio::time::sleep(Duration::from_millis(5)).await;
            let says = self
                .says
                .get(path)
                .cloned()
                .unwrap_or(Says::Kind(Kind::Missing));
            let answer = match says {
                Says::Kind(kind) => Ok(super::super::listing::Listing {
                    kind: Some(kind),
                    ..Default::default()
                }),
                Says::Fails(kind) => Err(std::io::Error::new(kind, "it failed")),
                Says::Never => std::future::pending().await,
            };
            self.at_once.fetch_sub(1, SeqCst);
            answer
        }
    }

    fn told(says: Vec<(&str, Says)>) -> (Told, Vec<PathBuf>) {
        let paths: Vec<PathBuf> = says.iter().map(|(p, _)| PathBuf::from(p)).collect();
        let told = Told {
            says: Arc::new(
                says.into_iter()
                    .map(|(p, s)| (PathBuf::from(p), s))
                    .collect(),
            ),
            ..Told::default()
        };
        (told, paths)
    }

    fn named(offered: &[(PathBuf, DragItem)]) -> Vec<(String, bool, bool)> {
        offered
            .iter()
            .map(|(_, item)| (item.name.clone(), item.folder, item.unknown))
            .collect()
    }

    #[tokio::test]
    async fn what_the_broker_cannot_tell_is_offered_by_name_only() {
        let (lister, paths) = told(vec![
            ("/u/report.txt", Says::Kind(Kind::File(12))),
            ("/u/photos", Says::Kind(Kind::Folder)),
            ("/u/locked", Says::Kind(Kind::Unreadable)),
            ("/u/slow", Says::Fails(std::io::ErrorKind::TimedOut)),
            ("/u/gone", Says::Kind(Kind::Missing)),
            ("/u/link", Says::Kind(Kind::Link)),
        ]);
        let offered = top_level_listed(lister, paths, Duration::from_secs(5)).await;
        assert_eq!(
            named(&offered),
            vec![
                ("report.txt".to_owned(), false, false),
                ("photos".to_owned(), true, false),
                ("locked".to_owned(), false, true),
                ("slow".to_owned(), false, true),
            ]
        );
        assert_eq!(offered[0].1.size, 12);
        assert_eq!(offered[2].1.size, 0);
    }

    #[tokio::test]
    async fn a_slow_broker_holds_the_offer_up_no_longer_than_its_budget() {
        let mut says: Vec<(String, Says)> = (0..30)
            .map(|i| (format!("/u/{i}.txt"), Says::Kind(Kind::File(1))))
            .collect();
        says.insert(3, ("/u/stuck.txt".to_owned(), Says::Never));
        let (lister, paths) = told(says.iter().map(|(p, s)| (p.as_str(), s.clone())).collect());
        let started = std::time::Instant::now();
        let offered = top_level_listed(lister.clone(), paths, Duration::from_millis(400)).await;
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "{:?}",
            started.elapsed()
        );
        // Asked a few at a time, the one that never answers offered by name.
        let most = lister
            .most_at_once
            .load(std::sync::atomic::Ordering::SeqCst);
        assert!(most <= LISTED_AT_ONCE, "{most} at once");
        assert_eq!(offered.len(), 31);
        assert!(
            offered
                .iter()
                .all(|(path, item)| item.unknown == path.ends_with("stuck.txt"))
        );
    }

    #[tokio::test]
    async fn nothing_more_is_asked_once_the_broker_is_gone() {
        let mut says = vec![(
            "/u/first.txt",
            Says::Fails(std::io::ErrorKind::NotConnected),
        )];
        let rest: Vec<String> = (0..40).map(|i| format!("/u/{i}.txt")).collect();
        says.extend(rest.iter().map(|p| (p.as_str(), Says::Kind(Kind::File(1)))));
        let (lister, paths) = told(says);
        let offered = top_level_listed(lister.clone(), paths, Duration::from_secs(5)).await;
        // Those already asked when it went may answer; the rest are not
        // asked, and all go by name only.
        let asked = lister.asked.load(std::sync::atomic::Ordering::SeqCst);
        assert!(asked <= 2 * LISTED_AT_ONCE, "{asked} asked");
        assert_eq!(offered.len(), 41);
        assert!(offered[0].1.unknown);
        assert!(offered.iter().filter(|(_, item)| item.unknown).count() >= 41 - asked);
    }

    #[test]
    fn files_arrive_once_every_item_sent_ended() {
        let mut arrival = Arrival::new();
        arrival
            .ended
            .insert(0, Some(PathBuf::from("/staging/item 0")));
        // Item 1 is not sent; item 2 still comes.
        assert_eq!(arrived(&arrival, &[0, 2], &[0, 1, 2]), None);
        arrival.ended.insert(2, None);
        assert_eq!(
            arrived(&arrival, &[0, 2], &[0, 1, 2]),
            Some(vec![Some(PathBuf::from("/staging/item 0")), None, None])
        );
        // An item said to be sent but not ended yet.
        assert_eq!(arrived(&arrival, &[0, 1], &[0, 1, 2]), None);
        assert_eq!(
            arrived(&Arrival::new(), &[], &[0, 1, 2]),
            Some(vec![None; 3])
        );
        // Item 1 was not promised here: where the others are, in the order
        // they were promised.
        assert_eq!(
            arrived(&arrival, &[0, 2], &[0, 2]),
            Some(vec![Some(PathBuf::from("/staging/item 0")), None])
        );
    }

    #[test]
    fn a_batch_declined_as_a_whole_is_not_waited_for_beyond_what_came() {
        let mut arrival = Arrival::new();
        arrival.ended.insert(0, Some(PathBuf::from("/staging/a")));
        // Item 1 has files under way, item 2 none.
        arrival.listed.extend([0, 1]);
        arrival.refused = true;
        assert_eq!(arrived(&arrival, &[0, 1, 2], &[0, 1, 2]), None);
        arrival.ended.insert(1, None);
        assert_eq!(
            arrived(&arrival, &[0, 1, 2], &[0, 1, 2]),
            Some(vec![Some(PathBuf::from("/staging/a")), None, None])
        );
    }

    #[test]
    fn only_items_promised_are_waited_for() {
        assert_eq!(sent_items(&[2, 0], &[0, 1, 2]), Some(vec![0, 2]));
        // Item 3 was left out here, item 1 by the origin.
        assert_eq!(sent_items(&[3, 0, 2], &[0, 1, 2]), Some(vec![0, 2]));
        assert_eq!(sent_items(&[1, 1], &[0, 1, 2]), None);
        assert_eq!(sent_items(&[], &[0, 1, 2]), Some(Vec::new()));
    }

    #[test]
    fn origins_are_given_time_to_list_and_wait_their_turn() {
        let patience = Patience::default();
        assert_eq!(patience.stall, STALL);
        assert!(patience.start > patience.stall);
        // Said often enough that one note lost is not a stall.
        assert!(patience.note_every() * 2 < patience.stall);
    }

    #[test]
    fn offers_look_at_the_top_only() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("report.txt");
        std::fs::write(&file, b"twelve bytes").unwrap();
        let folder = dir.path().join("photos");
        std::fs::create_dir_all(folder.join("deep/down")).unwrap();
        std::fs::write(folder.join("deep/down/a.jpg"), b"a").unwrap();
        let missing = dir.path().join("gone");
        #[cfg_attr(not(unix), allow(unused_mut))]
        let mut paths = vec![file.clone(), folder.clone(), missing];
        #[cfg(unix)]
        {
            let link = dir.path().join("link");
            std::os::unix::fs::symlink(&file, &link).unwrap();
            paths.push(link);
        }
        // Names a receiver would not keep: 100 characters but 304 bytes,
        // which APFS takes, and a backslash, which Windows reads as a
        // separator.
        for name in [format!("{}.txt", "長".repeat(100)), "a\\b.txt".to_owned()] {
            let path = dir.path().join(name);
            if std::fs::write(&path, b"left out").is_ok() {
                paths.push(path);
            }
        }
        let offered = top_level(&paths);
        assert_eq!(
            offered,
            vec![
                (
                    file,
                    DragItem {
                        name: "report.txt".to_owned(),
                        folder: false,
                        size: 12,
                        files: 1,
                        unknown: false,
                    }
                ),
                (
                    folder,
                    DragItem {
                        name: "photos".to_owned(),
                        folder: true,
                        size: 0,
                        files: 0,
                        unknown: false,
                    }
                ),
            ]
        );
    }

    #[test]
    fn folders_are_named_by_the_origin_and_offer_and_private() {
        let dir = tempfile::tempdir().unwrap();
        let staging = Staging {
            dir: dir.path().join("Clipboard"),
            grace: Duration::ZERO,
        };
        let mac = DeviceId::new("mac");
        let folder = staging.make(&mac, 0x2a).unwrap();
        assert_eq!(folder, dir.path().join("Clipboard/6d6163-000000000000002a"));
        assert!(folder.is_dir());
        // Made again, it is the same folder; the same offer id from another
        // device is another.
        assert_eq!(staging.make(&mac, 0x2a).unwrap(), folder);
        let other = staging.make(&DeviceId::new("win"), 0x2a).unwrap();
        assert_ne!(other, folder);
        // Even from a device whose id looks like a path.
        assert_eq!(
            staging.folder(&DeviceId::new(".."), 1).parent(),
            Some(staging.dir.as_path())
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            for dir in [&staging.dir, &folder, &other] {
                let mode = std::fs::metadata(dir).unwrap().permissions().mode();
                assert_eq!(mode & 0o777, 0o700, "{}", dir.display());
            }
        }
        remove(&folder).unwrap();
        assert!(!folder.exists());
        assert!(other.exists());
        remove(&folder).unwrap();
    }
}
