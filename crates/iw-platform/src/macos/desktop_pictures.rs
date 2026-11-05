use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

use iw_core::layout::Display;
use objc2::rc::{Retained, autoreleasepool};
use objc2::{AllocAnyThread, MainThreadMarker};
use objc2_app_kit::{
    NSBitmapImageFileType, NSBitmapImageRep, NSCompositingOperation, NSDeviceRGBColorSpace,
    NSGraphicsContext, NSImage, NSImageCompressionFactor, NSImageInterpolation, NSScreen,
    NSWorkspace,
};
use objc2_foundation::{NSDictionary, NSNumber, NSPoint, NSRect, NSSize, NSString, ns_string};

use crate::{DesktopPictures, PlatformError, preview};

/// Previews of the desktop picture on each display, from the file
/// `NSWorkspace` says each screen shows.
///
/// The file's path, modification time and length are checked on every
/// call; a picture is decoded and encoded again only when one of them
/// changed. Dynamic desktops point at a HEIC file, whose first image is
/// used. A solid color, or a desktop `NSImage` cannot read (some aerials),
/// has no preview.
#[derive(Debug, Default)]
pub struct MacDesktopPictures {
    made: Mutex<HashMap<String, Made>>,
}

#[derive(Debug)]
struct Made {
    source: Source,
    jpeg: Option<Vec<u8>>,
}

/// Which file a display shows, as far as telling a change goes.
#[derive(Clone, Debug, PartialEq)]
struct Source {
    path: PathBuf,
    modified: Option<SystemTime>,
    len: u64,
}

impl Source {
    fn of(path: PathBuf) -> Option<Self> {
        let meta = std::fs::metadata(&path).ok()?;
        Some(Self {
            modified: meta.modified().ok(),
            len: meta.len(),
            path,
        })
    }
}

impl DesktopPictures for MacDesktopPictures {
    fn pictures(&self, displays: &[Display]) -> Result<HashMap<String, Vec<u8>>, PlatformError> {
        autoreleasepool(|_| {
            let files = desktop_files();
            let mut made = self.made.lock().unwrap();
            made.retain(|id, _| displays.iter().any(|d| d.id == *id));
            let mut out = HashMap::new();
            for display in displays {
                let source = display
                    .id
                    .parse::<u32>()
                    .ok()
                    .and_then(|number| files.get(&number).cloned())
                    .and_then(Source::of);
                let Some(source) = source else {
                    made.remove(&display.id);
                    continue;
                };
                let entry = match made.get(&display.id) {
                    Some(m) if m.source == source => m,
                    _ => {
                        let jpeg = preview_of(&source.path);
                        made.insert(display.id.clone(), Made { source, jpeg });
                        &made[&display.id]
                    }
                };
                if let Some(jpeg) = &entry.jpeg {
                    out.insert(display.id.clone(), jpeg.clone());
                }
            }
            Ok(out)
        })
    }
}

/// The desktop picture file of each screen, by `CGDirectDisplayID`.
fn desktop_files() -> HashMap<u32, PathBuf> {
    // SAFETY: the marker is only used to list the screens and read their
    // device descriptions, which reads the window server's display
    // configuration; AppKit does not need the main thread for that
    // (checked on macOS 27). The agent's main thread runs no run loop to
    // hand the work to, and the marker and the screens never leave this
    // function.
    //
    // Without that run loop, AppKit may never learn of a display plugged
    // in while the agent runs: it can be missing from `NSScreen::screens`,
    // and then has no picture until the agent restarts.
    let mtm = unsafe { MainThreadMarker::new_unchecked() };
    let workspace = NSWorkspace::sharedWorkspace();
    let mut files = HashMap::new();
    for screen in NSScreen::screens(mtm).iter() {
        let number = screen
            .deviceDescription()
            .objectForKey(ns_string!("NSScreenNumber"))
            .and_then(|n| n.downcast::<NSNumber>().ok())
            .map(|n| n.as_u32());
        let Some(number) = number else { continue };
        let path = workspace
            .desktopImageURLForScreen(&screen)
            .filter(|url| url.isFileURL())
            .and_then(|url| url.to_file_path());
        if let Some(path) = path {
            files.insert(number, path);
        }
    }
    files
}

/// A JPEG preview of the image in the file at `path`, or `None` if it
/// cannot be read or no size of it is light enough.
pub(crate) fn preview_of(path: &Path) -> Option<Vec<u8>> {
    autoreleasepool(|_| {
        let path = NSString::from_str(path.to_str()?);
        let image = NSImage::initWithContentsOfFile(NSImage::alloc(), &path)?;
        let (width, height) = pixel_size(&image);
        preview::smallest_fitting(width, height, |w, h| render_jpeg(&image, w, h))
    })
}

/// The size of `image` in pixels, from its first bitmap. Its size in
/// points differs when the file records a resolution other than 72 dpi.
/// An image without a bitmap, such as a PDF, has only its size in points.
fn pixel_size(image: &NSImage) -> (f64, f64) {
    let pixels = image.representations().iter().find_map(|rep| {
        let bitmap = rep.downcast::<NSBitmapImageRep>().ok()?;
        let (w, h) = (bitmap.pixelsWide(), bitmap.pixelsHigh());
        (w > 0 && h > 0).then_some((w as f64, h as f64))
    });
    pixels.unwrap_or_else(|| {
        let size = image.size();
        (size.width, size.height)
    })
}

/// Draws `image` into a `width` × `height` bitmap and encodes it as JPEG.
fn render_jpeg(image: &NSImage, width: u32, height: u32) -> Option<Vec<u8>> {
    // SAFETY: with no planes the bitmap allocates its own memory; the rest
    // describes an 8-bit RGBA bitmap, which a graphics context can draw
    // into.
    let bitmap = unsafe {
        NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
            NSBitmapImageRep::alloc(),
            std::ptr::null_mut(),
            width as isize,
            height as isize,
            8,
            4,
            true,
            false,
            NSDeviceRGBColorSpace,
            0,
            0,
        )
    }?;
    let context = NSGraphicsContext::graphicsContextWithBitmapImageRep(&bitmap)?;
    // The current context belongs to this thread; put back whatever was
    // current before.
    NSGraphicsContext::saveGraphicsState_class();
    NSGraphicsContext::setCurrentContext(Some(&context));
    context.setImageInterpolation(NSImageInterpolation::High);
    let target = NSRect::new(
        NSPoint::new(0.0, 0.0),
        NSSize::new(f64::from(width), f64::from(height)),
    );
    image.drawInRect_fromRect_operation_fraction(
        target,
        NSRect::ZERO,
        NSCompositingOperation::Copy,
        1.0,
    );
    context.flushGraphics();
    NSGraphicsContext::restoreGraphicsState_class();
    jpeg(&bitmap)
}

fn jpeg(bitmap: &NSBitmapImageRep) -> Option<Vec<u8>> {
    let quality: Retained<NSNumber> = NSNumber::new_f64(preview::QUALITY);
    // SAFETY: reading an immutable framework constant.
    let key = unsafe { NSImageCompressionFactor };
    let properties = NSDictionary::from_slices(&[key], &[quality.as_ref()]);
    // SAFETY: the compression factor is an NSNumber, as the key requires.
    let data = unsafe {
        bitmap.representationUsingType_properties(NSBitmapImageFileType::JPEG, &properties)
    }?;
    Some(data.to_vec())
}

#[cfg(test)]
mod tests {
    use iw_core::geometry::Rect;
    use objc2_app_kit::NSImageRep;

    use super::*;
    use crate::Displays;
    use crate::macos::MacDisplays;

    /// Writes a `width` × `height` PNG with a gradient and some noise, so
    /// it does not compress to nothing.
    fn write_png(path: &Path, width: usize, height: usize) {
        write_png_in_points(path, width, height, None);
    }

    /// Like [`write_png`], recording a resolution that makes the picture
    /// `points` in size.
    fn write_png_in_points(path: &Path, width: usize, height: usize, points: Option<(f64, f64)>) {
        // SAFETY: as in `render_jpeg`.
        let bitmap = unsafe {
            NSBitmapImageRep::initWithBitmapDataPlanes_pixelsWide_pixelsHigh_bitsPerSample_samplesPerPixel_hasAlpha_isPlanar_colorSpaceName_bytesPerRow_bitsPerPixel(
                NSBitmapImageRep::alloc(),
                std::ptr::null_mut(),
                width as isize,
                height as isize,
                8,
                4,
                true,
                false,
                NSDeviceRGBColorSpace,
                0,
                0,
            )
        }
        .unwrap();
        let row = bitmap.bytesPerRow() as usize;
        // SAFETY: the bitmap owns `row * height` bytes, written only here.
        let pixels = unsafe { std::slice::from_raw_parts_mut(bitmap.bitmapData(), row * height) };
        let mut noise: u32 = 0x1234_5678;
        for y in 0..height {
            for x in 0..width {
                noise ^= noise << 13;
                noise ^= noise >> 17;
                noise ^= noise << 5;
                let p = &mut pixels[y * row + x * 4..][..4];
                p[0] = (x * 255 / width) as u8;
                p[1] = (y * 255 / height) as u8;
                p[2] = (noise & 0x3f) as u8 + 96;
                p[3] = 255;
            }
        }
        if let Some((w, h)) = points {
            let rep: &NSImageRep = &bitmap;
            rep.setSize(NSSize::new(w, h));
        }
        // SAFETY: an empty dictionary is a valid set of properties.
        let png = unsafe {
            bitmap.representationUsingType_properties(
                NSBitmapImageFileType::PNG,
                &NSDictionary::new(),
            )
        }
        .unwrap();
        std::fs::write(path, png.to_vec()).unwrap();
    }

    /// The pixel size of a JPEG.
    fn pixels(jpeg: &[u8]) -> (isize, isize) {
        let rep = NSBitmapImageRep::imageRepWithData(&objc2_foundation::NSData::with_bytes(jpeg))
            .unwrap();
        let rep: &NSImageRep = &rep;
        (rep.pixelsWide(), rep.pixelsHigh())
    }

    #[test]
    fn previews_a_picture_file_as_a_small_jpeg() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("desk.png");
        write_png(&path, 1600, 1000);
        let jpeg = preview_of(&path).unwrap();
        assert_eq!(&jpeg[..3], &[0xff, 0xd8, 0xff]);
        assert!(jpeg.len() <= preview::MAX_BYTES, "{} bytes", jpeg.len());
        let (w, h) = pixels(&jpeg);
        assert!(w <= 320 && h <= 200 && w * 10 == h * 16, "{w}x{h}");

        let portrait = dir.path().join("tall.png");
        write_png(&portrait, 90, 160);
        let jpeg = preview_of(&portrait).unwrap();
        assert_eq!(pixels(&jpeg), (90, 160), "not enlarged");
    }

    #[test]
    fn previews_are_sized_by_pixels_whatever_the_resolution() {
        let dir = tempfile::tempdir().unwrap();
        // 1600 × 1000 pixels at 720 dpi: 160 × 100 points.
        let path = dir.path().join("dense.png");
        write_png_in_points(&path, 1600, 1000, Some((160.0, 100.0)));
        let image = NSImage::initWithContentsOfFile(
            NSImage::alloc(),
            &NSString::from_str(path.to_str().unwrap()),
        )
        .unwrap();
        let size = image.size();
        assert_eq!(
            (size.width, size.height),
            (160.0, 100.0),
            "the resolution was kept"
        );
        assert_eq!(pixel_size(&image), (1600.0, 1000.0));
        assert_eq!(pixels(&preview_of(&path).unwrap()), (320, 200));
    }

    #[test]
    fn files_that_are_not_pictures_have_no_preview() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("notes.txt");
        std::fs::write(&text, "not a picture").unwrap();
        assert_eq!(preview_of(&text), None);
        assert_eq!(preview_of(&dir.path().join("missing.heic")), None);
    }

    /// Reads the real desktop pictures; needs no permission. What they show
    /// is up to the user, so only the limits are checked.
    #[test]
    fn previews_of_the_real_desktop_keep_to_the_limits() {
        let displays = MacDisplays.displays().unwrap();
        let mut asked = displays.clone();
        asked.push(Display {
            id: "not a display".to_owned(),
            bounds: Rect::new(0.0, 0.0, 10.0, 10.0),
            scale: 1.0,
            primary: false,
        });
        let pictures = MacDesktopPictures::default();
        let first = pictures.pictures(&asked).unwrap();
        for (id, jpeg) in &first {
            assert!(displays.iter().any(|d| d.id == *id), "{id}");
            assert_eq!(&jpeg[..3], &[0xff, 0xd8, 0xff]);
            assert!(jpeg.len() <= preview::MAX_BYTES);
        }
        // Unchanged pictures come from what was made before.
        assert_eq!(pictures.pictures(&asked).unwrap(), first);
    }
}
