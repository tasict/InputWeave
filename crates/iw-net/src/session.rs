//! An authenticated session between two paired devices: the protocol
//! version handshake and framed access to its streams and datagrams.
//!
//! Each session has one bidirectional control stream, opened by the dialer,
//! and at most one unidirectional input stream per direction, opened by
//! whichever side controls the other.

use std::time::Duration;

use bytes::BytesMut;
use iw_proto::frame::{self, FrameError};
use iw_proto::peer::v1::{Control, Datagram, Hello, control};
use iw_proto::{PEER_PROTOCOL_MAX, PEER_PROTOCOL_MIN};
use prost::Message;

use crate::endpoint::close;
use crate::{Fingerprint, NetError};

/// Send priorities of a session's streams. quinn sends data from a stream
/// only while no stream of a higher priority has any to send, and puts
/// datagrams (cursor motion and scrolling) in each packet before any
/// stream data. So keys, buttons and heartbeats never wait behind a
/// clipboard, and a clipboard never waits behind a file.
pub mod priority {
    /// Input streams: keys, buttons, entering and leaving.
    pub const INPUT: i32 = 1;
    /// The control stream: heartbeats, displays, the layout.
    pub const CONTROL: i32 = 1;
    /// Transfer streams carrying a clipboard; quinn's default.
    pub const CLIPBOARD: i32 = 0;
    /// Transfer streams carrying a file, both ways: only what everything
    /// else leaves.
    pub const FILE: i32 = -1;
}

/// The sending half of a framed QUIC stream.
#[derive(Debug)]
pub struct FramedSend(quinn::SendStream);

impl FramedSend {
    pub(crate) fn new(stream: quinn::SendStream) -> Self {
        Self(stream)
    }

    pub async fn send<M: Message>(&mut self, msg: &M) -> Result<(), FrameError> {
        frame::write_frame(&mut self.0, msg).await
    }

    /// Tells the peer nothing more will be sent.
    pub fn finish(&mut self) {
        let _ = self.0.finish();
    }

    /// Streams with a lower priority only get bandwidth left over by higher
    /// ones; bulk transfers use this to stay out of the way of input.
    pub fn set_priority(&self, priority: i32) {
        let _ = self.0.set_priority(priority);
    }

    /// Abandons the stream; the peer's reads fail with `code`.
    pub fn reset(&mut self, code: u32) {
        let _ = self.0.reset(code.into());
    }
}

/// The receiving half of a framed QUIC stream.
#[derive(Debug)]
pub struct FramedRecv {
    stream: quinn::RecvStream,
    buf: BytesMut,
    limit: usize,
}

impl FramedRecv {
    pub(crate) fn new(stream: quinn::RecvStream) -> Self {
        Self::with_limit(stream, frame::MAX_FRAME_LEN)
    }

    /// Rejects frames longer than `limit` bytes.
    pub(crate) fn with_limit(stream: quinn::RecvStream, limit: usize) -> Self {
        Self {
            stream,
            buf: BytesMut::new(),
            limit,
        }
    }

    /// The next message, or `None` once the peer finished the stream.
    /// Cancel-safe.
    pub async fn recv<M: Message + Default>(&mut self) -> Result<Option<M>, FrameError> {
        frame::read_frame_limited(&mut self.stream, &mut self.buf, self.limit).await
    }

    /// Tells the peer to stop sending; its writes fail with `code`.
    pub fn stop(&mut self, code: u32) {
        let _ = self.stream.stop(code.into());
    }
}

/// The code the peer reset or stopped a stream with, if that is what
/// `error` is: the way one side of a transfer tells the other it gave up.
pub fn stream_code(error: &FrameError) -> Option<u32> {
    let FrameError::Io(io) = error else {
        return None;
    };
    let inner = io.get_ref()?;
    let code = match (
        inner.downcast_ref::<quinn::ReadError>(),
        inner.downcast_ref::<quinn::WriteError>(),
    ) {
        (Some(quinn::ReadError::Reset(code)), _) => *code,
        (_, Some(quinn::WriteError::Stopped(code))) => *code,
        _ => return None,
    };
    u32::try_from(code.into_inner()).ok()
}

/// What both sides learned from the handshake.
#[derive(Clone, Debug)]
pub struct SessionInfo {
    pub connection: quinn::Connection,
    /// Fingerprint of the peer's certificate.
    pub peer: Fingerprint,
    /// The peer's Hello.
    pub hello: Hello,
    /// The protocol version both sides speak.
    pub version: u32,
}

impl SessionInfo {
    pub async fn open_input(&self) -> Result<FramedSend, NetError> {
        let stream = FramedSend(self.connection.open_uni().await?);
        stream.set_priority(priority::INPUT);
        Ok(stream)
    }

    pub async fn accept_input(&self) -> Result<FramedRecv, NetError> {
        Ok(FramedRecv::new(self.connection.accept_uni().await?))
    }

    /// Opens a transfer stream: a bidirectional stream for one clipboard
    /// or file transfer, which never holds up input. It starts at
    /// [`priority::CLIPBOARD`]; a file lowers it. Frames read from it are
    /// limited to `limit` bytes.
    pub async fn open_transfer(&self, limit: usize) -> Result<(FramedSend, FramedRecv), NetError> {
        let (send, recv) = self.connection.open_bi().await?;
        Ok((FramedSend(send), FramedRecv::with_limit(recv, limit)))
    }

    /// The next transfer stream the peer opened.
    pub async fn accept_transfer(
        &self,
        limit: usize,
    ) -> Result<(FramedSend, FramedRecv), NetError> {
        let (send, recv) = self.connection.accept_bi().await?;
        Ok((FramedSend(send), FramedRecv::with_limit(recv, limit)))
    }

    pub fn send_datagram(&self, msg: &Datagram) -> Result<(), NetError> {
        self.connection.send_datagram(msg.encode_to_vec().into())?;
        Ok(())
    }

    pub async fn recv_datagram(&self) -> Result<Datagram, NetError> {
        let bytes = self.connection.read_datagram().await?;
        Ok(Datagram::decode(bytes)?)
    }

    pub fn close(&self, code: u32, reason: &[u8]) {
        self.connection.close(code.into(), reason);
    }
}

#[derive(Debug)]
pub struct Session {
    pub info: SessionInfo,
    pub control_tx: FramedSend,
    pub control_rx: FramedRecv,
}

impl Session {
    /// Dialer side: opens the control stream and exchanges Hellos. If the
    /// versions are incompatible, the dialer closes the connection.
    pub async fn open(
        connection: quinn::Connection,
        peer: Fingerprint,
        ours: Hello,
    ) -> Result<Self, NetError> {
        let (send, recv) = connection.open_bi().await?;
        let (mut tx, mut rx) = (FramedSend(send), FramedRecv::new(recv));
        tx.set_priority(priority::CONTROL);
        tx.send(&hello_message(ours.clone())).await?;
        let theirs = read_hello(&mut rx).await?;
        let Some(version) = negotiate(&ours, &theirs) else {
            connection.close(close::INCOMPATIBLE.into(), b"incompatible protocol version");
            return Err(incompatible(&theirs));
        };
        Ok(Self::establish(connection, peer, theirs, version, tx, rx))
    }

    /// Acceptor side: waits for the dialer's control stream and Hello. If
    /// the versions are incompatible, it waits briefly for the dialer to
    /// close, so its own Hello is delivered and the dialer learns why.
    pub async fn accept(
        connection: quinn::Connection,
        peer: Fingerprint,
        ours: Hello,
    ) -> Result<Self, NetError> {
        let (send, recv) = connection.accept_bi().await?;
        let (mut tx, mut rx) = (FramedSend(send), FramedRecv::new(recv));
        tx.set_priority(priority::CONTROL);
        let theirs = read_hello(&mut rx).await?;
        tx.send(&hello_message(ours.clone())).await?;
        let Some(version) = negotiate(&ours, &theirs) else {
            let _ = tokio::time::timeout(INCOMPATIBLE_GRACE, connection.closed()).await;
            connection.close(close::INCOMPATIBLE.into(), b"incompatible protocol version");
            return Err(incompatible(&theirs));
        };
        Ok(Self::establish(connection, peer, theirs, version, tx, rx))
    }

    fn establish(
        connection: quinn::Connection,
        peer: Fingerprint,
        theirs: Hello,
        version: u32,
        control_tx: FramedSend,
        control_rx: FramedRecv,
    ) -> Self {
        Self {
            info: SessionInfo {
                connection,
                peer,
                hello: theirs,
                version,
            },
            control_tx,
            control_rx,
        }
    }
}

/// How long the acceptor waits for the dialer to close after an
/// incompatible handshake.
const INCOMPATIBLE_GRACE: Duration = Duration::from_secs(2);

fn incompatible(theirs: &Hello) -> NetError {
    NetError::Incompatible {
        min: theirs.protocol_min,
        max: theirs.protocol_max,
    }
}

/// A Hello for this build, advertising the protocol versions it speaks.
pub fn hello(device_id: &str, device_name: &str, os: iw_proto::peer::v1::Os) -> Hello {
    Hello {
        protocol_min: PEER_PROTOCOL_MIN,
        protocol_max: PEER_PROTOCOL_MAX,
        device_id: device_id.to_owned(),
        device_name: device_name.to_owned(),
        os: os.into(),
        app_version: env!("CARGO_PKG_VERSION").to_owned(),
    }
}

/// The highest version both sides support.
pub(crate) fn negotiate(ours: &Hello, theirs: &Hello) -> Option<u32> {
    let low = ours.protocol_min.max(theirs.protocol_min);
    let high = ours.protocol_max.min(theirs.protocol_max);
    (low <= high).then_some(high)
}

fn hello_message(hello: Hello) -> Control {
    Control {
        msg: Some(control::Msg::Hello(hello)),
    }
}

async fn read_hello(rx: &mut FramedRecv) -> Result<Hello, NetError> {
    match rx.recv::<Control>().await? {
        Some(Control {
            msg: Some(control::Msg::Hello(hello)),
        }) => Ok(hello),
        Some(_) => Err(NetError::Protocol(
            "expected Hello first on the control stream",
        )),
        None => Err(NetError::Protocol("control stream closed before Hello")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn versions(min: u32, max: u32) -> Hello {
        Hello {
            protocol_min: min,
            protocol_max: max,
            ..Default::default()
        }
    }

    #[test]
    fn negotiates_the_highest_common_version() {
        assert_eq!(negotiate(&versions(1, 3), &versions(2, 5)), Some(3));
        assert_eq!(negotiate(&versions(1, 1), &versions(1, 1)), Some(1));
        assert_eq!(negotiate(&versions(1, 2), &versions(3, 4)), None);
    }
}
