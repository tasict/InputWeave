//! Starting and stopping the agent and broker in a user session.

use std::ffi::c_void;
use std::fs::File;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::Path;

use windows::Win32::Foundation::{
    HANDLE, HANDLE_FLAG_INHERIT, SetHandleInformation, WAIT_OBJECT_0,
};
use windows::Win32::System::Environment::{CreateEnvironmentBlock, DestroyEnvironmentBlock};
use windows::Win32::System::Threading::{
    CREATE_NO_WINDOW, CREATE_SUSPENDED, CREATE_UNICODE_ENVIRONMENT, CreateProcessAsUserW,
    GetExitCodeProcess, PROCESS_INFORMATION, ResumeThread, STARTF_USESTDHANDLES, STARTUPINFOW,
    TerminateProcess, WaitForSingleObject,
};
use windows::core::{PCWSTR, PWSTR};

use crate::cmdline::command_line;
use crate::job::Job;
use crate::supervisor::Role;
use crate::{logs, token};

/// The interactive window station and desktop. The agent moves its input
/// threads to whichever desktop receives input from there.
const DESKTOP: &str = r"winsta0\default";

/// How long to wait for a terminated process to go away.
const STOP_WAIT_MS: u32 = 5_000;

/// A process the service started, and the job that ends it if the service
/// process dies. Dropping it closes the job, which kills whatever is still
/// in it.
#[derive(Debug)]
pub(crate) struct Child {
    process: OwnedHandle,
    _job: Option<Job>,
    pub pid: u32,
}

impl Child {
    /// Signaled once the process exits.
    pub(crate) fn handle(&self) -> HANDLE {
        HANDLE(self.process.as_raw_handle())
    }

    pub(crate) fn exit_code(&self) -> Option<u32> {
        let mut code = 0;
        // SAFETY: the handle is open and the out pointer valid.
        unsafe { GetExitCodeProcess(self.handle(), &mut code) }.ok()?;
        Some(code)
    }

    /// Ends the process at once and waits for it to go.
    ///
    /// The agent gets no chance to release keys it injected; the session it
    /// ran in is either going away or gets a new agent right after.
    pub(crate) fn terminate(self) {
        // SAFETY: the handle is open; terminating an exited process fails
        // harmlessly.
        unsafe {
            let _ = TerminateProcess(self.handle(), 1);
            if WaitForSingleObject(self.handle(), STOP_WAIT_MS) != WAIT_OBJECT_0 {
                tracing::warn!(pid = self.pid, "a stopped process did not exit");
            }
        }
    }
}

/// Starts `exe <role>` in `session`: the agent with a copy of this
/// service's SYSTEM token, the broker with the token of the user logged on
/// there. Output goes to the role's log file.
///
/// The process is put into a job of its own before it runs. If that fails
/// it runs anyway: the service still restarts and stops it, it just
/// outlives the service process if that dies.
pub(crate) fn start(role: Role, session: u32, exe: &Path) -> Result<Child, String> {
    let token = match role {
        Role::Agent => token::system_token_for(session),
        Role::Broker => token::user_token(session),
    }
    .map_err(|e| format!("cannot get a token for session {session}: {e}"))?;
    let environment =
        Environment::of(&token).map_err(|e| format!("cannot build the environment: {e}"))?;
    let program = exe
        .to_str()
        .ok_or_else(|| format!("the executable path {} is not Unicode", exe.display()))?;
    let application = wide(program);
    let mut line = wide(&command_line(program, &[role.subcommand()]));
    let mut desktop = wide(DESKTOP);
    let log = match inheritable_log(role) {
        Ok(log) => Some(log),
        Err(e) => {
            tracing::warn!(?role, error = %e, "cannot open the log file; output is discarded");
            None
        }
    };
    let mut startup = STARTUPINFOW {
        cb: size_of::<STARTUPINFOW>() as u32,
        lpDesktop: PWSTR(desktop.as_mut_ptr()),
        ..Default::default()
    };
    if let Some(log) = &log {
        startup.dwFlags = STARTF_USESTDHANDLES;
        startup.hStdOutput = HANDLE(log.as_raw_handle());
        startup.hStdError = HANDLE(log.as_raw_handle());
    }
    let mut info = PROCESS_INFORMATION::default();
    // SAFETY: every buffer is NUL-terminated and outlives the call; the
    // command line buffer is writable, as CreateProcessAsUserW requires. Only
    // the log handle is inheritable while this runs: the service starts one
    // process at a time and opens no other inheritable handles (the jobs'
    // in particular are not).
    unsafe {
        CreateProcessAsUserW(
            Some(token::raw(&token)),
            PCWSTR(application.as_ptr()),
            Some(PWSTR(line.as_mut_ptr())),
            None,
            None,
            log.is_some(),
            CREATE_UNICODE_ENVIRONMENT | CREATE_NO_WINDOW | CREATE_SUSPENDED,
            Some(environment.0),
            PCWSTR::null(),
            &startup,
            &mut info,
        )
    }
    .map_err(|e| format!("cannot start {}: {e}", role.subcommand()))?;
    // SAFETY: both handles were just returned to us and are closed on drop.
    let (process, thread) = unsafe {
        (
            OwnedHandle::from_raw_handle(info.hProcess.0),
            OwnedHandle::from_raw_handle(info.hThread.0),
        )
    };
    let child = Child {
        _job: contain(role, info.dwProcessId, HANDLE(process.as_raw_handle())),
        process,
        pid: info.dwProcessId,
    };
    // SAFETY: the thread handle is open.
    if unsafe { ResumeThread(HANDLE(thread.as_raw_handle())) } == u32::MAX {
        let e = windows::core::Error::from_thread();
        // Left suspended it would never exit, and the service would wait
        // for it forever.
        child.terminate();
        return Err(format!("cannot resume {}: {e}", role.subcommand()));
    }
    Ok(child)
}

/// A new job holding the suspended process, or `None`, logged, if it
/// cannot be made. The broker's lets the app it starts break away.
fn contain(role: Role, pid: u32, process: HANDLE) -> Option<Job> {
    Job::new(role == Role::Broker)
        .and_then(|job| job.assign(process).map(|()| job))
        .inspect_err(|e| {
            tracing::warn!(
                ?role,
                pid,
                error = %e,
                "cannot put the process in a job; it would outlive the service process"
            );
        })
        .ok()
}

/// The role's log file, opened so a child can inherit it.
fn inheritable_log(role: Role) -> std::io::Result<File> {
    let file = logs::open(role.subcommand())?;
    // SAFETY: the handle is open for as long as `file` lives.
    unsafe {
        SetHandleInformation(
            HANDLE(file.as_raw_handle()),
            HANDLE_FLAG_INHERIT.0,
            HANDLE_FLAG_INHERIT,
        )?;
    }
    Ok(file)
}

/// The environment a token's user would get, as built by Windows.
struct Environment(*mut c_void);

impl Environment {
    fn of(token: &OwnedHandle) -> windows::core::Result<Self> {
        let mut block = std::ptr::null_mut();
        // SAFETY: the out pointer is valid; the block is freed on drop.
        unsafe { CreateEnvironmentBlock(&mut block, Some(token::raw(token)), false)? };
        Ok(Self(block))
    }
}

impl Drop for Environment {
    fn drop(&mut self) {
        // SAFETY: the block came from CreateEnvironmentBlock and is freed once.
        unsafe {
            let _ = DestroyEnvironmentBlock(self.0);
        }
    }
}

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(Some(0)).collect()
}
