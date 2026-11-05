use iw_core::geometry::Rect;
use iw_core::layout::Display;
use objc2_core_graphics::{
    CGDirectDisplayID, CGDisplayBounds, CGDisplayCopyDisplayMode, CGDisplayIsMain,
    CGDisplayMirrorsDisplay, CGDisplayMode, CGError, CGGetActiveDisplayList,
};

use crate::{Displays, PlatformError};

const MAX_DISPLAYS: usize = 32;

/// Active displays in global display coordinates: points, origin at the
/// top left of the main display, y growing downwards. Mirrors of another
/// display are left out; the display they mirror stands for both.
#[derive(Debug, Default)]
pub struct MacDisplays;

impl Displays for MacDisplays {
    fn displays(&self) -> Result<Vec<Display>, PlatformError> {
        let mut ids: [CGDirectDisplayID; MAX_DISPLAYS] = [0; MAX_DISPLAYS];
        let mut count = 0u32;
        // SAFETY: both pointers are valid for the sizes passed.
        let err =
            unsafe { CGGetActiveDisplayList(MAX_DISPLAYS as u32, ids.as_mut_ptr(), &mut count) };
        if err != CGError::Success {
            return Err(PlatformError::Os(format!(
                "CGGetActiveDisplayList failed: {}",
                err.0
            )));
        }
        Ok(ids[..count as usize]
            .iter()
            .filter(|&&id| CGDisplayMirrorsDisplay(id) == 0)
            .map(|&id| {
                let b = CGDisplayBounds(id);
                Display {
                    id: id.to_string(),
                    bounds: Rect::new(b.origin.x, b.origin.y, b.size.width, b.size.height),
                    scale: scale_of(id),
                    primary: CGDisplayIsMain(id),
                }
            })
            .collect())
    }
}

/// Physical pixels per point in the display's current mode.
fn scale_of(id: CGDirectDisplayID) -> f64 {
    let mode = CGDisplayCopyDisplayMode(id);
    let points = CGDisplayMode::width(mode.as_deref());
    let pixels = CGDisplayMode::pixel_width(mode.as_deref());
    if points == 0 || pixels == 0 {
        1.0
    } else {
        pixels as f64 / points as f64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reads the real displays; needs no permission.
    #[test]
    fn lists_the_main_display() {
        let displays = MacDisplays.displays().unwrap();
        if displays.is_empty() {
            // A headless CI runner may have no display at all.
            return;
        }
        let main: Vec<&Display> = displays.iter().filter(|d| d.primary).collect();
        assert_eq!(main.len(), 1, "{displays:?}");
        assert_eq!((main[0].bounds.x, main[0].bounds.y), (0.0, 0.0));
        assert!(
            displays
                .iter()
                .all(|d| d.scale >= 1.0 && d.bounds.width > 0.0)
        );
    }
}
