//! The agent's log in a file of its own (`[paths] log_file`), for an agent
//! nothing else keeps the log of, such as one a plain service script starts
//! on a machine without systemd's journal or launchd.
//!
//! The file never grows much past [`MAX_LEN`]: once it does, its content is
//! copied to `<file>.1` and the file emptied in place, rather than moved
//! aside. Whatever else appends to it carries on in the same file that
//! way: a service script can send the agent's standard error there too,
//! with what it prints before logging starts and if it panics.

use std::fs::{File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

/// How large the log may grow before it starts over.
pub const MAX_LEN: u64 = 5 * 1024 * 1024;

pub struct CappedLog {
    path: PathBuf,
    max: u64,
    file: Mutex<File>,
}

impl CappedLog {
    /// Opens `path` for appending, making its folder if needed.
    pub fn open(path: &Path) -> io::Result<CappedLog> {
        Self::with_max(path, MAX_LEN)
    }

    fn with_max(path: &Path, max: u64) -> io::Result<CappedLog> {
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new().create(true).append(true).open(path)?;
        let log = CappedLog {
            path: path.to_owned(),
            max,
            file: Mutex::new(file),
        };
        // A log left too large by an earlier run starts over now.
        log.lock().cap();
        Ok(log)
    }

    fn lock(&self) -> Writer<'_> {
        Writer {
            log: self,
            file: self.file.lock().unwrap_or_else(|e| e.into_inner()),
        }
    }

    /// The previous log, which `<file>.1` keeps.
    pub fn previous(path: &Path) -> PathBuf {
        let mut name = path.as_os_str().to_owned();
        name.push(".1");
        PathBuf::from(name)
    }
}

/// One log line's write, which starts the file over when it went past
/// the limit.
pub struct Writer<'a> {
    log: &'a CappedLog,
    file: MutexGuard<'a, File>,
}

impl Writer<'_> {
    fn cap(&mut self) {
        let Ok(meta) = self.file.metadata() else {
            return;
        };
        if meta.len() <= self.log.max {
            return;
        }
        // Copied rather than renamed, so whoever else has the file open
        // for appending still writes to the one that stays. Read and
        // written whole, as a copy of the system's may refuse a file open
        // for writing (Windows).
        if let Ok(content) = std::fs::read(&self.log.path) {
            let _ = std::fs::write(CappedLog::previous(&self.log.path), content);
        }
        // A file opened for appending cannot be cut on Windows; another
        // handle can.
        if self.file.set_len(0).is_err() {
            let _ = OpenOptions::new()
                .write(true)
                .open(&self.log.path)
                .and_then(|f| f.set_len(0));
        }
    }
}

impl Write for Writer<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.file.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Drop for Writer<'_> {
    fn drop(&mut self) {
        self.cap();
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CappedLog {
    type Writer = Writer<'a>;

    fn make_writer(&'a self) -> Self::Writer {
        self.lock()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tracing_subscriber::fmt::MakeWriter;

    fn write(log: &CappedLog, line: &str) {
        let mut w = log.make_writer();
        w.write_all(line.as_bytes()).unwrap();
    }

    #[test]
    fn a_log_past_its_limit_starts_over_where_others_append_too() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("logs/agent.log");
        let log = CappedLog::with_max(&path, 20).unwrap();
        // Standard error, as the service script sends it to the same file.
        let mut other = OpenOptions::new().append(true).open(&path).unwrap();
        write(&log, "first line\n");
        other.write_all(b"stderr\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "first line\nstderr\n"
        );
        // Past 20 bytes: kept in .1, and the file is empty again.
        write(&log, "second line\n");
        assert_eq!(
            std::fs::read_to_string(CappedLog::previous(&path)).unwrap(),
            "first line\nstderr\nsecond line\n"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "");
        // Both carry on at the start of the emptied file.
        other.write_all(b"more stderr\n").unwrap();
        write(&log, "third\n");
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "more stderr\nthird\n"
        );
    }

    #[test]
    fn a_log_left_too_large_starts_over_when_opened() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.log");
        std::fs::write(&path, "x".repeat(30)).unwrap();
        let log = CappedLog::with_max(&path, 20).unwrap();
        assert_eq!(std::fs::read(&path).unwrap().len(), 0);
        assert_eq!(std::fs::read(CappedLog::previous(&path)).unwrap().len(), 30);
        write(&log, "fresh\n");
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "fresh\n");
    }
}
