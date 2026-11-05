//! One QUIC endpoint per device, acting as both server and client.

use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use quinn::crypto::rustls::{HandshakeData, QuicClientConfig, QuicServerConfig};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::CertificateDer;

use crate::tls::{ClientCertRequired, ServerPin};
use crate::{Fingerprint, Identity, NetError, tls};

/// ALPN of a normal session between paired devices.
pub const ALPN_SESSION: &[u8] = b"inputweave/1";
/// ALPN of a pairing attempt. Only pairing messages are accepted on it.
pub const ALPN_PAIRING: &[u8] = b"inputweave-pair/1";

/// Application close codes.
pub mod close {
    pub const NORMAL: u32 = 0;
    pub const UNTRUSTED: u32 = 1;
    pub const INCOMPATIBLE: u32 = 2;
    pub const TIMEOUT: u32 = 3;
    pub const DUPLICATE: u32 = 4;
}

/// QUIC keep-alive and idle timeout. They back up the application
/// heartbeat: a peer that loses power is noticed even if the heartbeat task
/// is stuck.
const KEEP_ALIVE: Duration = Duration::from_millis(500);
const IDLE_TIMEOUT: Duration = Duration::from_secs(3);

const SERVER_NAME: &str = "inputweave";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Purpose {
    Session,
    Pairing,
}

#[derive(Debug)]
pub struct PeerEndpoint {
    endpoint: quinn::Endpoint,
    identity: Arc<Identity>,
    provider: Arc<CryptoProvider>,
}

impl PeerEndpoint {
    pub fn bind(addr: SocketAddr, identity: Arc<Identity>) -> Result<Self, NetError> {
        let provider = tls::provider();
        let mut crypto = rustls::ServerConfig::builder_with_provider(provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .with_client_cert_verifier(Arc::new(ClientCertRequired {
                provider: provider.clone(),
            }))
            .with_single_cert(vec![identity.cert()], identity.key())?;
        crypto.alpn_protocols = vec![ALPN_SESSION.to_vec(), ALPN_PAIRING.to_vec()];
        let mut server =
            quinn::ServerConfig::with_crypto(Arc::new(QuicServerConfig::try_from(crypto)?));
        server.transport_config(transport());
        let endpoint = quinn::Endpoint::server(server, addr)?;
        Ok(Self {
            endpoint,
            identity,
            provider,
        })
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.endpoint.local_addr()
    }

    pub fn fingerprint(&self) -> Fingerprint {
        self.identity.fingerprint()
    }

    /// Connects to a paired device, which must present `expected`.
    pub async fn connect(
        &self,
        addr: SocketAddr,
        expected: Fingerprint,
    ) -> Result<quinn::Connection, NetError> {
        self.dial(addr, Some(expected), ALPN_SESSION).await
    }

    /// Connects to an unpaired device to pair with it. Returns the
    /// fingerprint it presented, to be confirmed by the pairing protocol.
    pub async fn connect_for_pairing(
        &self,
        addr: SocketAddr,
    ) -> Result<(quinn::Connection, Fingerprint), NetError> {
        let connection = self.dial(addr, None, ALPN_PAIRING).await?;
        let peer = peer_fingerprint(&connection)?;
        Ok((connection, peer))
    }

    async fn dial(
        &self,
        addr: SocketAddr,
        expected: Option<Fingerprint>,
        alpn: &[u8],
    ) -> Result<quinn::Connection, NetError> {
        let mut crypto = rustls::ClientConfig::builder_with_provider(self.provider.clone())
            .with_protocol_versions(&[&rustls::version::TLS13])?
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(ServerPin {
                expected,
                provider: self.provider.clone(),
            }))
            .with_client_auth_cert(vec![self.identity.cert()], self.identity.key())?;
        crypto.alpn_protocols = vec![alpn.to_vec()];
        let mut config = quinn::ClientConfig::new(Arc::new(QuicClientConfig::try_from(crypto)?));
        config.transport_config(transport());
        Ok(self
            .endpoint
            .connect_with(config, addr, SERVER_NAME)?
            .await?)
    }

    /// The next incoming connection attempt, or `None` once the endpoint is
    /// closed. Finish each one in its own task so a slow handshake does not
    /// hold up the others.
    pub async fn accept(&self) -> Option<IncomingPeer> {
        self.endpoint.accept().await.map(IncomingPeer)
    }

    pub fn close(&self) {
        self.endpoint.close(close::NORMAL.into(), b"shutting down");
    }

    /// Waits until every connection has finished closing.
    pub async fn wait_idle(&self) {
        self.endpoint.wait_idle().await;
    }
}

pub struct IncomingPeer(quinn::Incoming);

/// A connection whose peer is authenticated for its purpose.
#[derive(Debug)]
pub struct Accepted {
    pub connection: quinn::Connection,
    pub purpose: Purpose,
    pub peer: Fingerprint,
}

impl IncomingPeer {
    /// Completes the handshake. A session from a device that `is_trusted`
    /// rejects is closed before any stream is opened or read.
    pub async fn finish(
        self,
        is_trusted: impl Fn(&Fingerprint) -> bool,
    ) -> Result<Accepted, NetError> {
        let connection = self.0.await?;
        let purpose = match alpn(&connection).as_deref() {
            Some(ALPN_SESSION) => Purpose::Session,
            Some(ALPN_PAIRING) => Purpose::Pairing,
            _ => {
                connection.close(close::INCOMPATIBLE.into(), b"unknown protocol");
                return Err(NetError::Protocol("no supported ALPN protocol"));
            }
        };
        let peer = peer_fingerprint(&connection)?;
        if purpose == Purpose::Session && !is_trusted(&peer) {
            connection.close(close::UNTRUSTED.into(), b"device is not paired");
            return Err(NetError::Untrusted(peer));
        }
        Ok(Accepted {
            connection,
            purpose,
            peer,
        })
    }
}

fn transport() -> Arc<quinn::TransportConfig> {
    let mut transport = quinn::TransportConfig::default();
    transport.keep_alive_interval(Some(KEEP_ALIVE));
    transport.max_idle_timeout(Some(
        IDLE_TIMEOUT
            .try_into()
            .expect("idle timeout fits in a QUIC varint"),
    ));
    Arc::new(transport)
}

fn alpn(connection: &quinn::Connection) -> Option<Vec<u8>> {
    connection
        .handshake_data()?
        .downcast::<HandshakeData>()
        .ok()?
        .protocol
}

/// The fingerprint of the certificate the peer presented.
pub fn peer_fingerprint(connection: &quinn::Connection) -> Result<Fingerprint, NetError> {
    connection
        .peer_identity()
        .and_then(|id| id.downcast::<Vec<CertificateDer<'static>>>().ok())
        .and_then(|certs| certs.first().map(|c| Fingerprint::of_certificate(c)))
        .ok_or(NetError::Protocol("peer presented no certificate"))
}
