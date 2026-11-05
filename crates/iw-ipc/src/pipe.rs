use std::ffi::c_void;
use std::io;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use bytes::BytesMut;
use iw_proto::frame::{self, FrameError};
use prost::Message;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf, ReadHalf, WriteHalf};
use tokio::net::windows::named_pipe::{
    ClientOptions, NamedPipeClient, NamedPipeServer, ServerOptions,
};
use tokio::sync::Mutex;
use windows::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_INSUFFICIENT_BUFFER, ERROR_PIPE_BUSY, HANDLE, HLOCAL, LocalFree,
};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, IsWellKnownSid, PSECURITY_DESCRIPTOR, PSID, SECURITY_ATTRIBUTES,
    TOKEN_GROUPS, TOKEN_INFORMATION_CLASS, TOKEN_QUERY, TOKEN_USER, TokenGroups, TokenUser,
    WinLocalSystemSid,
};
use windows::Win32::System::RemoteDesktop::{ProcessIdToSessionId, WTSQueryUserToken};
use windows::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId, OpenProcessToken};
use windows::core::{HSTRING, PWSTR};

use crate::access::{self, PipeAccess};

/// How long `connect` keeps trying while every instance of the pipe is
/// busy with another client.
const BUSY_WAIT: Duration = Duration::from_secs(2);

/// The group attribute marking a token's logon SID (winnt.h). The `windows`
/// crate has it only in its large SystemServices module.
const SE_GROUP_LOGON_ID: u32 = 0xC000_0000;

/// The agent's pipe for the session this process runs in.
pub fn agent_pipe_name() -> io::Result<PathBuf> {
    Ok(PathBuf::from(access::agent_pipe_name(this_session()?)))
}

/// The pipe the agent serves the broker on, for the session this process
/// runs in.
pub fn broker_pipe_name() -> io::Result<PathBuf> {
    Ok(PathBuf::from(access::broker_pipe_name(this_session()?)))
}

fn this_session() -> io::Result<u32> {
    let mut session = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) }
        .map_err(io::Error::from)?;
    Ok(session)
}

/// A listening named pipe. Each accepted client gets its own instance; the
/// next one is created as soon as a client connects, so the name never goes
/// away while the listener lives.
#[derive(Debug)]
pub struct IpcListener {
    path: PathBuf,
    security: SecurityDescriptor,
    next: Mutex<Option<NamedPipeServer>>,
}

impl IpcListener {
    /// Creates the pipe `path` (`\\.\pipe\…`) for SYSTEM and the user
    /// logged on to this session; see [`PipeAccess::Session`].
    ///
    /// Fails with [`io::ErrorKind::AddrInUse`] if another process already
    /// serves that name. Must be called within a Tokio runtime.
    pub fn bind(path: impl AsRef<Path>) -> io::Result<Self> {
        Self::bind_with(path, PipeAccess::Session)
    }

    /// Like [`IpcListener::bind`], with the given access.
    pub fn bind_with(path: impl AsRef<Path>, access: PipeAccess) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let user = match access {
            PipeAccess::Session => session_logon_sid()?,
            PipeAccess::SystemOnly => None,
        };
        let sddl = access::pipe_sddl(access, user.as_deref())
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "unexpected SID format"))?;
        let security = SecurityDescriptor::from_sddl(&sddl)?;
        let first = create(&path, &security, true).map_err(|e| {
            if e.raw_os_error() == Some(ERROR_ACCESS_DENIED.0 as i32) {
                io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("another process already serves {}", path.display()),
                )
            } else {
                e
            }
        })?;
        Ok(Self {
            path,
            security,
            next: Mutex::new(Some(first)),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Waits for the next client. The pipe's DACL decides who may connect.
    ///
    /// Cancel-safe: the waiting instance stays for the next call.
    pub async fn accept(&self) -> io::Result<IpcConnection> {
        let mut next = self.next.lock().await;
        let server = match &mut *next {
            Some(server) => server,
            empty => empty.insert(create(&self.path, &self.security, false)?),
        };
        if let Err(e) = server.connect().await {
            // The instance may be unusable now; the next call makes a new one.
            *next = None;
            return Err(e);
        }
        let connected = next.take().expect("the instance connected above");
        // If this fails, the next call tries again; the connected instance
        // keeps the name alive meanwhile.
        *next = create(&self.path, &self.security, false).ok();
        Ok(IpcConnection::new(Pipe::Server(connected)))
    }
}

fn create(path: &Path, security: &SecurityDescriptor, first: bool) -> io::Result<NamedPipeServer> {
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: security.0.0,
        bInheritHandle: false.into(),
    };
    // SAFETY: `attributes` points to a valid SECURITY_ATTRIBUTES whose
    // descriptor lives as long as the listener; Windows copies it into the
    // pipe during the call.
    unsafe {
        ServerOptions::new()
            .first_pipe_instance(first)
            .reject_remote_clients(true)
            .create_with_security_attributes_raw(path, (&raw mut attributes).cast::<c_void>())
    }
}

/// A security descriptor allocated by Windows, freed on drop.
#[derive(Debug)]
struct SecurityDescriptor(PSECURITY_DESCRIPTOR);

// SAFETY: the descriptor is an immutable heap block once built; it is only
// read, and freed once.
unsafe impl Send for SecurityDescriptor {}
// SAFETY: as above.
unsafe impl Sync for SecurityDescriptor {}

impl SecurityDescriptor {
    fn from_sddl(sddl: &str) -> io::Result<Self> {
        let mut descriptor = PSECURITY_DESCRIPTOR::default();
        // SAFETY: the out pointer is valid; the string outlives the call.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                &HSTRING::from(sddl),
                SDDL_REVISION_1,
                &mut descriptor,
                None,
            )
        }
        .map_err(io::Error::from)?;
        Ok(Self(descriptor))
    }
}

impl Drop for SecurityDescriptor {
    fn drop(&mut self) {
        // SAFETY: allocated with LocalAlloc by the conversion above.
        unsafe {
            LocalFree(Some(HLOCAL(self.0.0)));
        }
    }
}

/// The logon SID of the user logged on to this session, as a string.
///
/// As SYSTEM (the agent started by the service) that is the session's
/// user, if anyone is logged on. Otherwise it is this process's own logon,
/// as when the agent runs as a plain user process during development or in
/// a Store build.
fn session_logon_sid() -> io::Result<Option<String>> {
    let own = process_token()?;
    if !is_local_system(&own)? {
        return logon_sid(&own).map(Some);
    }
    let mut session = 0;
    // SAFETY: the out pointer is valid for the call.
    unsafe { ProcessIdToSessionId(GetCurrentProcessId(), &mut session) }
        .map_err(io::Error::from)?;
    let mut token = HANDLE::default();
    // SAFETY: the out pointer is valid; on success the token is ours to close.
    match unsafe { WTSQueryUserToken(session, &mut token) } {
        // SAFETY: the handle was just returned to us and is owned here.
        Ok(()) => logon_sid(&unsafe { OwnedHandle::from_raw_handle(token.0) }).map(Some),
        // Nobody is logged on to this session.
        Err(_) => Ok(None),
    }
}

fn process_token() -> io::Result<OwnedHandle> {
    let mut token = HANDLE::default();
    // SAFETY: the pseudo handle needs no closing; the out pointer is valid
    // and the token returned is ours to close.
    unsafe {
        OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token).map_err(io::Error::from)?;
        Ok(OwnedHandle::from_raw_handle(token.0))
    }
}

fn is_local_system(token: &OwnedHandle) -> io::Result<bool> {
    let buf = token_information(token, TokenUser)?;
    // SAFETY: the buffer holds a TOKEN_USER, suitably aligned, whose SID
    // points into the same buffer.
    let user = unsafe { &*buf.as_ptr().cast::<TOKEN_USER>() };
    // SAFETY: the SID is valid while `buf` lives.
    Ok(unsafe { IsWellKnownSid(user.User.Sid, WinLocalSystemSid) }.as_bool())
}

/// The token's logon SID (`S-1-5-5-…`), or its user SID if it has none.
fn logon_sid(token: &OwnedHandle) -> io::Result<String> {
    let buf = token_information(token, TokenGroups)?;
    // SAFETY: the buffer holds a TOKEN_GROUPS, suitably aligned, followed by
    // its GroupCount entries; the SIDs point into the same buffer.
    let groups = unsafe {
        let groups = &*buf.as_ptr().cast::<TOKEN_GROUPS>();
        std::slice::from_raw_parts(groups.Groups.as_ptr(), groups.GroupCount as usize)
    };
    if let Some(group) = groups
        .iter()
        .find(|g| g.Attributes & SE_GROUP_LOGON_ID == SE_GROUP_LOGON_ID)
    {
        return sid_string(group.Sid);
    }
    let buf = token_information(token, TokenUser)?;
    // SAFETY: as in `is_local_system`.
    let user = unsafe { &*buf.as_ptr().cast::<TOKEN_USER>() };
    sid_string(user.User.Sid)
}

/// Variable-length token information, in a buffer aligned for the
/// structures it holds.
fn token_information(token: &OwnedHandle, class: TOKEN_INFORMATION_CLASS) -> io::Result<Vec<u64>> {
    let handle = HANDLE(token.as_raw_handle());
    let mut len = 0u32;
    // SAFETY: asking for the size only.
    if let Err(e) = unsafe { GetTokenInformation(handle, class, None, 0, &mut len) }
        && e.code() != ERROR_INSUFFICIENT_BUFFER.to_hresult()
    {
        return Err(e.into());
    }
    let mut buf = vec![0u64; (len as usize).div_ceil(8)];
    // SAFETY: the buffer is at least `len` bytes long.
    unsafe { GetTokenInformation(handle, class, Some(buf.as_mut_ptr().cast()), len, &mut len) }
        .map_err(io::Error::from)?;
    Ok(buf)
}

fn sid_string(sid: PSID) -> io::Result<String> {
    let mut text = PWSTR::null();
    // SAFETY: the SID is valid for the call; the string returned is
    // LocalAlloc'd and freed below.
    unsafe {
        ConvertSidToStringSidW(sid, &mut text).map_err(io::Error::from)?;
        let result = text
            .to_string()
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e));
        LocalFree(Some(HLOCAL(text.0.cast())));
        result
    }
}

/// Either end of a pipe.
#[derive(Debug)]
enum Pipe {
    Server(NamedPipeServer),
    Client(NamedPipeClient),
}

impl AsyncRead for Pipe {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Pipe::Server(p) => Pin::new(p).poll_read(cx, buf),
            Pipe::Client(p) => Pin::new(p).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for Pipe {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<io::Result<usize>> {
        match self.get_mut() {
            Pipe::Server(p) => Pin::new(p).poll_write(cx, buf),
            Pipe::Client(p) => Pin::new(p).poll_write(cx, buf),
        }
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Pipe::Server(p) => Pin::new(p).poll_flush(cx),
            Pipe::Client(p) => Pin::new(p).poll_flush(cx),
        }
    }

    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        match self.get_mut() {
            Pipe::Server(p) => Pin::new(p).poll_shutdown(cx),
            Pipe::Client(p) => Pin::new(p).poll_shutdown(cx),
        }
    }
}

/// A framed connection between the agent and one client.
#[derive(Debug)]
pub struct IpcConnection {
    pipe: Pipe,
    buf: BytesMut,
}

impl IpcConnection {
    fn new(pipe: Pipe) -> Self {
        Self {
            pipe,
            buf: BytesMut::new(),
        }
    }

    /// Opens the pipe `path`, waiting up to two seconds while all its
    /// instances are busy. Must be called within a Tokio runtime.
    pub async fn connect(path: impl AsRef<Path>) -> io::Result<Self> {
        let deadline = Instant::now() + BUSY_WAIT;
        loop {
            match ClientOptions::new().open(path.as_ref()) {
                Ok(client) => return Ok(Self::new(Pipe::Client(client))),
                Err(e)
                    if e.raw_os_error() == Some(ERROR_PIPE_BUSY.0 as i32)
                        && Instant::now() < deadline =>
                {
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                Err(e) => return Err(e),
            }
        }
    }

    pub async fn send<M: Message>(&mut self, msg: &M) -> Result<(), FrameError> {
        frame::write_frame(&mut self.pipe, msg).await
    }

    /// The next message, or `None` once the other side has closed.
    pub async fn recv<M: Message + Default>(&mut self) -> Result<Option<M>, FrameError> {
        frame::read_frame(&mut self.pipe, &mut self.buf).await
    }

    /// Splits the connection so one task can read while another writes.
    pub fn into_split(self) -> (IpcReader, IpcWriter) {
        let (read, write) = tokio::io::split(self.pipe);
        (
            IpcReader {
                read,
                buf: self.buf,
            },
            IpcWriter { write },
        )
    }
}

#[derive(Debug)]
pub struct IpcReader {
    read: ReadHalf<Pipe>,
    buf: BytesMut,
}

impl IpcReader {
    pub async fn recv<M: Message + Default>(&mut self) -> Result<Option<M>, FrameError> {
        frame::read_frame(&mut self.read, &mut self.buf).await
    }
}

#[derive(Debug)]
pub struct IpcWriter {
    write: WriteHalf<Pipe>,
}

impl IpcWriter {
    pub async fn send<M: Message>(&mut self, msg: &M) -> Result<(), FrameError> {
        frame::write_frame(&mut self.write, msg).await
    }
}

#[cfg(test)]
mod tests {
    use iw_proto::app::v1::{ClientHello, ClientMessage, client_message::Command};

    use super::*;

    fn hello() -> ClientMessage {
        ClientMessage {
            request_id: 1,
            command: Some(Command::Hello(ClientHello {
                ipc_version: 1,
                client: "test".to_owned(),
                ..ClientHello::default()
            })),
        }
    }

    /// The pipe name has a backslash after `pipe\`, as the agent's does,
    /// and the default DACL must admit this process's own logon.
    #[tokio::test]
    async fn exchanges_messages_and_refuses_a_second_server() {
        let path = PathBuf::from(format!(r"\\.\pipe\InputWeave\test-{}", std::process::id()));
        let listener = IpcListener::bind(&path).unwrap();
        let err = IpcListener::bind(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AddrInUse);

        let mut client = IpcConnection::connect(&path).await.unwrap();
        let server = listener.accept().await.unwrap();
        client.send(&hello()).await.unwrap();

        let (mut reader, mut writer) = server.into_split();
        assert_eq!(reader.recv::<ClientMessage>().await.unwrap(), Some(hello()));
        writer.send(&hello()).await.unwrap();
        assert_eq!(client.recv::<ClientMessage>().await.unwrap(), Some(hello()));

        // A second client gets the instance created after the first one
        // connected.
        let second = IpcConnection::connect(&path).await.unwrap();
        drop(listener.accept().await.unwrap());
        drop(second);

        drop(client);
        assert!(!matches!(reader.recv::<ClientMessage>().await, Ok(Some(_))));
    }

    #[test]
    fn the_agent_pipe_is_named_after_this_session() {
        let name = agent_pipe_name().unwrap();
        let name = name.to_str().unwrap();
        let session = name.strip_prefix(r"\\.\pipe\InputWeave\agent-").unwrap();
        assert!(session.parse::<u32>().is_ok(), "{name}");
    }
}
