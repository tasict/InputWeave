//! How large a preview of a display's desktop picture is drawn, and how
//! much it may weigh. Platform backends draw and encode the pictures; the
//! decisions live here, free of FFI, so they are tested everywhere.

/// The long edge of a preview, in pixels, when that is light enough.
pub const LONG_EDGE: u32 = 320;

/// JPEG quality, from 0 to 1.
pub const QUALITY: f64 = 0.7;

/// A preview weighs at most this many bytes.
pub const MAX_BYTES: usize = 96 * 1024;

/// Long edges tried in turn until the JPEG is light enough.
const STEPS: [u32; 5] = [LONG_EDGE, 256, 192, 128, 96];

/// The pixel size to draw a picture of `width` × `height` at so that its
/// long edge is `long_edge`. Pictures already smaller keep their size, and
/// neither side is less than 1 pixel. `None` for an empty or nonsensical
/// size.
pub fn fit(width: f64, height: f64, long_edge: u32) -> Option<(u32, u32)> {
    if !(width.is_finite() && height.is_finite() && width >= 1.0 && height >= 1.0) {
        return None;
    }
    let scale = (f64::from(long_edge) / width.max(height)).min(1.0);
    let side = |v: f64| ((v * scale).round() as u32).clamp(1, long_edge.max(1));
    Some((side(width), side(height)))
}

/// Encodes a preview of a picture of `width` × `height` at each size in
/// turn, from [`LONG_EDGE`] down, until the JPEG is at most [`MAX_BYTES`].
/// `encode` draws and encodes at the pixel size it is given, or returns
/// `None` if it cannot. `None` if no size is light enough.
pub fn smallest_fitting(
    width: f64,
    height: f64,
    mut encode: impl FnMut(u32, u32) -> Option<Vec<u8>>,
) -> Option<Vec<u8>> {
    let mut tried = None;
    for long_edge in STEPS {
        let size = fit(width, height, long_edge)?;
        // A small picture comes out the same size at several steps.
        if tried == Some(size) {
            continue;
        }
        tried = Some(size);
        let jpeg = encode(size.0, size.1)?;
        if jpeg.len() <= MAX_BYTES {
            return Some(jpeg);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_long_edge_becomes_320_pixels() {
        assert_eq!(fit(3024.0, 1964.0, LONG_EDGE), Some((320, 208)));
        assert_eq!(fit(1080.0, 1920.0, LONG_EDGE), Some((180, 320)));
        assert_eq!(fit(6016.0, 6016.0, LONG_EDGE), Some((320, 320)));
        // Extreme proportions keep at least a pixel.
        assert_eq!(fit(10_000.0, 1.0, LONG_EDGE), Some((320, 1)));
    }

    #[test]
    fn small_pictures_are_not_enlarged() {
        assert_eq!(fit(200.0, 100.0, LONG_EDGE), Some((200, 100)));
        assert_eq!(fit(320.0, 1.0, LONG_EDGE), Some((320, 1)));
    }

    #[test]
    fn empty_or_broken_sizes_have_no_preview() {
        for (w, h) in [
            (0.0, 100.0),
            (100.0, 0.5),
            (f64::NAN, 10.0),
            (f64::INFINITY, 1.0),
        ] {
            assert_eq!(fit(w, h, LONG_EDGE), None, "{w}x{h}");
        }
    }

    #[test]
    fn heavy_previews_shrink_until_they_fit() {
        let mut sizes = Vec::new();
        let jpeg = smallest_fitting(2560.0, 1600.0, |w, h| {
            sizes.push((w, h));
            // Pretend each pixel costs two bytes.
            Some(vec![0; (w * h) as usize * 2])
        })
        .unwrap();
        assert_eq!(sizes, vec![(320, 200), (256, 160)]);
        assert_eq!(jpeg.len(), 256 * 160 * 2);
        assert!(jpeg.len() <= MAX_BYTES);
    }

    #[test]
    fn a_preview_that_never_fits_is_left_out() {
        let mut calls = 0;
        let jpeg = smallest_fitting(4000.0, 3000.0, |_, _| {
            calls += 1;
            Some(vec![0; MAX_BYTES + 1])
        });
        assert_eq!(jpeg, None);
        assert_eq!(calls, STEPS.len());
    }

    #[test]
    fn a_small_picture_is_encoded_once() {
        let mut sizes = Vec::new();
        let jpeg = smallest_fitting(64.0, 48.0, |w, h| {
            sizes.push((w, h));
            Some(vec![0; MAX_BYTES * 2])
        });
        assert_eq!(jpeg, None);
        assert_eq!(sizes, vec![(64, 48)]);
    }

    #[test]
    fn a_picture_that_cannot_be_drawn_has_no_preview() {
        assert_eq!(smallest_fitting(800.0, 600.0, |_, _| None), None);
        assert_eq!(smallest_fitting(0.0, 600.0, |_, _| Some(vec![1])), None);
    }
}
