//! The app's log: lines on standard error, which the journal keeps when
//! the desktop session started the app. No file names or clipboard
//! contents go in, only counts and ids.

use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

pub fn info(line: &str) {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let mut err = std::io::stderr().lock();
    let _ = writeln!(
        err,
        "inputweave-app {}.{:03} {line}",
        now.as_secs(),
        now.subsec_millis()
    );
}
