//! Loopback tests of the endpoint and session handshake.

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;

use iw_proto::peer::v1::{
    Control, Datagram, Heartbeat, Hello, Input, Key, Os, PointerMotion, control, datagram, input,
};

use super::*;

fn endpoint() -> PeerEndpoint {
    PeerEndpoint::bind(
        "127.0.0.1:0".parse().unwrap(),
        Arc::new(Identity::generate().unwrap()),
    )
    .unwrap()
}

async fn within<T>(f: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), f)
        .await
        .expect("timed out")
}

/// Accepts one session on `server`, trusting only `trusted`.
fn serve(
    server: PeerEndpoint,
    trusted: Fingerprint,
    ours: Hello,
) -> tokio::task::JoinHandle<(PeerEndpoint, Result<Session, NetError>)> {
    tokio::spawn(async move {
        let incoming = server.accept().await.expect("endpoint open");
        let result = match incoming.finish(|fp| *fp == trusted).await {
            Ok(accepted) => {
                assert_eq!(accepted.purpose, Purpose::Session);
                Session::accept(accepted.connection, accepted.peer, ours).await
            }
            Err(e) => Err(e),
        };
        (server, result)
    })
}

#[tokio::test]
async fn paired_devices_establish_a_session() {
    let (mac, win) = (endpoint(), endpoint());
    let (mac_fp, win_fp, win_addr) = (
        mac.fingerprint(),
        win.fingerprint(),
        win.local_addr().unwrap(),
    );
    let server = serve(win, mac_fp, hello("win", "Windows PC", Os::Windows));

    let connection = within(mac.connect(win_addr, win_fp)).await.unwrap();
    let client = within(Session::open(
        connection,
        win_fp,
        hello("mac", "MacBook Pro", Os::Macos),
    ))
    .await
    .unwrap();
    let (_win, server) = within(server).await.unwrap();
    let server = server.unwrap();

    assert_eq!(client.info.hello.device_id, "win");
    assert_eq!(client.info.peer, win_fp);
    assert_eq!(server.info.hello.device_id, "mac");
    assert_eq!(server.info.peer, mac_fp);
    assert_eq!(client.info.version, iw_proto::PEER_PROTOCOL_MAX);
    assert_eq!(server.info.version, iw_proto::PEER_PROTOCOL_MAX);
}

#[tokio::test]
async fn refuses_a_server_with_an_unexpected_certificate() {
    let (mac, win) = (endpoint(), endpoint());
    let win_addr = win.local_addr().unwrap();
    let server = serve(
        win,
        mac.fingerprint(),
        hello("win", "Windows PC", Os::Windows),
    );

    let impostor = Identity::generate().unwrap().fingerprint();
    assert!(within(mac.connect(win_addr, impostor)).await.is_err());
    let (_win, result) = within(server).await.unwrap();
    assert!(result.is_err());
}

#[tokio::test]
async fn closes_an_unpaired_client_before_reading_anything() {
    let (mac, win) = (endpoint(), endpoint());
    let (win_fp, win_addr) = (win.fingerprint(), win.local_addr().unwrap());
    let someone_else = Identity::generate().unwrap().fingerprint();
    let server = serve(win, someone_else, hello("win", "Windows PC", Os::Windows));

    let connection = within(mac.connect(win_addr, win_fp)).await.unwrap();
    let opened = within(Session::open(
        connection.clone(),
        win_fp,
        hello("mac", "MacBook Pro", Os::Macos),
    ))
    .await;
    assert!(opened.is_err());
    match connection.close_reason() {
        Some(quinn::ConnectionError::ApplicationClosed(c)) => {
            assert_eq!(c.error_code, close::UNTRUSTED.into());
        }
        other => panic!("unexpected close reason: {other:?}"),
    }
    let (_win, result) = within(server).await.unwrap();
    assert!(matches!(result, Err(NetError::Untrusted(fp)) if fp == mac.fingerprint()));
}

#[tokio::test]
async fn incompatible_versions_fail_on_both_sides() {
    let (mac, win) = (endpoint(), endpoint());
    let (mac_fp, win_fp, win_addr) = (
        mac.fingerprint(),
        win.fingerprint(),
        win.local_addr().unwrap(),
    );
    let server = serve(win, mac_fp, hello("win", "Windows PC", Os::Windows));

    let future = Hello {
        protocol_min: 99,
        protocol_max: 99,
        ..hello("mac", "MacBook Pro", Os::Macos)
    };
    let connection = within(mac.connect(win_addr, win_fp)).await.unwrap();
    let client = within(Session::open(connection, win_fp, future)).await;
    assert!(matches!(
        client,
        Err(NetError::Incompatible {
            min: iw_proto::PEER_PROTOCOL_MIN,
            max: iw_proto::PEER_PROTOCOL_MAX
        })
    ));
    let (_win, server) = within(server).await.unwrap();
    assert!(matches!(
        server,
        Err(NetError::Incompatible { min: 99, max: 99 })
    ));
}

#[tokio::test]
async fn carries_control_input_and_datagrams() {
    let (mac, win) = (endpoint(), endpoint());
    let (mac_fp, win_fp, win_addr) = (
        mac.fingerprint(),
        win.fingerprint(),
        win.local_addr().unwrap(),
    );
    let server = serve(win, mac_fp, hello("win", "Windows PC", Os::Windows));
    let connection = within(mac.connect(win_addr, win_fp)).await.unwrap();
    let mut client = within(Session::open(
        connection,
        win_fp,
        hello("mac", "MacBook Pro", Os::Macos),
    ))
    .await
    .unwrap();
    let (_win, server) = within(server).await.unwrap();
    let mut server = server.unwrap();

    let key = Input {
        msg: Some(input::Msg::Key(Key {
            usage: 0x04,
            pressed: true,
        })),
    };
    let mut input_tx = within(client.info.open_input()).await.unwrap();
    input_tx.send(&key).await.unwrap();
    let mut input_rx = within(server.info.accept_input()).await.unwrap();
    assert_eq!(within(input_rx.recv::<Input>()).await.unwrap(), Some(key));

    let motion = Datagram {
        msg: Some(datagram::Msg::Motion(PointerMotion {
            seq: 1,
            x: 10.0,
            y: 20.0,
        })),
    };
    client.info.send_datagram(&motion).unwrap();
    assert_eq!(within(server.info.recv_datagram()).await.unwrap(), motion);

    let heartbeat = Control {
        msg: Some(control::Msg::Heartbeat(Heartbeat { seq: 7 })),
    };
    server.control_tx.send(&heartbeat).await.unwrap();
    assert_eq!(
        within(client.control_rx.recv::<Control>()).await.unwrap(),
        Some(heartbeat)
    );
}

mod pairing {
    use iw_proto::peer::v1::PairResult;
    use tokio::time::Instant;

    use super::*;
    use crate::pairing::MAX_ATTEMPTS;

    type Responding = tokio::task::JoinHandle<Result<Paired, PairingError>>;

    /// Accepts one pairing connection on `win` and lets the initiator try
    /// `code` until `deadline`.
    fn respond(win: PeerEndpoint, code: &str, deadline: Instant) -> Responding {
        let code: PairingCode = code.parse().unwrap();
        tokio::spawn(async move {
            let accepted = win
                .accept()
                .await
                .expect("endpoint open")
                // Pairing never consults the trust store.
                .finish(|_| false)
                .await
                .unwrap();
            assert_eq!(accepted.purpose, Purpose::Pairing);
            let mut responder = PairingResponder::accept(
                accepted,
                win.fingerprint(),
                hello("win", "Windows PC", Os::Windows),
            )
            .await?;
            assert_eq!(responder.peer_hello().device_name, "MacBook Pro");
            responder.run(&code, deadline).await
        })
    }

    async fn connect(
        mac: &PeerEndpoint,
        win: std::net::SocketAddr,
    ) -> Result<PairingInitiator, PairingError> {
        within(PairingInitiator::connect(
            mac,
            win,
            hello("mac", "MacBook Pro", Os::Macos),
        ))
        .await
    }

    fn later() -> Instant {
        Instant::now() + Duration::from_secs(30)
    }

    #[tokio::test]
    async fn the_right_code_pairs_both_sides() {
        let (mac, win) = (endpoint(), endpoint());
        let (mac_fp, win_fp) = (mac.fingerprint(), win.fingerprint());
        let win_addr = win.local_addr().unwrap();
        let responding = respond(win, "382914", later());

        let mut initiator = connect(&mac, win_addr).await.unwrap();
        assert_eq!(initiator.peer_hello().device_id, "win");
        assert_eq!(initiator.peer_fingerprint(), win_fp);
        let paired = within(initiator.try_code(&"382 914".parse().unwrap()))
            .await
            .unwrap();
        assert_eq!(paired.fingerprint, win_fp);
        assert_eq!(paired.hello.device_id, "win");
        assert_eq!(paired.address, win_addr);

        let theirs = within(responding).await.unwrap().unwrap();
        assert_eq!(theirs.fingerprint, mac_fp);
        assert_eq!(theirs.hello.device_id, "mac");
        assert_eq!(theirs.address, mac.local_addr().unwrap());
    }

    #[tokio::test]
    async fn a_wrong_code_can_be_corrected() {
        let (mac, win) = (endpoint(), endpoint());
        let win_addr = win.local_addr().unwrap();
        let responding = respond(win, "111111", later());
        let mut initiator = connect(&mac, win_addr).await.unwrap();

        let wrong = within(initiator.try_code(&"222222".parse().unwrap())).await;
        assert!(matches!(
            wrong,
            Err(PairingError::WrongCode { attempts_left }) if attempts_left == MAX_ATTEMPTS - 1
        ));
        within(initiator.try_code(&"111111".parse().unwrap()))
            .await
            .unwrap();
        within(responding).await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn too_many_wrong_codes_end_the_pairing() {
        let (mac, win) = (endpoint(), endpoint());
        let win_addr = win.local_addr().unwrap();
        let responding = respond(win, "111111", later());
        let mut initiator = connect(&mac, win_addr).await.unwrap();

        let wrong: PairingCode = "999999".parse().unwrap();
        for left in (1..MAX_ATTEMPTS).rev() {
            assert!(matches!(
                within(initiator.try_code(&wrong)).await,
                Err(PairingError::WrongCode { attempts_left }) if attempts_left == left
            ));
        }
        assert!(matches!(
            within(initiator.try_code(&wrong)).await,
            Err(PairingError::Ended(PairResult::TooManyAttempts))
        ));
        assert!(matches!(
            within(responding).await.unwrap(),
            Err(PairingError::Ended(PairResult::TooManyAttempts))
        ));
    }

    #[tokio::test]
    async fn the_initiator_learns_when_the_code_expires() {
        let (mac, win) = (endpoint(), endpoint());
        let win_addr = win.local_addr().unwrap();
        let soon = Instant::now() + Duration::from_millis(300);
        let responding = respond(win, "111111", soon);
        let mut initiator = connect(&mac, win_addr).await.unwrap();

        assert!(matches!(
            within(initiator.ended()).await,
            PairingError::Ended(PairResult::Expired)
        ));
        assert!(matches!(
            within(responding).await.unwrap(),
            Err(PairingError::Ended(PairResult::Expired))
        ));
    }

    #[tokio::test]
    async fn cancelling_stops_the_responder() {
        let (mac, win) = (endpoint(), endpoint());
        let win_addr = win.local_addr().unwrap();
        let responding = respond(win, "111111", later());
        let initiator = connect(&mac, win_addr).await.unwrap();

        within(initiator.cancel()).await;
        assert!(matches!(
            within(responding).await.unwrap(),
            Err(PairingError::Ended(PairResult::Cancelled))
        ));
    }

    #[tokio::test]
    async fn a_refusal_reaches_the_initiator() {
        let (mac, win) = (endpoint(), endpoint());
        let win_addr = win.local_addr().unwrap();
        let refusing = tokio::spawn(async move {
            let accepted = win.accept().await.unwrap().finish(|_| false).await.unwrap();
            let responder = PairingResponder::accept(
                accepted,
                win.fingerprint(),
                hello("win", "Windows PC", Os::Windows),
            )
            .await
            .unwrap();
            responder.refuse(PairResult::Busy).await;
        });
        assert!(matches!(
            connect(&mac, win_addr).await,
            Err(PairingError::Ended(PairResult::Busy))
        ));
        within(refusing).await.unwrap();
    }

    #[tokio::test]
    async fn the_responder_counts_wrong_guesses_however_it_ends() {
        let (mac, win) = (endpoint(), endpoint());
        let win_addr = win.local_addr().unwrap();
        let responding = tokio::spawn(async move {
            let accepted = win.accept().await.unwrap().finish(|_| false).await.unwrap();
            let mut responder = PairingResponder::accept(
                accepted,
                win.fingerprint(),
                hello("win", "Windows PC", Os::Windows),
            )
            .await
            .unwrap();
            let result = responder.run(&"111111".parse().unwrap(), later()).await;
            (result, responder.wrong_attempts())
        });
        let mut initiator = connect(&mac, win_addr).await.unwrap();
        assert!(matches!(
            within(initiator.try_code(&"222222".parse().unwrap())).await,
            Err(PairingError::WrongCode { .. })
        ));
        within(initiator.cancel()).await;
        let (result, wrong) = within(responding).await.unwrap();
        assert!(matches!(
            result,
            Err(PairingError::Ended(PairResult::Cancelled))
        ));
        assert_eq!(wrong, 1);
    }
}
