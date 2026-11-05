//! Opening the app at login: an XDG autostart entry, which every desktop
//! reads from `~/.config/autostart`. The app starts in the background
//! then, with only its tray icon.

use std::io;
use std::path::{Path, PathBuf};

/// The entry's file name, after the app id.
pub const FILE_NAME: &str = "cc.ctsite.inputweave.desktop";

/// The entry that starts `executable` in the background at login.
pub fn entry(executable: &Path) -> String {
    let exec = escape_string(&quote(&executable.to_string_lossy()));
    format!(
        "[Desktop Entry]\n\
         Type=Application\n\
         Name=InputWeave\n\
         Exec={exec} --background\n\
         Icon=cc.ctsite.inputweave\n\
         X-GNOME-Autostart-enabled=true\n\
         NoDisplay=true\n"
    )
}

/// An argument of `Exec` quoted as the Desktop Entry spec wants it, when
/// it needs it: in double quotes, with `"`, `` ` ``, `$` and `\` escaped by
/// a backslash, and `%` doubled, as it starts a field code.
fn quote(arg: &str) -> String {
    let plain = !arg.is_empty()
        && arg
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "/._-+".contains(c));
    if plain {
        return arg.to_owned();
    }
    let mut out = String::from("\"");
    for c in arg.chars() {
        if matches!(c, '"' | '`' | '$' | '\\') {
            out.push('\\');
        }
        if c == '%' {
            out.push('%');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// A value of type string, as the spec reads every one before `Exec`'s
/// own quoting: backslashes and line breaks escaped.
fn escape_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            other => out.push(other),
        }
    }
    out
}

/// How the desktop reads an `Exec` value back into its arguments: the
/// string escapes first, then the quoting. For the tests, which check the
/// path survives both.
#[cfg(test)]
fn read_exec(value: &str) -> Vec<String> {
    let mut unescaped = String::new();
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            unescaped.push(c);
            continue;
        }
        match chars.next() {
            Some('s') => unescaped.push(' '),
            Some('n') => unescaped.push('\n'),
            Some('t') => unescaped.push('\t'),
            Some('r') => unescaped.push('\r'),
            Some('\\') => unescaped.push('\\'),
            Some(other) => {
                unescaped.push('\\');
                unescaped.push(other);
            }
            None => unescaped.push('\\'),
        }
    }
    let mut args = Vec::new();
    let mut current = String::new();
    let mut quoted = false;
    let mut in_arg = false;
    let mut chars = unescaped.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                quoted = !quoted;
                in_arg = true;
            }
            '\\' if quoted => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            ' ' if !quoted => {
                if in_arg {
                    args.push(std::mem::take(&mut current));
                    in_arg = false;
                }
            }
            '%' => {
                if chars.next() == Some('%') {
                    current.push('%');
                }
                in_arg = true;
            }
            other => {
                current.push(other);
                in_arg = true;
            }
        }
    }
    if in_arg {
        args.push(current);
    }
    args
}

/// Whether an entry turns the app on at login: present, and not turned off
/// with `Hidden` or `X-GNOME-Autostart-enabled=false`, as desktops do when
/// the user switches it off in their own settings.
pub fn is_on(contents: Option<&str>) -> bool {
    let Some(contents) = contents else {
        return false;
    };
    !contents.lines().any(|line| {
        let line = line.trim();
        line.eq_ignore_ascii_case("Hidden=true")
            || line.eq_ignore_ascii_case("X-GNOME-Autostart-enabled=false")
    })
}

pub fn path_in(dir: &Path) -> PathBuf {
    dir.join(FILE_NAME)
}

pub fn read(dir: &Path) -> bool {
    is_on(std::fs::read_to_string(path_in(dir)).ok().as_deref())
}

/// Turns opening at login on or off.
pub fn set(dir: &Path, on: bool, executable: &Path) -> io::Result<()> {
    let path = path_in(dir);
    if on {
        std::fs::create_dir_all(dir)?;
        std::fs::write(&path, entry(executable))
    } else {
        match std::fs::remove_file(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            other => other,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exec_of(entry: &str) -> &str {
        entry
            .lines()
            .find_map(|l| l.strip_prefix("Exec="))
            .expect("an Exec line")
    }

    #[test]
    fn the_entry_starts_the_app_in_the_background() {
        let entry = entry(Path::new("/usr/bin/inputweave-app"));
        assert_eq!(exec_of(&entry), "/usr/bin/inputweave-app --background");
        assert_eq!(quote("/opt/My Apps/iw"), "\"/opt/My Apps/iw\"");
        assert_eq!(quote("/x/100%"), "\"/x/100%%\"");
    }

    #[test]
    fn awkward_paths_survive_both_levels_of_escaping() {
        // Exec quoting escapes the backslash once; the string level doubles
        // every backslash again.
        assert_eq!(
            exec_of(&entry(Path::new("/opt/a\\b/iw"))),
            "\"/opt/a\\\\\\\\b/iw\" --background"
        );
        for path in [
            "/opt/My Apps/inputweave-app",
            "/opt/say \"hi\"/iw",
            "/opt/back\\slash/iw",
            "/opt/$HOME/iw",
            "/opt/`tick`/iw",
            "/opt/100%/iw",
            "/opt/all \" \\ $ ` % of them/iw",
        ] {
            let entry = entry(Path::new(path));
            assert_eq!(
                read_exec(exec_of(&entry)),
                vec![path.to_owned(), "--background".to_owned()],
                "{path}"
            );
        }
    }

    #[test]
    fn on_and_off() {
        let dir = tempfile::tempdir().unwrap();
        let exe = Path::new("/usr/bin/inputweave-app");
        assert!(!read(dir.path()));
        set(dir.path(), true, exe).unwrap();
        assert!(read(dir.path()));
        set(dir.path(), false, exe).unwrap();
        assert!(!read(dir.path()));
        set(dir.path(), false, exe).unwrap();
        assert!(!is_on(Some("[Desktop Entry]\nHidden=true\n")));
        assert!(!is_on(Some(
            "[Desktop Entry]\nX-GNOME-Autostart-enabled=false\n"
        )));
    }
}
