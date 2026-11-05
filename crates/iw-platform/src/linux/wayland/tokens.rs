//! The restore tokens the desktop's portals hand out, kept so the desktop
//! does not ask the user again each time the agent starts.
//!
//! They live in `portal.toml` in the agent's folder (next to
//! `devices.toml`), one `<portal> = "<token>"` line each, readable by the
//! user only. A token is good for one start: each start hands out the next.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// The file's name in the agent's folder.
pub(crate) const FILE: &str = "portal.toml";

/// The longest token kept; portals hand out UUIDs.
const MAX_TOKEN: usize = 256;

/// Each portal's session keeps its token from a thread of its own, at the
/// same time at login: one writes the file at a time, so neither loses the
/// other's (nor shares its temporary file).
static WRITING: Mutex<()> = Mutex::new(());

/// Where the tokens are kept, if anywhere.
#[derive(Clone, Debug)]
pub(crate) struct Tokens {
    path: Option<PathBuf>,
}

impl Tokens {
    /// Tokens kept in `dir`, or nowhere.
    pub(crate) fn in_dir(dir: Option<&Path>) -> Self {
        Self {
            path: dir.map(|d| d.join(FILE)),
        }
    }

    /// The token kept for `portal`.
    pub(crate) fn load(&self, portal: &str) -> Option<String> {
        let text = std::fs::read_to_string(self.path.as_ref()?).ok()?;
        parse(&text)
            .into_iter()
            .find(|(p, _)| p == portal)
            .map(|(_, t)| t)
    }

    /// Keeps `token` for `portal`, or forgets it.
    pub(crate) fn save(&self, portal: &str, token: Option<&str>) -> std::io::Result<()> {
        self.save_all(&[(portal, token)])
    }

    /// Keeps (or forgets) several at once, so they never disagree.
    pub(crate) fn save_all(&self, tokens: &[(&str, Option<&str>)]) -> std::io::Result<()> {
        let Some(path) = &self.path else {
            return Ok(());
        };
        let _writing = WRITING.lock().unwrap_or_else(|e| e.into_inner());
        let mut all = std::fs::read_to_string(path)
            .map(|text| parse(&text))
            .unwrap_or_default();
        for (portal, token) in tokens {
            all.retain(|(p, _)| p != portal);
            if let Some(token) = token.filter(|t| valid(t)) {
                all.push(((*portal).to_owned(), token.to_owned()));
            }
        }
        let mut text = String::from(
            "# Restore tokens of the desktop's portals, kept by InputWeave's agent.\n",
        );
        for (p, t) in &all {
            text.push_str(&format!("{p} = \"{t}\"\n"));
        }
        write_private(path, text.as_bytes())
    }
}

/// Tokens are quoted in the file and used as D-Bus strings: only these
/// characters are kept, which any portal's tokens are made of.
fn valid(token: &str) -> bool {
    !token.is_empty()
        && token.len() <= MAX_TOKEN
        && token
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b':'))
}

fn parse(text: &str) -> Vec<(String, String)> {
    text.lines()
        .filter_map(|line| {
            let (key, value) = line.split_once('=')?;
            let key = key.trim();
            let value = value.trim().strip_prefix('"')?.strip_suffix('"')?;
            let key_ok =
                !key.is_empty() && key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_');
            (key_ok && valid(value)).then(|| (key.to_owned(), value.to_owned()))
        })
        .collect()
}

/// Writes `path` readable by its owner only, whole or not at all.
fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let temp = dir.join(format!(".{FILE}.{}", std::process::id()));
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = (|| {
        let mut file = options.open(&temp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_kept_per_portal() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = Tokens::in_dir(Some(dir.path()));
        assert_eq!(tokens.load("remote_desktop"), None);
        tokens
            .save("remote_desktop", Some("6b4e-2c1a-9f00"))
            .unwrap();
        tokens.save("input_capture", Some("abc.def")).unwrap();
        assert_eq!(
            tokens.load("remote_desktop").as_deref(),
            Some("6b4e-2c1a-9f00")
        );
        tokens.save("remote_desktop", Some("next-one")).unwrap();
        assert_eq!(tokens.load("remote_desktop").as_deref(), Some("next-one"));
        assert_eq!(tokens.load("input_capture").as_deref(), Some("abc.def"));
        tokens.save("remote_desktop", None).unwrap();
        assert_eq!(tokens.load("remote_desktop"), None);
        assert_eq!(tokens.load("input_capture").as_deref(), Some("abc.def"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn odd_tokens_and_lines_are_not_kept() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = Tokens::in_dir(Some(dir.path()));
        tokens
            .save("remote_desktop", Some("has \"quotes\""))
            .unwrap();
        assert_eq!(tokens.load("remote_desktop"), None);
        tokens
            .save("remote_desktop", Some(&"x".repeat(300)))
            .unwrap();
        assert_eq!(tokens.load("remote_desktop"), None);
        std::fs::write(
            dir.path().join(FILE),
            "remote_desktop = 'single'\nbroken\n = \"x\"\ninput_capture=\"ok-1\"\n",
        )
        .unwrap();
        assert_eq!(tokens.load("remote_desktop"), None);
        assert_eq!(tokens.load("input_capture").as_deref(), Some("ok-1"));
    }

    #[test]
    fn portals_keeping_tokens_at_once_keep_them_all() {
        let dir = tempfile::tempdir().unwrap();
        let tokens = Tokens::in_dir(Some(dir.path()));
        std::thread::scope(|s| {
            for portal in ["remote_desktop", "input_capture"] {
                let tokens = tokens.clone();
                s.spawn(move || {
                    for i in 0..200 {
                        tokens.save(portal, Some(&format!("t-{i}"))).unwrap();
                    }
                });
            }
        });
        assert_eq!(tokens.load("remote_desktop").as_deref(), Some("t-199"));
        assert_eq!(tokens.load("input_capture").as_deref(), Some("t-199"));
    }

    #[test]
    fn nowhere_to_keep_them() {
        let tokens = Tokens::in_dir(None);
        tokens.save("remote_desktop", Some("t")).unwrap();
        assert_eq!(tokens.load("remote_desktop"), None);
    }
}
