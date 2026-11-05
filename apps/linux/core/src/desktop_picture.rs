//! The checks on a desktop picture (`Screen.preview_jpeg`) before the app
//! hands it to a decoder, and the cache of decoded ones, as the Windows
//! app's `DesktopPicture.cs`. Other computers send these bytes, so nothing
//! is assumed about them: only a JPEG of a sensible size, whose frame
//! header says a sensible picture size, gets decoded at all.

use std::collections::HashMap;

/// The background service passes on previews of at most 96 KB; anything
/// far larger is not one.
pub const MAX_BYTES: usize = 512 * 1024;

/// Previews are at most 320 pixels on their long edge. A JPEG claiming far
/// more would take memory out of all proportion once drawn.
pub const MAX_PIXELS: u32 = 4096;

/// Whether `data` is worth decoding: a JPEG within the limits.
pub fn acceptable(data: &[u8]) -> bool {
    data.len() <= MAX_BYTES
        && size(data)
            .is_some_and(|(w, h)| (1..=MAX_PIXELS).contains(&w) && (1..=MAX_PIXELS).contains(&h))
}

/// The picture size a JPEG's frame header declares, or `None` when the
/// bytes are not a JPEG or end before the frame header.
pub fn size(data: &[u8]) -> Option<(u32, u32)> {
    // SOI, then the first marker.
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 || data[2] != 0xFF {
        return None;
    }
    let read16 = |at: usize| -> Option<u32> {
        Some(u32::from(*data.get(at)?) << 8 | u32::from(*data.get(at + 1)?))
    };
    let mut at = 2;
    while at < data.len() {
        if data[at] != 0xFF {
            return None;
        }
        // Fill bytes may precede a marker.
        while at < data.len() && data[at] == 0xFF {
            at += 1;
        }
        let marker = *data.get(at)?;
        at += 1;
        // Markers without a segment.
        if marker == 0x01 || (0xD0..=0xD7).contains(&marker) {
            continue;
        }
        // The image data or its end before any frame header.
        if marker == 0xD9 || marker == 0xDA || at + 2 > data.len() {
            return None;
        }
        let length = read16(at)? as usize;
        if length < 2 || at + length > data.len() {
            return None;
        }
        // SOF0–SOF15, except DHT (C4), JPG (C8) and DAC (CC).
        if (0xC0..=0xCF).contains(&marker) && !matches!(marker, 0xC4 | 0xC8 | 0xCC) {
            if length < 7 {
                return None;
            }
            let height = read16(at + 3)?;
            let width = read16(at + 5)?;
            return Some((width, height));
        }
        at += length;
    }
    None
}

/// Far more screens than any group of computers has; past it, the cache
/// starts over rather than keep screens that are long gone.
pub const CAPACITY: usize = 64;

struct Entry<T> {
    data: Vec<u8>,
    picture: Option<T>,
}

/// Decoded desktop pictures by screen, so a tile decodes its picture once
/// rather than on every redraw. A screen's entry is replaced when its bytes
/// change; empty bytes or ones that do not pass [`acceptable`] give no
/// picture. Screen ids come from each computer, so the key is both.
pub struct PictureCache<T> {
    entries: HashMap<(String, String), Entry<T>>,
    /// How many times bytes were decoded, for tests.
    pub decodes: usize,
}

impl<T> Default for PictureCache<T> {
    fn default() -> Self {
        PictureCache {
            entries: HashMap::new(),
            decodes: 0,
        }
    }
}

impl<T: Clone> PictureCache<T> {
    pub fn picture(
        &mut self,
        device: &str,
        screen: &str,
        data: &[u8],
        decode: impl FnOnce(&[u8]) -> Option<T>,
    ) -> Option<T> {
        let key = (device.to_owned(), screen.to_owned());
        if let Some(entry) = self.entries.get(&key)
            && entry.data == data
        {
            return entry.picture.clone();
        }
        if data.is_empty() {
            self.entries.remove(&key);
            return None;
        }
        if self.entries.len() >= CAPACITY {
            self.entries.clear();
        }
        let picture = if acceptable(data) {
            self.decodes += 1;
            decode(data)
        } else {
            None
        };
        self.entries.insert(
            key,
            Entry {
                data: data.to_vec(),
                picture: picture.clone(),
            },
        );
        picture
    }

    /// Forgets every picture, as when the theme changes how they are drawn.
    pub fn clear(&mut self) {
        self.entries.clear();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// A JPEG's start: SOI, an APP0 segment, then a baseline frame header.
    pub(crate) fn jpeg(width: u16, height: u16, padding: usize) -> Vec<u8> {
        let mut bytes = vec![0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10];
        bytes.extend_from_slice(b"JFIF\0");
        bytes.extend_from_slice(&[0; 9]);
        bytes.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11, 0x08]);
        bytes.extend_from_slice(&height.to_be_bytes());
        bytes.extend_from_slice(&width.to_be_bytes());
        bytes.push(0x03);
        bytes.extend_from_slice(&[0; 9]);
        bytes.extend(std::iter::repeat_n(0, padding));
        bytes
    }

    #[test]
    fn reads_the_size_from_the_frame_header() {
        assert_eq!(size(&jpeg(320, 180, 0)), Some((320, 180)));
        assert!(acceptable(&jpeg(320, 180, 0)));
        assert!(acceptable(&jpeg(90, 320, 0)));
    }

    #[test]
    fn refuses_what_is_not_a_picture() {
        assert!(!acceptable(&[]));
        let noise: Vec<u8> = (0..200u32).map(|i| (i * 37) as u8).collect();
        assert!(!acceptable(&noise));
        // A JPEG's first bytes, then noise.
        let mut start = vec![0xFF, 0xD8, 0xFF, 0xE0];
        start.extend_from_slice(&noise);
        assert!(!acceptable(&start));
        // Cut off before the frame header.
        assert!(!acceptable(&jpeg(320, 180, 0)[..20]));
        // A PNG.
        assert!(!acceptable(&[
            0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A
        ]));
        // Too many pixels, none, or too many bytes.
        assert!(!acceptable(&jpeg(5000, 100, 0)));
        assert!(!acceptable(&jpeg(0, 100, 0)));
        assert!(!acceptable(&jpeg(320, 180, MAX_BYTES)));
    }

    #[test]
    fn decodes_each_screens_picture_once_until_it_changes() {
        let mut cache = PictureCache::<String>::default();
        let decode = |bytes: &[u8]| Some(format!("picture of {} bytes", bytes.len()));
        let first = jpeg(320, 180, 0);
        let second = jpeg(320, 200, 1);

        let picture = cache.picture("mac", "builtin", &first, decode);
        assert!(picture.is_some());
        assert_eq!(cache.picture("mac", "builtin", &first, |_| None), picture);
        assert_eq!(cache.decodes, 1);
        // Another computer's screen of the same id is its own picture.
        assert!(cache.picture("pc", "builtin", &first, decode).is_some());
        assert_eq!(cache.decodes, 2);
        assert_ne!(cache.picture("mac", "builtin", &second, decode), picture);
        assert_eq!(cache.picture("mac", "builtin", &[], decode), None);
        assert_eq!(cache.decodes, 3);

        // Bytes that are not a picture are not decoded, and not again.
        let garbage = [0xFF, 0xD8, 0xFF, 1, 2, 3];
        assert_eq!(cache.picture("pc", "main", &garbage, decode), None);
        assert_eq!(cache.picture("pc", "main", &garbage, decode), None);
        assert_eq!(cache.decodes, 3);

        // A picture the decoder could not open shows no picture, and is not
        // tried again while its bytes stay the same.
        assert_eq!(cache.picture("pc", "side", &second, |_| None), None);
        assert_eq!(cache.picture("pc", "side", &second, decode), None);
        assert_eq!(cache.decodes, 4);
    }
}
