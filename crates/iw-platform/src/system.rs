//! Small facts about the system and requests to it that are not input:
//! free disk space, user accounts, reading a user's files safely as root,
//! and opening the app when the user must decide something.

use std::path::{Path, PathBuf};

/// Bytes available to this user on the volume holding `path`.
#[cfg(unix)]
pub fn free_space(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `path` is a valid C string and `stats` is valid for writing.
    if unsafe { libc::statvfs(path.as_ptr(), stats.as_mut_ptr()) } != 0 {
        return None;
    }
    // SAFETY: statvfs succeeded and filled it in.
    let stats = unsafe { stats.assume_init() };
    #[allow(clippy::unnecessary_cast)]
    Some(stats.f_bavail as u64 * stats.f_frsize as u64)
}

#[cfg(not(unix))]
pub fn free_space(_path: &Path) -> Option<u64> {
    None
}

/// A user account, as the password database has it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Account {
    pub uid: u32,
    pub home: PathBuf,
}

/// The account named `name`, if there is one with a home folder.
#[cfg(unix)]
pub fn account(name: &str) -> Option<Account> {
    use std::ffi::{CStr, CString, OsStr};
    use std::os::unix::ffi::OsStrExt;

    let name = CString::new(name).ok()?;
    let mut buffer = vec![0 as libc::c_char; 4096];
    let mut entry = std::mem::MaybeUninit::<libc::passwd>::uninit();
    loop {
        let mut found = std::ptr::null_mut();
        // SAFETY: every pointer is valid for the call, and the buffer for
        // its whole length; the entry's strings point into the buffer.
        let rc = unsafe {
            libc::getpwnam_r(
                name.as_ptr(),
                entry.as_mut_ptr(),
                buffer.as_mut_ptr(),
                buffer.len(),
                &mut found,
            )
        };
        if rc == libc::ERANGE && buffer.len() < 1 << 20 {
            buffer.resize(buffer.len() * 2, 0);
            continue;
        }
        if rc != 0 || found.is_null() {
            return None;
        }
        break;
    }
    // SAFETY: getpwnam_r found the account and filled the entry in.
    let entry = unsafe { entry.assume_init() };
    if entry.pw_dir.is_null() {
        return None;
    }
    // SAFETY: a NUL-terminated string in the buffer, which is still alive.
    let home = unsafe { CStr::from_ptr(entry.pw_dir) }.to_bytes();
    (!home.is_empty()).then(|| Account {
        uid: entry.pw_uid,
        home: PathBuf::from(OsStr::from_bytes(home)),
    })
}

#[cfg(not(unix))]
pub fn account(_name: &str) -> Option<Account> {
    None
}

/// Whether the user named `name` is logged in on the console, as utmpx
/// records it: a `USER_PROCESS` entry on line `console`, which macOS keeps
/// for every graphical login, also while fast user switching has moved it
/// to the background, until that user logs out.
#[cfg(all(unix, not(target_env = "musl")))]
pub fn console_session(name: &str) -> bool {
    use std::ffi::CStr;

    fn field(chars: &[libc::c_char]) -> &[u8] {
        // SAFETY: c_char and u8 have the same size and alignment.
        let bytes = unsafe { std::slice::from_raw_parts(chars.as_ptr().cast::<u8>(), chars.len()) };
        CStr::from_bytes_until_nul(bytes).map_or(bytes, CStr::to_bytes)
    }

    let mut found = false;
    // SAFETY: utmpx is read on this thread from setutxent to endutxent;
    // each entry is only looked at before the next call.
    unsafe {
        libc::setutxent();
        loop {
            let entry = libc::getutxent();
            if entry.is_null() {
                break;
            }
            let entry = &*entry;
            if entry.ut_type == libc::USER_PROCESS
                && field(&entry.ut_user) == name.as_bytes()
                && field(&entry.ut_line) == b"console"
            {
                found = true;
                break;
            }
        }
        libc::endutxent();
    }
    found
}

/// None to be found where there is no utmpx: elsewhere, and in musl,
/// which has only stubs (static Linux builds). Only the macOS login window
/// asks.
#[cfg(any(not(unix), target_env = "musl"))]
pub fn console_session(_name: &str) -> bool {
    false
}

/// A folder that belongs to one user, from which files are read only if
/// they are that user's own regular files, all opened without following
/// links. A process running as root reads a user's files this way: a link,
/// or a file someone else owns, could otherwise make it read what that user
/// cannot, and a FIFO could hold it up.
#[cfg(unix)]
pub struct OwnedDir {
    fd: std::os::fd::OwnedFd,
    uid: u32,
}

#[cfg(unix)]
impl OwnedDir {
    /// Opens `path`, which must be a folder `uid` owns and not a link.
    pub fn open(path: &Path, uid: u32) -> std::io::Result<Self> {
        use std::os::unix::ffi::OsStrExt;

        let c_path = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|_| owned_error(path.display(), "has a NUL in its name"))?;
        // SAFETY: a valid C string; the descriptor returned is owned below.
        let fd = unsafe {
            libc::open(
                c_path.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            let error = open_error(&path.display());
            if std::fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_symlink()) {
                return Err(owned_error(path.display(), "is a link"));
            }
            return Err(error);
        }
        Self::checked(fd, uid, &path.display())
    }

    /// The folder `name` inside this one, on the same terms.
    pub fn dir(&self, name: &str) -> std::io::Result<Self> {
        let fd = self.open_at(
            name,
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
        )?;
        Self::checked(fd, self.uid, &name)
    }

    /// The contents of the file `name` in this folder, or `None` when there
    /// is none: a regular file the folder's user owns, at most `max_len`
    /// bytes long.
    pub fn read(&self, name: &str, max_len: u64) -> std::io::Result<Option<Vec<u8>>> {
        use std::io::Read;
        use std::os::fd::FromRawFd;

        let fd = match self.open_at(
            name,
            libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
        ) {
            Ok(fd) => fd,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(e),
        };
        // SAFETY: a descriptor just opened, owned from here on.
        let file = unsafe { std::fs::File::from_raw_fd(fd) };
        let stat = fstat(&file)?;
        if stat.st_mode & libc::S_IFMT != libc::S_IFREG {
            return Err(owned_error(name, "is not a regular file"));
        }
        if stat.st_uid != self.uid {
            return Err(owned_error(name, "belongs to another user"));
        }
        if stat.st_size < 0 || stat.st_size as u64 > max_len {
            return Err(owned_error(
                name,
                &format!("is larger than {max_len} bytes"),
            ));
        }
        let mut data = Vec::new();
        file.take(max_len + 1).read_to_end(&mut data)?;
        if data.len() as u64 > max_len {
            return Err(owned_error(
                name,
                &format!("is larger than {max_len} bytes"),
            ));
        }
        Ok(Some(data))
    }

    fn open_at(&self, name: &str, flags: libc::c_int) -> std::io::Result<libc::c_int> {
        use std::os::fd::AsRawFd;

        if name.is_empty() || name == "." || name == ".." || name.contains('/') {
            return Err(owned_error(name, "is not a name in the folder"));
        }
        let c_name =
            std::ffi::CString::new(name).map_err(|_| owned_error(name, "has a NUL in its name"))?;
        // SAFETY: the folder's descriptor is open and the name a valid C
        // string; the descriptor returned is owned by the caller.
        let fd = unsafe { libc::openat(self.fd.as_raw_fd(), c_name.as_ptr(), flags) };
        if fd >= 0 {
            return Ok(fd);
        }
        let error = open_error(&name);
        // A link fails as ELOOP, or as ENOTDIR where a folder was asked
        // for; say which it is.
        let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
        // SAFETY: as above; `stat` is valid for writing.
        let found = unsafe {
            libc::fstatat(
                self.fd.as_raw_fd(),
                c_name.as_ptr(),
                stat.as_mut_ptr(),
                libc::AT_SYMLINK_NOFOLLOW,
            )
        } == 0;
        // SAFETY: fstatat succeeded and filled it in.
        if found && unsafe { stat.assume_init() }.st_mode & libc::S_IFMT == libc::S_IFLNK {
            return Err(owned_error(name, "is a link"));
        }
        Err(error)
    }

    /// Takes `fd`, just opened, if it is a folder `uid` owns.
    fn checked(fd: libc::c_int, uid: u32, what: &dyn std::fmt::Display) -> std::io::Result<Self> {
        use std::os::fd::FromRawFd;

        // SAFETY: a descriptor just opened, owned from here on.
        let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(fd) };
        let stat = fstat(&fd)?;
        if stat.st_mode & libc::S_IFMT != libc::S_IFDIR {
            return Err(owned_error(what, "is not a folder"));
        }
        if stat.st_uid != uid {
            return Err(owned_error(what, "belongs to another user"));
        }
        Ok(Self { fd, uid })
    }
}

#[cfg(unix)]
fn fstat(fd: &impl std::os::fd::AsRawFd) -> std::io::Result<libc::stat> {
    let mut stat = std::mem::MaybeUninit::<libc::stat>::uninit();
    // SAFETY: the descriptor is open and `stat` is valid for writing.
    if unsafe { libc::fstat(fd.as_raw_fd(), stat.as_mut_ptr()) } != 0 {
        return Err(std::io::Error::last_os_error());
    }
    // SAFETY: fstat succeeded and filled it in.
    Ok(unsafe { stat.assume_init() })
}

/// The error opening `what` failed with, saying so when it is a link.
#[cfg(unix)]
fn open_error(what: &dyn std::fmt::Display) -> std::io::Error {
    let error = std::io::Error::last_os_error();
    match error.raw_os_error() {
        Some(libc::ELOOP) => owned_error(what, "is a link"),
        Some(libc::ENOENT) => std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("{what} does not exist"),
        ),
        _ => std::io::Error::new(error.kind(), format!("{what}: {error}")),
    }
}

/// What a program run printed ([`run_for`]).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Ran {
    pub text: String,
    /// Whether all of it was read: false where reading stopped first.
    pub whole: bool,
}

/// What `program` with `args` prints, if it finishes within `wait` and
/// succeeds; else why not: what it printed on stderr, or how it ended.
/// Both outputs are read as they come, so a long one does not fill its pipe
/// and stall the program; past `wait` it is killed. Once it has ended, what
/// it printed last is read until the deadline, or for [`GRACE`] if that is
/// later, so output that ends just in time is whole. Something the program
/// left running may hold its pipes open (`sh -c 'sleep 60 & echo hi'`):
/// then reading stops there with what was read ([`Ran::whole`] false), and
/// the threads reading them end only once it closes them.
pub fn run_for(program: &str, args: &[&str], wait: std::time::Duration) -> Result<Ran, String> {
    use std::io::Read;
    use std::process::{Command, Stdio};
    use std::sync::{Arc, Mutex, mpsc};
    use std::time::{Duration, Instant};

    /// What a pipe brought so far, and word once it ended.
    struct Reading {
        text: Arc<Mutex<Vec<u8>>>,
        ended: mpsc::Receiver<()>,
    }

    impl Reading {
        fn of<R: Read + Send + 'static>(pipe: Option<R>) -> Self {
            let text = Arc::new(Mutex::new(Vec::new()));
            let (done, ended) = mpsc::channel();
            let into = text.clone();
            std::thread::spawn(move || {
                let mut chunk = [0u8; 8192];
                if let Some(mut pipe) = pipe {
                    loop {
                        match pipe.read(&mut chunk) {
                            Ok(0) => break,
                            Ok(n) => lock(&into).extend_from_slice(&chunk[..n]),
                            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                            Err(_) => break,
                        }
                    }
                }
                let _ = done.send(());
            });
            Self { text, ended }
        }

        /// What was read once the pipe ended, or by `deadline`.
        fn by(self, deadline: Instant) -> Ran {
            let left = deadline.saturating_duration_since(Instant::now());
            let whole = self.ended.recv_timeout(left).is_ok();
            Ran {
                text: String::from_utf8_lossy(&lock(&self.text)).into_owned(),
                whole,
            }
        }
    }

    fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        m.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("{program}: {e}"))?;
    let stdout = Reading::of(child.stdout.take());
    let stderr = Reading::of(child.stderr.take());
    let deadline = Instant::now() + wait;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(format!("{program} took too long"));
            }
        }
    };
    let reading = deadline.max(Instant::now() + GRACE);
    if !status.success() {
        let why = stderr.by(reading).text.trim().to_owned();
        return Err(if why.is_empty() {
            format!("{program} failed: {status}")
        } else {
            why
        });
    }
    Ok(stdout.by(reading))
}

/// How long [`run_for`] goes on reading a program's output once it ended.
pub const GRACE: std::time::Duration = std::time::Duration::from_secs(1);

#[cfg(unix)]
fn owned_error(what: impl std::fmt::Display, why: &str) -> std::io::Error {
    std::io::Error::new(std::io::ErrorKind::InvalidData, format!("{what} {why}"))
}

/// Opens the InputWeave app, which is where the user is asked things
/// (docs/architecture.md §3: the UI is the only thing that talks to the
/// user), in the background: on macOS without bringing it forward, and
/// started with `--background` (as the Windows broker and Linux's D-Bus
/// activation start it), so it shows only what the agent's state calls
/// for, such as a pairing code, and never its main window by itself.
/// Returns whether that was possible here.
pub fn open_app() -> bool {
    #[cfg(target_os = "macos")]
    {
        let child = std::process::Command::new("/usr/bin/open")
            .args(["-g", "-b", "cc.ctsite.inputweave", "--args", "--background"])
            .spawn();
        // Reap it once it exits.
        child
            .map(|mut c| std::thread::spawn(move || c.wait()))
            .is_ok()
    }
    #[cfg(target_os = "linux")]
    {
        crate::linux::app::activate()
    }
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    {
        false
    }
}

/// Reads a line from standard input as `BufRead::read_line` does, without
/// showing what is typed when standard input is a terminal (a license
/// key). The Enter that ends it still moves to the next line.
#[cfg(unix)]
pub fn read_line_hidden(line: &mut String) -> std::io::Result<usize> {
    let stdin = std::io::stdin();
    // SAFETY: termios is plain data, all zeros until tcgetattr fills it.
    let mut saved: libc::termios = unsafe { std::mem::zeroed() };
    // SAFETY: `saved` is valid for writing; standard input may be any file,
    // and tcgetattr fails for one that is not a terminal.
    if unsafe { libc::tcgetattr(libc::STDIN_FILENO, &mut saved) } != 0 {
        return stdin.read_line(line);
    }
    let mut quiet = saved;
    quiet.c_lflag &= !libc::ECHO;
    quiet.c_lflag |= libc::ECHONL;
    // SAFETY: a termios tcgetattr filled in, changed only in its flags.
    unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &quiet) };
    let read = stdin.read_line(line);
    // SAFETY: what tcgetattr read, as it was.
    unsafe { libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &saved) };
    read
}

/// Reads a line from standard input as `BufRead::read_line` does, without
/// showing what is typed when standard input is a console (a license key).
#[cfg(windows)]
pub fn read_line_hidden(line: &mut String) -> std::io::Result<usize> {
    use windows::Win32::System::Console::{
        CONSOLE_MODE, ENABLE_ECHO_INPUT, GetConsoleMode, GetStdHandle, STD_INPUT_HANDLE,
        SetConsoleMode,
    };
    let stdin = std::io::stdin();
    // SAFETY: GetStdHandle only looks the handle up.
    let Ok(handle) = (unsafe { GetStdHandle(STD_INPUT_HANDLE) }) else {
        return stdin.read_line(line);
    };
    let mut saved = CONSOLE_MODE::default();
    // SAFETY: `saved` is valid for writing; GetConsoleMode fails for a
    // handle that is not a console.
    if unsafe { GetConsoleMode(handle, &mut saved) }.is_err() {
        return stdin.read_line(line);
    }
    // SAFETY: the console's own mode, without echo.
    let _ = unsafe { SetConsoleMode(handle, CONSOLE_MODE(saved.0 & !ENABLE_ECHO_INPUT.0)) };
    let read = stdin.read_line(line);
    // SAFETY: the mode GetConsoleMode read, as it was.
    let _ = unsafe { SetConsoleMode(handle, saved) };
    // The Enter that ended it was not shown either.
    eprintln!();
    read
}

#[cfg(not(any(unix, windows)))]
pub fn read_line_hidden(line: &mut String) -> std::io::Result<usize> {
    std::io::stdin().read_line(line)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn a_program_run_is_read_whole_and_waited_for_no_longer_than_said() {
        use std::time::{Duration, Instant};

        // More than a pipe holds: read as it comes, or it would stall.
        let long = run_for(
            "sh",
            &["-c", "yes 0123456789 | head -c 500000"],
            Duration::from_secs(10),
        )
        .unwrap();
        assert_eq!((long.text.len(), long.whole), (500_000, true));
        let failed = run_for(
            "sh",
            &["-c", "echo nope >&2; exit 3"],
            Duration::from_secs(10),
        );
        assert_eq!(failed, Err("nope".to_owned()));
        let silent = run_for("sh", &["-c", "exit 4"], Duration::from_secs(10));
        assert!(silent.is_err_and(|why| why.starts_with("sh failed")));
        let started = Instant::now();
        let slow = run_for("sleep", &["5"], Duration::from_millis(200));
        assert_eq!(slow, Err("sleep took too long".to_owned()));
        assert!(started.elapsed() < Duration::from_secs(3));
        assert!(run_for("no-such-program-inputweave", &[], Duration::from_secs(1)).is_err());
        // What it leaves running keeps its output open: what was printed by
        // the deadline (a grace past it, as it ended) comes back then, cut.
        let started = Instant::now();
        let left = run_for("sh", &["-c", "sleep 60 & echo hi"], Duration::from_secs(1));
        assert_eq!(
            left,
            Ok(Ran {
                text: "hi\n".to_owned(),
                whole: false
            })
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "{:?}",
            started.elapsed()
        );
    }

    fn me() -> u32 {
        // SAFETY: takes no arguments.
        unsafe { libc::getuid() }
    }

    #[test]
    fn accounts_are_found_by_name() {
        let root = account("root").expect("root has an account");
        assert_eq!(root.uid, 0);
        assert!(root.home.is_absolute(), "{root:?}");
        assert!(account("no-such-user-inputweave").is_none());
        assert!(account("bad\0name").is_none());
    }

    #[test]
    fn console_sessions_are_looked_up_by_name() {
        // Whoever runs the tests may or may not be at the console.
        let _ = console_session("root");
        assert!(!console_session("no-such-user-inputweave"));
        assert!(!console_session(""));
    }

    #[test]
    fn a_users_own_files_are_read() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir(tmp.path().join("inner")).unwrap();
        std::fs::write(tmp.path().join("inner/a.toml"), b"x = 1").unwrap();
        let dir = OwnedDir::open(tmp.path(), me()).unwrap();
        let inner = dir.dir("inner").unwrap();
        assert_eq!(
            inner.read("a.toml", 5).unwrap().as_deref(),
            Some(&b"x = 1"[..])
        );
        assert_eq!(inner.read("missing.toml", 5).unwrap(), None);
        assert!(dir.dir("missing").is_err());
    }

    #[test]
    fn links_others_files_and_odd_files_are_refused() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::write(root.join("real.toml"), b"x = 1").unwrap();
        std::fs::create_dir(root.join("real")).unwrap();
        std::os::unix::fs::symlink(root.join("real.toml"), root.join("link.toml")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("linked")).unwrap();
        std::fs::write(root.join("big.toml"), vec![b'#'; 100]).unwrap();
        let fifo = std::ffi::CString::new(root.join("fifo").to_str().unwrap()).unwrap();
        // SAFETY: a valid C string.
        assert_eq!(unsafe { libc::mkfifo(fifo.as_ptr(), 0o600) }, 0);

        let dir = OwnedDir::open(root, me()).unwrap();
        let message = |e: std::io::Error| e.to_string();
        assert!(message(dir.read("link.toml", 64).unwrap_err()).contains("is a link"));
        assert!(message(dir.dir("linked").err().unwrap()).contains("is a link"));
        assert!(message(dir.read("big.toml", 64).unwrap_err()).contains("larger than 64"));
        assert!(message(dir.read("fifo", 64).unwrap_err()).contains("not a regular file"));
        assert!(message(dir.read("real", 64).unwrap_err()).contains("not a regular file"));
        assert!(dir.read("../real.toml", 64).is_err());
        assert!(OwnedDir::open(&root.join("linked"), me()).is_err());
        assert!(OwnedDir::open(&root.join("real.toml"), me()).is_err());

        // Folders and files of another user.
        assert!(message(OwnedDir::open(root, me() + 1).err().unwrap()).contains("another user"));
        let theirs = OwnedDir {
            fd: dir.fd.try_clone().unwrap(),
            uid: me() + 1,
        };
        assert!(message(theirs.read("real.toml", 64).unwrap_err()).contains("another user"));
        assert!(message(theirs.dir("real").err().unwrap()).contains("another user"));
    }

    #[test]
    fn the_temp_directory_has_some_space() {
        let free = free_space(&std::env::temp_dir()).unwrap();
        assert!(free > 0);
        assert!(free_space(Path::new("/no/such/place")).is_none());
    }
}
