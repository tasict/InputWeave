//! The clipboard of the window station this process runs in, for the
//! broker: the agent runs as SYSTEM and never touches the user's
//! clipboard (docs/architecture.md §8).
//!
//! Everything happens on one thread of its own, which owns a message-only
//! window: the window is the clipboard's owner while InputWeave writes it,
//! and it hears of every change (`AddClipboardFormatListener`). Changes
//! other processes make are reported with the clipboard's sequence number;
//! what InputWeave writes carries a format of its own (`OWN_FORMAT`), so
//! its own writes are not. How each format is laid out is in
//! `clipboard_formats`; images are converted between bitmaps and PNG with
//! WIC.
//!
//! Content another device offers is promised rather than written (delayed
//! rendering, `promise`): its formats go on the clipboard without data,
//! and when an app pastes one, Windows asks the window for it
//! (`WM_RENDERFORMAT`), which fetches it from the promise's source while
//! the clipboard thread waits. When something else is copied over it
//! (`WM_DESTROYCLIPBOARD`) the source is told. Formats not rendered when
//! the thread ends are left so and disappear with the window
//! (`WM_RENDERALLFORMATS` is not answered by fetching). A promise whose
//! source went away is withdrawn ([`WinClipboard::withdraw`]): the
//! clipboard is emptied while it still holds that promise.
//!
//! Files copied in File Explorer are read as their paths (`CF_HDROP`),
//! in the same opening of the clipboard as the rest
//! ([`WinClipboard::read_with_files`]), for the broker to check and report
//! with it.

use std::cell::RefCell;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, Sender, SyncSender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use ::windows::Win32::Foundation::{
    E_FAIL, GlobalFree, HANDLE, HGLOBAL, HINSTANCE, HWND, LPARAM, LRESULT, SetLastError,
    WIN32_ERROR, WPARAM,
};
use ::windows::Win32::Graphics::Imaging::{
    CLSID_WICImagingFactory, GUID_ContainerFormatPng, GUID_WICPixelFormat24bppBGR,
    GUID_WICPixelFormat32bppBGRA, GUID_WICPixelFormat32bppPBGRA, GUID_WICPixelFormat32bppPRGBA,
    GUID_WICPixelFormat32bppRGBA, GUID_WICPixelFormat64bppRGBA, IWICBitmapFrameDecode,
    IWICImagingFactory, IWICPalette, WICBitmapDitherTypeNone, WICBitmapEncoderNoCache,
    WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
};
use ::windows::Win32::System::Com::StructuredStorage::CreateStreamOnHGlobal;
use ::windows::Win32::System::Com::{CLSCTX_INPROC_SERVER, CoCreateInstance, STREAM_SEEK_SET};
use ::windows::Win32::System::DataExchange::{
    AddClipboardFormatListener, CloseClipboard, EmptyClipboard, GetClipboardData,
    GetClipboardSequenceNumber, IsClipboardFormatAvailable, OpenClipboard,
    RegisterClipboardFormatW, RemoveClipboardFormatListener, SetClipboardData,
};
use ::windows::Win32::System::LibraryLoader::GetModuleHandleW;
use ::windows::Win32::System::Memory::{
    GMEM_MOVEABLE, GlobalAlloc, GlobalLock, GlobalSize, GlobalUnlock,
};
use ::windows::Win32::System::Threading::GetCurrentThreadId;
use ::windows::Win32::UI::Shell::{DragQueryFileW, HDROP};
use ::windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, HWND_MESSAGE,
    KillTimer, MSG, PM_NOREMOVE, PeekMessageW, PostThreadMessageW, RegisterClassW, SetTimer,
    TranslateMessage, WINDOW_EX_STYLE, WINDOW_STYLE, WM_APP, WM_CLIPBOARDUPDATE,
    WM_DESTROYCLIPBOARD, WM_RENDERALLFORMATS, WM_RENDERFORMAT, WM_TIMER, WM_USER, WNDCLASSW,
};
use ::windows::core::{HSTRING, PCWSTR, w};

use super::clipboard_formats::{
    MAX_PIXELS, bmp_file_from_dib, dib_from_bgra, html_from_cf_html, sensitive, text_from_utf16,
    trim_nuls,
};
use super::desktop_pictures::{Apartment, os, read_all};
use super::promise::{IMAGE_KEEP, Promised, Rendering, clipboard_bytes, promised};
use crate::{
    Clipboard, ClipboardContent, ClipboardFormat, ClipboardPromise, ClipboardRead, ClipboardSource,
    PlatformError,
};

use super::clipboard_formats::{CF_DIB, CF_UNICODETEXT, HTML_FORMAT, PNG_FORMAT, RTF_FORMAT};

const CF_HDROP: u32 = 15;
/// Names read from `CF_HDROP` at most: twice the files a batch takes, so a
/// clipboard of many names the broker leaves out is not read to the end.
const MAX_DROPPED_NAMES: u32 = 2_000;
/// Put on the clipboard with everything InputWeave writes, so the change
/// it makes is not taken for a copy by the user.
const OWN_FORMAT: &str = "InputWeave";
const EXCLUDE_FORMAT: &str = "ExcludeClipboardContentFromMonitorProcessing";
const HISTORY_FORMAT: &str = "CanIncludeInClipboardHistory";
const CLOUD_FORMAT: &str = "CanUploadToCloudClipboard";
/// Posted to the clipboard thread when jobs are waiting.
const WM_JOBS: u32 = WM_APP + 1;
/// Posted to the clipboard thread to make it exit.
const WM_STOP: u32 = WM_APP + 2;
/// The timer that lets go of a promised image kept for its other format.
const IMAGE_TIMER: usize = 1;
/// Other apps hold the clipboard open for moments at a time; opening it
/// is tried this often, this far apart.
const OPEN_TRIES: u32 = 20;
const OPEN_RETRY: Duration = Duration::from_millis(15);
/// A bitmap whose header and pixels take more than this is not read.
const MAX_DIB: usize = 124 + 1024 + MAX_PIXELS as usize * 4;

/// Called on the clipboard thread with the clipboard's sequence number
/// whenever another process changes it.
pub type OnChange = Box<dyn Fn(u64) + Send>;

/// The clipboard, through a thread of its own.
pub struct WinClipboard {
    id: u32,
    jobs: Sender<Job>,
    join: Option<JoinHandle<()>>,
}

/// What reading the clipboard found, and the paths of the files copied
/// in File Explorer, when asked for.
type Read = (ClipboardRead, Vec<String>);

enum Job {
    Read {
        max_len: usize,
        files: bool,
        reply: SyncSender<Result<Read, PlatformError>>,
    },
    Write {
        content: ClipboardContent,
        reply: SyncSender<Result<u64, PlatformError>>,
    },
    Promise {
        promise: ClipboardPromise,
        reply: SyncSender<Result<u64, PlatformError>>,
    },
    Withdraw {
        token: u64,
        reply: SyncSender<Result<bool, PlatformError>>,
    },
    Files {
        reply: SyncSender<Result<Vec<String>, PlatformError>>,
    },
}

impl WinClipboard {
    /// Starts the clipboard thread. `on_change` hears of each change other
    /// processes make, and of the clipboard as it is now unless it holds
    /// what InputWeave wrote.
    pub fn start(on_change: OnChange) -> Result<Self, PlatformError> {
        let (ready_tx, ready_rx) = mpsc::channel();
        let (jobs, job_rx) = mpsc::channel();
        let join = std::thread::Builder::new()
            .name("inputweave-clipboard".to_owned())
            .spawn(move || run(on_change, job_rx, ready_tx))
            .map_err(|e| PlatformError::Os(e.to_string()))?;
        let id = ready_rx.recv().map_err(|_| stopped())??;
        Ok(Self {
            id,
            jobs,
            join: Some(join),
        })
    }

    fn job<T>(
        &self,
        job: impl FnOnce(SyncSender<Result<T, PlatformError>>) -> Job,
    ) -> Result<T, PlatformError> {
        let (reply, answer) = mpsc::sync_channel(1);
        self.jobs.send(job(reply)).map_err(|_| stopped())?;
        // SAFETY: no pointers are passed; the id is the clipboard thread's,
        // which has a message queue until it exits.
        unsafe { PostThreadMessageW(self.id, WM_JOBS, WPARAM(0), LPARAM(0)) }
            .map_err(|_| stopped())?;
        answer.recv().map_err(|_| stopped())?
    }
}

impl WinClipboard {
    /// Reads the clipboard as `read` does, with the paths of the files
    /// copied in File Explorer (`CF_HDROP`) taken in the same opening of
    /// it, as they are, at most [`MAX_DROPPED_NAMES`]: the broker checks
    /// them. None when the content is sensitive.
    pub fn read_with_files(&self, max_len: usize) -> Result<Read, PlatformError> {
        self.job(|reply| Job::Read {
            max_len,
            files: true,
            reply,
        })
    }

    /// Takes the promise `write_promise` returned `token` for off the
    /// clipboard, by emptying it, if the clipboard still holds that
    /// promise: its source is gone, so nothing could render it. Returns
    /// whether it did.
    pub fn withdraw(&self, token: u64) -> Result<bool, PlatformError> {
        self.job(|reply| Job::Withdraw { token, reply })
    }
}

fn stopped() -> PlatformError {
    PlatformError::Os("the clipboard thread stopped".to_owned())
}

impl Clipboard for WinClipboard {
    fn change_count(&self) -> Result<u64, PlatformError> {
        // SAFETY: takes no arguments.
        Ok(u64::from(unsafe { GetClipboardSequenceNumber() }))
    }

    fn read(&self, max_len: usize) -> Result<ClipboardRead, PlatformError> {
        self.job(|reply| Job::Read {
            max_len,
            files: false,
            reply,
        })
        .map(|(read, _)| read)
    }

    /// The paths of the files copied in File Explorer, as they are, read
    /// alone.
    fn files(&self) -> Result<Vec<std::path::PathBuf>, PlatformError> {
        let files = self.job(|reply| Job::Files { reply })?;
        Ok(files.into_iter().map(Into::into).collect())
    }

    fn write(&mut self, content: &ClipboardContent) -> Result<u64, PlatformError> {
        let content = content.clone();
        self.job(|reply| Job::Write { content, reply })
    }

    fn write_promise(&mut self, promise: ClipboardPromise) -> Result<u64, PlatformError> {
        self.job(|reply| Job::Promise { promise, reply })
    }
}

impl Drop for WinClipboard {
    fn drop(&mut self) {
        // SAFETY: as in `job`.
        let posted = unsafe { PostThreadMessageW(self.id, WM_STOP, WPARAM(0), LPARAM(0)) };
        if let (Ok(()), Some(join)) = (posted, self.join.take()) {
            let _ = join.join();
        }
    }
}

/// The formats this module reads and writes, by number.
#[derive(Clone, Copy)]
struct Formats {
    html: u32,
    rtf: u32,
    png: u32,
    own: u32,
    exclude: u32,
    history: u32,
    cloud: u32,
}

impl Formats {
    fn register() -> Result<Self, PlatformError> {
        let register = |name: &str| {
            // SAFETY: the name is a valid string for the call.
            match unsafe { RegisterClipboardFormatW(&HSTRING::from(name)) } {
                0 => Err(PlatformError::Os(format!(
                    "cannot register the clipboard format {name:?}"
                ))),
                id => Ok(id),
            }
        };
        Ok(Self {
            html: register(HTML_FORMAT)?,
            rtf: register(RTF_FORMAT)?,
            png: register(PNG_FORMAT)?,
            own: register(OWN_FORMAT)?,
            exclude: register(EXCLUDE_FORMAT)?,
            history: register(HISTORY_FORMAT)?,
            cloud: register(CLOUD_FORMAT)?,
        })
    }

    /// The format a promise puts on the clipboard for `promised`.
    fn of(self, promised: Promised) -> u32 {
        match promised {
            Promised::UnicodeText => CF_UNICODETEXT,
            Promised::Rtf => self.rtf,
            Promised::Html => self.html,
            Promised::Png => self.png,
            Promised::Dib => CF_DIB,
        }
    }

    /// Which promised format `format` is, if it is one.
    fn promised(self, format: u32) -> Option<Promised> {
        [
            Promised::UnicodeText,
            Promised::Rtf,
            Promised::Html,
            Promised::Png,
            Promised::Dib,
        ]
        .into_iter()
        .find(|p| self.of(*p) == format)
    }
}

/// What the window procedure reports changes with. Windows calls it on
/// the clipboard thread, from inside its message loop, so it lives in a
/// thread local of that thread.
struct Listener {
    on_change: OnChange,
    own: u32,
    last: Option<u32>,
}

/// The promise the clipboard holds, while this thread's window owns it.
/// Windows asks the window procedure for its formats, so it lives in a
/// thread local of the clipboard thread too.
struct HeldPromise {
    source: Arc<dyn ClipboardSource>,
    formats: Formats,
    factory: IWICImagingFactory,
    rendering: Rendering,
    /// What `write_promise` returned for it: the clipboard's sequence
    /// number once it was there.
    token: u64,
}

thread_local! {
    static LISTENER: RefCell<Option<Listener>> = const { RefCell::new(None) };
    static PROMISE: RefCell<Option<HeldPromise>> = const { RefCell::new(None) };
}

/// Whether the clipboard holds this thread's promise.
fn holds_promise() -> bool {
    PROMISE.with(|promise| promise.borrow().is_some())
}

/// The clipboard no longer holds the promise, if there was one: its
/// source is told.
fn promise_gone() {
    if let Some(held) = PROMISE.with(|promise| promise.borrow_mut().take()) {
        held.source.finished();
    }
}

/// An app pastes `format` of the promise (`WM_RENDERFORMAT`): fetched from
/// the source, which blocks this thread while it pulls, and put on the
/// clipboard, which is not opened for it. Nothing is put there if it
/// cannot be had. An image kept for its other format is let go by a timer
/// on `window` at the latest.
fn render(window: HWND, format: u32) {
    // Taken out while the source is asked, so nothing borrows the thread
    // local meanwhile; put back unless another promise came.
    let Some(mut held) = PROMISE.with(|promise| promise.borrow_mut().take()) else {
        return;
    };
    let bytes = held.formats.promised(format).and_then(|wanted| {
        let (source, factory) = (&held.source, &held.factory);
        held.rendering.render(
            wanted,
            Instant::now(),
            |source_format| source.try_fetch(source_format),
            |png| dib_from_png(factory, png),
        )
    });
    if held.rendering.holds_image() {
        let keep = u32::try_from(IMAGE_KEEP.as_millis()).unwrap_or(u32::MAX);
        // SAFETY: the window is this thread's; no callback is passed.
        unsafe { SetTimer(Some(window), IMAGE_TIMER, keep, None) };
    }
    PROMISE.with(|promise| {
        let mut promise = promise.borrow_mut();
        if promise.is_none() {
            *promise = Some(held);
        }
    });
    let Some(bytes) = bytes else {
        return;
    };
    if let Err(e) = set_data(format, &bytes) {
        tracing::debug!(error = %e, format, "cannot hand a promised format to the clipboard");
    }
}

/// Reports the clipboard as it is now, unless InputWeave wrote it or it
/// was reported already.
fn report_change() {
    LISTENER.with(|listener| {
        let mut listener = listener.borrow_mut();
        let Some(listener) = listener.as_mut() else {
            return;
        };
        // SAFETY: plain calls without pointers.
        let (sequence, own) = unsafe {
            (
                GetClipboardSequenceNumber(),
                IsClipboardFormatAvailable(listener.own).is_ok(),
            )
        };
        if own || listener.last == Some(sequence) {
            return;
        }
        listener.last = Some(sequence);
        (listener.on_change)(u64::from(sequence));
    });
}

extern "system" fn window_proc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    match msg {
        WM_CLIPBOARDUPDATE => report_change(),
        WM_RENDERFORMAT => render(hwnd, wparam.0 as u32),
        WM_TIMER if wparam.0 == IMAGE_TIMER => {
            // SAFETY: the timer is this window's.
            unsafe {
                let _ = KillTimer(Some(hwnd), IMAGE_TIMER);
            }
            PROMISE.with(|promise| {
                if let Some(held) = promise.borrow_mut().as_mut() {
                    held.rendering.drop_image();
                }
            });
        }
        // The window is going: what was not rendered is not fetched now,
        // as that could take minutes; those formats disappear with it.
        WM_RENDERALLFORMATS => {}
        WM_DESTROYCLIPBOARD => promise_gone(),
        // SAFETY: passes on what Windows gave this procedure.
        _ => return unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
    LRESULT(0)
}

/// A message-only window on this thread, destroyed on drop.
struct Window(HWND);

impl Window {
    fn create() -> Result<Self, PlatformError> {
        let class = w!("InputWeaveClipboard");
        // SAFETY: the class and window are this module's; the strings are
        // static. Registering the class again, for a second clipboard in
        // the process, fails harmlessly.
        unsafe {
            let instance: HINSTANCE = GetModuleHandleW(None)
                .map_err(os("cannot find this module"))?
                .into();
            RegisterClassW(&WNDCLASSW {
                lpfnWndProc: Some(window_proc),
                hInstance: instance,
                lpszClassName: class,
                ..Default::default()
            });
            let hwnd = CreateWindowExW(
                WINDOW_EX_STYLE::default(),
                class,
                PCWSTR::null(),
                WINDOW_STYLE::default(),
                0,
                0,
                0,
                0,
                Some(HWND_MESSAGE),
                None,
                Some(instance),
                None,
            )
            .map_err(os("cannot create the clipboard window"))?;
            Ok(Self(hwnd))
        }
    }
}

impl Drop for Window {
    fn drop(&mut self) {
        // SAFETY: the window was created on this thread.
        unsafe {
            let _ = DestroyWindow(self.0);
        }
    }
}

/// What the clipboard thread works with.
struct Worker {
    window: Window,
    formats: Formats,
    factory: IWICImagingFactory,
}

fn run(on_change: OnChange, jobs: Receiver<Job>, ready: Sender<Result<u32, PlatformError>>) {
    let mut msg = MSG::default();
    // SAFETY: `msg` is valid for the call. Peeking creates this thread's
    // message queue, so messages posted as soon as its id is known are kept.
    unsafe {
        let _ = PeekMessageW(&mut msg, None, WM_USER, WM_USER, PM_NOREMOVE);
    }
    // Declared first, so the COM objects are released before COM is left.
    let apartment = match Apartment::enter() {
        Ok(apartment) => apartment,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    let worker = match start_worker() {
        Ok(worker) => worker,
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };
    LISTENER.with(|listener| {
        *listener.borrow_mut() = Some(Listener {
            on_change,
            own: worker.formats.own,
            last: None,
        });
    });
    // SAFETY: the window is this thread's.
    let listening = unsafe { AddClipboardFormatListener(worker.window.0) };
    if let Err(e) = listening {
        let _ = ready.send(Err(PlatformError::Os(format!(
            "cannot listen for clipboard changes: {e}"
        ))));
        return;
    }
    report_change();
    // SAFETY: takes no arguments.
    let id = unsafe { GetCurrentThreadId() };
    if ready.send(Ok(id)).is_ok() {
        loop {
            // SAFETY: `msg` is valid for the call.
            let got = unsafe { GetMessageW(&mut msg, None, 0, 0) };
            // 0 is WM_QUIT, -1 an error.
            if got.0 <= 0 {
                break;
            }
            match msg.message {
                WM_STOP => break,
                WM_JOBS => {
                    while let Ok(job) = jobs.try_recv() {
                        worker.run(job);
                    }
                }
                _ => {
                    // SAFETY: `msg` came from GetMessageW.
                    unsafe {
                        let _ = TranslateMessage(&msg);
                        DispatchMessageW(&msg);
                    }
                }
            }
        }
    }
    // SAFETY: the window is this thread's and listens.
    unsafe {
        let _ = RemoveClipboardFormatListener(worker.window.0);
    }
    LISTENER.with(|listener| listener.borrow_mut().take());
    // Its formats go with the window.
    promise_gone();
    drop(worker);
    drop(apartment);
}

fn start_worker() -> Result<Worker, PlatformError> {
    let formats = Formats::register()?;
    // SAFETY: COM is initialized on this thread.
    let factory: IWICImagingFactory =
        unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
            .map_err(os("cannot start the Windows Imaging Component"))?;
    Ok(Worker {
        window: Window::create()?,
        formats,
        factory,
    })
}

/// The clipboard, open for as long as this lives.
struct Open;

impl Open {
    fn clipboard(owner: HWND) -> Result<Self, PlatformError> {
        let mut tries = 0;
        loop {
            // SAFETY: the window is the calling thread's.
            match unsafe { OpenClipboard(Some(owner)) } {
                Ok(()) => return Ok(Self),
                Err(e) if tries + 1 >= OPEN_TRIES => {
                    return Err(PlatformError::Os(format!(
                        "another app keeps the clipboard open: {e}"
                    )));
                }
                Err(_) => {
                    tries += 1;
                    std::thread::sleep(OPEN_RETRY);
                }
            }
        }
    }

    /// The data in `format`, unless it is missing or over `limit` bytes.
    fn data(&self, format: u32, limit: usize) -> Data {
        // SAFETY: the clipboard is open. The handle stays valid while it
        // is; it is locked for the copy and unlocked after, and the copy
        // reads no more than its size.
        unsafe {
            let Ok(handle) = GetClipboardData(format) else {
                return Data::Missing;
            };
            let global = HGLOBAL(handle.0);
            let size = GlobalSize(global);
            if size > limit {
                return Data::TooLarge(size);
            }
            let locked = GlobalLock(global);
            if locked.is_null() {
                return Data::Missing;
            }
            let bytes = std::slice::from_raw_parts(locked.cast::<u8>(), size).to_vec();
            let _ = GlobalUnlock(global);
            Data::Bytes(bytes)
        }
    }

    /// The paths `CF_HDROP` holds, as File Explorer copies files, in
    /// order, at most [`MAX_DROPPED_NAMES`]; names that are not Unicode are
    /// left out.
    fn files(&self) -> Vec<String> {
        // SAFETY: the clipboard is open, and the handle stays valid while
        // it is. Each name is copied into a buffer of the length Windows
        // reports, plus its NUL.
        unsafe {
            let Ok(handle) = GetClipboardData(CF_HDROP) else {
                return Vec::new();
            };
            let drop = HDROP(handle.0);
            let count = DragQueryFileW(drop, u32::MAX, None);
            let mut names = Vec::new();
            for i in 0..count.min(MAX_DROPPED_NAMES) {
                let len = DragQueryFileW(drop, i, None) as usize;
                if len == 0 {
                    continue;
                }
                let mut name = vec![0u16; len + 1];
                let copied = DragQueryFileW(drop, i, Some(&mut name)) as usize;
                name.truncate(copied.min(len));
                names.extend(String::from_utf16(&name).ok());
            }
            names
        }
    }

    /// Puts `bytes` on the clipboard as `format`.
    fn set(&self, format: u32, bytes: &[u8]) -> Result<(), PlatformError> {
        set_data(format, bytes)
    }

    /// Puts `format` on the clipboard without its data, which Windows asks
    /// the owner for when an app wants it (`WM_RENDERFORMAT`).
    fn promise(&self, format: u32) -> Result<(), PlatformError> {
        // SAFETY: the clipboard is open; plain calls without pointers. With
        // no data, success returns no handle either, so only an error the
        // thread recorded is one.
        unsafe {
            SetLastError(WIN32_ERROR(0));
            match SetClipboardData(format, None) {
                Err(e) if e.code().is_err() => Err(PlatformError::Os(format!(
                    "cannot promise data on the clipboard: {e}"
                ))),
                _ => Ok(()),
            }
        }
    }
}

/// Puts `bytes` on the clipboard as `format`: while it is open, or while
/// answering `WM_RENDERFORMAT`, when it is not opened.
fn set_data(format: u32, bytes: &[u8]) -> Result<(), PlatformError> {
    // SAFETY: the memory is allocated, filled within its size and handed
    // to the clipboard, which owns it from then on; if the clipboard
    // refuses it, it is freed here.
    unsafe {
        let global = GlobalAlloc(GMEM_MOVEABLE, bytes.len().max(1))
            .map_err(os("cannot allocate memory for the clipboard"))?;
        let locked = GlobalLock(global);
        if locked.is_null() {
            let _ = GlobalFree(Some(global));
            return Err(PlatformError::Os(
                "cannot lock memory for the clipboard".to_owned(),
            ));
        }
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), locked.cast::<u8>(), bytes.len());
        let _ = GlobalUnlock(global);
        if let Err(e) = SetClipboardData(format, Some(HANDLE(global.0))) {
            let _ = GlobalFree(Some(global));
            return Err(PlatformError::Os(format!(
                "cannot put data on the clipboard: {e}"
            )));
        }
    }
    Ok(())
}

impl Drop for Open {
    fn drop(&mut self) {
        // SAFETY: this thread opened the clipboard.
        unsafe {
            let _ = CloseClipboard();
        }
    }
}

enum Data {
    Missing,
    TooLarge(usize),
    Bytes(Vec<u8>),
}

impl Worker {
    fn run(&self, job: Job) {
        match job {
            Job::Read {
                max_len,
                files,
                reply,
            } => {
                let _ = reply.send(self.read(max_len, files));
            }
            Job::Write { content, reply } => {
                let _ = reply.send(self.write(&content));
            }
            Job::Promise { promise, reply } => {
                let _ = reply.send(self.promise(promise));
            }
            Job::Withdraw { token, reply } => {
                let _ = reply.send(self.withdraw(token));
            }
            Job::Files { reply } => {
                let _ = reply.send(Open::clipboard(self.window.0).map(|open| open.files()));
            }
        }
    }

    fn read(&self, max_len: usize, files: bool) -> Result<Read, PlatformError> {
        // What this thread promised is another device's content, not a
        // copy; reading it would only fetch it to send it back.
        if holds_promise() {
            return Ok((ClipboardRead::Empty, Vec::new()));
        }
        let formats = self.formats;
        let open = Open::clipboard(self.window.0)?;
        let marker = |format: u32| match open.data(format, 64) {
            Data::Bytes(bytes) => Some(bytes),
            _ => None,
        };
        // SAFETY: a plain call without pointers.
        let excluded = unsafe { IsClipboardFormatAvailable(formats.exclude) }.is_ok();
        if sensitive(
            excluded,
            marker(formats.history).as_deref(),
            marker(formats.cloud).as_deref(),
        ) {
            return Ok((ClipboardRead::Sensitive, Vec::new()));
        }
        let paths = if files { open.files() } else { Vec::new() };
        let mut content = ClipboardContent::default();
        // Raw data may be larger than what it becomes: UTF-16 text is
        // up to twice its UTF-8, and HTML has a header.
        let raw_limit = max_len.saturating_mul(2).saturating_add(4096);
        let mut too_large = None;
        let mut take = |format: u32, limit: usize| match open.data(format, limit) {
            Data::Bytes(bytes) => Some(bytes),
            Data::TooLarge(size) => {
                too_large = Some(size);
                None
            }
            Data::Missing => None,
        };
        if let Some(units) = take(CF_UNICODETEXT, raw_limit) {
            let units: Vec<u16> = units
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_le_bytes(*pair))
                .collect();
            let text = text_from_utf16(&units);
            if !text.is_empty() {
                content
                    .items
                    .push((ClipboardFormat::Text, text.into_bytes()));
            }
        }
        if let Some(rtf) = take(formats.rtf, raw_limit) {
            let rtf = trim_nuls(&rtf);
            if !rtf.is_empty() {
                content.items.push((ClipboardFormat::Rtf, rtf.to_vec()));
            }
        }
        if let Some(html) = take(formats.html, raw_limit).and_then(|h| html_from_cf_html(&h))
            && !html.is_empty()
        {
            content.items.push((ClipboardFormat::Html, html));
        }
        let mut dib = None;
        match take(formats.png, raw_limit) {
            Some(png) if !png.is_empty() => content.items.push((ClipboardFormat::Png, png)),
            _ => dib = take(CF_DIB, MAX_DIB),
        }
        drop(open);
        if let Some(size) = too_large {
            return Ok((ClipboardRead::TooLarge(size.max(content.len())), paths));
        }
        if let Some(png) = dib.as_deref().and_then(|dib| self.png_from_dib(dib)) {
            content.items.push((ClipboardFormat::Png, png));
        }
        let read = if content.len() > max_len {
            ClipboardRead::TooLarge(content.len())
        } else if content.is_empty() {
            ClipboardRead::Empty
        } else {
            ClipboardRead::Content(content)
        };
        Ok((read, paths))
    }

    fn write(&self, content: &ClipboardContent) -> Result<u64, PlatformError> {
        let formats = self.formats;
        let mut entries: Vec<(u32, Vec<u8>)> = Vec::new();
        for (format, data) in &content.items {
            let windows = match format {
                ClipboardFormat::Text => CF_UNICODETEXT,
                ClipboardFormat::Rtf => formats.rtf,
                ClipboardFormat::Html => formats.html,
                ClipboardFormat::Png => formats.png,
            };
            entries.push((windows, clipboard_bytes(*format, data)));
            if *format == ClipboardFormat::Png {
                match dib_from_png(&self.factory, data) {
                    Some(dib) => entries.push((CF_DIB, dib)),
                    None => tracing::debug!("cannot draw the image as a bitmap"),
                }
            }
        }
        entries.push((formats.own, 1u32.to_le_bytes().to_vec()));
        let open = Open::clipboard(self.window.0)?;
        // SAFETY: the clipboard is open, by this thread's window, which
        // becomes its owner. A promise it held ends here
        // (`WM_DESTROYCLIPBOARD`).
        unsafe { EmptyClipboard() }.map_err(os("cannot empty the clipboard"))?;
        for (format, bytes) in &entries {
            open.set(*format, bytes)?;
        }
        drop(open);
        // SAFETY: takes no arguments.
        Ok(u64::from(unsafe { GetClipboardSequenceNumber() }))
    }

    /// Puts a promise of `promise.formats` on the clipboard, marked as
    /// InputWeave's, to be rendered from `promise.source` as apps paste.
    fn promise(&self, promise: ClipboardPromise) -> Result<u64, PlatformError> {
        let formats = self.formats;
        let promised = promised(&promise.formats);
        if promised.is_empty() {
            return Err(PlatformError::Os("nothing to promise".to_owned()));
        }
        let open = Open::clipboard(self.window.0)?;
        // SAFETY: as in `write`; the promise held until now ends here.
        unsafe { EmptyClipboard() }.map_err(os("cannot empty the clipboard"))?;
        for p in promised {
            open.promise(formats.of(p))?;
        }
        open.set(formats.own, &1u32.to_le_bytes())?;
        drop(open);
        // SAFETY: takes no arguments.
        let token = u64::from(unsafe { GetClipboardSequenceNumber() });
        // Only now: emptying the clipboard ended the last one.
        PROMISE.with(|held| {
            *held.borrow_mut() = Some(HeldPromise {
                source: promise.source,
                formats,
                factory: self.factory.clone(),
                rendering: Rendering::default(),
                token,
            });
        });
        Ok(token)
    }

    /// Empties the clipboard if it still holds the promise placed as
    /// `token`, which ends it (`WM_DESTROYCLIPBOARD`).
    fn withdraw(&self, token: u64) -> Result<bool, PlatformError> {
        let held = PROMISE.with(|held| held.borrow().as_ref().is_some_and(|h| h.token == token));
        if !held {
            return Ok(false);
        }
        let open = Open::clipboard(self.window.0)?;
        // SAFETY: the clipboard is open, by this thread's window.
        unsafe { EmptyClipboard() }.map_err(os("cannot empty the clipboard"))?;
        drop(open);
        // In case Windows did not tell the window it lost the clipboard.
        promise_gone();
        Ok(true)
    }

    /// A PNG of the bitmap in `CF_DIB`.
    fn png_from_dib(&self, dib: &[u8]) -> Option<Vec<u8>> {
        let bmp = bmp_file_from_dib(dib)?;
        let frame = decode(&self.factory, &bmp)
            .inspect_err(|e| tracing::debug!(error = %e, "cannot read the clipboard's bitmap"))
            .ok()?;
        encode_png(&self.factory, &frame)
            .inspect_err(|e| tracing::debug!(error = %e, "cannot encode the clipboard's bitmap"))
            .ok()
    }
}

/// A `CF_DIB` of the image in `png`, drawn over white.
fn dib_from_png(factory: &IWICImagingFactory, png: &[u8]) -> Option<Vec<u8>> {
    let frame = decode(factory, png).ok()?;
    let (mut width, mut height) = (0u32, 0u32);
    // SAFETY: plain calls on live WIC objects; the out pointers and the
    // buffer are valid for the calls.
    unsafe {
        frame.GetSize(&mut width, &mut height).ok()?;
        if u64::from(width) * u64::from(height) > MAX_PIXELS {
            return None;
        }
        let converter = factory.CreateFormatConverter().ok()?;
        converter
            .Initialize(
                &frame,
                &GUID_WICPixelFormat32bppBGRA,
                WICBitmapDitherTypeNone,
                None::<&IWICPalette>,
                0.0,
                WICBitmapPaletteTypeCustom,
            )
            .ok()?;
        let stride = width as usize * 4;
        let mut pixels = vec![0u8; stride * height as usize];
        converter
            .CopyPixels(std::ptr::null(), stride as u32, &mut pixels)
            .ok()?;
        dib_from_bgra(width, height, stride, &pixels)
    }
}

/// The first frame of the image in `bytes`, in any format WIC reads.
fn decode(
    factory: &IWICImagingFactory,
    bytes: &[u8],
) -> ::windows::core::Result<IWICBitmapFrameDecode> {
    let len = u32::try_from(bytes.len()).map_err(|_| ::windows::core::Error::from(E_FAIL))?;
    // SAFETY: plain calls on live COM objects; the stream copies `bytes`,
    // which are valid for `len` bytes, and the decoder keeps the stream.
    unsafe {
        let stream = CreateStreamOnHGlobal(HGLOBAL::default(), true)?;
        stream.Write(bytes.as_ptr().cast(), len, None).ok()?;
        stream.Seek(0, STREAM_SEEK_SET, None)?;
        let decoder = factory.CreateDecoderFromStream(
            &stream,
            std::ptr::null(),
            WICDecodeMetadataCacheOnDemand,
        )?;
        decoder.GetFrame(0)
    }
}

/// `source` as a PNG: with an alpha channel if it has one, 24-bit
/// otherwise.
fn encode_png(
    factory: &IWICImagingFactory,
    source: &IWICBitmapFrameDecode,
) -> ::windows::core::Result<Vec<u8>> {
    // SAFETY: plain calls on live WIC objects created here; every pointer
    // passed is valid for its call.
    unsafe {
        let (mut width, mut height) = (0u32, 0u32);
        source.GetSize(&mut width, &mut height)?;
        let alpha = [
            GUID_WICPixelFormat32bppBGRA,
            GUID_WICPixelFormat32bppPBGRA,
            GUID_WICPixelFormat32bppRGBA,
            GUID_WICPixelFormat32bppPRGBA,
            GUID_WICPixelFormat64bppRGBA,
        ]
        .contains(&source.GetPixelFormat()?);
        let mut format = if alpha {
            GUID_WICPixelFormat32bppBGRA
        } else {
            GUID_WICPixelFormat24bppBGR
        };
        let converter = factory.CreateFormatConverter()?;
        converter.Initialize(
            source,
            &format,
            WICBitmapDitherTypeNone,
            None::<&IWICPalette>,
            0.0,
            WICBitmapPaletteTypeCustom,
        )?;
        let stream = CreateStreamOnHGlobal(HGLOBAL::default(), true)?;
        let encoder = factory.CreateEncoder(&GUID_ContainerFormatPng, std::ptr::null())?;
        encoder.Initialize(&stream, WICBitmapEncoderNoCache)?;
        let (mut target, mut options) = (None, None);
        encoder.CreateNewFrame(&mut target, &mut options)?;
        let (Some(target), Some(options)) = (target, options) else {
            return Err(E_FAIL.into());
        };
        target.Initialize(&options)?;
        target.SetSize(width, height)?;
        target.SetPixelFormat(&mut format)?;
        target.WriteSource(&converter, std::ptr::null())?;
        target.Commit()?;
        encoder.Commit()?;
        read_all(&stream)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};

    use super::super::clipboard_formats::utf16_from_text;
    use super::*;

    /// A WIC factory and nothing else, to test the image conversions
    /// without touching the clipboard.
    fn with_worker_factory<T>(f: impl FnOnce(&IWICImagingFactory) -> T) -> T {
        let _apartment = Apartment::enter().unwrap();
        // SAFETY: COM is initialized on this thread.
        let factory: IWICImagingFactory =
            unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
                .unwrap();
        f(&factory)
    }

    /// A 3 × 2 32-bit bitmap, bottom up, with an alpha byte of 0 as
    /// screenshots have.
    fn screenshot_dib() -> Vec<u8> {
        let pixels: Vec<[u8; 4]> = vec![
            [0, 0, 255, 0],
            [0, 255, 0, 0],
            [255, 0, 0, 0],
            [10, 20, 30, 0],
            [40, 50, 60, 0],
            [70, 80, 90, 0],
        ];
        dib_from_bgra(3, 2, 12, &pixels.concat()).unwrap()
    }

    #[test]
    fn bitmaps_and_pngs_convert_both_ways() {
        with_worker_factory(|factory| {
            let dib = screenshot_dib();
            let bmp = bmp_file_from_dib(&dib).unwrap();
            let frame = decode(factory, &bmp).unwrap();
            let png = encode_png(factory, &frame).unwrap();
            assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");

            // Back to a bitmap: the same 24-bit pixels.
            let frame = decode(factory, &png).unwrap();
            let (mut width, mut height) = (0, 0);
            // SAFETY: valid out pointers.
            unsafe { frame.GetSize(&mut width, &mut height) }.unwrap();
            assert_eq!((width, height), (3, 2));
        });
    }

    /// Uses the real clipboard: run it in a signed-in session, not over
    /// ssh. It replaces what the user copied.
    #[test]
    #[ignore = "uses the signed-in user's clipboard"]
    fn reads_back_what_it_writes_and_hears_only_other_changes() {
        let heard = Arc::new(Mutex::new(Vec::new()));
        let mut clipboard = WinClipboard::start(Box::new({
            let heard = heard.clone();
            move |sequence| heard.lock().unwrap().push(sequence)
        }))
        .unwrap();
        let png = with_worker_factory(|factory| {
            let bmp = bmp_file_from_dib(&screenshot_dib()).unwrap();
            encode_png(factory, &decode(factory, &bmp).unwrap()).unwrap()
        });
        let content = ClipboardContent {
            items: vec![
                (ClipboardFormat::Text, "第一行\nsecond".as_bytes().to_vec()),
                (ClipboardFormat::Rtf, b"{\\rtf1\\ansi hi}".to_vec()),
                (ClipboardFormat::Html, b"<b>hi</b>".to_vec()),
                (ClipboardFormat::Png, png.clone()),
            ],
        };
        std::thread::sleep(Duration::from_millis(200));
        heard.lock().unwrap().clear();
        let sequence = clipboard.write(&content).unwrap();
        assert_eq!(clipboard.change_count().unwrap(), sequence);
        let ClipboardRead::Content(read) = clipboard.read(8 << 20).unwrap() else {
            panic!("nothing read back");
        };
        assert_eq!(
            read.get(ClipboardFormat::Text),
            Some("第一行\nsecond".as_bytes())
        );
        assert_eq!(
            read.get(ClipboardFormat::Rtf),
            Some(&b"{\\rtf1\\ansi hi}"[..])
        );
        assert_eq!(read.get(ClipboardFormat::Png), Some(&png[..]));
        let html = read.get(ClipboardFormat::Html).unwrap();
        assert!(
            String::from_utf8_lossy(html)
                .contains("<!--StartFragment--><b>hi</b><!--EndFragment-->")
        );
        assert!(matches!(clipboard.read(10), Ok(ClipboardRead::TooLarge(_))));
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            heard.lock().unwrap().is_empty(),
            "its own write was reported"
        );

        // Another writer, with a window of its own, which marks its text as
        // not to be monitored, as password managers do.
        std::thread::spawn(|| {
            let window = Window::create().unwrap();
            let open = Open::clipboard(window.0).unwrap();
            // SAFETY: a plain call; the clipboard is open by this thread.
            unsafe { EmptyClipboard() }.unwrap();
            // SAFETY: the name is a valid string for the call.
            let exclude = unsafe { RegisterClipboardFormatW(&HSTRING::from(EXCLUDE_FORMAT)) };
            let units: Vec<u8> = utf16_from_text("hunter2")
                .iter()
                .flat_map(|u| u.to_le_bytes())
                .collect();
            open.set(CF_UNICODETEXT, &units).unwrap();
            open.set(exclude, &[0]).unwrap();
        })
        .join()
        .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert_eq!(heard.lock().unwrap().len(), 1, "another app's write");
        assert_eq!(clipboard.read(1024).unwrap(), ClipboardRead::Sensitive);
    }

    /// Puts `paths` on the clipboard as File Explorer copies files, as
    /// another writer with a window of its own: `CF_HDROP` with wide names,
    /// text besides, and the mark of content not to be monitored if
    /// `excluded`.
    fn copy_as_explorer(paths: Vec<String>, text: &'static str, excluded: bool) {
        std::thread::spawn(move || {
            let window = Window::create().unwrap();
            let open = Open::clipboard(window.0).unwrap();
            // SAFETY: a plain call; the clipboard is open by this thread.
            unsafe { EmptyClipboard() }.unwrap();
            // DROPFILES: where the names start, a point, whether it is in
            // the nonclient area, and whether the names are wide.
            let mut drop = Vec::new();
            for field in [20u32, 0, 0, 0, 1] {
                drop.extend_from_slice(&field.to_le_bytes());
            }
            for path in &paths {
                for unit in path.encode_utf16().chain([0]) {
                    drop.extend_from_slice(&unit.to_le_bytes());
                }
            }
            drop.extend_from_slice(&0u16.to_le_bytes());
            open.set(CF_HDROP, &drop).unwrap();
            let units: Vec<u8> = utf16_from_text(text)
                .iter()
                .flat_map(|u| u.to_le_bytes())
                .collect();
            open.set(CF_UNICODETEXT, &units).unwrap();
            if excluded {
                // SAFETY: the name is a valid string for the call.
                let exclude = unsafe { RegisterClipboardFormatW(&HSTRING::from(EXCLUDE_FORMAT)) };
                open.set(exclude, &[0]).unwrap();
            }
        })
        .join()
        .unwrap();
        std::thread::sleep(Duration::from_millis(300));
    }

    /// Uses the real clipboard: run it in a signed-in session, not over
    /// ssh. It replaces what the user copied.
    #[test]
    #[ignore = "uses the signed-in user's clipboard"]
    fn reads_the_paths_of_files_copied_in_explorer() {
        let clipboard = WinClipboard::start(Box::new(|_| {})).unwrap();
        let dir = std::env::temp_dir().join(format!("iw-hdrop-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = dir.join("報告 a.txt");
        let b = dir.join("b.txt");
        std::fs::write(&a, b"a").unwrap();
        std::fs::write(&b, b"b").unwrap();
        let (a, b) = (
            a.to_string_lossy().into_owned(),
            b.to_string_lossy().into_owned(),
        );

        // Both files, and a device path, which the broker leaves out.
        let device = r"\\?\C:\Windows\win.ini".to_owned();
        copy_as_explorer(vec![a.clone(), device.clone(), b.clone()], "names", false);
        let (read, files) = clipboard.read_with_files(1024).unwrap();
        assert_eq!(files, vec![a.clone(), device.clone(), b.clone()]);
        assert_eq!(
            read,
            ClipboardRead::Content(ClipboardContent {
                items: vec![(ClipboardFormat::Text, b"names".to_vec())],
            })
        );
        // Read alone, without the content.
        assert_eq!(
            clipboard.files().unwrap(),
            [&a, &device, &b].map(std::path::PathBuf::from)
        );
        // A plain read does not look for them.
        assert!(matches!(
            clipboard.read(1024),
            Ok(ClipboardRead::Content(_))
        ));

        // Marked as not to be monitored: nothing of it, its files included.
        copy_as_explorer(vec![a, b], "names", true);
        assert_eq!(
            clipboard.read_with_files(1024).unwrap(),
            (ClipboardRead::Sensitive, Vec::new())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Promised content that arrives a moment after it is asked for, as
    /// pulling it from another device would, counting what is asked.
    struct Delayed {
        content: ClipboardContent,
        asked: Mutex<Vec<ClipboardFormat>>,
        finished: AtomicBool,
    }

    impl ClipboardSource for Delayed {
        fn fetch(&self, format: ClipboardFormat) -> Option<Vec<u8>> {
            self.asked.lock().unwrap().push(format);
            std::thread::sleep(Duration::from_millis(300));
            self.content.get(format).map(<[u8]>::to_vec)
        }

        fn finished(&self) {
            self.finished.store(true, Ordering::SeqCst);
        }
    }

    /// Uses the real clipboard and starts PowerShell: run it in a
    /// signed-in session, not over ssh. It replaces what the user copied.
    #[test]
    #[ignore = "uses the signed-in user's clipboard"]
    fn renders_a_promise_as_other_apps_paste_it() {
        let heard = Arc::new(Mutex::new(Vec::new()));
        let mut clipboard = WinClipboard::start(Box::new({
            let heard = heard.clone();
            move |sequence| heard.lock().unwrap().push(sequence)
        }))
        .unwrap();
        let png = with_worker_factory(|factory| {
            let bmp = bmp_file_from_dib(&screenshot_dib()).unwrap();
            encode_png(factory, &decode(factory, &bmp).unwrap()).unwrap()
        });
        let source = Arc::new(Delayed {
            content: ClipboardContent {
                items: vec![
                    (ClipboardFormat::Text, "第一行\nsecond".as_bytes().to_vec()),
                    (ClipboardFormat::Html, b"<b>hi</b>".to_vec()),
                    (ClipboardFormat::Png, png.clone()),
                ],
            },
            asked: Mutex::default(),
            finished: AtomicBool::new(false),
        });
        std::thread::sleep(Duration::from_millis(200));
        heard.lock().unwrap().clear();
        let sequence = clipboard
            .write_promise(ClipboardPromise {
                formats: vec![
                    ClipboardFormat::Text,
                    ClipboardFormat::Html,
                    ClipboardFormat::Png,
                ],
                bytes: 1024,
                files: Vec::new(),
                source: source.clone(),
            })
            .unwrap();
        assert_eq!(clipboard.change_count().unwrap(), sequence);
        // Nothing is fetched until an app pastes, and reading it here is
        // no paste.
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(clipboard.read(8 << 20).unwrap(), ClipboardRead::Empty);
        assert!(source.asked.lock().unwrap().is_empty());

        // Another process pastes the text.
        let pasted = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-Command",
                "[Console]::OutputEncoding = [Text.Encoding]::UTF8; Get-Clipboard -Raw",
            ])
            .output()
            .unwrap();
        let pasted = String::from_utf8_lossy(&pasted.stdout);
        assert!(
            pasted.contains("第一行\r\nsecond"),
            "PowerShell pasted {pasted:?}"
        );

        // Another thread, with a window of its own, pastes the rest.
        let (html, png_read, dib, rtf) = std::thread::spawn(|| {
            let window = Window::create().unwrap();
            let formats = Formats::register().unwrap();
            let open = Open::clipboard(window.0).unwrap();
            let get = |format| match open.data(format, 64 << 20) {
                Data::Bytes(bytes) => Some(bytes),
                _ => None,
            };
            (
                get(formats.html),
                get(formats.png),
                get(CF_DIB),
                get(formats.rtf),
            )
        })
        .join()
        .unwrap();
        let html = String::from_utf8_lossy(&html.unwrap()).into_owned();
        assert!(html.contains("<!--StartFragment--><b>hi</b><!--EndFragment-->"));
        assert_eq!(png_read.unwrap(), png);
        let dib = dib.unwrap();
        assert_eq!(u32::from_le_bytes(dib[..4].try_into().unwrap()), 40);
        assert_eq!(rtf, None, "a format never promised");
        // One fetch each, the image's for both its PNG and its bitmap, in
        // whatever order the apps asked: an app pasting text may look at
        // the image too.
        let mut asked = source.asked.lock().unwrap().clone();
        asked.sort_by_key(|f| f.mime());
        assert_eq!(
            asked,
            vec![
                ClipboardFormat::Png,
                ClipboardFormat::Html,
                ClipboardFormat::Text
            ]
        );
        std::thread::sleep(Duration::from_millis(300));
        assert!(
            heard.lock().unwrap().is_empty(),
            "its own promise was reported"
        );
        assert!(!source.finished.load(Ordering::SeqCst));

        // Another app copies over it: the promise is gone.
        std::thread::spawn(|| {
            let window = Window::create().unwrap();
            let open = Open::clipboard(window.0).unwrap();
            // SAFETY: a plain call; the clipboard is open by this thread.
            unsafe { EmptyClipboard() }.unwrap();
            let units: Vec<u8> = utf16_from_text("later")
                .iter()
                .flat_map(|u| u.to_le_bytes())
                .collect();
            open.set(CF_UNICODETEXT, &units).unwrap();
        })
        .join()
        .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        assert!(source.finished.load(Ordering::SeqCst));
        assert_eq!(heard.lock().unwrap().len(), 1, "another app's copy");
        let ClipboardRead::Content(read) = clipboard.read(1024).unwrap() else {
            panic!("the later copy is not read");
        };
        assert_eq!(read.get(ClipboardFormat::Text), Some(&b"later"[..]));

        // A promise whose source went away is withdrawn: only while the
        // clipboard still holds it, and then the clipboard is emptied.
        let stale = sequence;
        let orphan = Arc::new(Delayed {
            content: ClipboardContent::default(),
            asked: Mutex::default(),
            finished: AtomicBool::new(false),
        });
        let token = clipboard
            .write_promise(ClipboardPromise {
                formats: vec![ClipboardFormat::Text],
                bytes: 5,
                files: Vec::new(),
                source: orphan.clone(),
            })
            .unwrap();
        assert!(!clipboard.withdraw(stale).unwrap());
        assert!(clipboard.withdraw(token).unwrap());
        assert!(orphan.finished.load(Ordering::SeqCst));
        assert!(orphan.asked.lock().unwrap().is_empty());
        let empty = std::thread::spawn(|| {
            let window = Window::create().unwrap();
            let _open = Open::clipboard(window.0).unwrap();
            // SAFETY: a plain call; the clipboard is open by this thread.
            unsafe { IsClipboardFormatAvailable(CF_UNICODETEXT) }.is_err()
        })
        .join()
        .unwrap();
        assert!(empty, "the withdrawn promise is still on the clipboard");
        assert!(!clipboard.withdraw(token).unwrap());
    }
}
