//! Content that travels on transfer streams rather than the control or
//! input streams, so it never holds them up: clipboards, and the offers
//! that start file transfers (see `files`).

use iw_net::{FramedRecv, NetError, SessionInfo};
use iw_platform::{ClipboardContent, ClipboardFormat};
use iw_proto::peer::v1::{Chunk, ClipboardHeader, ClipboardPart, FileOffer, Transfer, transfer};

#[cfg(test)]
use super::lazy_clipboard::LAZY_MAX;
use super::lazy_clipboard::{is_pull, lazy_max};

/// Content is sent in chunks of this size.
const CHUNK: usize = 256 * 1024;
/// Frames on transfer streams: a chunk and its framing, or a header.
pub(crate) const FRAME_LIMIT: usize = CHUNK + 4096;
/// Formats in one clipboard; there are only four.
const MAX_PARTS: usize = ClipboardFormat::ALL.len();
/// How long a transfer stream may go without its next frame while a header
/// or a clipboard is read from it, before it is given up.
pub(crate) const STALL: std::time::Duration = std::time::Duration::from_secs(30);

/// A clipboard on its way between devices.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ClipboardTransfer {
    /// The request this answers, or 0 when not asked for.
    pub(crate) request_id: u64,
    /// Whose clipboard it is.
    pub(crate) origin: String,
    pub(crate) content: ClipboardContent,
}

pub(crate) enum Incoming {
    /// A clipboard, whose content follows on the stream (`read_clipboard`).
    Clipboard(ClipboardHeader),
    /// The rest of the stream belongs to the file transfer.
    File(FileOffer),
}

pub(crate) async fn send_clipboard(
    info: &SessionInfo,
    clipboard: &ClipboardTransfer,
) -> Result<(), NetError> {
    let (mut tx, _rx) = info.open_transfer(FRAME_LIMIT).await?;
    let header = ClipboardHeader {
        request_id: clipboard.request_id,
        origin_device_id: clipboard.origin.clone(),
        parts: clipboard
            .content
            .items
            .iter()
            .map(|(format, data)| ClipboardPart {
                mime: format.mime().to_owned(),
                size: data.len() as u64,
            })
            .collect(),
    };
    tx.send(&Transfer {
        kind: Some(transfer::Kind::Clipboard(header)),
    })
    .await?;
    for (_, data) in &clipboard.content.items {
        for chunk in data.chunks(CHUNK) {
            tx.send(&Chunk {
                data: chunk.to_vec(),
            })
            .await?;
        }
    }
    tx.finish();
    Ok(())
}

/// The most a clipboard announced under `request_id` may hold: up to the
/// lazy limit for the answer to a pull this device `expects` from the
/// sender, `max_clipboard` for anything else that is not a pull. An answer
/// to a pull not expected is refused.
fn clipboard_limit(
    request_id: u64,
    max_clipboard: usize,
    expects: impl Fn(u64) -> bool,
) -> Result<usize, NetError> {
    if !is_pull(request_id) {
        Ok(max_clipboard)
    } else if expects(request_id) {
        Ok(lazy_max(max_clipboard))
    } else {
        Err(NetError::Protocol("a pulled clipboard nobody asked for"))
    }
}

fn stalled<T>(_: T) -> NetError {
    NetError::Protocol("transfer stalled")
}

/// Reads what a transfer stream starts with: a clipboard's header, or a
/// file offer.
pub(crate) async fn receive(rx: &mut FramedRecv) -> Result<Incoming, NetError> {
    let first = tokio::time::timeout(STALL, rx.recv::<Transfer>())
        .await
        .map_err(stalled)??;
    match first {
        Some(Transfer {
            kind: Some(transfer::Kind::Clipboard(header)),
        }) => Ok(Incoming::Clipboard(header)),
        Some(Transfer {
            kind: Some(transfer::Kind::File(offer)),
        }) => Ok(Incoming::File(offer)),
        _ => Err(NetError::Protocol("unknown transfer")),
    }
}

/// Whether a clipboard `header` announces may be read: at most four
/// formats, within its limit (`clipboard_limit`: `max_clipboard`, or the
/// lazy limit for a pull this device `expects` from the sender).
pub(crate) fn check_clipboard(
    header: &ClipboardHeader,
    max_clipboard: usize,
    expects: impl Fn(u64) -> bool,
) -> Result<(), NetError> {
    if header.parts.len() > MAX_PARTS {
        return Err(NetError::Protocol("too many clipboard parts"));
    }
    let total = header
        .parts
        .iter()
        .try_fold(0u64, |sum, p| sum.checked_add(p.size))
        .ok_or(NetError::Protocol("clipboard size overflows"))?;
    let max_clipboard = clipboard_limit(header.request_id, max_clipboard, expects)?;
    if total > max_clipboard as u64 {
        return Err(NetError::Protocol("clipboard larger than allowed"));
    }
    Ok(())
}

/// Reads the content `header` announced (checked with `check_clipboard`).
/// Memory grows only as the content arrives, and a stream that stalls for
/// [`STALL`] is given up.
pub(crate) async fn read_clipboard(
    rx: &mut FramedRecv,
    header: ClipboardHeader,
) -> Result<ClipboardTransfer, NetError> {
    let mut content = ClipboardContent::default();
    for part in &header.parts {
        let format = ClipboardFormat::from_mime(&part.mime)
            .ok_or(NetError::Protocol("unknown clipboard format"))?;
        let size = part.size as usize;
        let mut data = Vec::new();
        while data.len() < size {
            let chunk = tokio::time::timeout(STALL, rx.recv::<Chunk>())
                .await
                .map_err(stalled)??
                .ok_or(NetError::Protocol("clipboard ended early"))?;
            if data.len() + chunk.data.len() > size {
                return Err(NetError::Protocol("clipboard part longer than announced"));
            }
            data.extend_from_slice(&chunk.data);
        }
        content.items.push((format, data));
    }
    Ok(ClipboardTransfer {
        request_id: header.request_id,
        origin: header.origin_device_id,
        content,
    })
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use iw_net::{Identity, PeerEndpoint, Session, hello};
    use iw_proto::peer::v1::Os;

    use super::*;
    use crate::agent::lazy_clipboard::PULL_BIT;

    const MAX: usize = 8 * 1024 * 1024;

    fn endpoint() -> PeerEndpoint {
        PeerEndpoint::bind(
            "127.0.0.1:0".parse().unwrap(),
            Arc::new(Identity::generate().unwrap()),
        )
        .unwrap()
    }

    /// A session between two devices, with their endpoints kept open.
    async fn session() -> (Session, Session, [PeerEndpoint; 2]) {
        let (mac, win) = (endpoint(), endpoint());
        let (mac_fp, win_fp, win_addr) = (
            mac.fingerprint(),
            win.fingerprint(),
            win.local_addr().unwrap(),
        );
        let server = tokio::spawn(async move {
            let accepted = win
                .accept()
                .await
                .unwrap()
                .finish(|fp| *fp == mac_fp)
                .await
                .unwrap();
            let session = Session::accept(
                accepted.connection,
                accepted.peer,
                hello("win", "Windows PC", Os::Windows),
            )
            .await
            .unwrap();
            (win, session)
        });
        let connection = mac.connect(win_addr, win_fp).await.unwrap();
        let mac_session = Session::open(connection, win_fp, hello("mac", "Mac", Os::Macos))
            .await
            .unwrap();
        let (win, win_session) = server.await.unwrap();
        (mac_session, win_session, [mac, win])
    }

    /// Sends a clipboard header announcing `size` bytes under `request_id`,
    /// then `sent` bytes, and reads it on the other side.
    async fn announce(
        request_id: u64,
        size: u64,
        sent: usize,
        expects: impl Fn(u64) -> bool,
    ) -> Result<ClipboardTransfer, NetError> {
        let (mac, win, _endpoints) = session().await;
        let (mut tx, _rx) = mac.info.open_transfer(FRAME_LIMIT).await.unwrap();
        tx.send(&Transfer {
            kind: Some(transfer::Kind::Clipboard(ClipboardHeader {
                request_id,
                origin_device_id: "mac".to_owned(),
                parts: vec![ClipboardPart {
                    mime: ClipboardFormat::Text.mime().to_owned(),
                    size,
                }],
            })),
        })
        .await
        .unwrap();
        if sent > 0 {
            tx.send(&Chunk {
                data: vec![b'x'; sent],
            })
            .await
            .unwrap();
        }
        let (_tx, mut rx) = win.info.accept_transfer(FRAME_LIMIT).await.unwrap();
        // Refused or read, never left waiting for content.
        let read = async {
            let Incoming::Clipboard(header) = receive(&mut rx).await? else {
                panic!("not a clipboard");
            };
            check_clipboard(&header, MAX, expects)?;
            read_clipboard(&mut rx, header).await
        };
        tokio::time::timeout(std::time::Duration::from_secs(10), read)
            .await
            .expect("read in time")
    }

    #[tokio::test]
    async fn only_a_pull_waited_on_may_be_larger() {
        let pull = PULL_BIT | 5;
        // A pull this device never sent: refused before anything is read.
        assert!(
            announce(pull, 200 * 1024 * 1024, 0, |_| false)
                .await
                .is_err()
        );
        // Not a pull: the usual limit.
        assert!(announce(1, MAX as u64 + 1, 0, |_| true).await.is_err());
        // A pull waited on may announce more than the usual limit, but no
        // more than the lazy one.
        assert!(
            announce(pull, LAZY_MAX as u64 + 1, 0, |id| id == pull)
                .await
                .is_err()
        );
        let read = announce(pull, 10, 10, |id| id == pull).await.unwrap();
        assert_eq!((read.request_id, read.content.len()), (pull, 10));
    }
}
