//! The folders macOS protects (Files & Folders) that files are sent from
//! and saved in: Downloads, Desktop and Documents. macOS asks the user the first time a process opens one, and there
//! is no asking it what the user decided without that, so the agent lists
//! each one when the app asks it to (`RequestFolderAccess`: first-run
//! setup's "Allow Access", Troubleshooting's "Check Again"), rather than in
//! the middle of a transfer or a drag.
//!
//! The agent never lists them by itself, not even as it starts: macOS
//! forgets what the user decided when the record is reset or the app is
//! signed differently, and would then ask out of nowhere. What it said last
//! is kept in `devices.toml` (`[folder_access]`) and shown until the app
//! asks again.
//!
//! Folders on network shares and removable volumes are asked about only
//! when a transfer uses them: there is none to list ahead.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use iw_proto::app::v1 as app;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;
use tracing::info;

use super::directory::Directory;
use super::engine::EngineEvent;
use crate::store::FolderAccessFile;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UserFolder {
    Downloads,
    Desktop,
    Documents,
}

impl UserFolder {
    pub fn name(self) -> &'static str {
        match self {
            Self::Downloads => "Downloads",
            Self::Desktop => "Desktop",
            Self::Documents => "Documents",
        }
    }

    pub fn to_proto(self) -> app::UserFolder {
        match self {
            Self::Downloads => app::UserFolder::Downloads,
            Self::Desktop => app::UserFolder::Desktop,
            Self::Documents => app::UserFolder::Documents,
        }
    }

    pub fn from_proto(folder: app::UserFolder) -> Option<Self> {
        match folder {
            app::UserFolder::Downloads => Some(Self::Downloads),
            app::UserFolder::Desktop => Some(Self::Desktop),
            app::UserFolder::Documents => Some(Self::Documents),
            app::UserFolder::Unspecified => None,
        }
    }
}

/// What macOS said of a folder. Written in `devices.toml` in lowercase.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Access {
    /// Never asked about on this Mac.
    Unknown,
    /// Being listed; macOS may be asking the user.
    Asking,
    Granted,
    Denied,
    /// The folder is not there.
    Missing,
    /// Listing it failed for another reason.
    Failed,
}

impl Access {
    pub fn name(self) -> &'static str {
        match self {
            Self::Unknown => "not asked",
            Self::Asking => "asking",
            Self::Granted => "granted",
            Self::Denied => "denied",
            Self::Missing => "missing",
            Self::Failed => "failed",
        }
    }

    pub fn to_proto(self) -> app::FolderAccessState {
        match self {
            Self::Unknown => app::FolderAccessState::Unknown,
            Self::Asking => app::FolderAccessState::Asking,
            Self::Granted => app::FolderAccessState::Granted,
            Self::Denied => app::FolderAccessState::Denied,
            Self::Missing => app::FolderAccessState::Missing,
            Self::Failed => app::FolderAccessState::Failed,
        }
    }

    pub fn from_proto(state: app::FolderAccessState) -> Self {
        match state {
            app::FolderAccessState::Unknown => Self::Unknown,
            app::FolderAccessState::Asking => Self::Asking,
            app::FolderAccessState::Granted => Self::Granted,
            app::FolderAccessState::Denied => Self::Denied,
            app::FolderAccessState::Missing => Self::Missing,
            app::FolderAccessState::Failed => Self::Failed,
        }
    }
}

/// The folders, in the order they are asked about, and what macOS said of
/// each, as `Status` has them. No folders where there are none to ask for.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct FolderAccess {
    pub folders: Vec<(UserFolder, Access)>,
    /// What macOS said of every one of them is known.
    pub asked: bool,
    /// Counts the rounds of listing started since the agent did; the
    /// states are this round's.
    pub round: u64,
}

impl FolderAccess {
    pub(crate) fn to_proto(&self) -> Vec<app::FolderAccess> {
        self.folders
            .iter()
            .map(|&(folder, access)| app::FolderAccess {
                folder: folder.to_proto().into(),
                state: access.to_proto().into(),
            })
            .collect()
    }
}

/// The folders to ask for and where they are: `paths::user_folders`.
pub type UserFolders = Vec<(UserFolder, PathBuf)>;

/// The state before this agent listed anything: what macOS said last time,
/// as kept in `devices.toml`.
pub(crate) fn initial(folders: &UserFolders, kept: &FolderAccessFile) -> FolderAccess {
    let folders: Vec<_> = folders
        .iter()
        .map(|&(folder, _)| (folder, kept.get(folder).unwrap_or(Access::Unknown)))
        .collect();
    FolderAccess {
        asked: !folders.is_empty() && folders.iter().all(|&(_, a)| a != Access::Unknown),
        folders,
        round: 0,
    }
}

/// Lists the folders when asked to, off the runtime, since macOS keeps a
/// listing waiting while it asks the user, and tells the engine what came
/// of each.
pub(crate) struct FolderAsker {
    folders: UserFolders,
    directory: Arc<Directory>,
    reported: Reported,
}

impl FolderAsker {
    pub(crate) fn new(
        folders: UserFolders,
        initial: FolderAccess,
        directory: Arc<Directory>,
        engine: mpsc::UnboundedSender<EngineEvent>,
    ) -> Arc<Self> {
        Arc::new(Self {
            folders,
            directory,
            reported: Reported {
                state: Mutex::new(Round {
                    access: initial,
                    busy: false,
                }),
                engine,
            },
        })
    }

    /// Starts a round: shows every folder as being asked about, then lists
    /// them one after the other, so macOS asks about one at a time. While a
    /// round is going on already, joins it. Returns the round.
    pub(crate) fn ask(self: &Arc<Self>) -> u64 {
        let (started, round) = self.reported.change(|state| {
            if !state.busy {
                state.busy = true;
                state.access.round += 1;
                for (_, access) in &mut state.access.folders {
                    *access = Access::Asking;
                }
                return (true, state.access.round);
            }
            (false, state.access.round)
        });
        if !started {
            return round;
        }
        let this = self.clone();
        tokio::task::spawn_blocking(move || {
            let _end = EndRound(&this.reported);
            for (index, &(folder, ref path)) in this.folders.iter().enumerate() {
                let access = probe(path);
                info!(
                    folder = folder.name(),
                    access = access.name(),
                    "listed a protected folder"
                );
                this.directory.set_folder_access(folder, access);
                this.reported
                    .change(|state| state.access.folders[index].1 = access);
            }
        });
        round
    }
}

/// Ends the round however the listing ends. Should it panic, the folders
/// not listed yet count as failed, so whoever waits for the round sees it
/// end, and the next request starts another.
struct EndRound<'a>(&'a Reported);

impl Drop for EndRound<'_> {
    fn drop(&mut self) {
        self.0.change(|state| {
            for (_, access) in &mut state.access.folders {
                if *access == Access::Asking {
                    *access = Access::Failed;
                }
            }
            state.access.asked = !state.access.folders.is_empty()
                && state
                    .access
                    .folders
                    .iter()
                    .all(|&(_, access)| access != Access::Unknown);
            state.busy = false;
        });
    }
}

struct Round {
    access: FolderAccess,
    /// The folders are being listed.
    busy: bool,
}

/// The state, and the engine told of every change to it in the order the
/// changes were made: each is sent while the state is still locked, so an
/// older snapshot never lands after a newer one.
struct Reported {
    state: Mutex<Round>,
    engine: mpsc::UnboundedSender<EngineEvent>,
}

impl Reported {
    fn change<T>(&self, change: impl FnOnce(&mut Round) -> T) -> T {
        // Still usable after a panic elsewhere: `EndRound` ends the round.
        let mut state = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let out = change(&mut state);
        let _ = self
            .engine
            .send(EngineEvent::FolderAccess(state.access.clone()));
        out
    }
}

/// Lists `folder` far enough for macOS to decide: opening it, and reading
/// the first entry.
pub(crate) fn probe(folder: &Path) -> Access {
    let first = std::fs::read_dir(folder).and_then(|mut entries| entries.next().transpose());
    match first {
        Ok(_) => Access::Granted,
        Err(e) => from_error(&e),
    }
}

/// macOS refuses a protected folder with EPERM, which is
/// `PermissionDenied` like the file system's own EACCES.
fn from_error(error: &io::Error) -> Access {
    match error.kind() {
        io::ErrorKind::PermissionDenied => Access::Denied,
        io::ErrorKind::NotFound => Access::Missing,
        _ => Access::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listing_tells_what_came_of_each_folder() {
        let dir = tempfile::tempdir().unwrap();
        let empty = dir.path().join("Empty");
        std::fs::create_dir(&empty).unwrap();
        assert_eq!(probe(&empty), Access::Granted);
        let full = dir.path().join("Full");
        std::fs::create_dir(&full).unwrap();
        std::fs::write(full.join("notes.txt"), "hello").unwrap();
        assert_eq!(probe(&full), Access::Granted);
        assert_eq!(probe(&dir.path().join("Gone")), Access::Missing);
        // A file where the folder should be.
        let file = dir.path().join("Documents");
        std::fs::write(&file, "").unwrap();
        assert_eq!(probe(&file), Access::Failed);
    }

    #[cfg(unix)]
    #[test]
    fn a_folder_that_cannot_be_read_is_denied() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let locked = dir.path().join("Locked");
        std::fs::create_dir(&locked).unwrap();
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
        // Root reads it anyway.
        if std::fs::read_dir(&locked).is_err() {
            assert_eq!(probe(&locked), Access::Denied);
        }
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn errors_map_to_states() {
        let os = |code| from_error(&io::Error::from_raw_os_error(code));
        // EPERM, what macOS returns for a folder the user did not allow.
        assert_eq!(os(1), Access::Denied);
        assert_eq!(os(2), Access::Missing);
        assert_eq!(os(5), Access::Failed);
    }

    #[test]
    fn what_was_kept_shows_until_asked_again() {
        let folders: UserFolders = vec![
            (UserFolder::Downloads, "/d".into()),
            (UserFolder::Desktop, "/e".into()),
        ];
        let mut kept = FolderAccessFile::default();
        let access = initial(&folders, &kept);
        assert_eq!(
            access.folders,
            [
                (UserFolder::Downloads, Access::Unknown),
                (UserFolder::Desktop, Access::Unknown)
            ]
        );
        assert!(!access.asked);
        kept.set(UserFolder::Downloads, Access::Granted);
        assert!(!initial(&folders, &kept).asked);
        kept.set(UserFolder::Desktop, Access::Denied);
        let access = initial(&folders, &kept);
        assert!(access.asked);
        let sent = access.to_proto();
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0].folder(), app::UserFolder::Downloads);
        assert_eq!(sent[0].state(), app::FolderAccessState::Granted);
        assert_eq!(sent[1].folder(), app::UserFolder::Desktop);
        assert_eq!(sent[1].state(), app::FolderAccessState::Denied);
        assert!(!initial(&Vec::new(), &kept).asked);
    }

    #[test]
    fn names_go_both_ways_through_the_protocol() {
        for folder in [
            UserFolder::Downloads,
            UserFolder::Desktop,
            UserFolder::Documents,
        ] {
            assert_eq!(UserFolder::from_proto(folder.to_proto()), Some(folder));
        }
        assert_eq!(UserFolder::from_proto(app::UserFolder::Unspecified), None);
        for access in [
            Access::Unknown,
            Access::Asking,
            Access::Granted,
            Access::Denied,
            Access::Missing,
            Access::Failed,
        ] {
            assert_eq!(Access::from_proto(access.to_proto()), access);
        }
    }

    /// A listing that panics still ends its round: what was not listed
    /// counts as failed, and the next request starts a round.
    #[test]
    fn a_round_ends_even_if_the_listing_panics() {
        let (engine, mut events) = mpsc::unbounded_channel();
        let reported = Reported {
            state: Mutex::new(Round {
                access: FolderAccess {
                    folders: vec![
                        (UserFolder::Downloads, Access::Asking),
                        (UserFolder::Desktop, Access::Asking),
                    ],
                    asked: false,
                    round: 1,
                },
                busy: true,
            }),
            engine,
        };
        let listing = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _end = EndRound(&reported);
            reported.change(|state| {
                state.access.folders[0].1 = Access::Granted;
            });
            panic!("listing Desktop failed");
        }));
        assert!(listing.is_err());
        let mut last = None;
        while let Ok(EngineEvent::FolderAccess(access)) = events.try_recv() {
            last = Some(access);
        }
        let last = last.expect("the end of the round was sent");
        assert_eq!(
            last.folders,
            [
                (UserFolder::Downloads, Access::Granted),
                (UserFolder::Desktop, Access::Failed),
            ]
        );
        assert!(last.asked);
        assert!(!reported.state.lock().unwrap().busy);
    }

    /// Changes made on many threads at once reach the engine in the order
    /// they were made.
    #[test]
    fn the_engine_never_gets_an_older_state_after_a_newer_one() {
        let (engine, mut events) = mpsc::unbounded_channel();
        let reported = Arc::new(Reported {
            state: Mutex::new(Round {
                access: FolderAccess::default(),
                busy: false,
            }),
            engine,
        });
        let threads: Vec<_> = (0..8)
            .map(|_| {
                let reported = reported.clone();
                std::thread::spawn(move || {
                    for _ in 0..200 {
                        reported.change(|state| state.access.round += 1);
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        let mut last = 0;
        while let Ok(event) = events.try_recv() {
            let EngineEvent::FolderAccess(access) = event else {
                panic!("not a folder access event");
            };
            assert_eq!(access.round, last + 1);
            last = access.round;
        }
        assert_eq!(last, 8 * 200);
    }
}
