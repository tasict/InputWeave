//! IPC transport between the agent and its clients (native UI, `ctl`, the
//! Windows broker).
//!
//! Messages are framed with [`iw_proto::frame`]. On macOS and Linux the
//! transport is a Unix domain socket that only the agent's own user can
//! reach. On Windows it is a named pipe,
//! `\\.\pipe\InputWeave\agent-<SessionId>`, whose DACL admits SYSTEM and
//! the user logged on to that session, and which refuses remote clients;
//! the agent serves the broker on `\\.\pipe\InputWeave\broker-<SessionId>`
//! with the same access. Both offer the same types.

pub use iw_proto::frame::{FrameError, MAX_FRAME_LEN};

#[cfg(any(windows, test))]
mod access;
#[cfg(windows)]
mod pipe;
#[cfg(unix)]
mod unix;

#[cfg(windows)]
pub use access::PipeAccess;
#[cfg(windows)]
pub use pipe::{
    IpcConnection, IpcListener, IpcReader, IpcWriter, agent_pipe_name, broker_pipe_name,
};
#[cfg(unix)]
pub use unix::{IpcConnection, IpcListener, IpcReader, IpcWriter};
