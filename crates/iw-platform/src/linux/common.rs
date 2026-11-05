//! What the X11 and Wayland backends share: errors, locks that outlive a
//! panic elsewhere, waiting for descriptors, and when to connect again.
//! Free of all but `poll`, so it is tested on every Unix.

use std::os::fd::{AsRawFd, RawFd};
use std::sync::{Condvar, MutexGuard};
use std::time::{Duration, Instant};

use crate::PlatformError;

/// An error from the display server, as the agent reports platform errors.
pub(crate) fn os(what: &str, e: impl std::fmt::Display) -> PlatformError {
    PlatformError::Os(format!("{what}: {e}"))
}

pub(crate) use super::lock;

/// Waits on `condvar` with `guard`, as `lock` does if another thread
/// panicked.
pub(crate) fn wait<'a, T>(condvar: &Condvar, guard: MutexGuard<'a, T>) -> MutexGuard<'a, T> {
    condvar.wait(guard).unwrap_or_else(|e| e.into_inner())
}

/// Like `wait`, for at most `wait`.
pub(crate) fn wait_timeout<'a, T>(
    condvar: &Condvar,
    guard: MutexGuard<'a, T>,
    wait: Duration,
) -> MutexGuard<'a, T> {
    condvar
        .wait_timeout(guard, wait)
        .map_or_else(|e| e.into_inner().0, |(guard, _)| guard)
}

/// Waits until `fd` is ready for `events`, or `wait` passed; whether it
/// is. A signal arriving meanwhile does not cut the wait short.
pub(crate) fn ready(fd: &impl AsRawFd, events: libc::c_short, wait: Duration) -> bool {
    poll([(fd.as_raw_fd(), events)], Some(wait)).is_ok_and(|[revents]| revents != 0)
}

/// Waits until any of `fds` is ready for its events (or hung up, or in
/// error), or `wait` passed, for ever without one: what each is ready for
/// (`revents`). A descriptor that is no longer open, or poll failing, is
/// an error, never a wait that ends at once again and again. A signal
/// arriving meanwhile does not cut the wait short.
pub(crate) fn poll_each<const N: usize>(
    fds: [(RawFd, libc::c_short); N],
    wait: Option<Duration>,
) -> std::io::Result<[libc::c_short; N]> {
    let revents = poll(fds, wait)?;
    if revents.iter().any(|r| r & libc::POLLNVAL != 0) {
        return Err(std::io::Error::from_raw_os_error(libc::EBADF));
    }
    Ok(revents)
}

fn poll<const N: usize>(
    fds: [(RawFd, libc::c_short); N],
    wait: Option<Duration>,
) -> std::io::Result<[libc::c_short; N]> {
    let deadline = wait.map(|w| Instant::now() + w);
    loop {
        let mut polls = fds.map(|(fd, events)| libc::pollfd {
            fd,
            events,
            revents: 0,
        });
        let ms = match deadline {
            // Rounded up, so a wait under a millisecond still waits.
            Some(at) => {
                let left = at.saturating_duration_since(Instant::now());
                i32::try_from(left.as_micros().div_ceil(1000)).unwrap_or(i32::MAX)
            }
            None => -1,
        };
        // SAFETY: N valid pollfds for the call.
        let ready = unsafe { libc::poll(polls.as_mut_ptr(), N as libc::nfds_t, ms) };
        if ready >= 0 {
            return Ok(polls.map(|p| p.revents));
        }
        let e = std::io::Error::last_os_error();
        if e.kind() != std::io::ErrorKind::Interrupted {
            return Err(e);
        }
    }
}

/// When to try connecting to the display server again: from 0.5 s,
/// doubling to 10 s, after each failure.
pub(crate) struct Backoff {
    next: Duration,
    retry_at: Option<Instant>,
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

impl Backoff {
    const FIRST: Duration = Duration::from_millis(500);
    const LONGEST: Duration = Duration::from_secs(10);

    pub(crate) fn new() -> Self {
        Self {
            next: Self::FIRST,
            retry_at: None,
        }
    }

    /// Whether it may be tried now.
    pub(crate) fn ready(&self) -> bool {
        self.retry_at.is_none_or(|at| Instant::now() >= at)
    }

    /// How long to wait before the next try.
    pub(crate) fn delay(&self) -> Duration {
        self.next
    }

    pub(crate) fn failed(&mut self) {
        self.retry_at = Some(Instant::now() + self.next);
        self.next = (self.next * 2).min(Self::LONGEST);
    }

    pub(crate) fn succeeded(&mut self) {
        *self = Self::new();
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::os::fd::OwnedFd;

    use super::*;

    #[test]
    fn a_failure_puts_the_next_try_off_longer_each_time() {
        let mut backoff = Backoff::new();
        assert!(backoff.ready());
        let mut delays = Vec::new();
        for _ in 0..7 {
            delays.push(backoff.delay());
            backoff.failed();
            assert!(!backoff.ready());
        }
        let secs: Vec<f64> = delays.iter().map(Duration::as_secs_f64).collect();
        assert_eq!(secs, [0.5, 1.0, 2.0, 4.0, 8.0, 10.0, 10.0]);
        backoff.succeeded();
        assert!(backoff.ready());
        assert_eq!(backoff.delay(), Duration::from_millis(500));
        // Ready again once the delay passed.
        backoff.failed();
        std::thread::sleep(Duration::from_millis(550));
        assert!(backoff.ready());
    }

    #[test]
    fn ready_waits_its_time_and_no_less() {
        let (reader, writer) = std::io::pipe().unwrap();
        // Nothing to read: the whole wait, a part of a millisecond too.
        for wait in [Duration::from_micros(300), Duration::from_millis(30)] {
            let started = Instant::now();
            assert!(!ready(&reader, libc::POLLIN, wait));
            assert!(
                started.elapsed() >= wait,
                "{:?} of {wait:?}",
                started.elapsed()
            );
        }
        // Something to read: at once.
        let mut writer = std::fs::File::from(OwnedFd::from(writer));
        writer.write_all(b"x").unwrap();
        let started = Instant::now();
        assert!(ready(&reader, libc::POLLIN, Duration::from_secs(5)));
        assert!(started.elapsed() < Duration::from_secs(1));
    }

    #[test]
    fn poll_each_tells_what_each_is_ready_for() {
        let (first, _first_writer) = std::io::pipe().unwrap();
        let (second, writer) = std::io::pipe().unwrap();
        let fds = [
            (first.as_raw_fd(), libc::POLLIN),
            (second.as_raw_fd(), libc::POLLIN),
        ];
        assert_eq!(
            poll_each(fds, Some(Duration::from_millis(20))).unwrap(),
            [0, 0]
        );
        // A pipe with room is ready to write to at once.
        let room = poll_each([(writer.as_raw_fd(), libc::POLLOUT)], None).unwrap();
        assert_ne!(room[0] & libc::POLLOUT, 0);
        let mut writer = std::fs::File::from(OwnedFd::from(writer));
        writer.write_all(b"x").unwrap();
        // Without a deadline too, once one is.
        let [a, b] = poll_each(fds, None).unwrap();
        assert_eq!((a, b & libc::POLLIN), (0, libc::POLLIN));
    }

    #[test]
    fn a_descriptor_no_longer_open_is_an_error_not_a_wait() {
        // One no test running beside this one can have opened meanwhile, as
        // it might a number just closed.
        let closed = i32::MAX;
        // Even without a deadline: it returns at once, as an error.
        assert!(poll_each([(closed, libc::POLLIN)], None).is_err());
    }

    #[test]
    fn a_signal_does_not_cut_a_wait_short() {
        extern "C" fn nothing(_: libc::c_int) {}
        // SAFETY: a handler that does nothing, installed without
        // SA_RESTART so poll is interrupted.
        unsafe {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = nothing as extern "C" fn(libc::c_int) as libc::sighandler_t;
            libc::sigaction(libc::SIGUSR2, &action, std::ptr::null_mut());
        }
        // SAFETY: pthread_self is always valid.
        let waiting = unsafe { libc::pthread_self() };
        let (reader, writer) = std::io::pipe().unwrap();
        let signalling = |then: Option<std::io::PipeWriter>| {
            std::thread::spawn(move || {
                std::thread::sleep(Duration::from_millis(100));
                // SAFETY: the waiting thread outlives this one's join.
                unsafe { libc::pthread_kill(waiting, libc::SIGUSR2) };
                std::thread::sleep(Duration::from_millis(100));
                if let Some(writer) = then {
                    let mut writer = std::fs::File::from(OwnedFd::from(writer));
                    writer.write_all(b"late").unwrap();
                }
            })
        };
        // Interrupted, it goes on waiting the rest of its time.
        let signal = signalling(None);
        let started = Instant::now();
        assert!(!ready(&reader, libc::POLLIN, Duration::from_millis(400)));
        assert!(started.elapsed() >= Duration::from_millis(400));
        signal.join().unwrap();
        // And sees what comes after.
        let signal = signalling(Some(writer));
        let started = Instant::now();
        assert!(ready(&reader, libc::POLLIN, Duration::from_secs(5)));
        assert!(started.elapsed() >= Duration::from_millis(200));
        signal.join().unwrap();
    }
}
