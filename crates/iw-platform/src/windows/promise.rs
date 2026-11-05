//! Clipboard content promised rather than written (delayed rendering,
//! docs/architecture.md §8): which Windows formats a promise puts on the
//! clipboard, and what each gets when an app pastes it. The content is
//! fetched once for each format asked for, and an image once for both its
//! PNG and its bitmap, kept meanwhile for at most [`IMAGE_KEEP`]; once
//! fetching failed, nothing is fetched again for the promise, but a source
//! that is only busy is asked again at the next paste. Free of FFI, so it
//! is tested on every OS.

use std::time::{Duration, Instant};

use super::clipboard_formats::{cf_html_from_html, utf16_from_text};
use crate::{ClipboardFormat, Fetch};

/// How long an image fetched for its PNG or its bitmap is kept for the
/// other: apps ask for both within moments, and it may be 64 MiB.
pub(crate) const IMAGE_KEEP: Duration = Duration::from_secs(30);

/// A Windows clipboard format a promise puts on the clipboard.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Promised {
    /// `CF_UNICODETEXT`.
    UnicodeText,
    /// "Rich Text Format".
    Rtf,
    /// "HTML Format".
    Html,
    /// "PNG".
    Png,
    /// `CF_DIB`, drawn from the PNG for apps that only take bitmaps.
    Dib,
}

impl Promised {
    /// What it is made from.
    pub(crate) fn source(self) -> ClipboardFormat {
        match self {
            Self::UnicodeText => ClipboardFormat::Text,
            Self::Rtf => ClipboardFormat::Rtf,
            Self::Html => ClipboardFormat::Html,
            Self::Png | Self::Dib => ClipboardFormat::Png,
        }
    }
}

/// The Windows formats that stand for `formats`, each once, in order: an
/// image as a PNG and as a bitmap, as a whole write puts it.
pub(crate) fn promised(formats: &[ClipboardFormat]) -> Vec<Promised> {
    let mut promised = Vec::new();
    for format in formats {
        let windows: &[Promised] = match format {
            ClipboardFormat::Text => &[Promised::UnicodeText],
            ClipboardFormat::Rtf => &[Promised::Rtf],
            ClipboardFormat::Html => &[Promised::Html],
            ClipboardFormat::Png => &[Promised::Png, Promised::Dib],
        };
        for w in windows {
            if !promised.contains(w) {
                promised.push(*w);
            }
        }
    }
    promised
}

/// The bytes Windows keeps for `format`'s content `data`, apart from a
/// bitmap: UTF-16 text with CRLF line ends and a NUL, RTF with a NUL, HTML
/// with the "HTML Format" header and a NUL, a PNG as it is.
pub(crate) fn clipboard_bytes(format: ClipboardFormat, data: &[u8]) -> Vec<u8> {
    match format {
        ClipboardFormat::Text => utf16_from_text(&String::from_utf8_lossy(data))
            .iter()
            .flat_map(|u| u.to_le_bytes())
            .collect(),
        ClipboardFormat::Rtf => {
            let mut rtf = data.to_vec();
            rtf.push(0);
            rtf
        }
        ClipboardFormat::Html => {
            let mut html = cf_html_from_html(data);
            html.push(0);
            html
        }
        ClipboardFormat::Png => data.to_vec(),
    }
}

/// What one promise has rendered so far.
#[derive(Debug, Default)]
pub(crate) struct Rendering {
    /// Fetching failed: nothing more is asked for.
    failed: bool,
    /// The image, and when it was fetched, until both its PNG and its
    /// bitmap are rendered or [`IMAGE_KEEP`] passed.
    image: Option<(Vec<u8>, Instant)>,
    rendered: Vec<Promised>,
}

impl Rendering {
    /// The bytes for `wanted`, an app pasting it at `now`: its content
    /// from `fetch`, or for an image from what was fetched for its other
    /// format, made into what Windows keeps, a bitmap by `dib`. `None` if
    /// it cannot be had, for now when `fetch` is busy.
    pub(crate) fn render(
        &mut self,
        wanted: Promised,
        now: Instant,
        fetch: impl FnOnce(ClipboardFormat) -> Fetch,
        dib: impl FnOnce(&[u8]) -> Option<Vec<u8>>,
    ) -> Option<Vec<u8>> {
        if self.failed {
            return None;
        }
        self.expire(now);
        let source = wanted.source();
        let other = match wanted {
            Promised::Png => Some(Promised::Dib),
            Promised::Dib => Some(Promised::Png),
            _ => None,
        };
        let cached = other.and_then(|_| self.image.take()).map(|(png, _)| png);
        let data = match cached {
            Some(png) => png,
            None => match fetch(source) {
                Fetch::Content(data) => data,
                Fetch::Busy => return None,
                Fetch::Unavailable => {
                    self.failed = true;
                    return None;
                }
            },
        };
        self.rendered.push(wanted);
        // Kept for the other format, if that is still to come.
        let keep = other.is_some_and(|other| !self.rendered.contains(&other));
        match wanted {
            Promised::Dib => {
                let bitmap = dib(&data);
                if keep {
                    self.image = Some((data, now));
                }
                bitmap
            }
            Promised::Png => {
                if keep {
                    self.image = Some((data.clone(), now));
                }
                Some(data)
            }
            _ => Some(clipboard_bytes(source, &data)),
        }
    }

    /// Lets go of the image if it was kept for [`IMAGE_KEEP`] by `now`.
    pub(crate) fn expire(&mut self, now: Instant) {
        if self
            .image
            .as_ref()
            .is_some_and(|(_, at)| now.duration_since(*at) >= IMAGE_KEEP)
        {
            self.image = None;
        }
    }

    /// Whether an image is kept for its other format.
    pub(crate) fn holds_image(&self) -> bool {
        self.image.is_some()
    }

    /// Lets go of the image kept for its other format.
    pub(crate) fn drop_image(&mut self) {
        self.image = None;
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    #[test]
    fn an_image_is_promised_as_a_png_and_a_bitmap() {
        use ClipboardFormat::*;
        assert_eq!(
            promised(&[Text, Png]),
            vec![Promised::UnicodeText, Promised::Png, Promised::Dib]
        );
        assert_eq!(
            promised(&[Png, Html, Png, Rtf]),
            vec![Promised::Png, Promised::Dib, Promised::Html, Promised::Rtf]
        );
        assert!(promised(&[]).is_empty());
    }

    #[test]
    fn formats_are_kept_as_a_whole_write_keeps_them() {
        let text = clipboard_bytes(ClipboardFormat::Text, "a\nb".as_bytes());
        let units: Vec<u16> = text
            .chunks(2)
            .map(|p| u16::from_le_bytes([p[0], p[1]]))
            .collect();
        assert_eq!(String::from_utf16(&units).unwrap(), "a\r\nb\0");
        assert_eq!(
            clipboard_bytes(ClipboardFormat::Rtf, b"{\\rtf1}"),
            b"{\\rtf1}\0"
        );
        let html = clipboard_bytes(ClipboardFormat::Html, b"<b>hi</b>");
        assert!(html.starts_with(b"Version:"));
        assert!(html.ends_with(b"\0"));
        assert_eq!(
            clipboard_bytes(ClipboardFormat::Png, b"\x89PNG"),
            b"\x89PNG"
        );
    }

    /// A source that counts what it is asked for, and answers with
    /// `gives` or, when that is busy or unavailable, with that.
    struct Source {
        asked: RefCell<Vec<ClipboardFormat>>,
        gives: RefCell<Fetch>,
    }

    impl Source {
        fn new(gives: bool) -> Self {
            Self {
                asked: RefCell::default(),
                gives: RefCell::new(if gives {
                    Fetch::Content(Vec::new())
                } else {
                    Fetch::Unavailable
                }),
            }
        }

        fn fetch(&self, format: ClipboardFormat) -> Fetch {
            self.asked.borrow_mut().push(format);
            match &*self.gives.borrow() {
                Fetch::Content(_) => Fetch::Content(match format {
                    ClipboardFormat::Png => b"png".to_vec(),
                    _ => b"text".to_vec(),
                }),
                other => other.clone(),
            }
        }
    }

    fn dib(png: &[u8]) -> Option<Vec<u8>> {
        Some([b"dib of ", png].concat())
    }

    fn now() -> Instant {
        Instant::now()
    }

    #[test]
    fn an_image_is_fetched_once_for_its_png_and_its_bitmap() {
        for (first, second) in [
            (Promised::Png, Promised::Dib),
            (Promised::Dib, Promised::Png),
        ] {
            let source = Source::new(true);
            let mut rendering = Rendering::default();
            let render = |rendering: &mut Rendering, wanted| {
                rendering.render(wanted, now(), |f| source.fetch(f), dib)
            };
            let expect = |wanted| match wanted {
                Promised::Png => b"png".to_vec(),
                _ => b"dib of png".to_vec(),
            };
            assert_eq!(render(&mut rendering, first), Some(expect(first)));
            assert!(rendering.holds_image(), "kept for the other format");
            assert_eq!(render(&mut rendering, second), Some(expect(second)));
            assert_eq!(*source.asked.borrow(), vec![ClipboardFormat::Png]);
            assert!(!rendering.holds_image(), "let go once both are rendered");
        }
    }

    #[test]
    fn other_formats_are_fetched_each_and_text_is_made_utf16() {
        let source = Source::new(true);
        let mut rendering = Rendering::default();
        let text = rendering
            .render(Promised::UnicodeText, now(), |f| source.fetch(f), dib)
            .unwrap();
        assert_eq!(text, clipboard_bytes(ClipboardFormat::Text, b"text"));
        rendering.render(Promised::Html, now(), |f| source.fetch(f), dib);
        assert_eq!(
            *source.asked.borrow(),
            vec![ClipboardFormat::Text, ClipboardFormat::Html]
        );
        // Nothing of an image is kept for text.
        assert!(!rendering.holds_image());
    }

    #[test]
    fn once_fetching_failed_nothing_more_is_asked_for() {
        let source = Source::new(false);
        let mut rendering = Rendering::default();
        assert_eq!(
            rendering.render(Promised::Html, now(), |f| source.fetch(f), dib),
            None
        );
        assert!(rendering.failed);
        assert_eq!(
            rendering.render(Promised::Png, now(), |f| source.fetch(f), dib),
            None
        );
        assert_eq!(
            rendering.render(Promised::UnicodeText, now(), |f| source.fetch(f), dib),
            None
        );
        assert_eq!(*source.asked.borrow(), vec![ClipboardFormat::Html]);
    }

    #[test]
    fn text_asked_for_between_an_images_formats_keeps_the_image() {
        let source = Source::new(true);
        let mut rendering = Rendering::default();
        rendering.render(Promised::Dib, now(), |f| source.fetch(f), dib);
        rendering.render(Promised::UnicodeText, now(), |f| source.fetch(f), dib);
        rendering.render(Promised::Png, now(), |f| source.fetch(f), dib);
        assert_eq!(
            *source.asked.borrow(),
            vec![ClipboardFormat::Png, ClipboardFormat::Text]
        );
    }

    #[test]
    fn a_busy_source_is_asked_again_and_is_no_failure() {
        let source = Source::new(true);
        *source.gives.borrow_mut() = Fetch::Busy;
        let mut rendering = Rendering::default();
        assert_eq!(
            rendering.render(Promised::Html, now(), |f| source.fetch(f), dib),
            None
        );
        assert!(!rendering.failed);
        *source.gives.borrow_mut() = Fetch::Content(Vec::new());
        assert!(
            rendering
                .render(Promised::Html, now(), |f| source.fetch(f), dib)
                .is_some()
        );
        assert_eq!(
            *source.asked.borrow(),
            vec![ClipboardFormat::Html, ClipboardFormat::Html]
        );
    }

    #[test]
    fn an_image_kept_for_its_other_format_is_let_go_after_a_while() {
        let source = Source::new(true);
        let mut rendering = Rendering::default();
        let start = now();
        rendering.render(Promised::Png, start, |f| source.fetch(f), dib);
        rendering.expire(start + IMAGE_KEEP / 2);
        assert!(rendering.holds_image());
        rendering.expire(start + IMAGE_KEEP);
        assert!(!rendering.holds_image());
        // Asked for later, the bitmap fetches the image again.
        let later = start + IMAGE_KEEP * 2;
        assert_eq!(
            rendering.render(Promised::Dib, later, |f| source.fetch(f), dib),
            Some(b"dib of png".to_vec())
        );
        assert_eq!(
            *source.asked.borrow(),
            vec![ClipboardFormat::Png, ClipboardFormat::Png]
        );
        assert!(!rendering.holds_image(), "both rendered now");

        // The window's timer lets go of it too.
        let mut rendering = Rendering::default();
        rendering.render(Promised::Dib, start, |f| source.fetch(f), dib);
        assert!(rendering.holds_image());
        rendering.drop_image();
        assert!(!rendering.holds_image());
    }

    #[test]
    fn a_bitmap_that_cannot_be_drawn_is_no_failure_to_fetch() {
        let source = Source::new(true);
        let mut rendering = Rendering::default();
        assert_eq!(
            rendering.render(Promised::Dib, now(), |f| source.fetch(f), |_| None),
            None
        );
        assert!(!rendering.failed);
        assert_eq!(
            rendering.render(Promised::Png, now(), |f| source.fetch(f), dib),
            Some(b"png".to_vec())
        );
        assert_eq!(*source.asked.borrow(), vec![ClipboardFormat::Png]);
    }
}
