use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use ::windows::Win32::Foundation::{LPARAM, RECT, TRUE};
use ::windows::Win32::Graphics::Gdi::{
    EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
};
use ::windows::Win32::UI::HiDpi::{GetDpiForMonitor, MDT_EFFECTIVE_DPI};
use ::windows::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;
use ::windows::core::BOOL;
use iw_core::layout::Display;

use super::monitors::{DesktopMap, Monitor, PixelRect};
use crate::{Displays, PlatformError};

/// The monitor arrangement, shared by capture, injection and display
/// reporting so all three map coordinates the same way. Re-read when older
/// than [`MonitorCache::MAX_AGE`], since there is no window here to
/// receive `WM_DISPLAYCHANGE`.
#[derive(Debug, Default)]
pub(crate) struct MonitorCache {
    state: Mutex<Option<(Arc<DesktopMap>, Instant)>>,
}

impl MonitorCache {
    const MAX_AGE: Duration = Duration::from_secs(1);

    /// The current arrangement, re-reading it if it is stale. If reading
    /// fails, the last good one is kept.
    pub(crate) fn current(&self) -> Result<Arc<DesktopMap>, PlatformError> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((map, read_at)) = &*state
            && read_at.elapsed() < Self::MAX_AGE
        {
            return Ok(map.clone());
        }
        match monitors() {
            Ok(monitors) => {
                let map = Arc::new(DesktopMap::new(monitors));
                *state = Some((map.clone(), Instant::now()));
                Ok(map)
            }
            Err(e) => match &*state {
                Some((map, _)) => {
                    tracing::warn!(error = %e, "cannot read the monitors; keeping the last arrangement");
                    Ok(map.clone())
                }
                None => Err(e),
            },
        }
    }
}

/// The monitors in logical desktop coordinates, arranged as described in
/// `monitors`. A set of mirrored monitors is one monitor to Windows.
#[derive(Debug)]
pub struct WinDisplays {
    monitors: Arc<MonitorCache>,
}

impl WinDisplays {
    pub(crate) fn new(monitors: Arc<MonitorCache>) -> Self {
        Self { monitors }
    }
}

impl Displays for WinDisplays {
    fn displays(&self) -> Result<Vec<Display>, PlatformError> {
        Ok(self.monitors.current()?.displays())
    }
}

/// Every monitor attached to the desktop, in physical pixels.
pub(super) fn monitors() -> Result<Vec<Monitor>, PlatformError> {
    unsafe extern "system" fn collect(
        monitor: HMONITOR,
        _: HDC,
        _: *mut RECT,
        data: LPARAM,
    ) -> BOOL {
        // SAFETY: `data` is the vector passed to EnumDisplayMonitors below,
        // which outlives the enumeration.
        let handles = unsafe { &mut *(data.0 as *mut Vec<HMONITOR>) };
        handles.push(monitor);
        TRUE
    }

    let mut handles: Vec<HMONITOR> = Vec::new();
    // SAFETY: the callback only runs during this call, while `handles` is
    // borrowed here.
    let ok = unsafe {
        EnumDisplayMonitors(
            None,
            None,
            Some(collect),
            LPARAM(&mut handles as *mut Vec<HMONITOR> as isize),
        )
    };
    if !ok.as_bool() {
        return Err(PlatformError::Os(format!(
            "EnumDisplayMonitors failed: {}",
            ::windows::core::Error::from_thread()
        )));
    }
    Ok(handles.into_iter().filter_map(describe).collect())
}

fn describe(handle: HMONITOR) -> Option<Monitor> {
    let mut info = MONITORINFOEXW {
        monitorInfo: MONITORINFO {
            cbSize: size_of::<MONITORINFOEXW>() as u32,
            ..Default::default()
        },
        ..Default::default()
    };
    // SAFETY: `info` is a MONITORINFOEXW with cbSize set accordingly, which
    // GetMonitorInfoW accepts through a MONITORINFO pointer.
    if !unsafe { GetMonitorInfoW(handle, &mut info.monitorInfo) }.as_bool() {
        return None;
    }
    let (mut dpi_x, mut dpi_y) = (0u32, 0u32);
    // SAFETY: both out pointers are valid for the call.
    let scale = match unsafe { GetDpiForMonitor(handle, MDT_EFFECTIVE_DPI, &mut dpi_x, &mut dpi_y) }
    {
        Ok(()) if dpi_x > 0 => f64::from(dpi_x) / 96.0,
        _ => 1.0,
    };
    let name = &info.szDevice;
    let len = name.iter().position(|&c| c == 0).unwrap_or(name.len());
    let r = info.monitorInfo.rcMonitor;
    Some(Monitor {
        id: String::from_utf16_lossy(&name[..len]),
        pixels: PixelRect::new(r.left, r.top, r.right, r.bottom),
        scale,
        primary: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
    })
}
