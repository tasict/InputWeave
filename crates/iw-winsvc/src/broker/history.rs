//! The user's transfer history, which the broker keeps as the user in
//! `%LOCALAPPDATA%\InputWeave\transfers.toml`: the agent runs as SYSTEM,
//! and the history holds the names of the user's files
//! (docs/architecture.md §9).
//!
//! The broker never reads what it keeps. The agent asks for the file once
//! connected (`LoadHistory`) and replaces it whole whenever the history
//! changes (`SaveHistory`). Either way it is at most [`MAX_BYTES`]: a
//! larger file, which only something other than InputWeave can have
//! written, is an empty history, and is replaced at the next save.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use iw_proto::service::v1::{BrokerMessage, HistoryLoaded, HistorySaved, broker_message};

use super::files::blocking;

/// The most the history holds, as the agent holds it to.
pub(crate) const MAX_BYTES: usize = 1024 * 1024;

/// Where the history is kept in `local_app_data`, the user's local
/// application data folder.
pub(crate) fn history_file(local_app_data: &Path) -> PathBuf {
    local_app_data.join("InputWeave").join("transfers.toml")
}

fn loaded(toml: Vec<u8>, error: String) -> BrokerMessage {
    BrokerMessage {
        msg: Some(broker_message::Msg::HistoryLoaded(HistoryLoaded {
            toml,
            error,
        })),
    }
}

fn saved(error: String) -> BrokerMessage {
    BrokerMessage {
        msg: Some(broker_message::Msg::HistorySaved(HistorySaved { error })),
    }
}

/// The history kept in `file`, for `LoadHistory`: empty when there is
/// none yet, and empty with why when it cannot be read or is too large.
pub(crate) async fn load(file: Option<PathBuf>) -> BrokerMessage {
    let Some(file) = file else {
        return loaded(
            Vec::new(),
            "there is no folder to keep the history in".to_owned(),
        );
    };
    match blocking(move || read(&file)).await {
        Ok(toml) => loaded(toml, String::new()),
        Err(e) => {
            tracing::warn!(error = %e, "cannot load the transfer history; starting a new one");
            loaded(Vec::new(), e.to_string())
        }
    }
}

/// Replaces the history kept in `file` with `toml`, for `SaveHistory`.
pub(crate) async fn save(file: Option<PathBuf>, toml: Vec<u8>) -> BrokerMessage {
    let Some(file) = file else {
        return saved("there is no folder to keep the history in".to_owned());
    };
    if toml.len() > MAX_BYTES {
        return saved(format!("refused a history larger than {MAX_BYTES} bytes"));
    }
    match blocking(move || write(&file, &toml)).await {
        Ok(()) => saved(String::new()),
        Err(e) => {
            tracing::debug!(error = %e, "cannot save the transfer history");
            saved(e.to_string())
        }
    }
}

/// The file's bytes; none if it does not exist.
fn read(path: &Path) -> io::Result<Vec<u8>> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e),
    };
    let mut bytes = Vec::new();
    file.take(MAX_BYTES as u64 + 1).read_to_end(&mut bytes)?;
    if bytes.len() > MAX_BYTES {
        return Err(io::Error::other(format!(
            "the history is larger than {MAX_BYTES} bytes"
        )));
    }
    Ok(bytes)
}

/// Replaces the file in one step with `bytes`, creating its folder if
/// needed.
fn write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("toml.tmp");
    let mut out = std::fs::File::create(&tmp)?;
    out.write_all(bytes)?;
    out.sync_all()?;
    drop(out);
    // Replaces the file there, on Windows too.
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn answer_loaded(msg: BrokerMessage) -> HistoryLoaded {
        match msg.msg {
            Some(broker_message::Msg::HistoryLoaded(loaded)) => loaded,
            other => panic!("expected the history loaded, got {other:?}"),
        }
    }

    fn answer_saved(msg: BrokerMessage) -> HistorySaved {
        match msg.msg {
            Some(broker_message::Msg::HistorySaved(saved)) => saved,
            other => panic!("expected the history saved, got {other:?}"),
        }
    }

    #[test]
    fn the_history_is_kept_in_the_users_local_folder() {
        assert_eq!(
            history_file(Path::new("/Users/pc/AppData/Local")),
            Path::new("/Users/pc/AppData/Local/InputWeave/transfers.toml")
        );
    }

    #[tokio::test]
    async fn saves_and_loads_the_bytes_it_is_given() {
        let dir = tempfile::tempdir().unwrap();
        let file = history_file(dir.path());

        // None yet.
        let first = answer_loaded(load(Some(file.clone())).await);
        assert!(first.toml.is_empty() && first.error.is_empty(), "{first:?}");

        let toml = b"[[transfers]]\nid = \"a-1\"\n".to_vec();
        let answer = answer_saved(save(Some(file.clone()), toml.clone()).await);
        assert!(answer.error.is_empty(), "{answer:?}");
        assert_eq!(std::fs::read(&file).unwrap(), toml);
        assert!(!file.with_extension("toml.tmp").exists());
        assert_eq!(answer_loaded(load(Some(file.clone())).await).toml, toml);

        // Replaced whole.
        answer_saved(save(Some(file.clone()), b"x".to_vec()).await);
        assert_eq!(answer_loaded(load(Some(file)).await).toml, b"x");
    }

    #[tokio::test]
    async fn keeps_to_the_limit() {
        let dir = tempfile::tempdir().unwrap();
        let file = history_file(dir.path());
        let refused = answer_saved(save(Some(file.clone()), vec![b'#'; MAX_BYTES + 1]).await);
        assert!(!refused.error.is_empty());
        assert!(!file.exists());
        let full = answer_saved(save(Some(file.clone()), vec![b'#'; MAX_BYTES]).await);
        assert!(full.error.is_empty(), "{full:?}");

        // Something else made it larger: an empty history, and why.
        std::fs::write(&file, vec![b'#'; MAX_BYTES + 10]).unwrap();
        let loaded = answer_loaded(load(Some(file)).await);
        assert!(loaded.toml.is_empty());
        assert!(loaded.error.contains("larger"), "{loaded:?}");
    }

    #[tokio::test]
    async fn without_a_folder_nothing_is_kept() {
        let loaded = answer_loaded(load(None).await);
        assert!(loaded.toml.is_empty() && !loaded.error.is_empty());
        assert!(
            !answer_saved(save(None, b"x".to_vec()).await)
                .error
                .is_empty()
        );
    }
}
