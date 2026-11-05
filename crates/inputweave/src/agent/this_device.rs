//! This device as others see it: its id, its name, the Hello it opens
//! sessions with, and its mDNS announcement. The name can change while the
//! agent runs; sessions opened afterwards and the announcement use the new
//! one.

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
    discovery: Mutex<Option<(Discovery, Announcement)>>,
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

    /// Keeps the running mDNS announcement, to update it on rename.
    pub(crate) fn announce(&self, discovery: Discovery, announcement: Announcement) {
        *self.discovery.lock().unwrap() = Some((discovery, announcement));
    }

    pub(crate) fn rename(&self, name: &str) {
        *self.name.write().unwrap() = name.to_owned();
        if let Some((discovery, announcement)) = self.discovery.lock().unwrap().as_mut() {
            announcement.name = name.to_owned();
            if let Err(e) = discovery.update(announcement) {
                warn!(error = %e, "cannot announce the new name");
            }
        }
    }

    pub(crate) fn stop_announcing(&self) {
        if let Some((discovery, _)) = self.discovery.lock().unwrap().take() {
            discovery.shutdown();
        }
    }
}
