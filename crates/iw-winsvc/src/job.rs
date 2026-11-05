//! The job objects that tie the agent and broker to the service process.
//!
//! Windows does not end a process's children when it exits. If the service
//! process itself died, the agent and broker it started would keep running,
//! the agent holding its pipe and QUIC port, and the agent of the service
//! the service control manager restarts could not bind them. So every
//! process the service starts goes into a job that kills its processes once
//! its last handle closes. Only the service holds a handle, so the system
//! closes it when the service process ends, however it ends.
//!
//! Each process gets a job of its own ([`crate::launch::Child`] owns it). A
//! job takes the session of the first process put in it and refuses
//! processes of any other session with `ERROR_ACCESS_DENIED`, even once it
//! is empty again, and the service starts the agent in whichever session
//! is attached to the console. A single job for the service's lifetime
//! held nothing after the console moved to another session.
//!
//! On Windows 11 the service itself already runs inside a job, as the
//! other services there do, but that job lets the processes started from
//! it break away silently, so it holds neither the agent nor the broker.
//!
//! This is the safety net. Stopping the service still ends the agent and
//! broker one by one first ([`crate::launch::Child::terminate`]).
//!
//! The broker's job lets processes break away when they are started with
//! `CREATE_BREAKAWAY_FROM_JOB`: the broker starts the user's app that way
//! (`crate::broker::launch_installed_app`), which must not end whenever the
//! service restarts the broker. Nothing else the broker or agent starts
//! leaves its job.

use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};

use windows::Win32::Foundation::HANDLE;
use windows::Win32::System::JobObjects::{
    AssignProcessToJobObject, CreateJobObjectW, IsProcessInJob, JOB_OBJECT_LIMIT,
    JOB_OBJECT_LIMIT_BREAKAWAY_OK, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JobObjectExtendedLimitInformation,
    QueryInformationJobObject, SetInformationJobObject,
};
use windows::Win32::System::Threading::GetCurrentProcess;
use windows::core::{BOOL, PCWSTR, Result};

/// A job whose processes are killed when it closes.
#[derive(Debug)]
pub(crate) struct Job(OwnedHandle);

impl Job {
    /// An unnamed job, with a handle that is not inheritable: a process
    /// the service starts must not hold the job open after the service is
    /// gone. With `breakaway`, processes started in it with
    /// `CREATE_BREAKAWAY_FROM_JOB` are left out of it.
    pub(crate) fn new(breakaway: bool) -> Result<Self> {
        // SAFETY: without security attributes the handle is not
        // inheritable; the handle returned is ours to close.
        let job =
            unsafe { OwnedHandle::from_raw_handle(CreateJobObjectW(None, PCWSTR::null())?.0) };
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        if breakaway {
            limits.BasicLimitInformation.LimitFlags |= JOB_OBJECT_LIMIT_BREAKAWAY_OK;
        }
        // SAFETY: the buffer is the structure the information class names,
        // of the size passed.
        unsafe {
            SetInformationJobObject(
                HANDLE(job.as_raw_handle()),
                JobObjectExtendedLimitInformation,
                (&raw const limits).cast(),
                size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )?;
        }
        Ok(Self(job))
    }

    /// Puts a process into the job. The handle needs `PROCESS_SET_QUOTA`
    /// and `PROCESS_TERMINATE`, which a handle from `CreateProcessAsUserW`
    /// has. Fails with `ERROR_ACCESS_DENIED` for a process in another
    /// session than the job's first one.
    pub(crate) fn assign(&self, process: HANDLE) -> Result<()> {
        // SAFETY: both handles are open.
        unsafe { AssignProcessToJobObject(HANDLE(self.0.as_raw_handle()), process) }
    }
}

/// The limits of the job this process runs in, or `None` outside a job.
pub(crate) fn enclosing_limits() -> Result<Option<JOB_OBJECT_LIMIT>> {
    let mut in_job = BOOL::default();
    // SAFETY: the pseudo handle needs no closing; the out pointer is valid.
    unsafe { IsProcessInJob(GetCurrentProcess(), None, &mut in_job)? };
    if !in_job.as_bool() {
        return Ok(None);
    }
    let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
    // SAFETY: without a handle this queries the job of the calling
    // process; the buffer is the structure the information class names,
    // of the size passed.
    unsafe {
        QueryInformationJobObject(
            None,
            JobObjectExtendedLimitInformation,
            (&raw mut limits).cast(),
            size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            None,
        )?;
    }
    Ok(Some(limits.BasicLimitInformation.LimitFlags))
}
