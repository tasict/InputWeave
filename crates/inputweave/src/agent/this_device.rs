//! This device as others see it: its id, its name, the Hello it opens
//! sessions with, and its mDNS announcement. The name can change while the
//! agent runs; sessions opened afterwards and the announcement use the new
//! one. The announcement stops while the profile in use is paused, and
//! starts again after it (`agent::advertise`).

use std::sync::{Mutex, RwLock};

use iw_net::discovery::{Announcement, Discovery};
use iw_proto::peer::v1::{Hello, Os};
use tracing::warn;

pub(crate) struct ThisDevice {
    pub(crate) id: String,
    name: RwLock<String>,
    os: Os,
    /// The highest peer protocol version to offer, when lower than this
    /// build's (`Options::peer_protocol_max`).
    protocol_max: Option<u32>,
    /// What this device announces on the network, where it does at all,
    /// and the announcement while it runs: not while the profile in use is
    /// paused.
    discovery: Mutex<Option<Announcing>>,
}

struct Announcing {
    announcement: Announcement,
    running: Option<Running>,
}

/// An announcement that runs.
pub(crate) enum Running {
    Mdns(Discovery),
    /// One that touches no network, for tests (`Options::fake_discovery`).
    #[cfg(feature = "test-hooks")]
    Fake,
}

impl Running {
    fn update(&self, announcement: &Announcement) {
        match self {
            Self::Mdns(discovery) => {
                if let Err(e) = discovery.update(announcement) {
                    warn!(error = %e, "cannot announce the new name");
                }
            }
            #[cfg(feature = "test-hooks")]
            Self::Fake => {}
        }
    }

    fn shutdown(self) {
        match self {
            Self::Mdns(discovery) => discovery.shutdown(),
            #[cfg(feature = "test-hooks")]
            Self::Fake => {}
        }
    }
}

impl ThisDevice {
    pub(crate) fn new(id: String, name: String, os: Os, protocol_max: Option<u32>) -> Self {
        Self {
            id,
            name: RwLock::new(name),
            os,
            protocol_max,
            discovery: Mutex::new(None),
        }
    }

    pub(crate) fn name(&self) -> String {
        self.name.read().unwrap().clone()
    }

    pub(crate) fn hello(&self) -> Hello {
        let mut hello = iw_net::hello(&self.id, &self.name(), self.os);
        if let Some(max) = self.protocol_max {
            hello.protocol_max = hello.protocol_max.min(max).max(hello.protocol_min);
        }
        hello
    }

    /// Makes this device one that announces itself as `announcement` says,
    /// whenever it does (`announce`).
    pub(crate) fn announces(&self, announcement: Announcement) {
        *self.discovery.lock().unwrap() = Some(Announcing {
            announcement,
            running: None,
        });
    }

    /// What this device announces, with its name now; none where it
    /// announces nothing.
    pub(crate) fn announcement(&self) -> Option<Announcement> {
        let discovery = self.discovery.lock().unwrap();
        let mut announcement = discovery.as_ref()?.announcement.clone();
        announcement.name = self.name();
        Some(announcement)
    }

    /// Keeps the running announcement, to update it on rename and stop it;
    /// shuts it down at once where this device announces nothing, or no
    /// longer (`stop`).
    pub(crate) fn announce(&self, running: Running) {
        let mut discovery = self.discovery.lock().unwrap();
        match discovery.as_mut() {
            Some(announcing) => {
                if let Some(before) = announcing.running.replace(running) {
                    before.shutdown();
                }
            }
            None => running.shutdown(),
        }
    }

    /// Whether this device announces itself now.
    pub(crate) fn announcing(&self) -> bool {
        self.discovery
            .lock()
            .unwrap()
            .as_ref()
            .is_some_and(|a| a.running.is_some())
    }

    pub(crate) fn rename(&self, name: &str) {
        *self.name.write().unwrap() = name.to_owned();
        if let Some(announcing) = self.discovery.lock().unwrap().as_mut() {
            announcing.announcement.name = name.to_owned();
            if let Some(running) = &announcing.running {
                running.update(&announcing.announcement);
            }
        }
    }

    /// Withdraws the announcement and stops browsing, until announced
    /// again.
    pub(crate) fn stop_announcing(&self) {
        let running = self
            .discovery
            .lock()
            .unwrap()
            .as_mut()
            .and_then(|a| a.running.take());
        if let Some(running) = running {
            running.shutdown();
        }
    }

    /// Stops announcing for good, as the agent stops: an announcement
    /// started after this is shut down at once (`announce`).
    pub(crate) fn stop(&self) {
        let stopped = self.discovery.lock().unwrap().take();
        if let Some(running) = stopped.and_then(|a| a.running) {
            running.shutdown();
        }
    }
}

#[cfg(all(test, feature = "test-hooks"))]
mod tests {
    use super::*;

    #[test]
    fn once_stopped_nothing_is_announced() {
        let this = ThisDevice::new("mac".to_owned(), "Mac".to_owned(), Os::Macos, None);
        this.announces(Announcement {
            device_id: "mac".to_owned(),
            name: "Mac".to_owned(),
            os: Os::Macos,
            fingerprint: iw_net::Fingerprint::from_bytes([7; 32]),
            addr: "127.0.0.1:0".parse().unwrap(),
            pairable: false,
        });
        this.announce(Running::Fake);
        assert!(this.announcing());
        this.stop();
        assert!(!this.announcing());
        // One started as the agent stopped is not kept running.
        this.announce(Running::Fake);
        assert!(!this.announcing());
        assert!(this.announcement().is_none());
    }
}
