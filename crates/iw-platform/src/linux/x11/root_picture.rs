//! The picture an X11 desktop without GNOME's or Plasma's settings shows:
//! the pixmap a program drawing the desktop picture puts on the root
//! window and names in the root window's `_XROOTPMAP_ID` property (or the
//! older `ESETROOT_PMAP_ID`), as feh, nitrogen, hsetroot and Esetroot do,
//! for terminals and panels to draw through. Each display's preview is
//! the part of the pixmap under it, read with `GetImage`.
//!
//! The pixel layout and the region of each display are worked out here
//! on every OS; reading the pixmap is Linux only, tested against Xvfb
//! (`scripts/linux-x11-tests.sh`).

use iw_core::geometry::Rect;

/// How the pixels of an image the X server sends are laid out: a
/// `ZPixmap` of a depth, by the server's pixmap format for that depth and
/// the TrueColor visual of it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PixelFormat {
    pub bits_per_pixel: u8,
    /// Each row is padded to a multiple of this many bits.
    pub scanline_pad: u8,
    /// The server's image byte order is most significant byte first.
    pub msb_first: bool,
    pub red_mask: u32,
    pub green_mask: u32,
    pub blue_mask: u32,
}

impl PixelFormat {
    /// The bytes of one row of `width` pixels.
    pub(crate) fn stride(&self, width: u32) -> usize {
        let bits = width as usize * usize::from(self.bits_per_pixel);
        let pad = usize::from(self.scanline_pad.max(8));
        bits.div_ceil(pad) * pad / 8
    }

    /// `rows` rows of `width` pixels of `data` as RGB, three bytes a
    /// pixel; `None` for a layout this does not read (other than 16, 24 or
    /// 32 bits a pixel, or without a mask for each colour) or data too
    /// short for it.
    pub(crate) fn rgb(&self, data: &[u8], width: u32, rows: u32) -> Option<Vec<u8>> {
        let bytes = match self.bits_per_pixel {
            16 => 2,
            24 => 3,
            32 => 4,
            _ => return None,
        };
        if self.red_mask == 0 || self.green_mask == 0 || self.blue_mask == 0 {
            return None;
        }
        let stride = self.stride(width);
        if data.len() < stride * rows as usize {
            return None;
        }
        let mut out = Vec::with_capacity(width as usize * rows as usize * 3);
        for row in data.chunks_exact(stride).take(rows as usize) {
            for pixel in row[..width as usize * bytes].chunks_exact(bytes) {
                let value = if self.msb_first {
                    pixel.iter().fold(0u32, |v, b| (v << 8) | u32::from(*b))
                } else {
                    pixel
                        .iter()
                        .rev()
                        .fold(0u32, |v, b| (v << 8) | u32::from(*b))
                };
                out.push(channel(value, self.red_mask));
                out.push(channel(value, self.green_mask));
                out.push(channel(value, self.blue_mask));
            }
        }
        Some(out)
    }
}

/// The colour `mask` picks of `pixel`, from 0 to 255.
fn channel(pixel: u32, mask: u32) -> u8 {
    let shift = mask.trailing_zeros();
    let max = u64::from(mask >> shift);
    let value = u64::from((pixel & mask) >> shift);
    ((value * 255 + max / 2) / max) as u8
}

/// Whether every pixel of `rgb` is the same colour: a desktop of a solid
/// colour, which has no preview on any other desktop either.
pub(crate) fn solid(rgb: &[u8]) -> bool {
    let (pixels, _) = rgb.as_chunks::<3>();
    match pixels.split_first() {
        Some((first, rest)) => rest.iter().all(|p| p == first),
        None => true,
    }
}

/// The part of a `width` × `height` pixmap on the root window under a
/// display at `bounds` (root window pixels: the logical units on X11), as
/// x, y, width and height; `None` where the display is off the pixmap.
pub(crate) fn region(bounds: Rect, width: u16, height: u16) -> Option<(u16, u16, u16, u16)> {
    let clamp = |v: f64, max: u16| v.round().clamp(0.0, f64::from(max)) as u16;
    let (left, top) = (clamp(bounds.x, width), clamp(bounds.y, height));
    let (right, bottom) = (clamp(bounds.right(), width), clamp(bounds.bottom(), height));
    (right > left && bottom > top).then(|| (left, top, right - left, bottom - top))
}

#[cfg(target_os = "linux")]
mod platform {
    use std::sync::Mutex;

    use x11rb::connection::Connection;
    use x11rb::errors::{ConnectionError, ReplyError};
    use x11rb::protocol::xproto::{
        AtomEnum, ConnectionExt as _, ImageFormat, ImageOrder, VisualClass, Window,
    };
    use x11rb::rust_connection::RustConnection;

    use super::PixelFormat;
    use crate::linux::common::lock;

    /// The most a single `GetImage` asks for, so a large picture comes in
    /// strips.
    const STRIP_BYTES: usize = 4 << 20;

    /// The root window's published picture, read for the agent over a
    /// connection of its own, made when first needed and again after it
    /// broke.
    pub(crate) struct RootPicture {
        conn: Mutex<Option<(RustConnection, Window)>>,
    }

    /// The picture the root window shows: the server's pixmap and how it
    /// is laid out. A program that draws a new picture makes a new pixmap.
    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    pub(crate) struct Shown {
        pub pixmap: u32,
        pub width: u16,
        pub height: u16,
        depth: u8,
        format: PixelFormat,
    }

    /// Why a request got nothing: what it asked about is not there (a
    /// pixmap freed, a property never set), or the connection broke.
    enum Failed {
        Missing,
        Broken,
    }

    impl From<ConnectionError> for Failed {
        fn from(_: ConnectionError) -> Self {
            Failed::Broken
        }
    }

    impl From<ReplyError> for Failed {
        fn from(e: ReplyError) -> Self {
            match e {
                ReplyError::X11Error(_) => Failed::Missing,
                ReplyError::ConnectionError(_) => Failed::Broken,
            }
        }
    }

    impl RootPicture {
        pub(crate) fn new() -> Self {
            Self {
                conn: Mutex::new(None),
            }
        }

        /// `f` with the connection, made if there is none; a connection
        /// that broke is made again next time.
        fn with<T>(
            &self,
            f: impl FnOnce(&RustConnection, Window) -> Result<Option<T>, Failed>,
        ) -> Option<T> {
            let mut conn = lock(&self.conn);
            if conn.is_none() {
                match super::super::platform::connect() {
                    Ok(made) => *conn = Some(made),
                    Err(e) => {
                        tracing::debug!(error = %e, "cannot read the root window's picture");
                        return None;
                    }
                }
            }
            let (c, root) = conn.as_ref()?;
            match f(c, *root) {
                Ok(found) => found,
                Err(Failed::Missing) => None,
                Err(Failed::Broken) => {
                    *conn = None;
                    None
                }
            }
        }

        /// The picture the root window shows now, if a program published
        /// one that is still there and of a layout this reads.
        pub(crate) fn shown(&self) -> Option<Shown> {
            self.with(|conn, root| {
                let mut pixmap = None;
                for name in [&b"_XROOTPMAP_ID"[..], b"ESETROOT_PMAP_ID"] {
                    let atom = conn.intern_atom(true, name)?.reply()?.atom;
                    if atom == 0 {
                        continue;
                    }
                    let property = conn
                        .get_property(false, root, atom, AtomEnum::PIXMAP, 0, 1)?
                        .reply()?;
                    pixmap = property
                        .value32()
                        .and_then(|mut v| v.next())
                        .filter(|p| *p != 0);
                    if pixmap.is_some() {
                        break;
                    }
                }
                let Some(pixmap) = pixmap else {
                    return Ok(None);
                };
                let geometry = conn.get_geometry(pixmap)?.reply()?;
                Ok(format_of(conn, root, geometry.depth).map(|format| Shown {
                    pixmap,
                    width: geometry.width,
                    height: geometry.height,
                    depth: geometry.depth,
                    format,
                }))
            })
        }

        /// The pixels of `region` (x, y, width, height) of `shown`, as
        /// RGB; `None` if it is gone or changed meanwhile.
        pub(crate) fn read(
            &self,
            shown: &Shown,
            (x, y, width, height): (u16, u16, u16, u16),
        ) -> Option<image::RgbImage> {
            self.with(|conn, _| {
                let stride = shown.format.stride(u32::from(width));
                let strip = (STRIP_BYTES / stride.max(1)).clamp(1, usize::from(u16::MAX)) as u16;
                let mut rgb = Vec::with_capacity(usize::from(width) * usize::from(height) * 3);
                let mut row = 0;
                while row < height {
                    let rows = strip.min(height - row);
                    let reply = conn
                        .get_image(
                            ImageFormat::Z_PIXMAP,
                            shown.pixmap,
                            x as i16,
                            (y + row) as i16,
                            width,
                            rows,
                            !0,
                        )?
                        .reply()?;
                    if reply.depth != shown.depth {
                        return Ok(None);
                    }
                    let Some(pixels) =
                        shown
                            .format
                            .rgb(&reply.data, u32::from(width), u32::from(rows))
                    else {
                        return Ok(None);
                    };
                    rgb.extend_from_slice(&pixels);
                    row += rows;
                }
                Ok(image::RgbImage::from_raw(
                    u32::from(width),
                    u32::from(height),
                    rgb,
                ))
            })
        }
    }

    /// How the server lays out a `ZPixmap` of `depth` on the screen of
    /// `root`: its pixmap format, and the masks of the screen's TrueColor
    /// (or DirectColor) visual of that depth, the root window's own first.
    fn format_of(conn: &RustConnection, root: Window, depth: u8) -> Option<PixelFormat> {
        let setup = conn.setup();
        let format = setup.pixmap_formats.iter().find(|f| f.depth == depth)?;
        let screen = setup.roots.iter().find(|s| s.root == root)?;
        let visuals = &screen
            .allowed_depths
            .iter()
            .find(|d| d.depth == depth)?
            .visuals;
        let visual = visuals
            .iter()
            .find(|v| v.visual_id == screen.root_visual)
            .or_else(|| visuals.first())
            .filter(|v| matches!(v.class, VisualClass::TRUE_COLOR | VisualClass::DIRECT_COLOR))?;
        Some(PixelFormat {
            bits_per_pixel: format.bits_per_pixel,
            scanline_pad: format.scanline_pad,
            msb_first: setup.image_byte_order == ImageOrder::MSB_FIRST,
            red_mask: visual.red_mask,
            green_mask: visual.green_mask,
            blue_mask: visual.blue_mask,
        })
    }
}

#[cfg(target_os = "linux")]
pub(crate) use platform::{RootPicture, Shown};

#[cfg(test)]
mod tests {
    use super::*;

    const XRGB: PixelFormat = PixelFormat {
        bits_per_pixel: 32,
        scanline_pad: 32,
        msb_first: false,
        red_mask: 0xff0000,
        green_mask: 0x00ff00,
        blue_mask: 0x0000ff,
    };

    #[test]
    fn a_24_bit_desktop_reads_as_rgb() {
        // Two pixels, little-endian XRGB: orange, then teal.
        let data = [0x00, 0x80, 0xff, 0x00, 0x80, 0x80, 0x00, 0x00];
        assert_eq!(
            XRGB.rgb(&data, 2, 1).unwrap(),
            [0xff, 0x80, 0x00, 0x00, 0x80, 0x80]
        );
        // The same, most significant byte first.
        let big = PixelFormat {
            msb_first: true,
            ..XRGB
        };
        let data = [0x00, 0xff, 0x80, 0x00, 0x00, 0x00, 0x80, 0x80];
        assert_eq!(
            big.rgb(&data, 2, 1).unwrap(),
            [0xff, 0x80, 0x00, 0x00, 0x80, 0x80]
        );
        // Too little data for the rows asked.
        assert_eq!(XRGB.rgb(&data, 2, 2), None);
    }

    #[test]
    fn a_16_bit_desktop_widens_its_colours() {
        let rgb565 = PixelFormat {
            bits_per_pixel: 16,
            scanline_pad: 32,
            msb_first: false,
            red_mask: 0xf800,
            green_mask: 0x07e0,
            blue_mask: 0x001f,
        };
        // Three pixels of 2 bytes, each row padded to 8 bytes: white,
        // pure red, then black; a second row of pure blue.
        let data = [
            0xff, 0xff, 0x00, 0xf8, 0x00, 0x00, 0, 0, //
            0x1f, 0x00, 0x1f, 0x00, 0x1f, 0x00, 0, 0,
        ];
        assert_eq!(rgb565.stride(3), 8);
        let rgb = rgb565.rgb(&data, 3, 2).unwrap();
        assert_eq!(&rgb[..9], [255, 255, 255, 255, 0, 0, 0, 0, 0]);
        assert_eq!(&rgb[9..], [0, 0, 255, 0, 0, 255, 0, 0, 255]);
    }

    #[test]
    fn layouts_it_does_not_read_are_none() {
        let indexed = PixelFormat {
            bits_per_pixel: 8,
            ..XRGB
        };
        assert_eq!(indexed.rgb(&[0; 8], 2, 1), None);
        let no_masks = PixelFormat {
            red_mask: 0,
            ..XRGB
        };
        assert_eq!(no_masks.rgb(&[0; 8], 2, 1), None);
    }

    #[test]
    fn rows_are_padded() {
        let packed = PixelFormat {
            bits_per_pixel: 24,
            scanline_pad: 32,
            ..XRGB
        };
        // 3 pixels of 3 bytes: 9 bytes, padded to 12.
        assert_eq!(packed.stride(3), 12);
        assert_eq!(XRGB.stride(3), 12);
        assert_eq!(packed.stride(4), 12);
    }

    #[test]
    fn a_solid_colour_is_told_from_a_picture() {
        assert!(solid(&[1, 2, 3, 1, 2, 3, 1, 2, 3]));
        assert!(!solid(&[1, 2, 3, 1, 2, 4]));
        assert!(solid(&[]));
    }

    #[test]
    fn each_display_reads_the_part_of_the_pixmap_under_it() {
        // Two monitors side by side on a pixmap of the whole root window.
        let left = Rect::new(0.0, 0.0, 1920.0, 1080.0);
        let right = Rect::new(1920.0, 0.0, 1920.0, 1080.0);
        assert_eq!(region(left, 3840, 1080), Some((0, 0, 1920, 1080)));
        assert_eq!(region(right, 3840, 1080), Some((1920, 0, 1920, 1080)));
        // A pixmap smaller than the root window: what of it is under.
        assert_eq!(region(right, 2400, 800), Some((1920, 0, 480, 800)));
        // A display off the pixmap has none.
        assert_eq!(region(right, 1920, 1080), None);
        assert_eq!(region(Rect::new(-10.0, 0.0, 5.0, 5.0), 100, 100), None);
    }
}
