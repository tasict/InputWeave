//! The Windows service and broker roles (see `docs/architecture.md` §1
//! and §2).
//!
//! - `inputweave service` ([`service`]) runs as LocalSystem in session 0
//!   and never touches the network. It starts `inputweave agent` with a
//!   copy of its SYSTEM token in the session attached to the console, so
//!   the agent can capture and inject on the secure desktop too, and
//!   `inputweave broker` with the token of the user logged on there. It
//!   restarts either when it exits and when the console changes hands or
//!   a user logs on or off, and sends Ctrl+Alt+Del when the agent asks
//!   ([`sas`]). Each runs in a job object of its own that ends it if the
//!   service process itself dies (`job`), so the service the service
//!   control manager restarts finds the agent's pipe and port free.
//! - `inputweave broker` ([`broker`]) runs as that user and does for the
//!   agent what must not run as SYSTEM: reading the desktop pictures,
//!   reading and writing the clipboard, writing received files, reading
//!   the files the user sends, and keeping the user's transfer history. It
//!   also starts the app when the user has to see or decide something and
//!   no app is connected to the agent.
//!
//! The installer (`packaging/windows/InputWeave.wxs`) installs the service
//! and sets the `SoftwareSASGeneration` policy Ctrl+Alt+Del needs (see
//! `policy`). The agent it starts reads
//! `%ProgramData%\InputWeave\config.toml`; the service, agent and broker
//! log to `%ProgramData%\InputWeave\logs`.
//!
//! On Windows 11 the service has been seen to start the agent and broker in
//! the console session, restart them when they exit, stop them with the
//! service, and take them down with it when its process is killed; logons,
//! user switches and Ctrl+Alt+Del have not been tried there yet. The
//! decisions (which process runs where and when, service state reports,
//! command lines, the policy check) are FFI-free and tested on every OS;
//! apart from them the crate is empty elsewhere.

#[cfg(any(windows, test))]
pub mod broker;
#[cfg(any(windows, test))]
mod cmdline;
#[cfg(any(windows, test))]
mod policy;
#[cfg(any(windows, test))]
mod status;
#[cfg(any(windows, test))]
mod supervisor;

#[cfg(windows)]
mod job;
#[cfg(windows)]
mod launch;
#[cfg(windows)]
mod logs;
#[cfg(windows)]
pub mod sas;
#[cfg(windows)]
pub mod service;
#[cfg(windows)]
mod token;

#[cfg(windows)]
pub use logs::open as open_log;
