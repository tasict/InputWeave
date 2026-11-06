//! Platform-independent core of InputWeave.
//!
//! This crate is sans-IO: it holds no sockets, threads, timers or OS handles.
//! Callers feed it events (captured input, peer state changes, the current
//! time) and execute the actions it returns. Everything here is unit-testable
//! on any host.

pub mod clipboard;
pub mod geometry;
pub mod hotkey;
pub mod input;
pub mod layout;
pub mod receiver;
pub mod router;
pub mod safety;
pub mod shake;
pub mod speed;

#[cfg(test)]
mod test_support;

pub use iw_keymap::HidUsage;
