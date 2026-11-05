//! Starting the app for the agent (docs/architecture.md §3: the app is
//! the only thing that talks to the user).
//!
//! When the user has to see or decide something, such as a pairing code
//! another computer asked for, and no app is connected to the agent, the
//! agent asks the broker to start the app (`LaunchApp`). The agent runs as
//! SYSTEM; the broker runs as the user, in the user's session, so the app
//! it starts is the user's, as if started from the Start menu.
//!
//! It starts the app with [`BACKGROUND`]: started without arguments, the
//! app takes the launch for the user opening it and, until first-run setup
//! has been finished, opens setup behind the pairing code. In the
//! background it shows only its icon and what the agent's state calls for:
//! the pairing code window, the notification for a file offer.
//!
//! The installer puts the app in `App\InputWeave.exe` beside
//! `inputweave.exe`. It runs once per session, holding a mutex while it
//! does; a second copy only hands its arguments to the first one, so the
//! broker leaves it alone when it runs already. It starts the app at most
//! once every [`INTERVAL`], also across reconnections, whatever the agent
//! asks.
//!
//! For files copied on another device, which only the app can put on the
//! clipboard (`LaunchReason::ClipboardFiles`), the app is not started while
//! the user quit it: the app notes `QuitByUser` under
//! `HKCU\Software\InputWeave\App` as the user chooses Exit InputWeave…, and
//! clears it whenever it is opened other than by the broker. Something the
//! user has to see, such as a pairing code, starts it all the same.
//!
//! The service puts the broker in a job object that ends what is in it
//! when the service restarts the broker; the app breaks away from it, since
//! the user may keep it open long after.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use crate::cmdline::command_line;
use iw_proto::service::v1::LaunchReason;

/// The shortest time between two starts of the app.
pub(crate) const INTERVAL: Duration = Duration::from_secs(10);

/// The app's argument for a start by the background service, which opens
/// no window of its own (`LaunchArguments.Background` in
/// `apps/windows/InputWeave.Core/Support/LaunchArguments.cs`).
pub(crate) const BACKGROUND: &str = "--background";

/// What starting the app did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Launch {
    Started,
    /// It ran already; nothing was started.
    AlreadyRunning,
}

/// Where the installer puts the app: `App\InputWeave.exe` in the folder of
/// `exe`, the running `inputweave.exe`.
pub(crate) fn installed_app(exe: &Path) -> Option<PathBuf> {
    Some(exe.parent()?.join("App").join("InputWeave.exe"))
}

/// The command line that starts `app` for the agent: its path, quoted, and
/// [`BACKGROUND`].
pub(crate) fn app_command_line(app: &Path) -> String {
    command_line(&app.display().to_string(), &[BACKGROUND])
}

/// When the app was last started, which holds off the next start for
/// [`INTERVAL`]. Free of FFI and clocks (time is passed in), so it is
/// tested on every OS.
#[derive(Debug, Default)]
pub(crate) struct Gate {
    last: Option<Instant>,
}

impl Gate {
    /// Whether to try starting the app at `now`; if so, notes it.
    pub(crate) fn open(&mut self, now: Instant) -> bool {
        let due = self
            .last
            .is_none_or(|at| now.saturating_duration_since(at) >= INTERVAL);
        if due {
            self.last = Some(now);
        }
        due
    }
}

/// Whether starting the app for `reason` is refused: for files on the
/// clipboard while the user quit it, which they did not ask for.
pub(crate) fn refused(reason: LaunchReason, quit_by_user: bool) -> bool {
    reason == LaunchReason::ClipboardFiles && quit_by_user
}

/// Whether the user quit the app, as it notes for this process's user.
#[cfg(windows)]
pub(crate) fn quit_by_user() -> bool {
    use windows::Win32::Foundation::ERROR_SUCCESS;
    use windows::Win32::System::Registry::{HKEY_CURRENT_USER, RRF_RT_REG_DWORD, RegGetValueW};
    use windows::core::w;

    let mut value = 0u32;
    let mut size = size_of::<u32>() as u32;
    // SAFETY: the buffer is a u32 and `size` says so.
    let status = unsafe {
        RegGetValueW(
            HKEY_CURRENT_USER,
            w!(r"Software\InputWeave\App"),
            w!("QuitByUser"),
            RRF_RT_REG_DWORD,
            None,
            Some((&raw mut value).cast()),
            Some(&mut size),
        )
    };
    status == ERROR_SUCCESS && value != 0
}

/// Nobody quits an app here.
#[cfg(not(windows))]
pub(crate) fn quit_by_user() -> bool {
    false
}

/// The app's single-instance mutex, which it holds while it runs
/// (`apps/windows/InputWeave/Support/SingleInstance.cs`). `Local\` names
/// are the session's, the broker's own.
#[cfg(windows)]
const APP_MUTEX: &str = r"Local\InputWeave.App";

/// Starts the installed app as this process's user, in its session, unless
/// it runs already.
#[cfg(windows)]
pub fn launch_installed_app() -> Result<Launch, String> {
    if running() {
        return Ok(Launch::AlreadyRunning);
    }
    let exe = std::env::current_exe().map_err(|e| format!("cannot find inputweave.exe: {e}"))?;
    let app = installed_app(&exe).ok_or("inputweave.exe is in no folder")?;
    if !app.is_file() {
        return Err(format!("the app is not installed at {}", app.display()));
    }
    let pid = start(&app)?;
    tracing::info!(pid, app = %app.display(), args = BACKGROUND, "started the app");
    Ok(Launch::Started)
}

/// Whether the app runs in this session: its mutex exists.
#[cfg(windows)]
fn running() -> bool {
    use windows::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED};
    use windows::Win32::System::Threading::{OpenMutexW, SYNCHRONIZATION_SYNCHRONIZE};
    use windows::core::HSTRING;

    // SAFETY: the name is a valid string; the handle returned is closed.
    match unsafe {
        OpenMutexW(
            SYNCHRONIZATION_SYNCHRONIZE,
            false,
            &HSTRING::from(APP_MUTEX),
        )
    } {
        Ok(mutex) => {
            // SAFETY: the handle was just opened.
            let _ = unsafe { CloseHandle(mutex) };
            true
        }
        // Someone else's, which still means it exists.
        Err(e) => e.code() == ERROR_ACCESS_DENIED.to_hresult(),
    }
}

/// Starts `app` in the background ([`app_command_line`]) without inherited
/// handles, in its folder, with this process's token, environment and
/// desktop. Returns its pid.
#[cfg(windows)]
fn start(app: &Path) -> Result<u32, String> {
    use windows::Win32::Foundation::{CloseHandle, ERROR_ACCESS_DENIED};
    use windows::Win32::System::Threading::{
        CREATE_BREAKAWAY_FROM_JOB, CreateProcessW, PROCESS_CREATION_FLAGS, PROCESS_INFORMATION,
        STARTUPINFOW,
    };
    use windows::core::{HSTRING, PWSTR};

    let application = HSTRING::from(app.as_os_str());
    let folder = HSTRING::from(app.parent().unwrap_or(app).as_os_str());
    let startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        ..Default::default()
    };
    let create = |flags: PROCESS_CREATION_FLAGS| {
        // In a buffer the call may write to.
        let mut line: Vec<u16> = app_command_line(app)
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let mut info = PROCESS_INFORMATION::default();
        // SAFETY: every string is NUL-terminated and outlives the call;
        // the command line buffer is writable. No handle is inherited.
        unsafe {
            CreateProcessW(
                &application,
                Some(PWSTR(line.as_mut_ptr())),
                None,
                None,
                false,
                flags,
                None,
                &folder,
                &startup,
                &mut info,
            )
        }
        .map(|()| info)
    };
    let info = match create(CREATE_BREAKAWAY_FROM_JOB) {
        Ok(info) => info,
        // A job that does not allow it, as one from an older service.
        Err(e) if e.code() == ERROR_ACCESS_DENIED.to_hresult() => {
            tracing::warn!("the app cannot leave the broker's job; it ends with the broker");
            create(PROCESS_CREATION_FLAGS(0)).map_err(|e| format!("cannot start the app: {e}"))?
        }
        Err(e) => return Err(format!("cannot start the app: {e}")),
    };
    // SAFETY: both handles were just returned and are closed once.
    unsafe {
        let _ = CloseHandle(info.hThread);
        let _ = CloseHandle(info.hProcess);
    }
    Ok(info.dwProcessId)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Files on the clipboard do not start the app the user quit; what the
    /// user has to see does.
    #[test]
    fn the_app_the_user_quit_is_started_only_to_show_something() {
        assert!(refused(LaunchReason::ClipboardFiles, true));
        assert!(!refused(LaunchReason::ClipboardFiles, false));
        assert!(!refused(LaunchReason::AskUser, true));
        assert!(!refused(LaunchReason::AskUser, false));
    }

    #[test]
    fn the_app_is_installed_beside_the_executable() {
        assert_eq!(
            installed_app(Path::new("/Program Files/InputWeave/inputweave.exe")),
            Some(PathBuf::from(
                "/Program Files/InputWeave/App/InputWeave.exe"
            ))
        );
    }

    #[test]
    fn the_app_is_started_in_the_background() {
        // Quoted, or Windows would try C:\Program.exe first.
        assert_eq!(
            app_command_line(Path::new(r"C:\Program Files\InputWeave\App\InputWeave.exe")),
            r#""C:\Program Files\InputWeave\App\InputWeave.exe" --background"#
        );
    }

    #[test]
    fn the_app_is_started_at_most_once_per_interval() {
        let start = Instant::now();
        let mut gate = Gate::default();
        assert!(gate.open(start));
        assert!(!gate.open(start + Duration::from_secs(1)));
        assert!(!gate.open(start + INTERVAL - Duration::from_millis(1)));
        assert!(gate.open(start + INTERVAL));
        assert!(!gate.open(start + INTERVAL + Duration::from_secs(9)));
    }
}
