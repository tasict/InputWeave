//! Pairing: two devices that have never met prove that one user can see
//! both of them, and learn each other's certificate fingerprint.
//!
//! The responder (the device being added, which may have no keyboard or
//! mouse attached) shows a six-digit [`PairingCode`] and the user types it on
//! the initiator. Over a pairing connection (ALPN `inputweave-pair/1`) both
//! sides run SPAKE2 with the code as the password, then confirm the shared
//! key with HMACs. Each side's SPAKE2 identity is its certificate
//! fingerprint and device id as the two sides saw them over TLS, so a man in
//! the middle, who has to terminate TLS separately with each side, ends up
//! with mismatched keys and fails confirmation. Every attempt costs an
//! attacker one online guess; the responder allows [`MAX_ATTEMPTS`] per code,
//! and a code expires after [`CODE_LIFETIME`].
//!
//! [`PairingInitiator`] and [`PairingResponder`] run the exchange over a
//! connection. Which pairings to accept at all (one at a time, policy, id
//! conflicts) is the caller's decision.

use std::fmt;
use std::net::SocketAddr;
use std::str::FromStr;
use std::time::Duration;

use hmac::{Hmac, KeyInit, Mac};
use iw_proto::frame::FrameError;
use iw_proto::peer::v1::{
    Hello, PairConfirm, PairOutcome, PairPake, PairResult, Pairing, pairing::Msg,
};
use sha2::Sha256;
use spake2::{Ed25519Group, Identity as SpakeIdentity, Password, Spake2};
use tokio::time::Instant;

use crate::endpoint::{Accepted, Purpose, close};
use crate::session::{FramedRecv, FramedSend, negotiate};
use crate::{Fingerprint, NetError, PeerEndpoint, tls};

pub const CODE_DIGITS: usize = 6;
/// Codes the initiator may try before the responder gives up.
pub const MAX_ATTEMPTS: u32 = 3;
/// How long a code stays valid after it is first shown.
pub const CODE_LIFETIME: Duration = Duration::from_secs(120);

/// Pairing messages are small; a larger frame can only be hostile.
const FRAME_LIMIT: usize = 1024;
/// How long to wait for the other side's next message when no user is
/// involved in producing it.
const STEP_TIMEOUT: Duration = Duration::from_secs(10);
/// How long the side that sends the final message waits for the other side
/// to close, so the message is not discarded along with the connection.
const CLOSE_GRACE: Duration = Duration::from_secs(2);

const MAX_ID_LEN: usize = 64;
const MAX_NAME_CHARS: usize = 64;

/// Whether `id` can serve as a device id: 1 to 64 ASCII letters, digits,
/// `.`, `_` or `-`. Ids appear in mDNS records and are compared to decide
/// which device dials, so they are kept plain.
pub fn is_valid_device_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= MAX_ID_LEN
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Whether `name` can be shown as a device name: 1 to 64 characters, not
/// all whitespace, without control characters.
pub fn is_valid_device_name(name: &str) -> bool {
    !name.trim().is_empty()
        && name.chars().count() <= MAX_NAME_CHARS
        && !name.chars().any(char::is_control)
}

/// The six digits the responder shows and the initiator's user types.
#[derive(Clone, PartialEq, Eq)]
pub struct PairingCode([u8; CODE_DIGITS]);

impl PairingCode {
    /// A uniformly random code from the operating system's CSPRNG.
    pub fn generate() -> Self {
        // The largest multiple of 10^6 that fits in a u32; values at or
        // above it are redrawn so that every code is equally likely.
        const BOUND: u32 = 4_294_000_000;
        let random = tls::provider().secure_random;
        loop {
            let mut bytes = [0u8; 4];
            random
                .fill(&mut bytes)
                .expect("the OS random number generator is available");
            let n = u32::from_be_bytes(bytes);
            if n < BOUND {
                return Self::from_number(n % 1_000_000);
            }
        }
    }

    fn from_number(mut n: u32) -> Self {
        let mut digits = [b'0'; CODE_DIGITS];
        for d in digits.iter_mut().rev() {
            *d = b'0' + (n % 10) as u8;
            n /= 10;
        }
        Self(digits)
    }

    /// The six digits without separators.
    pub fn as_str(&self) -> &str {
        std::str::from_utf8(&self.0).expect("codes are ASCII digits")
    }
}

/// Shown in two groups of three, as on screen: `382 914`.
impl fmt::Display for PairingCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = self.as_str();
        write!(f, "{} {}", &s[..3], &s[3..])
    }
}

/// Redacted, so codes stay out of logs.
impl fmt::Debug for PairingCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PairingCode(…)")
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("a pairing code is six digits")]
pub struct ParsePairingCodeError;

/// Accepts the digits with or without the spaces or dashes people type
/// between groups.
impl FromStr for PairingCode {
    type Err = ParsePairingCodeError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        let digits: Vec<u8> = s.bytes().filter(|b| !matches!(b, b' ' | b'-')).collect();
        let digits: [u8; CODE_DIGITS] = digits.try_into().map_err(|_| ParsePairingCodeError)?;
        if !digits.iter().all(u8::is_ascii_digit) {
            return Err(ParsePairingCodeError);
        }
        Ok(Self(digits))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PairingError {
    /// The initiator may try another code.
    #[error("the pairing code is not right; {attempts_left} attempts left")]
    WrongCode { attempts_left: u32 },
    /// One side ended the exchange, for the given reason.
    #[error("pairing ended: {}", describe(*.0))]
    Ended(PairResult),
    #[error("timed out waiting for the other device")]
    Timeout,
    #[error(transparent)]
    Net(#[from] NetError),
}

impl From<FrameError> for PairingError {
    fn from(e: FrameError) -> Self {
        Self::Net(e.into())
    }
}

impl From<quinn::ConnectionError> for PairingError {
    fn from(e: quinn::ConnectionError) -> Self {
        Self::Net(e.into())
    }
}

fn protocol(what: &'static str) -> PairingError {
    PairingError::Net(NetError::Protocol(what))
}

fn describe(result: PairResult) -> &'static str {
    match result {
        PairResult::Unspecified => "for an unknown reason",
        PairResult::Paired => "paired",
        PairResult::WrongCode => "wrong code",
        PairResult::TooManyAttempts => "too many wrong codes",
        PairResult::Expired => "the code expired",
        PairResult::Rejected => "the other device declined",
        PairResult::NotAllowed => "the other device does not accept new pairings",
        PairResult::Busy => "the other device is busy pairing",
        PairResult::IdConflict => "both devices use the same device id",
        PairResult::Incompatible => "incompatible versions",
        PairResult::Cancelled => "cancelled",
    }
}

/// The other device, once pairing has succeeded.
#[derive(Clone, Debug)]
pub struct Paired {
    pub fingerprint: Fingerprint,
    pub hello: Hello,
    /// Where the other device's endpoint is. Endpoints dial and accept on
    /// the same UDP socket, so this is also where it accepts sessions.
    pub address: SocketAddr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Role {
    Initiator,
    Responder,
}

/// One device as the other saw it over TLS.
#[derive(Clone, Copy, Debug)]
struct Party<'a> {
    fingerprint: &'a Fingerprint,
    device_id: &'a str,
}

impl Party<'_> {
    fn spake_identity(&self) -> SpakeIdentity {
        // Fingerprints have a fixed length, so the concatenation is
        // unambiguous.
        let mut bytes = self.fingerprint.as_bytes().to_vec();
        bytes.extend_from_slice(self.device_id.as_bytes());
        SpakeIdentity::new(&bytes)
    }
}

/// One SPAKE2 run with one code.
struct Exchange {
    role: Role,
    spake: Spake2<Ed25519Group>,
}

impl Exchange {
    /// Starts an attempt. The returned message goes to the other side.
    fn start(
        role: Role,
        code: &PairingCode,
        initiator: Party<'_>,
        responder: Party<'_>,
    ) -> (Self, Vec<u8>) {
        let password = Password::new(code.as_str());
        let (a, b) = (initiator.spake_identity(), responder.spake_identity());
        let (spake, msg) = match role {
            Role::Initiator => Spake2::<Ed25519Group>::start_a(&password, &a, &b),
            Role::Responder => Spake2::<Ed25519Group>::start_b(&password, &a, &b),
        };
        (Self { role, spake }, msg)
    }

    /// Combines the other side's message into the key both sides confirm.
    fn finish(self, theirs: &[u8]) -> Result<Confirmation, PairingError> {
        let key = self
            .spake
            .finish(theirs)
            .map_err(|_| protocol("malformed SPAKE2 message"))?;
        Ok(Confirmation {
            role: self.role,
            key,
        })
    }
}

/// The SPAKE2 key, used only to prove knowledge of the code. Identical on
/// both sides exactly when they used the same code and saw the same
/// fingerprints and device ids.
struct Confirmation {
    role: Role,
    key: Vec<u8>,
}

impl Confirmation {
    fn ours(&self) -> Vec<u8> {
        confirmation_mac(&self.key, self.role)
            .finalize()
            .into_bytes()
            .to_vec()
    }

    /// Constant-time check of the other side's MAC.
    fn verify(&self, theirs: &[u8]) -> bool {
        let other = match self.role {
            Role::Initiator => Role::Responder,
            Role::Responder => Role::Initiator,
        };
        confirmation_mac(&self.key, other)
            .verify_slice(theirs)
            .is_ok()
    }
}

fn confirmation_mac(key: &[u8], role: Role) -> Hmac<Sha256> {
    let mut mac =
        <Hmac<Sha256> as KeyInit>::new_from_slice(key).expect("HMAC takes keys of any length");
    mac.update(match role {
        Role::Initiator => b"inputweave-pair/1 initiator",
        Role::Responder => b"inputweave-pair/1 responder",
    });
    mac
}

fn outcome(result: PairResult) -> Msg {
    Msg::Outcome(PairOutcome {
        result: result.into(),
        attempts_left: 0,
    })
}

/// The pairing stream of one connection.
struct Channel {
    connection: quinn::Connection,
    tx: FramedSend,
    rx: FramedRecv,
}

impl Channel {
    fn new(
        connection: quinn::Connection,
        send: quinn::SendStream,
        recv: quinn::RecvStream,
    ) -> Self {
        Self {
            connection,
            tx: FramedSend::new(send),
            rx: FramedRecv::with_limit(recv, FRAME_LIMIT),
        }
    }

    async fn send(&mut self, msg: Msg) -> Result<(), PairingError> {
        self.tx.send(&Pairing { msg: Some(msg) }).await?;
        Ok(())
    }

    /// The other side's next message, if it arrives by `deadline`.
    /// Cancel-safe.
    async fn recv_until(&mut self, deadline: Instant) -> Result<Msg, PairingError> {
        match tokio::time::timeout_at(deadline, self.rx.recv::<Pairing>()).await {
            Err(_) => Err(PairingError::Timeout),
            Ok(Ok(Some(Pairing { msg: Some(msg) }))) => Ok(msg),
            Ok(Ok(Some(Pairing { msg: None }))) => Err(protocol("empty pairing message")),
            Ok(Ok(None)) => Err(protocol("the pairing stream ended early")),
            Ok(Err(e)) => Err(e.into()),
        }
    }

    async fn recv(&mut self) -> Result<Msg, PairingError> {
        self.recv_until(Instant::now() + STEP_TIMEOUT).await
    }

    fn close(&self, code: u32, reason: &[u8]) {
        self.connection.close(code.into(), reason);
    }

    /// Sends the exchange's final message and waits briefly for the other
    /// side, which closes on receiving it, before closing.
    async fn finish(&mut self, msg: Msg) {
        if self.send(msg).await.is_ok() {
            self.tx.finish();
            let _ = tokio::time::timeout(CLOSE_GRACE, self.connection.closed()).await;
        }
        self.close(close::NORMAL, b"pairing finished");
    }

    /// Ends the exchange after a message that does not fit: an outcome
    /// means the other side gave up; anything else breaks the protocol.
    fn unexpected(&self, msg: Msg) -> PairingError {
        match msg {
            Msg::Outcome(o) => {
                self.close(close::NORMAL, b"pairing ended");
                PairingError::Ended(o.result())
            }
            _ => {
                self.close(close::INCOMPATIBLE, b"unexpected pairing message");
                protocol("unexpected pairing message")
            }
        }
    }
}

fn check_hello(hello: &Hello) -> Result<(), PairingError> {
    if !is_valid_device_id(&hello.device_id) {
        return Err(protocol("invalid device id"));
    }
    if !is_valid_device_name(&hello.device_name) {
        return Err(protocol("invalid device name"));
    }
    Ok(())
}

/// The side whose user types the code.
pub struct PairingInitiator {
    channel: Channel,
    own: Fingerprint,
    ours: Hello,
    peer: Fingerprint,
    theirs: Hello,
}

impl PairingInitiator {
    /// Connects to the device at `addr` to pair with it. Once this returns,
    /// that device is showing a code.
    pub async fn connect(
        endpoint: &PeerEndpoint,
        addr: SocketAddr,
        ours: Hello,
    ) -> Result<Self, PairingError> {
        let (connection, peer) = endpoint.connect_for_pairing(addr).await?;
        let (send, recv) = connection.open_bi().await?;
        let mut channel = Channel::new(connection, send, recv);
        channel.send(Msg::Hello(ours.clone())).await?;
        let theirs = match channel.recv().await {
            Ok(Msg::Hello(theirs)) => theirs,
            Ok(other) => return Err(channel.unexpected(other)),
            Err(e) => {
                channel.close(close::NORMAL, b"pairing failed");
                return Err(e);
            }
        };
        if let Err(e) = check_hello(&theirs) {
            channel.close(close::INCOMPATIBLE, b"invalid hello");
            return Err(e);
        }
        if negotiate(&ours, &theirs).is_none() {
            channel.finish(outcome(PairResult::Incompatible)).await;
            return Err(PairingError::Ended(PairResult::Incompatible));
        }
        Ok(Self {
            channel,
            own: endpoint.fingerprint(),
            ours,
            peer,
            theirs,
        })
    }

    /// The responder's Hello.
    pub fn peer_hello(&self) -> &Hello {
        &self.theirs
    }

    pub fn peer_fingerprint(&self) -> Fingerprint {
        self.peer
    }

    /// Tries a code. After [`PairingError::WrongCode`] another code may be
    /// tried; any other error ends the pairing.
    pub async fn try_code(&mut self, code: &PairingCode) -> Result<Paired, PairingError> {
        let initiator = Party {
            fingerprint: &self.own,
            device_id: &self.ours.device_id,
        };
        let responder = Party {
            fingerprint: &self.peer,
            device_id: &self.theirs.device_id,
        };
        let (exchange, message) = Exchange::start(Role::Initiator, code, initiator, responder);
        self.channel.send(Msg::Pake(PairPake { message })).await?;
        let theirs = match self.channel.recv().await? {
            Msg::Pake(p) => p.message,
            other => return Err(self.channel.unexpected(other)),
        };
        let confirmation = exchange.finish(&theirs).inspect_err(|_| {
            self.channel
                .close(close::INCOMPATIBLE, b"malformed SPAKE2 message");
        })?;
        let mac = confirmation.ours();
        self.channel.send(Msg::Confirm(PairConfirm { mac })).await?;
        match self.channel.recv().await? {
            Msg::Confirm(c) if confirmation.verify(&c.mac) => {
                self.channel.finish(outcome(PairResult::Paired)).await;
                Ok(Paired {
                    fingerprint: self.peer,
                    hello: self.theirs.clone(),
                    address: self.channel.connection.remote_address(),
                })
            }
            Msg::Confirm(_) => {
                self.channel.close(close::UNTRUSTED, b"confirmation failed");
                Err(protocol("the other device could not confirm the code"))
            }
            Msg::Outcome(o) if o.result() == PairResult::WrongCode => {
                Err(PairingError::WrongCode {
                    attempts_left: o.attempts_left,
                })
            }
            other => Err(self.channel.unexpected(other)),
        }
    }

    /// Completes when the responder ends the pairing on its own, for
    /// example because its code expired or its user declined. Await it while
    /// waiting for the user to type a code. Cancel-safe.
    pub async fn ended(&mut self) -> PairingError {
        match self.channel.rx.recv::<Pairing>().await {
            Ok(Some(Pairing { msg: Some(msg) })) => self.channel.unexpected(msg),
            Ok(_) => protocol("the pairing stream ended early"),
            Err(e) => e.into(),
        }
    }

    /// Gives up, so the responder stops showing its code.
    pub async fn cancel(mut self) {
        self.channel.finish(outcome(PairResult::Cancelled)).await;
    }
}

/// The side that shows the code.
pub struct PairingResponder {
    channel: Channel,
    own: Fingerprint,
    ours: Hello,
    peer: Fingerprint,
    theirs: Hello,
    /// Codes the initiator got wrong so far.
    wrong: u32,
}

impl PairingResponder {
    /// Reads the initiator's Hello from an accepted pairing connection. The
    /// caller then decides between [`run`](Self::run) and
    /// [`refuse`](Self::refuse).
    pub async fn accept(
        accepted: Accepted,
        own: Fingerprint,
        ours: Hello,
    ) -> Result<Self, PairingError> {
        if accepted.purpose != Purpose::Pairing {
            return Err(protocol("not a pairing connection"));
        }
        let connection = accepted.connection;
        let (send, recv) = match tokio::time::timeout(STEP_TIMEOUT, connection.accept_bi()).await {
            Ok(streams) => streams?,
            Err(_) => {
                connection.close(close::TIMEOUT.into(), b"no pairing request");
                return Err(PairingError::Timeout);
            }
        };
        let mut channel = Channel::new(connection, send, recv);
        let theirs = match channel.recv().await {
            Ok(Msg::Hello(theirs)) => theirs,
            Ok(other) => return Err(channel.unexpected(other)),
            Err(e) => {
                channel.close(close::NORMAL, b"pairing failed");
                return Err(e);
            }
        };
        if let Err(e) = check_hello(&theirs) {
            channel.close(close::INCOMPATIBLE, b"invalid hello");
            return Err(e);
        }
        let mut responder = Self {
            channel,
            own,
            ours,
            peer: accepted.peer,
            theirs,
            wrong: 0,
        };
        if negotiate(&responder.ours, &responder.theirs).is_none() {
            responder
                .channel
                .finish(outcome(PairResult::Incompatible))
                .await;
            return Err(PairingError::Ended(PairResult::Incompatible));
        }
        Ok(responder)
    }

    /// The initiator's Hello.
    pub fn peer_hello(&self) -> &Hello {
        &self.theirs
    }

    pub fn peer_fingerprint(&self) -> Fingerprint {
        self.peer
    }

    /// How many codes the initiator got wrong, whatever ended the pairing.
    /// Each one was a guess, so a caller limiting how fast codes can be
    /// guessed counts these rather than how the pairing ended.
    pub fn wrong_attempts(&self) -> u32 {
        self.wrong
    }

    /// Declines to pair, telling the initiator why.
    pub async fn refuse(mut self, result: PairResult) {
        self.channel.finish(outcome(result)).await;
    }

    /// Introduces this device and lets the initiator try `code` until it
    /// succeeds, runs out of attempts, or `deadline` passes. To stop early
    /// (the user declined), drop the future and call
    /// [`refuse`](Self::refuse).
    pub async fn run(
        &mut self,
        code: &PairingCode,
        deadline: Instant,
    ) -> Result<Paired, PairingError> {
        self.channel.send(Msg::Hello(self.ours.clone())).await?;
        loop {
            let theirs = match self.channel.recv_until(deadline).await {
                Ok(Msg::Pake(p)) => p.message,
                Ok(other) => return Err(self.channel.unexpected(other)),
                Err(PairingError::Timeout) => {
                    self.channel.finish(outcome(PairResult::Expired)).await;
                    return Err(PairingError::Ended(PairResult::Expired));
                }
                Err(e) => return Err(e),
            };
            let initiator = Party {
                fingerprint: &self.peer,
                device_id: &self.theirs.device_id,
            };
            let responder = Party {
                fingerprint: &self.own,
                device_id: &self.ours.device_id,
            };
            let (exchange, message) = Exchange::start(Role::Responder, code, initiator, responder);
            let confirmation = exchange.finish(&theirs).inspect_err(|_| {
                self.channel
                    .close(close::INCOMPATIBLE, b"malformed SPAKE2 message");
            })?;
            self.channel.send(Msg::Pake(PairPake { message })).await?;
            let mac = match self.channel.recv().await? {
                Msg::Confirm(c) => c.mac,
                other => return Err(self.channel.unexpected(other)),
            };
            if !confirmation.verify(&mac) {
                self.wrong += 1;
                let attempts_left = MAX_ATTEMPTS.saturating_sub(self.wrong);
                if attempts_left == 0 {
                    self.channel
                        .finish(outcome(PairResult::TooManyAttempts))
                        .await;
                    return Err(PairingError::Ended(PairResult::TooManyAttempts));
                }
                let wrong = PairOutcome {
                    result: PairResult::WrongCode.into(),
                    attempts_left,
                };
                self.channel.send(Msg::Outcome(wrong)).await?;
                continue;
            }
            let mac = confirmation.ours();
            self.channel.send(Msg::Confirm(PairConfirm { mac })).await?;
            return match self.channel.recv().await? {
                Msg::Outcome(o) if o.result() == PairResult::Paired => {
                    self.channel.close(close::NORMAL, b"paired");
                    Ok(Paired {
                        fingerprint: self.peer,
                        hello: self.theirs.clone(),
                        address: self.channel.connection.remote_address(),
                    })
                }
                other => Err(self.channel.unexpected(other)),
            };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fp(byte: u8) -> Fingerprint {
        Fingerprint::from_bytes([byte; 32])
    }

    fn code(s: &str) -> PairingCode {
        s.parse().unwrap()
    }

    /// Runs one exchange where each side may have seen different things.
    /// Returns whether each side accepts the other's confirmation.
    fn exchange(
        initiator_view: (Party<'_>, Party<'_>, &PairingCode),
        responder_view: (Party<'_>, Party<'_>, &PairingCode),
    ) -> (bool, bool) {
        let (i, i_msg) = Exchange::start(
            Role::Initiator,
            initiator_view.2,
            initiator_view.0,
            initiator_view.1,
        );
        let (r, r_msg) = Exchange::start(
            Role::Responder,
            responder_view.2,
            responder_view.0,
            responder_view.1,
        );
        let i = i.finish(&r_msg).unwrap();
        let r = r.finish(&i_msg).unwrap();
        (i.verify(&r.ours()), r.verify(&i.ours()))
    }

    fn party<'a>(fingerprint: &'a Fingerprint, device_id: &'a str) -> Party<'a> {
        Party {
            fingerprint,
            device_id,
        }
    }

    #[test]
    fn the_same_code_confirms_on_both_sides() {
        let (a, b) = (fp(1), fp(2));
        let c = code("382914");
        let view = (party(&a, "mac"), party(&b, "win"), &c);
        assert_eq!(exchange(view, view), (true, true));
    }

    #[test]
    fn a_different_code_fails_on_both_sides() {
        let (a, b) = (fp(1), fp(2));
        let (right, wrong) = (code("382914"), code("382915"));
        assert_eq!(
            exchange(
                (party(&a, "mac"), party(&b, "win"), &wrong),
                (party(&a, "mac"), party(&b, "win"), &right),
            ),
            (false, false)
        );
    }

    #[test]
    fn a_man_in_the_middle_fails_even_with_the_right_code() {
        // Each side saw the attacker's certificate in place of the other's.
        let (a, b, m) = (fp(1), fp(2), fp(3));
        let c = code("382914");
        assert_eq!(
            exchange(
                (party(&a, "mac"), party(&m, "win"), &c),
                (party(&m, "mac"), party(&b, "win"), &c),
            ),
            (false, false)
        );
        // The same holds for a device id swapped in transit.
        assert_eq!(
            exchange(
                (party(&a, "mac"), party(&b, "win"), &c),
                (party(&a, "evil"), party(&b, "win"), &c),
            ),
            (false, false)
        );
    }

    #[test]
    fn a_side_cannot_pass_off_its_own_confirmation() {
        let (a, b) = (fp(1), fp(2));
        let c = code("000000");
        let (i, _) = Exchange::start(Role::Initiator, &c, party(&a, "mac"), party(&b, "win"));
        let (_, r_msg) = Exchange::start(Role::Responder, &c, party(&a, "mac"), party(&b, "win"));
        let i = i.finish(&r_msg).unwrap();
        assert!(!i.verify(&i.ours()));
    }

    #[test]
    fn rejects_malformed_spake_messages() {
        let (a, b) = (fp(1), fp(2));
        let c = code("123456");
        let (i, _) = Exchange::start(Role::Initiator, &c, party(&a, "mac"), party(&b, "win"));
        assert!(i.finish(&[0x42; 5]).is_err());
    }

    #[test]
    fn codes_are_six_digits_and_parse_back() {
        for _ in 0..100 {
            let c = PairingCode::generate();
            assert_eq!(c.as_str().len(), CODE_DIGITS);
            assert!(c.as_str().bytes().all(|b| b.is_ascii_digit()));
            assert_eq!(c.to_string().parse::<PairingCode>(), Ok(c));
        }
        assert_eq!(PairingCode::from_number(42).as_str(), "000042");
        assert_eq!(code("382-914").to_string(), "382 914");
        assert_eq!(format!("{:?}", code("382914")), "PairingCode(…)");
        for bad in ["", "12345", "1234567", "12a456", "１２３４５６"] {
            assert_eq!(bad.parse::<PairingCode>(), Err(ParsePairingCodeError));
        }
    }

    #[test]
    fn device_ids_and_names_are_checked() {
        assert!(is_valid_device_id("mac"));
        assert!(is_valid_device_id("3f9a0c1d2e4b5a69"));
        assert!(is_valid_device_id("office-pc_2.lan"));
        assert!(!is_valid_device_id(""));
        assert!(!is_valid_device_id("has space"));
        assert!(!is_valid_device_id("ünï"));
        assert!(!is_valid_device_id(&"x".repeat(65)));

        assert!(is_valid_device_name("小明的 Mac mini"));
        assert!(!is_valid_device_name("   "));
        assert!(!is_valid_device_name("line\nbreak"));
        assert!(!is_valid_device_name(&"名".repeat(65)));
    }
}
