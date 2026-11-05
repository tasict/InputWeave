//! Previews of the logged-on user's desktop pictures, from the files the
//! shell's `IDesktopWallpaper` names for each monitor, drawn and encoded
//! as JPEG with WIC.
//!
//! The pictures and the wallpaper settings belong to the user, so only the
//! broker, which runs as that user, reads them; the agent runs as SYSTEM,
//! never opens the user's files, and gets the previews from the broker.

use std::collections::BTreeMap;
use std::ffi::c_void;
use std::mem::ManuallyDrop;
use std::path::Path;
use std::sync::Mutex;

use ::windows::Win32::Foundation::{E_FAIL, GENERIC_READ, HGLOBAL};
use ::windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatJpeg, GUID_WICPixelFormat24bppBGR,
    IWICBitmapFrameDecode, IWICImagingFactory, IWICPalette, WICBitmapDitherTypeNone,
    WICBitmapEncoderNoCache, WICBitmapInterpolationModeHighQualityCubic,
    WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
};
use ::windows::Win32::System::Com::StructuredStorage::{
    CreateStreamOnHGlobal, IPropertyBag2, PROPBAG2,
};
use ::windows::Win32::System::Com::{
    CLSCTX_ALL, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx,
    CoTaskMemFree, CoUninitialize, IStream, STREAM_SEEK_END, STREAM_SEEK_SET,
};
use ::windows::Win32::System::Variant::{VARIANT, VARIANT_0, VARIANT_0_0, VARIANT_0_0_0, VT_R4};
use ::windows::Win32::UI::Shell::{DesktopWallpaper, IDesktopWallpaper};
use ::windows::core::{HSTRING, PCWSTR, PWSTR};

use super::displays;
use super::monitors::PixelRect;
use super::wallpaper::{Previews, ShellMonitor, files_by_display};
use crate::{PlatformError, preview};

/// Reads a preview of the picture on each monitor's desktop. Pictures that
/// did not change come from what was made before.
#[derive(Debug)]
pub struct WallpaperPreviews {
    previews: Mutex<Previews>,
}

impl WallpaperPreviews {
    /// Makes the process per-monitor DPI aware (v2), as the agent is, so
    /// Windows and the shell give monitor rectangles in the same physical
    /// pixels.
    pub fn new() -> Result<Self, PlatformError> {
        super::make_per_monitor_aware()?;
        Ok(Self {
            previews: Mutex::default(),
        })
    }

    /// A JPEG of the picture on each monitor, by display id as the agent
    /// names displays (`\\.\DISPLAY1`): its long edge at most
    /// [`preview::LONG_EDGE`] pixels, its size at most
    /// [`preview::MAX_BYTES`]. Monitors showing a solid color, or a picture
    /// that cannot be read, are left out.
    ///
    /// It may decode large pictures; call it off the async runtime.
    pub fn read(&self) -> Result<BTreeMap<String, Vec<u8>>, PlatformError> {
        // Declared first, so the COM objects below are released before
        // COM is left.
        let _apartment = Apartment::enter()?;
        let monitors = displays::monitors()?;
        let (shell, shared) = shell_monitors()?;
        let files = files_by_display(&monitors, &shell, &shared);
        // SAFETY: COM is initialized on this thread.
        let factory: IWICImagingFactory =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
                .map_err(os("cannot start the Windows Imaging Component"))?;
        let mut previews = self.previews.lock().unwrap_or_else(|e| e.into_inner());
        Ok(previews.update(&files, |path| preview_of(&factory, path)))
    }
}

pub(super) fn os(what: &'static str) -> impl Fn(::windows::core::Error) -> PlatformError {
    move |e| PlatformError::Os(format!("{what}: {e}"))
}

/// This thread in COM's multithreaded apartment, for as long as it lives.
pub(super) struct Apartment;

impl Apartment {
    pub(super) fn enter() -> Result<Self, PlatformError> {
        // SAFETY: a plain call; a success, including S_FALSE for a thread
        // already in the apartment, is balanced by CoUninitialize on drop.
        unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }
            .ok()
            .map_err(os("cannot initialize COM"))?;
        Ok(Self)
    }
}

impl Drop for Apartment {
    fn drop(&mut self) {
        // SAFETY: balances the CoInitializeEx that made this value.
        unsafe { CoUninitialize() };
    }
}

/// The monitors the shell knows, with their rectangles and pictures, and
/// the picture every monitor shows when they show the same one.
fn shell_monitors() -> Result<(Vec<ShellMonitor>, String), PlatformError> {
    // SAFETY: COM is initialized on this thread (`Apartment`).
    let wallpaper: IDesktopWallpaper =
        unsafe { CoCreateInstance(&DesktopWallpaper, None, CLSCTX_ALL) }
            .map_err(os("cannot reach the desktop wallpaper settings"))?;
    // SAFETY: plain calls on a live interface. The strings they return are
    // the caller's, freed by `CoString`.
    unsafe {
        let count = wallpaper
            .GetMonitorDevicePathCount()
            .map_err(os("cannot list the monitors the desktop knows"))?;
        let mut monitors = Vec::new();
        for index in 0..count {
            let Ok(id) = wallpaper.GetMonitorDevicePathAt(index).map(CoString) else {
                continue;
            };
            // A monitor no longer attached has no rectangle.
            let Ok(rect) = wallpaper.GetMonitorRECT(id.as_pcwstr()) else {
                continue;
            };
            let picture = wallpaper.GetWallpaper(id.as_pcwstr()).map(CoString);
            monitors.push(ShellMonitor {
                rect: PixelRect::new(rect.left, rect.top, rect.right, rect.bottom),
                wallpaper: picture.map(|p| p.text()).unwrap_or_default(),
            });
        }
        // Empty unless every monitor shows the same picture.
        let shared = wallpaper.GetWallpaper(PCWSTR::null()).map(CoString);
        Ok((monitors, shared.map(|p| p.text()).unwrap_or_default()))
    }
}

/// A string a COM call allocated for its caller, freed on drop.
struct CoString(PWSTR);

impl CoString {
    fn as_pcwstr(&self) -> PCWSTR {
        PCWSTR(self.0.0)
    }

    fn text(&self) -> String {
        if self.0.is_null() {
            return String::new();
        }
        // SAFETY: a non-null string from COM is NUL-terminated UTF-16.
        unsafe { self.0.to_string() }.unwrap_or_default()
    }
}

impl Drop for CoString {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: the string was allocated with CoTaskMemAlloc for this
            // value alone.
            unsafe { CoTaskMemFree(Some(self.0.0 as *const c_void)) };
        }
    }
}

/// A JPEG preview of the picture in the file at `path`, or `None` if it
/// cannot be read or no size of it is light enough.
fn preview_of(factory: &IWICImagingFactory, path: &Path) -> Option<Vec<u8>> {
    let name = HSTRING::from(path.as_os_str());
    // SAFETY: plain calls on a live factory; `name` outlives the call that
    // reads it, and the out pointers are valid.
    let frame = unsafe {
        let decoder = factory
            .CreateDecoderFromFilename(&name, None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)
            .ok()?;
        decoder.GetFrame(0).ok()?
    };
    let (mut width, mut height) = (0u32, 0u32);
    // SAFETY: both out pointers are valid for the call.
    unsafe { frame.GetSize(&mut width, &mut height) }.ok()?;
    preview::smallest_fitting(f64::from(width), f64::from(height), |w, h| {
        encode(factory, &frame, w, h)
            .inspect_err(|e| tracing::debug!(error = %e, "cannot encode a desktop picture"))
            .ok()
    })
}

/// Scales `frame` to `width` × `height` and encodes it as JPEG.
fn encode(
    factory: &IWICImagingFactory,
    frame: &IWICBitmapFrameDecode,
    width: u32,
    height: u32,
) -> ::windows::core::Result<Vec<u8>> {
    // SAFETY: plain calls on live WIC objects created here; every pointer
    // passed is valid for its call.
    unsafe {
        let scaler = factory.CreateBitmapScaler()?;
        scaler.Initialize(
            frame,
            width,
            height,
            WICBitmapInterpolationModeHighQualityCubic,
        )?;
        // The JPEG encoder takes 24-bit BGR, not the 32-bit formats many
        // decoders produce.
        let converter = factory.CreateFormatConverter()?;
        converter.Initialize(
            &scaler,
            &GUID_WICPixelFormat24bppBGR,
            WICBitmapDitherTypeNone,
            None::<&IWICPalette>,
            0.0,
            WICBitmapPaletteTypeCustom,
        )?;
        let stream = CreateStreamOnHGlobal(HGLOBAL::default(), true)?;
        let encoder = factory.CreateEncoder(&GUID_ContainerFormatJpeg, std::ptr::null())?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let (mut target, mut options) = (None, None);
        encoder.CreateNewFrame(&mut target, &mut options)?;
        let (Some(target), Some(options)) = (target, options) else {
            return Err(E_FAIL.into());
        };
        set_quality(&options, preview::QUALITY as f32)?;
        target.Initialize(&options)?;
        target.SetSize(width, height)?;
        let mut format = GUID_WICPixelFormat24bppBGR;
        target.SetPixelFormat(&mut format)?;
        target.WriteSource(&converter, std::ptr::null())?;
        target.Commit()?;
        encoder.Commit()?;
        read_all(&stream)
    }
}

/// Sets the JPEG encoder's quality, from 0 to 1.
fn set_quality(options: &IPropertyBag2, quality: f32) -> ::windows::core::Result<()> {
    let mut name: Vec<u16> = "ImageQuality\0".encode_utf16().collect();
    let property = PROPBAG2 {
        pstrName: PWSTR(name.as_mut_ptr()),
        ..Default::default()
    };
    let value = VARIANT {
        Anonymous: VARIANT_0 {
            Anonymous: ManuallyDrop::new(VARIANT_0_0 {
                vt: VT_R4,
                Anonymous: VARIANT_0_0_0 { fltVal: quality },
                ..Default::default()
            }),
        },
    };
    // SAFETY: one property and one value, a VT_R4 as the encoder expects
    // for its quality; `name` and `value` outlive the call.
    unsafe { options.Write(1, &property, &value) }
}

/// Everything written to `stream`.
pub(super) fn read_all(stream: &IStream) -> ::windows::core::Result<Vec<u8>> {
    let mut len = 0u64;
    // SAFETY: plain calls on a live stream; the out pointers and the buffer
    // are valid for as many bytes as they are given.
    unsafe {
        stream.Seek(0, STREAM_SEEK_END, Some(&mut len))?;
        stream.Seek(0, STREAM_SEEK_SET, None)?;
        let mut bytes = vec![0u8; usize::try_from(len).map_err(|_| E_FAIL)?];
        let mut read = 0u32;
        stream
            .Read(
                bytes.as_mut_ptr().cast(),
                u32::try_from(bytes.len()).map_err(|_| E_FAIL)?,
                Some(&mut read),
            )
            .ok()?;
        bytes.truncate(read as usize);
        Ok(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs `f` with a WIC factory, in COM's multithreaded apartment.
    fn with_factory<T>(f: impl FnOnce(&IWICImagingFactory) -> T) -> T {
        let _apartment = Apartment::enter().unwrap();
        // SAFETY: COM is initialized on this thread.
        let factory: IWICImagingFactory =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
                .unwrap();
        f(&factory)
    }

    /// Writes a `width` × `height` 24-bit BMP with a gradient and some
    /// noise, so it does not compress to nothing.
    fn write_bmp(path: &Path, width: usize, height: usize) {
        let row = (width * 3 + 3) & !3;
        let size = 54 + row * height;
        let mut bmp = Vec::with_capacity(size);
        bmp.extend_from_slice(b"BM");
        bmp.extend_from_slice(&(size as u32).to_le_bytes());
        bmp.extend_from_slice(&[0; 4]);
        bmp.extend_from_slice(&54u32.to_le_bytes());
        bmp.extend_from_slice(&40u32.to_le_bytes());
        bmp.extend_from_slice(&(width as i32).to_le_bytes());
        bmp.extend_from_slice(&(height as i32).to_le_bytes());
        bmp.extend_from_slice(&1u16.to_le_bytes());
        bmp.extend_from_slice(&24u16.to_le_bytes());
        // No compression, the image size, resolution and palette unset.
        bmp.extend_from_slice(&[0; 4]);
        bmp.extend_from_slice(&((row * height) as u32).to_le_bytes());
        bmp.extend_from_slice(&[0; 16]);
        let mut noise: u32 = 0x1234_5678;
        for y in 0..height {
            for x in 0..width {
                noise ^= noise << 13;
                noise ^= noise >> 17;
                noise ^= noise << 5;
                bmp.push((noise & 0x3f) as u8 + 96);
                bmp.push((y * 255 / height) as u8);
                bmp.push((x * 255 / width) as u8);
            }
            bmp.resize(bmp.len() + row - width * 3, 0);
        }
        std::fs::write(path, bmp).unwrap();
    }

    /// The pixel size of a JPEG.
    fn pixels(factory: &IWICImagingFactory, dir: &Path, jpeg: &[u8]) -> (u32, u32) {
        let path = dir.join("preview.jpg");
        std::fs::write(&path, jpeg).unwrap();
        let name = HSTRING::from(path.as_os_str());
        let (mut width, mut height) = (0, 0);
        // SAFETY: as in `preview_of`.
        unsafe {
            let decoder = factory
                .CreateDecoderFromFilename(
                    &name,
                    None,
                    GENERIC_READ,
                    WICDecodeMetadataCacheOnDemand,
                )
                .unwrap();
            decoder
                .GetFrame(0)
                .unwrap()
                .GetSize(&mut width, &mut height)
                .unwrap();
        }
        (width, height)
    }

    #[test]
    fn previews_a_picture_file_as_a_small_jpeg() {
        let dir = tempfile::tempdir().unwrap();
        with_factory(|factory| {
            let wide = dir.path().join("desk.bmp");
            write_bmp(&wide, 1600, 1000);
            let jpeg = preview_of(factory, &wide).unwrap();
            assert_eq!(&jpeg[..3], &[0xff, 0xd8, 0xff]);
            assert!(jpeg.len() <= preview::MAX_BYTES, "{} bytes", jpeg.len());
            assert_eq!(pixels(factory, dir.path(), &jpeg), (320, 200));

            let tall = dir.path().join("tall.bmp");
            write_bmp(&tall, 90, 160);
            let jpeg = preview_of(factory, &tall).unwrap();
            assert_eq!(
                pixels(factory, dir.path(), &jpeg),
                (90, 160),
                "not enlarged"
            );
        });
    }

    #[test]
    fn files_that_are_not_pictures_have_no_preview() {
        let dir = tempfile::tempdir().unwrap();
        let text = dir.path().join("notes.txt");
        std::fs::write(&text, "not a picture").unwrap();
        with_factory(|factory| {
            assert_eq!(preview_of(factory, &text), None);
            assert_eq!(preview_of(factory, &dir.path().join("missing.jpg")), None);
        });
    }

    /// Reads the real desktop pictures, which needs the user's desktop:
    /// run it in a signed-in session, not over ssh. What they show is up
    /// to the user, so only the limits are checked.
    #[test]
    #[ignore = "needs a signed-in user's desktop"]
    fn previews_of_the_real_desktop_keep_to_the_limits() {
        let previews = WallpaperPreviews::new().unwrap();
        let first = previews.read().unwrap();
        let monitors = displays::monitors().unwrap();
        for (id, jpeg) in &first {
            println!("{id}: {} bytes", jpeg.len());
            assert!(monitors.iter().any(|m| m.id == *id), "{id}");
            assert_eq!(&jpeg[..3], &[0xff, 0xd8, 0xff]);
            assert!(jpeg.len() <= preview::MAX_BYTES);
        }
        // Unchanged pictures come from what was made before.
        assert_eq!(previews.read().unwrap(), first);
    }
}
