//! The main thread's run loop, which AppKit hands requests for promised
//! pasteboard content to: a provider is only ever asked through it,
//! whichever thread declared the promise, and never while the main thread
//! waits on anything else.

use std::ffi::c_void;
use std::ptr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use objc2_core_foundation::{
    CFRetained, CFRunLoop, CFRunLoopSource, CFRunLoopSourceContext, kCFRunLoopDefaultMode,
};

/// Runs `work` on a thread of its own while this thread, the main thread
/// in the agent, runs its run loop, and returns what `work` returns once
/// it is done. A panic in `work` carries on here.
pub fn run_beside_main_loop<T: Send + 'static>(work: impl FnOnce() -> T + Send + 'static) -> T {
    let this_loop = CFRunLoop::current().expect("every thread has a run loop");
    // Performing this source stops the run loop. Signalled before the loop
    // runs, it is performed as soon as it does, so no stop is lost; and as
    // a source it keeps the loop running until then.
    let mut context = CFRunLoopSourceContext {
        version: 0,
        info: ptr::null_mut(),
        retain: None,
        release: None,
        copyDescription: None,
        equal: None,
        hash: None,
        schedule: None,
        cancel: None,
        perform: Some(stop_this_loop),
    };
    // SAFETY: the context holds no pointers; CF copies it.
    let stop = unsafe { CFRunLoopSource::new(None, 0, &mut context) }
        .expect("cannot make a run loop source");
    // SAFETY: reading an immutable framework constant.
    let mode = unsafe { kCFRunLoopDefaultMode };
    this_loop.add_source(Some(&stop), mode);
    let finished = Arc::new(AtomicBool::new(false));
    let done = Done {
        stop: stop.clone(),
        run_loop: this_loop.clone(),
        finished: finished.clone(),
    };
    let worker = std::thread::Builder::new()
        .name("agent".to_owned())
        .spawn(move || {
            // Dropped when the work returns or panics.
            let _done = done;
            work()
        })
        .expect("cannot start the agent's thread");
    while !finished.load(Ordering::SeqCst) {
        CFRunLoop::run();
    }
    stop.invalidate();
    match worker.join() {
        Ok(value) => value,
        Err(panic) => std::panic::resume_unwind(panic),
    }
}

/// Tells the run loop the work is done, as the work's thread ends.
struct Done {
    stop: CFRetained<CFRunLoopSource>,
    run_loop: CFRetained<CFRunLoop>,
    finished: Arc<AtomicBool>,
}

// SAFETY: signalling a source and waking a run loop may be done from any
// thread.
unsafe impl Send for Done {}

impl Drop for Done {
    fn drop(&mut self) {
        self.finished.store(true, Ordering::SeqCst);
        self.stop.signal();
        self.run_loop.wake_up();
    }
}

unsafe extern "C-unwind" fn stop_this_loop(_info: *mut c_void) {
    if let Some(run_loop) = CFRunLoop::current() {
        run_loop.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Off the main thread in tests, the loop is this thread's own; it
    /// still waits for the work, done at once or later, and returns what
    /// it returns.
    #[test]
    fn returns_what_the_work_returns() {
        assert_eq!(run_beside_main_loop(|| 6 * 7), 42);
        let later = run_beside_main_loop(|| {
            std::thread::sleep(std::time::Duration::from_millis(200));
            "later"
        });
        assert_eq!(later, "later");
    }

    #[test]
    #[should_panic(expected = "in the work")]
    fn a_panic_in_the_work_carries_on() {
        run_beside_main_loop(|| panic!("in the work"));
    }
}
