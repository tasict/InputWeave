//! How clipboard content is laid out on Linux, X11 and Wayland alike, and
//! how it maps to what devices exchange (`ClipboardFormat`). Both offer
//! content as a list of targets, MIME types mostly, with a few X11 names
//! that Wayland toolkits offer too. Free of FFI, so it is tested on every
//! OS; the X11 and Wayland clipboards move the bytes.
//!
//! - Text: `UTF8_STRING` and `text/plain;charset=utf-8` are UTF-8, as is
//!   plain `text/plain` in practice; `STRING` is read only when nothing
//!   better is offered. On X11 `STRING` is Latin-1, and offered so with
//!   `TEXT`. Wayland has no such type: apps offering it put UTF-8 there
//!   (wl-copy offers `TEXT`, `STRING` and `UTF8_STRING` with the same
//!   UTF-8), so it is read as UTF-8 when it is, and neither is offered
//!   (`Wire`). Devices exchange UTF-8 with LF, which is what Linux apps
//!   use: CRLF read here becomes LF, as on Windows, and the NULs some apps
//!   end text with go.
//! - HTML: `text/html`, UTF-8; Firefox has put it as UTF-16 with a byte
//!   order mark, which is read too.
//! - RTF: `text/rtf` or `application/rtf`, as it is.
//! - Images: `image/png` as it is.
//! - Files: `x-special/gnome-copied-files` (GNOME's file managers: `copy`
//!   or `cut`, then a URI a line) or `text/uri-list` (RFC 2483: a URI a
//!   line, CRLF, `#` comments). Only `file:` URIs of this computer count.
//! - Content offering `x-kde-passwordManagerHint` with the value `secret`
//!   is sensitive: KeePassXC and other password managers mark what they
//!   copy so, and KDE's Klipper and CopyQ leave it out of their history.
//!   InputWeave puts the same mark on files it promises, so those
//!   clipboard managers do not read them, which would fetch the files.

use std::borrow::Cow;
use std::ffi::OsString;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::{ClipboardFormat, PromisedFile};

pub(crate) const PASSWORD_HINT: &str = "x-kde-passwordManagerHint";
/// What `PASSWORD_HINT` says of content not to be shared.
pub(crate) const SECRET: &[u8] = b"secret";
pub(crate) const URI_LIST: &str = "text/uri-list";
pub(crate) const GNOME_COPIED_FILES: &str = "x-special/gnome-copied-files";
/// X11's Latin-1 text.
pub(crate) const LATIN1: &str = "STRING";

/// Which display server the targets are of: text differs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Wire {
    X11,
    Wayland,
}

/// The targets `format` is read from, best first.
pub(crate) fn read_targets(format: ClipboardFormat) -> &'static [&'static str] {
    match format {
        ClipboardFormat::Text => &[
            "UTF8_STRING",
            "text/plain;charset=utf-8",
            "text/plain;charset=UTF-8",
            "text/plain",
            LATIN1,
        ],
        ClipboardFormat::Html => &["text/html"],
        ClipboardFormat::Rtf => &["text/rtf", "application/rtf"],
        ClipboardFormat::Png => &["image/png"],
    }
}

/// The targets content in `format` is offered as over `wire`: on Wayland,
/// text as UTF-8 only. (GTK on Wayland offers `text/plain;charset=utf-8`
/// and `text/plain`; `UTF8_STRING` is for apps that ask by X11's name.)
pub(crate) fn offered_targets(wire: Wire, format: ClipboardFormat) -> &'static [&'static str] {
    match (wire, format) {
        (Wire::Wayland, ClipboardFormat::Text) => {
            &["text/plain;charset=utf-8", "UTF8_STRING", "text/plain"]
        }
        _ => write_targets(format),
    }
}

/// The targets content in `format` is offered as on X11, and answered as
/// on either.
pub(crate) fn write_targets(format: ClipboardFormat) -> &'static [&'static str] {
    match format {
        ClipboardFormat::Text => &[
            "UTF8_STRING",
            "text/plain;charset=utf-8",
            "text/plain",
            LATIN1,
            "TEXT",
        ],
        ClipboardFormat::Html => &["text/html"],
        ClipboardFormat::Rtf => &["text/rtf", "application/rtf"],
        ClipboardFormat::Png => &["image/png"],
    }
}

/// The format a target asked of content InputWeave offers holds.
pub(crate) fn format_of(target: &str) -> Option<ClipboardFormat> {
    ClipboardFormat::ALL
        .into_iter()
        .find(|f| write_targets(*f).contains(&target))
}

/// The targets `offered` has for `format`, best first: the next is tried
/// when one is refused or holds nothing.
pub(crate) fn choices<'a, 'b>(
    format: ClipboardFormat,
    offered: &'b [&'a str],
) -> impl Iterator<Item = &'a str> + 'b {
    let mut seen: Vec<&'a str> = Vec::new();
    read_targets(format).iter().filter_map(move |t| {
        // Once each, though two names of it differ only in case.
        let found = offered
            .iter()
            .find(|o| o.eq_ignore_ascii_case(t))
            .copied()?;
        if seen.contains(&found) {
            return None;
        }
        seen.push(found);
        Some(found)
    })
}

/// What `data`, read as `target` over `wire`, holds in `format`, as
/// devices exchange it: `None` when it holds nothing of it.
pub(crate) fn decode(
    wire: Wire,
    format: ClipboardFormat,
    target: &str,
    data: &[u8],
) -> Option<Vec<u8>> {
    let latin1 = |data: &[u8]| {
        data.iter()
            .map(|&b| char::from(b))
            .collect::<String>()
            .replace("\r\n", "\n")
    };
    let decoded = match format {
        ClipboardFormat::Text if target == LATIN1 => match wire {
            Wire::X11 => latin1(trim_nuls(data)),
            Wire::Wayland => match std::str::from_utf8(trim_nuls(data)) {
                Ok(text) => text.replace("\r\n", "\n"),
                Err(_) => latin1(trim_nuls(data)),
            },
        },
        // LF, as Windows' CRLF arrives here too (Windows makes it LF).
        ClipboardFormat::Text => String::from_utf8_lossy(trim_nuls(data)).replace("\r\n", "\n"),
        // Its NULs are trimmed once decoded: UTF-16 has them in its
        // characters.
        ClipboardFormat::Html => html(data),
        ClipboardFormat::Rtf => {
            let data = trim_nuls(data);
            return (!data.is_empty()).then(|| data.to_vec());
        }
        ClipboardFormat::Png => {
            return data
                .starts_with(b"\x89PNG\r\n\x1a\n")
                .then(|| data.to_vec());
        }
    };
    (!decoded.is_empty()).then(|| decoded.into_bytes())
}

/// `data` in `format` as it is offered as `target`: Latin-1 for `STRING`,
/// with `?` for what it cannot hold.
pub(crate) fn encode<'a>(format: ClipboardFormat, target: &str, data: &'a [u8]) -> Cow<'a, [u8]> {
    if format == ClipboardFormat::Text && target == LATIN1 {
        return Cow::Owned(
            String::from_utf8_lossy(data)
                .chars()
                .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
                .collect(),
        );
    }
    Cow::Borrowed(data)
}

/// HTML as UTF-8: a byte order mark says UTF-16 (as Firefox has put it)
/// or UTF-8; without one, UTF-16 shows as NULs between ASCII characters.
fn html(data: &[u8]) -> String {
    let utf16 = |data: &[u8], big: bool| {
        let units: Vec<u16> = data
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&p| {
                if big {
                    u16::from_be_bytes(p)
                } else {
                    u16::from_le_bytes(p)
                }
            })
            .collect();
        String::from_utf16_lossy(&units)
    };
    match data {
        [0xff, 0xfe, rest @ ..] => utf16(rest, false),
        [0xfe, 0xff, rest @ ..] => utf16(rest, true),
        [0xef, 0xbb, 0xbf, rest @ ..] => String::from_utf8_lossy(rest).into_owned(),
        [first, 0, ..] if *first != 0 && data.len().is_multiple_of(2) => utf16(data, false),
        _ => String::from_utf8_lossy(data).into_owned(),
    }
    .trim_end_matches('\0')
    .to_owned()
}

/// `data` without the NULs at its end.
fn trim_nuls(data: &[u8]) -> &[u8] {
    let end = data.iter().rposition(|&b| b != 0).map_or(0, |i| i + 1);
    &data[..end]
}

/// Whether `PASSWORD_HINT`'s value says the content is not to be shared.
pub(crate) fn is_secret(hint: &[u8]) -> bool {
    trim_nuls(hint).trim_ascii() == SECRET
}

/// The local paths of the files in a list read as `target`
/// (`GNOME_COPIED_FILES` or `URI_LIST`), in order; URIs of other schemes
/// or computers are left out.
pub(crate) fn files_of(target: &str, data: &[u8]) -> Vec<PathBuf> {
    let text = String::from_utf8_lossy(trim_nuls(data));
    let mut lines = text.lines().map(str::trim).filter(|l| !l.is_empty());
    if target == GNOME_COPIED_FILES {
        // The first line says whether they were copied or cut.
        lines.next();
    }
    lines
        .filter(|l| !l.starts_with('#'))
        .filter_map(path_of_uri)
        .collect()
}

/// The local path a `file:` URI names: `file:///path` or
/// `file://localhost/path`, percent-encoded.
pub(crate) fn path_of_uri(uri: &str) -> Option<PathBuf> {
    let rest = uri
        .strip_prefix("file://")
        .or_else(|| uri.strip_prefix("FILE://"))?;
    let path = rest.strip_prefix("localhost").unwrap_or(rest);
    if !path.starts_with('/') {
        return None;
    }
    let bytes = percent_decode(path)?;
    Some(PathBuf::from(OsString::from_vec(bytes)))
}

fn percent_decode(text: &str) -> Option<Vec<u8>> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            // Two hex digits; `from_str_radix` alone would take a sign.
            let hex = bytes.get(i + 1..i + 3)?;
            if !hex.iter().all(u8::is_ascii_hexdigit) {
                return None;
            }
            out.push(u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Some(out)
}

/// The `file:` URI of an absolute local path, every byte but unreserved
/// ones and `/` percent-encoded.
pub(crate) fn file_uri(path: &Path) -> String {
    let mut uri = String::from("file://");
    for &b in path.as_os_str().as_bytes() {
        if b.is_ascii_alphanumeric() || b"/-._~".contains(&b) {
            uri.push(char::from(b));
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri
}

/// `paths` as a `text/uri-list`.
pub(crate) fn uri_list(paths: &[PathBuf]) -> Vec<u8> {
    paths
        .iter()
        .map(|p| file_uri(p) + "\r\n")
        .collect::<String>()
        .into_bytes()
}

/// `paths` as GNOME's file managers copy them.
pub(crate) fn gnome_copied_files(paths: &[PathBuf]) -> Vec<u8> {
    std::iter::once("copy".to_owned())
        .chain(paths.iter().map(|p| file_uri(p)))
        .collect::<Vec<_>>()
        .join("\n")
        .into_bytes()
}

/// The names of promised files, a line each, as the text of a promise of
/// files that has no text of its own.
pub(crate) fn names(files: &[PromisedFile]) -> Vec<u8> {
    files
        .iter()
        .map(|f| f.name.as_str())
        .collect::<Vec<_>>()
        .join("\n")
        .into_bytes()
}

/// A transfer received a part at a time (X11's INCR), up to a limit.
#[derive(Debug)]
pub(crate) struct Incoming {
    data: Vec<u8>,
    limit: usize,
}

/// What a part of an `Incoming` transfer made of it.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Received {
    /// More is to come.
    More,
    /// It all came: the empty part ends it.
    Done(Vec<u8>),
    /// It grew past the limit, to this many bytes so far.
    TooLarge(usize),
}

impl Incoming {
    pub(crate) fn new(limit: usize) -> Self {
        Self {
            data: Vec::new(),
            limit,
        }
    }

    pub(crate) fn part(&mut self, part: &[u8]) -> Received {
        if part.is_empty() {
            return Received::Done(std::mem::take(&mut self.data));
        }
        if self.data.len() + part.len() > self.limit {
            return Received::TooLarge(self.data.len() + part.len());
        }
        self.data.extend_from_slice(part);
        Received::More
    }
}

/// A transfer sent a part at a time (X11's INCR): each part goes once the
/// receiver took the last, and an empty part ends it.
#[derive(Debug)]
pub(crate) struct Outgoing {
    data: Arc<Vec<u8>>,
    sent: usize,
    part: usize,
    ended: bool,
}

impl Outgoing {
    pub(crate) fn new(data: Arc<Vec<u8>>, part: usize) -> Self {
        Self {
            data,
            sent: 0,
            part: part.max(1),
            ended: false,
        }
    }

    /// The next part, empty for the last; `None` once that went.
    pub(crate) fn next_part(&mut self) -> Option<&[u8]> {
        if self.ended {
            return None;
        }
        let start = self.sent;
        let end = (start + self.part).min(self.data.len());
        self.sent = end;
        if start == end {
            self.ended = true;
        }
        Some(&self.data[start..end])
    }

    pub(crate) fn len(&self) -> usize {
        self.data.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_is_read_from_the_best_target_offered() {
        let offered = [
            "TARGETS",
            "STRING",
            "text/plain;charset=utf-8",
            "UTF8_STRING",
        ];
        let all = |format, offered: &[&'static str]| choices(format, offered).collect::<Vec<_>>();
        assert_eq!(
            all(ClipboardFormat::Text, &offered),
            ["UTF8_STRING", "text/plain;charset=utf-8", "STRING"]
        );
        assert_eq!(all(ClipboardFormat::Text, &["STRING", "TEXT"]), ["STRING"]);
        assert!(all(ClipboardFormat::Html, &offered).is_empty());
        assert_eq!(
            all(ClipboardFormat::Rtf, &["APPLICATION/RTF"]),
            ["APPLICATION/RTF"]
        );
    }

    #[test]
    fn text_comes_as_utf8_without_nuls_and_latin1_is_converted() {
        let decode = |format, target: &str, data: &[u8]| decode(Wire::X11, format, target, data);
        let utf8 = decode(
            ClipboardFormat::Text,
            "UTF8_STRING",
            "鍵鼠\n共享\0".as_bytes(),
        );
        assert_eq!(utf8.unwrap(), "鍵鼠\n共享".as_bytes());
        // CRLF becomes LF, as Windows has it; a CR alone stays.
        let crlf = decode(ClipboardFormat::Text, "UTF8_STRING", b"a\r\nb\rc\n");
        assert_eq!(crlf.unwrap(), b"a\nb\rc\n");
        let crlf = decode(ClipboardFormat::Text, LATIN1, b"a\r\nb");
        assert_eq!(crlf.unwrap(), b"a\nb");
        let latin1 = decode(ClipboardFormat::Text, LATIN1, b"caf\xe9");
        assert_eq!(latin1.unwrap(), "café".as_bytes());
        // On X11, STRING is Latin-1 whatever it holds.
        let x11 = decode(ClipboardFormat::Text, LATIN1, "鍵".as_bytes()).unwrap();
        assert_ne!(x11, "鍵".as_bytes());
        assert_eq!(decode(ClipboardFormat::Text, "UTF8_STRING", b"\0\0"), None);
        assert_eq!(
            &*encode(ClipboardFormat::Text, LATIN1, "café 鍵".as_bytes()),
            b"caf\xe9 ?"
        );
        assert!(matches!(
            encode(ClipboardFormat::Text, "UTF8_STRING", "鍵".as_bytes()),
            Cow::Borrowed(b) if b == "鍵".as_bytes()
        ));
    }

    #[test]
    fn wayland_text_is_utf8_only() {
        // Read: STRING as UTF-8 where it is, as wl-copy puts it.
        let wayland = |data: &[u8]| decode(Wire::Wayland, ClipboardFormat::Text, LATIN1, data);
        assert_eq!(
            wayland("鍵鼠 café\r\n\0".as_bytes()).unwrap(),
            "鍵鼠 café\n".as_bytes()
        );
        assert_eq!(wayland(b"caf\xe9").unwrap(), "café".as_bytes());
        // Offered: never as STRING or TEXT, whose bytes would be Latin-1.
        let text = offered_targets(Wire::Wayland, ClipboardFormat::Text);
        assert!(!text.contains(&LATIN1) && !text.contains(&"TEXT"));
        assert!(text.contains(&"text/plain;charset=utf-8") && text.contains(&"UTF8_STRING"));
        for target in text {
            assert_eq!(format_of(target), Some(ClipboardFormat::Text));
        }
        assert_eq!(
            offered_targets(Wire::X11, ClipboardFormat::Text),
            write_targets(ClipboardFormat::Text)
        );
        assert_eq!(
            offered_targets(Wire::Wayland, ClipboardFormat::Png),
            ["image/png"]
        );
    }

    #[test]
    fn html_in_utf16_is_read_as_utf8() {
        let le: Vec<u8> = [0xff, 0xfe]
            .into_iter()
            .chain("<b>鍵</b>".encode_utf16().flat_map(u16::to_le_bytes))
            .collect();
        assert_eq!(
            decode(Wire::X11, ClipboardFormat::Html, "text/html", &le).unwrap(),
            "<b>鍵</b>".as_bytes()
        );
        let be: Vec<u8> = [0xfe, 0xff]
            .into_iter()
            .chain("<i>x</i>".encode_utf16().flat_map(u16::to_be_bytes))
            .collect();
        assert_eq!(
            decode(Wire::X11, ClipboardFormat::Html, "text/html", &be).unwrap(),
            b"<i>x</i>"
        );
        let bare: Vec<u8> = "<p>hi</p>"
            .encode_utf16()
            .flat_map(u16::to_le_bytes)
            .collect();
        assert_eq!(
            decode(Wire::X11, ClipboardFormat::Html, "text/html", &bare).unwrap(),
            b"<p>hi</p>"
        );
        assert_eq!(
            decode(
                Wire::X11,
                ClipboardFormat::Html,
                "text/html",
                b"\xef\xbb\xbf<p>x</p>"
            )
            .unwrap(),
            b"<p>x</p>"
        );
        assert_eq!(
            decode(Wire::X11, ClipboardFormat::Html, "text/html", b"<p>x</p>").unwrap(),
            b"<p>x</p>"
        );
    }

    #[test]
    fn rtf_loses_its_nuls_and_png_must_be_png() {
        assert_eq!(
            decode(Wire::X11, ClipboardFormat::Rtf, "text/rtf", b"{\\rtf1 x}\0").unwrap(),
            b"{\\rtf1 x}"
        );
        assert!(
            decode(
                Wire::X11,
                ClipboardFormat::Png,
                "image/png",
                b"\x89PNG\r\n\x1a\nIHDR"
            )
            .is_some()
        );
        assert_eq!(
            decode(Wire::X11, ClipboardFormat::Png, "image/png", b"GIF89a"),
            None
        );
    }

    #[test]
    fn targets_offered_map_back_to_their_formats() {
        for format in ClipboardFormat::ALL {
            for target in write_targets(format) {
                assert_eq!(format_of(target), Some(format), "{target}");
            }
        }
        assert_eq!(format_of(URI_LIST), None);
        assert_eq!(format_of("TARGETS"), None);
    }

    #[test]
    fn the_password_hint_says_secret() {
        assert!(is_secret(b"secret"));
        assert!(is_secret(b"secret\n"));
        assert!(is_secret(b"secret\0"));
        assert!(!is_secret(b"public"));
        assert!(!is_secret(b""));
    }

    #[test]
    fn copied_files_are_local_paths() {
        let gnome = b"copy\nfile:///home/me/a%20b.txt\nfile:///home/me/%E9%8D%B5\n";
        assert_eq!(
            files_of(GNOME_COPIED_FILES, gnome),
            [
                PathBuf::from("/home/me/a b.txt"),
                PathBuf::from("/home/me/鍵")
            ]
        );
        let list = b"# from Dolphin\r\nfile:///tmp/x\r\nfile://localhost/tmp/y\r\nhttps://example.com/z\r\nfile://other/tmp/w\r\nsmb://nas/share\r\n";
        assert_eq!(
            files_of(URI_LIST, list),
            [PathBuf::from("/tmp/x"), PathBuf::from("/tmp/y")]
        );
        assert!(files_of(URI_LIST, b"file:///bad%zz").is_empty());
        // A sign is no hex digit.
        assert!(files_of(URI_LIST, b"file:///tmp/a%+fb").is_empty());
        assert!(files_of(GNOME_COPIED_FILES, b"cut").is_empty());
    }

    #[test]
    fn files_are_offered_as_uris_both_ways() {
        let paths = vec![
            PathBuf::from("/home/me/My Files/鍵.txt"),
            PathBuf::from("/tmp/100%"),
        ];
        let list = uri_list(&paths);
        assert_eq!(
            String::from_utf8(list.clone()).unwrap(),
            "file:///home/me/My%20Files/%E9%8D%B5.txt\r\nfile:///tmp/100%25\r\n"
        );
        assert_eq!(files_of(URI_LIST, &list), paths);
        let gnome = gnome_copied_files(&paths);
        assert!(gnome.starts_with(b"copy\nfile:///home/"));
        assert_eq!(files_of(GNOME_COPIED_FILES, &gnome), paths);
        // Bytes that are not UTF-8 survive too.
        let odd = vec![PathBuf::from(OsString::from_vec(b"/tmp/\xff".to_vec()))];
        assert_eq!(files_of(URI_LIST, &uri_list(&odd)), odd);
    }

    #[test]
    fn promised_files_are_named_a_line_each() {
        let files = [
            PromisedFile {
                name: "a.txt".into(),
                folder: false,
            },
            PromisedFile {
                name: "Photos".into(),
                folder: true,
            },
        ];
        assert_eq!(names(&files), b"a.txt\nPhotos");
    }

    #[test]
    fn parts_come_in_until_an_empty_one_or_the_limit() {
        let mut incoming = Incoming::new(6);
        assert_eq!(incoming.part(b"abc"), Received::More);
        assert_eq!(incoming.part(b"de"), Received::More);
        assert_eq!(incoming.part(b""), Received::Done(b"abcde".to_vec()));
        let mut incoming = Incoming::new(4);
        assert_eq!(incoming.part(b"abc"), Received::More);
        assert_eq!(incoming.part(b"de"), Received::TooLarge(5));
    }

    #[test]
    fn parts_go_out_until_an_empty_one() {
        let mut outgoing = Outgoing::new(Arc::new(b"abcde".to_vec()), 2);
        assert_eq!(outgoing.len(), 5);
        assert_eq!(outgoing.next_part(), Some(&b"ab"[..]));
        assert_eq!(outgoing.next_part(), Some(&b"cd"[..]));
        assert_eq!(outgoing.next_part(), Some(&b"e"[..]));
        assert_eq!(outgoing.next_part(), Some(&b""[..]));
        assert_eq!(outgoing.next_part(), None);
    }
}
