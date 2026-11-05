//! The service itself: the service control manager's side, and the loop
//! that keeps the agent and broker running.

use std::collections::HashMap;
use std::ffi::c_void;
use std::path::Path;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{
    ERROR_CALL_NOT_IMPLEMENTED, ERROR_FAILED_SERVICE_CONTROLLER_CONNECT, HANDLE, NO_ERROR,
    WAIT_FAILED, WAIT_OBJECT_0,
};
use windows::Win32::System::RemoteDesktop::{
    WTSGetActiveConsoleSessionId, WTSSESSION_NOTIFICATION,
};
use windows::Win32::System::Services::{
    RegisterServiceCtrlHandlerExW, SERVICE_ACCEPT_SESSIONCHANGE, SERVICE_ACCEPT_SHUTDOWN,
    SERVICE_ACCEPT_STOP, SERVICE_CONTROL_INTERROGATE, SERVICE_CONTROL_SESSIONCHANGE,
    SERVICE_CONTROL_SHUTDOWN, SERVICE_CONTROL_STOP, SERVICE_RUNNING, SERVICE_START_PENDING,
    SERVICE_STATUS, SERVICE_STATUS_CURRENT_STATE, SERVICE_STATUS_HANDLE, SERVICE_STOP_PENDING,
    SERVICE_STOPPED, SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS, SetServiceStatus,
    StartServiceCtrlDispatcherW,
};
use windows::Win32::System::Threading::{CreateEventW, SetEvent, WaitForMultipleObjects};
use windows::core::{PCWSTR, PWSTR};

use crate::job;
use crate::launch::{self, Child};
use crate::status::{State, Status};
use crate::supervisor::{Action, Console, Role, SessionEvent, Supervisor};
use crate::{sas, token};

/// The name the service is installed under.
pub const SERVICE_NAME: &str = "InputWeave";

/// How often the console is checked even without notifications.
const POLL: Duration = Duration::from_secs(1);

#[derive(Debug, thiserror::Error)]
pub enum ServiceError {
    #[error(
        "not started by the service control manager; install the service and start it \
         with `sc start {SERVICE_NAME}`"
    )]
    NotAService,
    #[error(transparent)]
    Os(#[from] windows::core::Error),
}

/// Runs as the InputWeave service until it is stopped. Fails at once when
/// the process was not started by the service control manager.
pub fn run() -> Result<(), ServiceError> {
    let mut name = wide(SERVICE_NAME);
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: PWSTR(name.as_mut_ptr()),
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW::default(),
    ];
    // SAFETY: the table and the name it points to outlive the dispatcher,
    // which returns only once the service has stopped.
    match unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) } {
        Ok(()) => Ok(()),
        Err(e) if e.code() == ERROR_FAILED_SERVICE_CONTROLLER_CONNECT.to_hresult() => {
            Err(ServiceError::NotAService)
        }
        Err(e) => Err(e.into()),
    }
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut PWSTR) {
    if let Err(e) = serve() {
        tracing::error!(error = %e, "the service failed");
    }
}

/// A control from the service control manager, queued by [`handler`] for
/// the supervising loop.
#[derive(Debug)]
enum Control {
    Stop,
    Session { event: SessionEvent, session: u32 },
}

/// Shared between [`handler`], which runs on the dispatcher's thread and
/// must return at once, and the supervising loop.
struct Controls {
    queue: Mutex<Vec<Control>>,
    /// An auto-reset event that wakes the loop. Never closed: the service
    /// control manager may call the handler until the process exits.
    wake: HANDLE,
}

// SAFETY: the queue is behind a mutex, and an event handle may be signaled
// and waited on from any thread.
unsafe impl Send for Controls {}
// SAFETY: as above.
unsafe impl Sync for Controls {}

impl Controls {
    fn push(&self, control: Control) {
        self.queue
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(control);
        // SAFETY: the event lives as long as the process.
        let _ = unsafe { SetEvent(self.wake) };
    }

    fn take(&self) -> Vec<Control> {
        std::mem::take(&mut *self.queue.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

unsafe extern "system" fn handler(
    control: u32,
    event_type: u32,
    event_data: *mut c_void,
    context: *mut c_void,
) -> u32 {
    // SAFETY: the context is the `Controls` registered with this handler,
    // which is never freed.
    let controls = unsafe { &*context.cast::<Controls>() };
    match control {
        SERVICE_CONTROL_STOP | SERVICE_CONTROL_SHUTDOWN => {
            controls.push(Control::Stop);
            NO_ERROR.0
        }
        SERVICE_CONTROL_SESSIONCHANGE if !event_data.is_null() => {
            // SAFETY: for session changes the event data is a
            // WTSSESSION_NOTIFICATION.
            let session = unsafe { (*event_data.cast::<WTSSESSION_NOTIFICATION>()).dwSessionId };
            controls.push(Control::Session {
                event: SessionEvent::from_code(event_type),
                session,
            });
            NO_ERROR.0
        }
        SERVICE_CONTROL_INTERROGATE => NO_ERROR.0,
        _ => ERROR_CALL_NOT_IMPLEMENTED.0,
    }
}

/// Reports the service's state to the service control manager.
struct Reporter {
    handle: SERVICE_STATUS_HANDLE,
    status: Status,
}

impl Reporter {
    fn report(&mut self, state: State) {
        let report = match self.status.enter(state) {
            Ok(report) => report,
            Err(e) => {
                tracing::error!(error = %e, "not reported");
                return;
            }
        };
        let current: SERVICE_STATUS_CURRENT_STATE = match report.state {
            State::StartPending => SERVICE_START_PENDING,
            State::Running => SERVICE_RUNNING,
            State::StopPending => SERVICE_STOP_PENDING,
            State::Stopped => SERVICE_STOPPED,
        };
        let status = SERVICE_STATUS {
            dwServiceType: SERVICE_WIN32_OWN_PROCESS,
            dwCurrentState: current,
            dwControlsAccepted: if report.accepts_controls {
                SERVICE_ACCEPT_STOP | SERVICE_ACCEPT_SHUTDOWN | SERVICE_ACCEPT_SESSIONCHANGE
            } else {
                0
            },
            dwWin32ExitCode: NO_ERROR.0,
            dwServiceSpecificExitCode: 0,
            dwCheckPoint: report.checkpoint,
            dwWaitHint: report.wait_hint_ms,
        };
        // SAFETY: the handle came from RegisterServiceCtrlHandlerExW and
        // the status is valid for the call.
        if let Err(e) = unsafe { SetServiceStatus(self.handle, &status) } {
            tracing::error!(error = %e, ?state, "cannot report the service state");
        }
    }
}

fn serve() -> Result<(), ServiceError> {
    // SAFETY: an unnamed auto-reset event with default security; it lives
    // as long as the process, like the `Controls` holding it.
    let wake = unsafe { CreateEventW(None, false, false, PCWSTR::null())? };
    let controls: &'static Controls = Box::leak(Box::new(Controls {
        queue: Mutex::new(Vec::new()),
        wake,
    }));
    let name = wide(SERVICE_NAME);
    // SAFETY: the handler only reads `controls`, which is never freed.
    let handle = unsafe {
        RegisterServiceCtrlHandlerExW(
            PCWSTR(name.as_ptr()),
            Some(handler),
            Some(std::ptr::from_ref(controls).cast()),
        )?
    };
    let mut reporter = Reporter {
        handle,
        status: Status::new(),
    };
    reporter.report(State::StartPending);
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "service starting");
    let exe = match std::env::current_exe() {
        Ok(exe) => exe,
        Err(e) => {
            tracing::error!(error = %e, "cannot locate the executable");
            reporter.report(State::Stopped);
            return Ok(());
        }
    };
    match job::enclosing_limits() {
        Ok(Some(limits)) => tracing::info!(
            limits = %format!("{:#x}", limits.0),
            "the service runs inside a job"
        ),
        Ok(None) => {}
        Err(e) => tracing::warn!(error = %e, "cannot tell whether the service runs in a job"),
    }
    if let Err(e) = std::thread::Builder::new()
        .name("inputweave-sas".to_owned())
        .spawn(|| {
            if let Err(e) = sas::serve() {
                tracing::error!(error = %e, "cannot serve Ctrl+Alt+Del requests");
            }
        })
    {
        tracing::error!(error = %e, "cannot serve Ctrl+Alt+Del requests");
    }
    reporter.report(State::Running);
    supervise(controls, &exe, || reporter.report(State::StopPending));
    reporter.report(State::Stopped);
    tracing::info!("service stopped");
    Ok(())
}

/// Keeps the agent and broker running until a stop control arrives, then
/// calls `stopping` and terminates them.
fn supervise(controls: &Controls, exe: &Path, stopping: impl FnOnce()) {
    let mut supervisor = Supervisor::default();
    let mut children: HashMap<Role, Child> = HashMap::new();
    'run: loop {
        for control in controls.take() {
            match control {
                Control::Stop => break 'run,
                Control::Session { event, session } => {
                    tracing::info!(?event, session, "session change");
                    supervisor.session_changed(event, session);
                }
            }
        }
        supervisor.observe(console());
        let now = Instant::now();
        for action in supervisor.actions(now) {
            match action {
                Action::Stop { role } => {
                    if let Some(child) = children.remove(&role) {
                        tracing::info!(?role, pid = child.pid, "stopping");
                        child.terminate();
                    }
                }
                Action::Start { role, session } => match launch::start(role, session, exe) {
                    Ok(child) => {
                        tracing::info!(?role, session, pid = child.pid, "started");
                        children.insert(role, child);
                    }
                    Err(e) => {
                        tracing::warn!(?role, session, error = %e, "cannot start");
                        supervisor.exited(role, now);
                    }
                },
            }
        }

        let roles: Vec<Role> = children.keys().copied().collect();
        let handles: Vec<HANDLE> = std::iter::once(controls.wake)
            .chain(roles.iter().map(|role| children[role].handle()))
            .collect();
        let timeout = supervisor
            .next_start()
            .map_or(POLL, |at| at.saturating_duration_since(Instant::now()))
            .min(POLL);
        // SAFETY: every handle is open: the event for the process lifetime,
        // the children until removed from `children` below.
        let woke = unsafe { WaitForMultipleObjects(&handles, false, timeout.as_millis() as u32) };
        if woke == WAIT_FAILED {
            tracing::error!(error = %windows::core::Error::from_thread(), "wait failed");
            std::thread::sleep(POLL);
            continue;
        }
        let index = woke.0.wrapping_sub(WAIT_OBJECT_0.0) as usize;
        if let Some(&role) = index.checked_sub(1).and_then(|i| roles.get(i)) {
            if let Some(child) = children.remove(&role) {
                tracing::warn!(?role, pid = child.pid, code = ?child.exit_code(), "exited");
            }
            supervisor.exited(role, Instant::now());
        }
    }
    stopping();
    for (role, child) in children {
        tracing::info!(?role, pid = child.pid, "stopping");
        child.terminate();
    }
}

/// The session attached to the physical console, and whether a user is
/// logged on to it.
fn console() -> Console {
    // SAFETY: takes no arguments.
    let session = unsafe { WTSGetActiveConsoleSessionId() };
    if session == u32::MAX {
        return Console::default();
    }
    Console {
        session: Some(session),
        user_logged_on: token::has_user(session),
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
