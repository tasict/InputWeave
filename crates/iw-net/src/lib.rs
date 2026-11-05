//! Networking between InputWeave peers: device identity and fingerprints,
//! the QUIC endpoint with fingerprint-pinned mutual TLS, the session
//! handshake, SPAKE2 pairing and mDNS discovery.
//!
//! Which devices are trusted is the caller's business: the endpoint asks it
//! for every session, and pairing hands it the fingerprint to remember.

pub mod discovery;
mod endpoint;
mod fingerprint;
mod identity;
pub mod pairing;
mod session;
mod tls;

pub use endpoint::{
    ALPN_PAIRING, ALPN_SESSION, Accepted, IncomingPeer, PeerEndpoint, Purpose, close,
    peer_fingerprint,
};
pub use fingerprint::{Fingerprint, ParseFingerprintError};
pub use identity::{CERT_FILE, Identity, IdentityError, KEY_FILE};
pub use pairing::{
    Paired, PairingCode, PairingError, PairingInitiator, PairingResponder, is_valid_device_id,
    is_valid_device_name,
};
pub use session::{FramedRecv, FramedSend, Session, SessionInfo, hello, priority, stream_code};

use iw_proto::frame::FrameError;

#[derive(Debug, thiserror::Error)]
pub enum NetError {
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("TLS configuration failed: {0}")]
    Tls(#[from] rustls::Error),
    #[error("no TLS 1.3 cipher suite available for QUIC")]
    NoCipherSuite(#[from] quinn::crypto::rustls::NoInitialCipherSuite),
    #[error(transparent)]
    Connect(#[from] quinn::ConnectError),
    #[error(transparent)]
    Connection(#[from] quinn::ConnectionError),
    #[error(transparent)]
    SendDatagram(#[from] quinn::SendDatagramError),
    #[error(transparent)]
    Frame(#[from] FrameError),
    #[error("malformed message: {0}")]
    Decode(#[from] prost::DecodeError),
    #[error("device {0} is not paired")]
    Untrusted(Fingerprint),
    #[error("peer speaks protocol versions {min}..={max}, which this build does not support")]
    Incompatible { min: u32, max: u32 },
    #[error("protocol error: {0}")]
    Protocol(&'static str),
    #[error("mDNS: {0}")]
    Discovery(String),
}

#[cfg(test)]
mod tests;
