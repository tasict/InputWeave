use std::io;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};

use bytes::BytesMut;
use iw_proto::frame::{self, FrameError};
use prost::Message;
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};

/// The agent's listening socket. The socket file is removed on drop.
#[derive(Debug)]
pub struct IpcListener {
    listener: UnixListener,
    path: PathBuf,
    uid: u32,
}

impl IpcListener {
    /// Binds a socket at `path` that only the current user can use.
    ///
    /// A socket file left behind by a previous run is replaced, but if
    /// another agent is still listening on it this fails with
    /// [`io::ErrorKind::AddrInUse`].
    pub fn bind(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        check_length(&path)?;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        if path.exists() {
            if std::os::unix::net::UnixStream::connect(&path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("an agent is already listening on {}", path.display()),
                ));
            }
            std::fs::remove_file(&path)?;
        }
        let listener = UnixListener::bind(&path)?;
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
        let uid = std::fs::metadata(&path)?.uid();
        Ok(Self {
            listener,
            path,
            uid,
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Waits for the next client running as the same user. Connections from
    /// other users are dropped.
    pub async fn accept(&self) -> io::Result<IpcConnection> {
        loop {
            let (stream, _) = self.listener.accept().await?;
            match stream.peer_cred() {
                Ok(cred) if cred.uid() == self.uid => return Ok(IpcConnection::new(stream)),
                _ => continue,
            }
        }
    }
}

/// Longest socket path the kernel accepts, excluding the terminating NUL
/// (`sun_path` is 104 bytes on macOS and the BSDs, 108 on Linux).
const MAX_PATH_LEN: usize = if cfg!(any(target_os = "linux", target_os = "android")) {
    107
} else {
    103
};

fn check_length(path: &Path) -> io::Result<()> {
    let len = path.as_os_str().len();
    if len > MAX_PATH_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "socket path {} is {len} bytes long, but the limit is {MAX_PATH_LEN}; \
                 choose a shorter path",
                path.display()
            ),
        ));
    }
    Ok(())
}

impl Drop for IpcListener {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.path);
    }
}

/// A framed connection between the agent and one client.
#[derive(Debug)]
pub struct IpcConnection {
    stream: UnixStream,
    buf: BytesMut,
}

impl IpcConnection {
    fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            buf: BytesMut::new(),
        }
    }

    pub async fn connect(path: impl AsRef<Path>) -> io::Result<Self> {
        check_length(path.as_ref())?;
        Ok(Self::new(UnixStream::connect(path).await?))
    }

    pub async fn send<M: Message>(&mut self, msg: &M) -> Result<(), FrameError> {
        frame::write_frame(&mut self.stream, msg).await
    }

    /// The next message, or `None` once the other side has closed.
    pub async fn recv<M: Message + Default>(&mut self) -> Result<Option<M>, FrameError> {
        frame::read_frame(&mut self.stream, &mut self.buf).await
    }

    /// Splits the connection so one task can read while another writes.
    pub fn into_split(self) -> (IpcReader, IpcWriter) {
        let (read, write) = self.stream.into_split();
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
    read: OwnedReadHalf,
    buf: BytesMut,
}

impl IpcReader {
    pub async fn recv<M: Message + Default>(&mut self) -> Result<Option<M>, FrameError> {
        frame::read_frame(&mut self.read, &mut self.buf).await
    }
}

#[derive(Debug)]
pub struct IpcWriter {
    write: OwnedWriteHalf,
}

impl IpcWriter {
    pub async fn send<M: Message>(&mut self, msg: &M) -> Result<(), FrameError> {
        frame::write_frame(&mut self.write, msg).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use iw_proto::app::v1::{ClientHello, ClientMessage, client_message::Command};

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

    #[tokio::test]
    async fn exchanges_messages_and_cleans_up() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sub").join("agent.sock");
        let listener = IpcListener::bind(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600);

        let mut client = IpcConnection::connect(&path).await.unwrap();
        let server = listener.accept().await.unwrap();
        client.send(&hello()).await.unwrap();

        let (mut reader, mut writer) = server.into_split();
        assert_eq!(reader.recv::<ClientMessage>().await.unwrap(), Some(hello()));
        writer.send(&hello()).await.unwrap();
        assert_eq!(client.recv::<ClientMessage>().await.unwrap(), Some(hello()));

        drop(client);
        assert_eq!(reader.recv::<ClientMessage>().await.unwrap(), None);
        drop(listener);
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn explains_paths_that_are_too_long() {
        let long = PathBuf::from("/tmp").join("x".repeat(MAX_PATH_LEN));
        let err = IpcListener::bind(&long).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(err.to_string().contains("choose a shorter path"));
        let err = IpcConnection::connect(&long).await.unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
    }

    #[tokio::test]
    async fn refuses_to_steal_a_live_socket_but_replaces_a_stale_one() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("agent.sock");

        let first = IpcListener::bind(&path).unwrap();
        let err = IpcListener::bind(&path).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::AddrInUse);
        drop(first);

        // A socket file nobody listens on, as left by a crashed agent.
        drop(std::os::unix::net::UnixListener::bind(&path).unwrap());
        assert!(path.exists());
        IpcListener::bind(&path).unwrap();
    }
}
