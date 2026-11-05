//! The app's own settings, which the agent knows nothing of: whether
//! first-run setup was finished, how many times the take-back hint was
//! shown, and whether the screen's edge lights up where the cursor crosses.
//! One `key = value` per line in `app.toml`, next to the agent's
//! configuration (`$XDG_CONFIG_HOME/inputweave`).

use std::io::Write;
use std::path::{Path, PathBuf};

use crate::paths;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub finished_setup: bool,
    pub hud_hints: u32,
    /// On unless turned off.
    pub edge_glow: bool,
    /// The packaged agent's digest the service was last restarted for
    /// (`service::OutdatedWatch`).
    pub restarted_for: String,
}

impl Default for Settings {
    fn default() -> Self {
        Settings {
            finished_setup: false,
            hud_hints: 0,
            edge_glow: true,
            restarted_for: String::new(),
        }
    }
}

/// `$XDG_CONFIG_HOME/inputweave/app.toml`.
pub fn path() -> Option<PathBuf> {
    paths::config_dir().map(|d| d.join("app.toml"))
}

impl Settings {
    /// What `text` says; anything not understood is left at its default.
    pub fn parse(text: &str) -> Settings {
        let mut settings = Settings::default();
        for line in text.lines() {
            let Some((key, value)) = line.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key.trim() {
                "finished_setup" => settings.finished_setup = value == "true",
                "hud_hints" => settings.hud_hints = value.parse().unwrap_or(0),
                "edge_glow" => settings.edge_glow = value != "false",
                "restarted_for" => {
                    settings.restarted_for = value.trim_matches('"').to_owned();
                }
                _ => {}
            }
        }
        settings
    }

    pub fn text(&self) -> String {
        format!(
            "# The InputWeave app's own settings.\nfinished_setup = {}\nhud_hints = {}\nedge_glow = {}\nrestarted_for = \"{}\"\n",
            self.finished_setup, self.hud_hints, self.edge_glow, self.restarted_for
        )
    }

    /// The settings in `path`; the defaults when there are none.
    pub fn load(path: &Path) -> Settings {
        std::fs::read_to_string(path)
            .map(|t| Settings::parse(&t))
            .unwrap_or_default()
    }

    /// Writes the settings to `path`, through a file beside it, so a crash
    /// leaves either the old ones or the new ones.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let temporary = path.with_extension("toml.part");
        let mut file = std::fs::File::create(&temporary)?;
        file.write_all(self.text().as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_back_what_it_wrote() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deep/app.toml");
        assert_eq!(Settings::load(&path), Settings::default());
        let settings = Settings {
            finished_setup: true,
            hud_hints: 2,
            edge_glow: false,
            restarted_for: "ab".repeat(32),
        };
        settings.save(&path).unwrap();
        assert_eq!(Settings::load(&path), settings);
        assert!(!dir.path().join("deep/app.toml.part").exists());
    }

    #[test]
    fn leaves_what_it_does_not_understand_at_the_default() {
        let settings = Settings::parse(
            "finished_setup = yes\nhud_hints = many\nedge_glow = 2\nother = 1\nnoise",
        );
        assert_eq!(settings, Settings::default());
        assert!(Settings::parse(" finished_setup=true ").finished_setup);
        // The glow is on until turned off, also in files from before it.
        assert!(Settings::parse("finished_setup = true\n").edge_glow);
        assert!(!Settings::parse("edge_glow = false").edge_glow);
    }
}
