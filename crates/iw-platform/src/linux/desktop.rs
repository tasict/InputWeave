//! Which desktop the session runs, and reading its settings: GNOME keeps
//! them in GSettings, read with the `gsettings` tool (no D-Bus service
//! offers them), and KDE Plasma in ini files and KWin's D-Bus objects.

/// The desktops whose settings InputWeave reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Desktop {
    /// GNOME, and the desktops that keep GNOME's settings (Ubuntu's,
    /// Unity, Budgie).
    Gnome,
    Kde,
    Other,
}

/// The desktop `XDG_CURRENT_DESKTOP` names: a `:`-separated list, most
/// specific first ("ubuntu:GNOME", "KDE", "Budgie:GNOME").
pub(crate) fn desktop_of(current: &str) -> Desktop {
    let names: Vec<String> = current
        .split(':')
        .map(|n| n.trim().to_ascii_lowercase())
        .collect();
    if names.iter().any(|n| n == "kde") {
        Desktop::Kde
    } else if names
        .iter()
        .any(|n| matches!(n.as_str(), "gnome" | "unity" | "budgie"))
    {
        Desktop::Gnome
    } else {
        Desktop::Other
    }
}

/// A string as `gsettings get` prints it (a GVariant in text form:
/// `'value'`, with `\'` and `\\` inside), or `None` for anything else.
pub(crate) fn gvariant_string(text: &str) -> Option<String> {
    let text = text.trim();
    let inner = text
        .strip_prefix('\'')
        .and_then(|t| t.strip_suffix('\''))
        .or_else(|| text.strip_prefix('"').and_then(|t| t.strip_suffix('"')))?;
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            out.push(chars.next()?);
        } else {
            out.push(c);
        }
    }
    Some(out)
}

/// The value of `schema`'s `key` in what `gsettings list-recursively`
/// printed (a line `schema key value`, child schemas under their own ids),
/// as GVariant text.
pub(crate) fn listed<'a>(list: &'a str, schema: &str, key: &str) -> Option<&'a str> {
    list.lines().find_map(|line| {
        let (s, rest) = line.split_once(' ')?;
        let (k, value) = rest.split_once(' ')?;
        (s == schema && k == key).then_some(value)
    })
}

/// A boolean as `gsettings get` prints it.
pub(crate) fn gvariant_bool(text: &str) -> Option<bool> {
    match text.trim() {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

#[cfg(target_os = "linux")]
mod platform {
    use std::path::PathBuf;
    use std::time::Duration;

    /// How long `gsettings` may take.
    const WAIT: Duration = Duration::from_secs(2);

    /// What `gsettings get <schema> <key>` prints, or `None` without the
    /// tool, the schema or the key, or if it takes too long.
    pub(crate) fn gsettings_get(schema: &str, key: &str) -> Option<String> {
        crate::system::run_for("gsettings", &["get", schema, key], WAIT)
            .ok()
            .map(|ran| ran.text)
    }

    /// A string setting.
    pub(crate) fn gsettings_string(schema: &str, key: &str) -> Option<String> {
        super::gvariant_string(&gsettings_get(schema, key)?)
    }

    /// What `gsettings list-recursively <schema>` prints: every key of
    /// the schema and of its children, in one process.
    pub(crate) fn gsettings_list(schema: &str) -> Option<String> {
        crate::system::run_for("gsettings", &["list-recursively", schema], WAIT)
            .ok()
            .map(|ran| ran.text)
    }

    /// Where the user's settings are: `$XDG_CONFIG_HOME`, or `~/.config`.
    pub(crate) fn config_home() -> PathBuf {
        std::env::var_os("XDG_CONFIG_HOME")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
            .unwrap_or_default()
    }
}

#[cfg(target_os = "linux")]
pub(crate) use platform::{config_home, gsettings_list, gsettings_string};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_desktop_is_read_from_the_list_of_names() {
        assert_eq!(desktop_of("ubuntu:GNOME"), Desktop::Gnome);
        assert_eq!(desktop_of("GNOME"), Desktop::Gnome);
        assert_eq!(desktop_of("Budgie:GNOME"), Desktop::Gnome);
        assert_eq!(desktop_of("KDE"), Desktop::Kde);
        assert_eq!(desktop_of("sway"), Desktop::Other);
        assert_eq!(desktop_of("X-Cinnamon"), Desktop::Other);
        assert_eq!(desktop_of(""), Desktop::Other);
    }

    #[test]
    fn gsettings_values_are_read_as_gvariant_text() {
        assert_eq!(
            gvariant_string("'file:///usr/share/backgrounds/warty-final-ubuntu.png'\n").as_deref(),
            Some("file:///usr/share/backgrounds/warty-final-ubuntu.png")
        );
        assert_eq!(
            gvariant_string(r"'it\'s \\ here'").as_deref(),
            Some(r"it's \ here")
        );
        assert_eq!(gvariant_string("\"double\"").as_deref(), Some("double"));
        assert_eq!(gvariant_string("@as []"), None);
        assert_eq!(gvariant_string("'cut \\"), None);
        assert_eq!(gvariant_bool("true\n"), Some(true));
        assert_eq!(gvariant_bool("false"), Some(false));
        assert_eq!(gvariant_bool("'true'"), None);
    }

    #[test]
    fn keys_are_found_in_a_recursive_listing() {
        // As GNOME 46's gsettings prints them.
        let list = "\
org.gnome.desktop.peripherals.mouse natural-scroll false
org.gnome.desktop.peripherals.touchpad natural-scroll true
org.gnome.desktop.background picture-options 'zoom'
org.gnome.desktop.background picture-uri 'file:///usr/share/backgrounds/gnome/adwaita-l.jpg'
org.gnome.desktop.background picture-uri-dark 'file:///usr/share/backgrounds/gnome/adwaita-d.jpg'
";
        let touchpad = listed(
            list,
            "org.gnome.desktop.peripherals.touchpad",
            "natural-scroll",
        );
        assert_eq!(touchpad.and_then(gvariant_bool), Some(true));
        let mouse = listed(
            list,
            "org.gnome.desktop.peripherals.mouse",
            "natural-scroll",
        );
        assert_eq!(mouse.and_then(gvariant_bool), Some(false));
        let uri = listed(list, "org.gnome.desktop.background", "picture-uri");
        assert_eq!(
            uri.and_then(gvariant_string).as_deref(),
            Some("file:///usr/share/backgrounds/gnome/adwaita-l.jpg")
        );
        assert_eq!(
            listed(list, "org.gnome.desktop.background", "picture-opacity"),
            None
        );
        assert_eq!(listed(list, "org.gnome.desktop", "background"), None);
    }
}
