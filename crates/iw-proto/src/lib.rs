//! Protobuf messages generated from `proto/`, and the framing used to carry
//! them over byte streams.

pub mod frame;
pub mod network;

pub mod peer {
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/inputweave.peer.v1.rs"));
    }
}

pub mod app {
    // `Status` is far larger than the other responses and events. Boxing
    // it would change every client's code for one allocation fewer per
    // message, which IPC does not need.
    #[allow(clippy::large_enum_variant)]
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/inputweave.app.v1.rs"));
    }
}

/// Messages between the processes on Windows: the agent's requests to the
/// service, and the agent and the broker.
pub mod service {
    pub mod v1 {
        include!(concat!(env!("OUT_DIR"), "/inputweave.service.v1.rs"));
    }

    /// The most clipboard content the broker reads or writes at once, all
    /// formats together, in bytes.
    pub const MAX_CLIPBOARD: u64 = 64 * 1024 * 1024;

    /// How long an app pasting promised content on Windows waits for it:
    /// the broker answers `WM_RENDERFORMAT` meanwhile, and the clipboard
    /// stays locked for other apps, so this is short. The agent stops
    /// waiting for the content then too.
    pub const RENDER_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

    /// The most files copied in File Explorer the broker reports with the
    /// clipboard (`ClipboardContents.files`), as many as a batch's items,
    /// and the most bytes their paths take together.
    pub const MAX_COPIED_FILES: usize = 1_000;
    pub const MAX_COPIED_PATH_BYTES: usize = 512 * 1024;

    /// Whether `path`, of a file copied in File Explorer, may go to the
    /// agent and be offered: on Windows an absolute path on a drive
    /// (`C:\…`) or a share (`\\server\share\…`), never a device path
    /// (`\\?\`, `\\.\`); elsewhere, where only tests run a broker, an
    /// absolute path. Never with empty, `.` or `..` parts, NULs, or the
    /// other platform's separator. The broker keeps to it, and the agent
    /// checks it again.
    pub fn copied_path(path: &str) -> bool {
        if cfg!(windows) {
            windows_copied_path(path)
        } else {
            unix_copied_path(path)
        }
    }

    fn windows_copied_path(path: &str) -> bool {
        if path.contains(['\0', '/']) {
            return false;
        }
        let rest = if let Some(share) = path.strip_prefix(r"\\") {
            // `\\server\share\…`; `\\?\` and `\\.\` are device paths.
            let mut parts = share.splitn(3, '\\');
            match (parts.next(), parts.next(), parts.next()) {
                (Some(server), Some(share), Some(rest))
                    if !server.is_empty()
                        && !share.is_empty()
                        && server != "?"
                        && server != "." =>
                {
                    rest
                }
                _ => return false,
            }
        } else {
            let bytes = path.as_bytes();
            if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || &bytes[1..3] != b":\\" {
                return false;
            }
            &path[3..]
        };
        plain_parts(rest, '\\')
    }

    fn unix_copied_path(path: &str) -> bool {
        !path.contains(['\0', '\\'])
            && path
                .strip_prefix('/')
                .is_some_and(|rest| plain_parts(rest, '/'))
    }

    /// Whether `rest` is one or more parts separated by `separator`, none
    /// of them empty, `.` or `..`.
    fn plain_parts(rest: &str, separator: char) -> bool {
        rest.split(separator)
            .all(|part| !part.is_empty() && part != "." && part != "..")
    }

    /// Whether `paths` keep to the limits of `ClipboardContents.files`.
    pub fn within_copied_limits(paths: &[String]) -> bool {
        paths.len() <= MAX_COPIED_FILES
            && paths.iter().map(String::len).sum::<usize>() <= MAX_COPIED_PATH_BYTES
    }

    /// The paths of files copied in File Explorer that may go to the agent,
    /// in order: those [`copied_path`] takes, at most [`MAX_COPIED_FILES`]
    /// taking at most [`MAX_COPIED_PATH_BYTES`] together.
    pub fn copied_paths(paths: impl IntoIterator<Item = String>) -> Vec<String> {
        let mut kept = Vec::new();
        let mut bytes = 0;
        for path in paths {
            if kept.len() >= MAX_COPIED_FILES {
                break;
            }
            if !copied_path(&path) {
                continue;
            }
            if bytes + path.len() > MAX_COPIED_PATH_BYTES {
                break;
            }
            bytes += path.len();
            kept.push(path);
        }
        kept
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn only_paths_on_drives_and_shares_are_taken_on_windows() {
            for good in [r"C:\Users\pc\報告 v3.pdf", r"\\nas\share\photos", r"d:\x"] {
                assert!(windows_copied_path(good), "{good}");
            }
            for bad in [
                r"\\?\C:\Users\pc\a.txt",
                r"\\.\PhysicalDrive0",
                r"\\?\UNC\nas\share\a",
                r"Users\pc\a.txt",
                r"C:a.txt",
                r"C:\",
                r"C:\Users\..\Windows\win.ini",
                r"C:\Users\.\pc",
                r"C:\Users\\pc",
                "C:/Users/pc/a.txt",
                r"\\nas",
                r"\\nas\share",
                r"\\\share\a",
                "C:\\a\0b",
            ] {
                assert!(!windows_copied_path(bad), "{bad}");
            }
        }

        #[test]
        fn only_absolute_paths_are_taken_elsewhere() {
            assert!(unix_copied_path("/Users/pc/report.txt"));
            for bad in [
                "relative/a",
                "/",
                "/x/../etc/passwd",
                "/y/./z",
                "/a//b",
                r"/a\b",
                "/a\0b",
            ] {
                assert!(!unix_copied_path(bad), "{bad}");
            }
        }

        #[test]
        fn as_many_copied_paths_as_a_batch_holds_are_taken() {
            let at = |name: &str| {
                if cfg!(windows) {
                    format!(r"C:\{name}")
                } else {
                    format!("/{name}")
                }
            };
            let many = (0..MAX_COPIED_FILES + 5).map(|i| at(&format!("{i}.txt")));
            let taken = copied_paths(many);
            assert_eq!(taken.len(), MAX_COPIED_FILES);
            assert!(within_copied_limits(&taken));
            assert!(!within_copied_limits(
                &[taken.clone(), vec![at("one more")]].concat()
            ));
            // Long paths stop at the byte limit, in order; bad ones are
            // passed over.
            let long = at(&"a".repeat(200 * 1024));
            let taken = copied_paths([at("../x"), long.clone(), long.clone(), long, at("b")]);
            assert_eq!(taken.len(), 2);
            assert!(within_copied_limits(&taken));
        }
    }
}

/// Peer protocol versions this build can speak, exchanged in `peer::v1::Hello`.
/// Version 2 adds clipboards offered rather than sent (`ClipboardOffer`,
/// `ClipboardPull`); a session at version 1 gets neither. Version 3 adds
/// files in offers (`ClipboardOffer.items`, whose parts may then be
/// empty, `ClipboardFilesPull`) and `CLIPBOARD_SKIP_BUSY`; a session at
/// version 2 gets offers without files. Version 4 adds drags of files on a
/// controlled device (`RemoteDrag`, `DragOffer.source_device_id`,
/// `DragAccept.device_id`); a session below it gets no `RemoteDrag`, and is
/// offered no drag whose files are on another device. Version 5 adds
/// relative mode (`Input.relative`, `RelativeMotion`, `PointerCaptured`); a
/// device in a session below it cannot be put in relative mode. Version 6
/// adds licenses passed between paired devices (`LicenseInfo`); a session
/// below it gets none.
pub const PEER_PROTOCOL_MIN: u32 = 1;
pub const PEER_PROTOCOL_MAX: u32 = 6;

/// IPC version exchanged in `app::v1::ClientHello` / `AgentHello`.
pub const IPC_VERSION: u32 = 1;
