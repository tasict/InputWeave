//! Log files. The service and the processes it starts have no console, so
//! their output goes to `%ProgramData%\InputWeave\logs`.

use std::fs::{File, OpenOptions};
use std::io;
use std::path::PathBuf;

/// A log larger than this is moved to `<name>.old.log` when reopened.
const ROTATE_AT: u64 = 10 * 1024 * 1024;

fn dir() -> io::Result<PathBuf> {
    let data = std::env::var_os("ProgramData")
        .filter(|v| !v.is_empty())
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "%ProgramData% is not set"))?;
    let dir = PathBuf::from(data).join("InputWeave").join("logs");
    std::fs::create_dir_all(&dir)?;
    Ok(dir)
}

/// Opens `<name>.log` for appending, after moving it aside if it grew too
/// large.
pub fn open(name: &str) -> io::Result<File> {
    let dir = dir()?;
    let path = dir.join(format!("{name}.log"));
    if std::fs::metadata(&path).is_ok_and(|m| m.len() > ROTATE_AT) {
        std::fs::rename(&path, dir.join(format!("{name}.old.log")))?;
    }
    OpenOptions::new().create(true).append(true).open(path)
}
