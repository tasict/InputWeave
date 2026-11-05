//! Pacing file data so it never holds up input (docs/architecture.md §9).
//!
//! Stream priorities keep file data from going out ahead of keys and
//! buttons, but once it is out it can still fill the queues on the way, in
//! a Wi-Fi access point or a switch, and input then waits behind it. That
//! shows as a longer round trip. [`Throttle`] watches the round trip the
//! session measures (QUIC's smoothed RTT, the one devices report as
//! `rtt_ms`), sampled every heartbeat:
//!
//! - While no file is being sent it learns the usual round trip, the
//!   baseline. A lower round trip, sending or not, lowers the baseline at
//!   once; a higher one while nothing is sent raises it by an eighth of the
//!   difference, as when the path became slower.
//! - While files are being sent, a round trip more than [`THRESHOLD`] above
//!   the baseline halves the rate they may go at: half of what was actually
//!   sent, or of the limit if that is lower, and never below
//!   [`Throttle::MIN_RATE`]. Halving again waits [`Throttle::HOLD`], for the
//!   queue to drain.
//! - Within half the threshold, and while nothing is sent, the rate grows
//!   by [`Throttle::GROWTH`] every sample, until it no longer limits.
//! - If the round trip stays over the threshold for [`Throttle::REBASE`]
//!   at the lowest rate, files are not what holds it up: the baseline moves
//!   to the lowest round trip seen meanwhile.
//!
//! [`TokenBucket`] holds file data to the rate, and [`chunk`] sizes the
//! pieces it is sent in. These three are pure: the caller passes the time,
//! the round trips and the bytes. [`Pacer`] puts them together for one
//! device.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A round trip this much above the baseline means file data is queueing
/// on the way to the device. Shown in diagnostics reports.
pub(crate) const THRESHOLD: Duration = Duration::from_millis(15);

/// Pieces of a file are at least this large, so pacing does not cost much
/// framing, and at most [`MAX_CHUNK`].
pub(crate) const MIN_CHUNK: usize = 64 * 1024;
/// The largest piece of a file, which also bounds the frames on transfer
/// streams. Sent at the lowest priority, its packets still go only when
/// input and control have none to send.
pub(crate) const MAX_CHUNK: usize = 256 * 1024;

/// How large a piece of a file to send at `rate` bytes per second (none:
/// unlimited): an eighth of a second's worth, so a limited transfer goes
/// out steadily rather than in bursts.
pub(crate) fn chunk(rate: Option<u64>) -> usize {
    match rate {
        None => MAX_CHUNK,
        Some(rate) => usize::try_from(rate / 8)
            .unwrap_or(MAX_CHUNK)
            .clamp(MIN_CHUNK, MAX_CHUNK),
    }
}

/// What a sample changed, for the log.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Change {
    /// Files may now go at most this many bytes per second, less than
    /// before.
    Slower(u64),
    /// More than before.
    Faster(u64),
    /// Files go as fast as the connection takes them.
    Unlimited,
    /// The baseline moved up to this.
    Rebased(Duration),
}

/// Decides how fast files may go to one device; see the module docs.
#[derive(Clone, Debug)]
pub(crate) struct Throttle {
    threshold: Duration,
    baseline: Option<Duration>,
    /// Bytes per second; none while not limited.
    rate: Option<u64>,
    /// When the last sample while sending was taken, and the bytes sent by
    /// then.
    last: Option<(Instant, u64)>,
    /// Bytes per second actually sent, smoothed.
    throughput: Option<f64>,
    /// When the rate was last halved.
    cut: Option<Instant>,
    /// Since when the round trip has stayed over the threshold at the
    /// lowest rate, and the lowest round trip since then.
    stuck: Option<(Instant, Duration)>,
}

impl Throttle {
    /// The rate is never halved below this, so files keep moving whatever
    /// else holds the connection up.
    pub(crate) const MIN_RATE: u64 = 1024 * 1024;
    /// A rate grown past this no longer limits anything.
    pub(crate) const MAX_RATE: u64 = 1024 * 1024 * 1024;
    /// The least time between two halvings.
    pub(crate) const HOLD: Duration = Duration::from_millis(500);
    /// How much the rate grows each sample when the round trip is fine.
    /// Going 3% faster than the link takes, for one 250 ms sample, queues
    /// at most 7.5 ms of data: half the threshold.
    pub(crate) const GROWTH: f64 = 1.03;
    /// How long the round trip may stay high at the lowest rate before the
    /// baseline follows it.
    pub(crate) const REBASE: Duration = Duration::from_secs(30);

    pub(crate) fn new(threshold: Duration) -> Self {
        Self {
            threshold,
            baseline: None,
            rate: None,
            last: None,
            throughput: None,
            cut: None,
            stuck: None,
        }
    }

    /// Bytes per second files may go at; none when not limited.
    pub(crate) fn rate(&self) -> Option<u64> {
        self.rate
    }

    pub(crate) fn baseline(&self) -> Option<Duration> {
        self.baseline
    }

    /// Forgets the baseline, as when a new connection may take another
    /// path. The rate stays.
    pub(crate) fn forget_baseline(&mut self) {
        self.baseline = None;
        self.stuck = None;
    }

    /// A round trip measured while no file was being sent, one heartbeat
    /// after the last sample.
    pub(crate) fn idle(&mut self, rtt: Duration) -> Option<Change> {
        self.last = None;
        self.throughput = None;
        self.stuck = None;
        self.baseline = Some(match self.baseline {
            Some(baseline) if rtt > baseline => baseline + (rtt - baseline) / 8,
            _ => rtt,
        });
        self.grow()
    }

    /// A round trip measured at `now` while files were being sent, `sent`
    /// being the bytes of file data sent by then, counted from any point.
    pub(crate) fn busy(&mut self, rtt: Duration, sent: u64, now: Instant) -> Option<Change> {
        if let Some((at, before)) = self.last {
            let elapsed = now.saturating_duration_since(at).as_secs_f64();
            if elapsed > 0.0 {
                let measured = sent.saturating_sub(before) as f64 / elapsed;
                self.throughput = Some(self.throughput.map_or(measured, |t| (t + measured) / 2.0));
            }
        }
        self.last = Some((now, sent));
        // Sending only ever adds to the round trip: a lower one is the
        // baseline, a higher one says nothing about it.
        let baseline = match self.baseline {
            Some(baseline) if baseline <= rtt => baseline,
            _ => {
                self.baseline = Some(rtt);
                rtt
            }
        };
        let queueing = rtt - baseline;
        if queueing > self.threshold {
            if self.rate == Some(Self::MIN_RATE) {
                return self.stuck(rtt, now);
            }
            return self.slow_down(now);
        }
        self.stuck = None;
        if queueing <= self.threshold / 2 {
            self.grow()
        } else {
            None
        }
    }

    fn slow_down(&mut self, now: Instant) -> Option<Change> {
        if self
            .cut
            .is_some_and(|at| now.saturating_duration_since(at) < Self::HOLD)
        {
            return None;
        }
        let current = match (self.rate, self.throughput) {
            (Some(rate), Some(sent)) => (rate as f64).min(sent),
            (Some(rate), None) => rate as f64,
            (None, Some(sent)) => sent,
            // Nothing to halve until the next sample measures what is sent.
            (None, None) => return None,
        };
        let rate = ((current / 2.0) as u64).max(Self::MIN_RATE);
        self.cut = Some(now);
        self.rate = Some(rate);
        Some(Change::Slower(rate))
    }

    fn stuck(&mut self, rtt: Duration, now: Instant) -> Option<Change> {
        let (since, lowest) = self.stuck.get_or_insert((now, rtt));
        *lowest = (*lowest).min(rtt);
        if now.saturating_duration_since(*since) < Self::REBASE {
            return None;
        }
        let lowest = *lowest;
        self.stuck = None;
        self.baseline = Some(lowest);
        Some(Change::Rebased(lowest))
    }

    fn grow(&mut self) -> Option<Change> {
        let rate = self.rate?;
        let grown = (rate as f64 * Self::GROWTH) as u64;
        if grown >= Self::MAX_RATE {
            self.rate = None;
            Some(Change::Unlimited)
        } else {
            self.rate = Some(grown);
            Some(Change::Faster(grown))
        }
    }
}

/// Holds data to a rate: each piece reserves its bytes, and is sent once
/// the time they take at the rate has passed.
#[derive(Clone, Debug, Default)]
pub(crate) struct TokenBucket {
    /// Bytes that may go at once; below zero, bytes reserved but not yet
    /// earned.
    tokens: f64,
    at: Option<Instant>,
}

impl TokenBucket {
    /// Bytes saved up while idle are capped at this much time's worth, so
    /// a pause does not turn into a burst.
    pub(crate) const BURST: Duration = Duration::from_millis(100);

    /// Takes `bytes` at `rate` bytes per second (none: unlimited). Returns
    /// how long to wait before sending them.
    pub(crate) fn take(&mut self, bytes: u64, rate: Option<u64>, now: Instant) -> Duration {
        let Some(rate) = rate.filter(|r| *r > 0) else {
            self.tokens = 0.0;
            self.at = Some(now);
            return Duration::ZERO;
        };
        let rate = rate as f64;
        if let Some(at) = self.at {
            let earned = rate * now.saturating_duration_since(at).as_secs_f64();
            self.tokens = (self.tokens + earned).min(rate * Self::BURST.as_secs_f64());
        }
        self.at = Some(now);
        self.tokens -= bytes as f64;
        if self.tokens >= 0.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64(-self.tokens / rate)
        }
    }
}

/// Samples kept for diagnostics of each kind, idle and sending: a minute's
/// worth at one per heartbeat.
const KEEP_SAMPLES: usize = 240;
/// The latest samples of either kind kept in order, five seconds' worth.
const KEEP_RECENT: usize = 20;

fn keep<T>(samples: &mut VecDeque<T>, sample: T, max: usize) {
    if samples.len() == max {
        samples.pop_front();
    }
    samples.push_back(sample);
}

/// One device's pacing, as diagnostics reports show it.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct PacingStatus {
    pub(crate) device: String,
    pub(crate) baseline: Option<Duration>,
    /// The last round trip sampled.
    pub(crate) rtt: Option<Duration>,
    /// Bytes per second files may go at; none when not limited.
    pub(crate) rate: Option<u64>,
    /// Files sending data now.
    pub(crate) sending: usize,
    /// How often the rate was halved.
    pub(crate) slowdowns: u64,
    /// Recent round trips while nothing was sent, oldest first.
    pub(crate) idle: Vec<Duration>,
    /// Recent round trips while files were sent, oldest first.
    pub(crate) busy: Vec<Duration>,
    /// The latest round trips, oldest first, and whether files were sent
    /// at the time.
    pub(crate) recent: Vec<(Duration, bool)>,
}

struct PacerState {
    throttle: Throttle,
    bucket: TokenBucket,
    /// Files sending data now.
    sending: usize,
    /// Bytes of file data let through so far.
    sent: u64,
    rtt: Option<Duration>,
    slowdowns: u64,
    idle: VecDeque<Duration>,
    busy: VecDeque<Duration>,
    recent: VecDeque<(Duration, bool)>,
}

/// The throttle and token bucket shared by the files going to one device.
pub(crate) struct Pacer {
    state: Mutex<PacerState>,
}

impl Pacer {
    pub(crate) fn new() -> Self {
        Self {
            state: Mutex::new(PacerState {
                throttle: Throttle::new(THRESHOLD),
                bucket: TokenBucket::default(),
                sending: 0,
                sent: 0,
                rtt: None,
                slowdowns: 0,
                idle: VecDeque::new(),
                busy: VecDeque::new(),
                recent: VecDeque::new(),
            }),
        }
    }

    /// A round trip measured at `now`. Returns what it changed.
    pub(crate) fn sample(&self, rtt: Duration, now: Instant) -> Option<Change> {
        let mut state = self.state.lock().unwrap();
        let state = &mut *state;
        state.rtt = Some(rtt);
        let busy = state.sending > 0;
        let change = if busy {
            keep(&mut state.busy, rtt, KEEP_SAMPLES);
            state.throttle.busy(rtt, state.sent, now)
        } else {
            keep(&mut state.idle, rtt, KEEP_SAMPLES);
            state.throttle.idle(rtt)
        };
        keep(&mut state.recent, (rtt, busy), KEEP_RECENT);
        if matches!(change, Some(Change::Slower(_))) {
            state.slowdowns += 1;
        }
        change
    }

    /// A new connection to the device, which may take another path.
    pub(crate) fn connected(&self) {
        self.state.lock().unwrap().throttle.forget_baseline();
    }

    /// Counts a file as sending data for as long as the guard lives.
    pub(crate) fn sending(&self) -> Sending<'_> {
        self.state.lock().unwrap().sending += 1;
        Sending(self)
    }

    /// How large a piece of a file to read next.
    pub(crate) fn chunk(&self) -> usize {
        chunk(self.state.lock().unwrap().throttle.rate())
    }

    /// Waits until `bytes` more file data may go.
    pub(crate) async fn take(&self, bytes: usize) {
        let wait = {
            let mut state = self.state.lock().unwrap();
            let state = &mut *state;
            state.sent += bytes as u64;
            let rate = state.throttle.rate();
            state.bucket.take(bytes as u64, rate, Instant::now())
        };
        if !wait.is_zero() {
            tokio::time::sleep(wait).await;
        }
    }

    pub(crate) fn status(&self, device: &str) -> PacingStatus {
        let state = self.state.lock().unwrap();
        PacingStatus {
            device: device.to_owned(),
            baseline: state.throttle.baseline(),
            rtt: state.rtt,
            rate: state.throttle.rate(),
            sending: state.sending,
            slowdowns: state.slowdowns,
            idle: state.idle.iter().copied().collect(),
            busy: state.busy.iter().copied().collect(),
            recent: state.recent.iter().copied().collect(),
        }
    }
}

/// A file sending data; see [`Pacer::sending`].
pub(crate) struct Sending<'a>(&'a Pacer);

impl Drop for Sending<'_> {
    fn drop(&mut self) {
        self.0.state.lock().unwrap().sending -= 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MB: u64 = 1_000_000;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// Samples one heartbeat apart, starting at `start`.
    struct Clock {
        now: Instant,
    }

    impl Clock {
        fn new() -> Self {
            Self {
                now: Instant::now(),
            }
        }

        fn tick(&mut self) -> Instant {
            self.now += ms(250);
            self.now
        }
    }

    /// A throttle that learned a 2 ms baseline, then sent at 40 MB/s for
    /// one heartbeat with the round trip still fine.
    fn sending_at_40mb(clock: &mut Clock, sent: &mut u64) -> Throttle {
        let mut throttle = Throttle::new(THRESHOLD);
        for _ in 0..4 {
            assert_eq!(throttle.idle(ms(2)), None);
        }
        assert_eq!(throttle.baseline(), Some(ms(2)));
        assert_eq!(throttle.busy(ms(3), *sent, clock.tick()), None);
        *sent += 10 * MB;
        assert_eq!(throttle.busy(ms(4), *sent, clock.tick()), None);
        assert_eq!(throttle.rate(), None);
        throttle
    }

    #[test]
    fn a_long_round_trip_while_sending_halves_what_was_sent() {
        let mut clock = Clock::new();
        let mut sent = 0;
        let mut throttle = sending_at_40mb(&mut clock, &mut sent);

        sent += 10 * MB;
        // 2 + 15 ms is still within the threshold.
        assert_eq!(throttle.busy(ms(17), sent, clock.tick()), None);
        sent += 10 * MB;
        assert_eq!(
            throttle.busy(ms(18), sent, clock.tick()),
            Some(Change::Slower(20 * MB))
        );
        assert_eq!(throttle.rate(), Some(20 * MB));
        // Only once until the queue had time to drain.
        sent += 5 * MB;
        assert_eq!(throttle.busy(ms(30), sent, clock.tick()), None);
        sent += 5 * MB;
        // Still long after that: halved again, from the limit, which is now
        // lower than the smoothed measure of what went.
        assert_eq!(
            throttle.busy(ms(25), sent, clock.tick()),
            Some(Change::Slower(10 * MB))
        );
    }

    #[test]
    fn the_rate_grows_back_gradually_once_the_round_trip_recovers() {
        let mut clock = Clock::new();
        let mut sent = 0;
        let mut throttle = sending_at_40mb(&mut clock, &mut sent);
        sent += 10 * MB;
        assert_eq!(
            throttle.busy(ms(40), sent, clock.tick()),
            Some(Change::Slower(20 * MB))
        );

        // Between half the threshold and the threshold: held.
        sent += 5 * MB;
        assert_eq!(throttle.busy(ms(12), sent, clock.tick()), None);
        assert_eq!(throttle.rate(), Some(20 * MB));
        // Within half of it: 3% more every sample.
        sent += 5 * MB;
        assert_eq!(
            throttle.busy(ms(5), sent, clock.tick()),
            Some(Change::Faster(20_600_000))
        );
        sent += 5 * MB;
        assert_eq!(
            throttle.busy(ms(5), sent, clock.tick()),
            Some(Change::Faster(21_218_000))
        );
        // Until it limits nothing.
        let mut samples = 0;
        while throttle.rate().is_some() {
            sent += 5 * MB;
            throttle.busy(ms(5), sent, clock.tick());
            samples += 1;
        }
        assert!(samples > 100, "{samples}");
    }

    #[test]
    fn the_limit_eases_off_while_nothing_is_sent() {
        let mut clock = Clock::new();
        let mut sent = 0;
        let mut throttle = sending_at_40mb(&mut clock, &mut sent);
        sent += 10 * MB;
        throttle.busy(ms(40), sent, clock.tick());
        assert_eq!(throttle.rate(), Some(20 * MB));
        assert_eq!(throttle.idle(ms(2)), Some(Change::Faster(20_600_000)));
        while throttle.rate().is_some() {
            throttle.idle(ms(2));
        }
        // Sending again: the first sample only starts measuring.
        assert_eq!(throttle.busy(ms(40), sent, clock.tick()), None);
    }

    #[test]
    fn the_rate_never_drops_below_the_minimum() {
        let mut clock = Clock::new();
        let mut throttle = Throttle::new(THRESHOLD);
        throttle.idle(ms(2));
        let mut sent = 0;
        for _ in 0..40 {
            sent += 100_000;
            throttle.busy(ms(50), sent, clock.tick());
        }
        assert_eq!(throttle.rate(), Some(Throttle::MIN_RATE));
    }

    #[test]
    fn the_baseline_comes_from_idle_round_trips_not_busy_ones() {
        let mut clock = Clock::new();
        let mut throttle = Throttle::new(THRESHOLD);
        throttle.idle(ms(4));
        // Long round trips while sending say nothing about the baseline.
        for sent in 1..20 {
            throttle.busy(ms(15), sent * MB, clock.tick());
        }
        assert_eq!(throttle.baseline(), Some(ms(4)));
        // A shorter one does: the path is faster than thought.
        throttle.busy(ms(3), 21 * MB, clock.tick());
        assert_eq!(throttle.baseline(), Some(ms(3)));
        // While idle, a longer one raises it by an eighth of the difference.
        throttle.idle(ms(11));
        assert_eq!(throttle.baseline(), Some(ms(4)));
        throttle.idle(ms(1));
        assert_eq!(throttle.baseline(), Some(ms(1)));

        // A new connection learns it again.
        throttle.forget_baseline();
        throttle.idle(ms(6));
        assert_eq!(throttle.baseline(), Some(ms(6)));
    }

    #[test]
    fn a_round_trip_that_stays_long_at_the_lowest_rate_becomes_the_baseline() {
        let mut clock = Clock::new();
        let mut throttle = Throttle::new(THRESHOLD);
        throttle.idle(ms(2));
        let mut sent = 0;
        // The path got slower: 30 ms and more however little is sent.
        while throttle.rate() != Some(Throttle::MIN_RATE) {
            sent += 250_000;
            throttle.busy(ms(35), sent, clock.tick());
        }
        let stuck_at = clock.now;
        let mut change = None;
        while change.is_none() {
            sent += 250_000;
            let rtt = if sent % 1_000_000 == 0 {
                ms(30)
            } else {
                ms(36)
            };
            change = throttle.busy(rtt, sent, clock.tick());
        }
        assert_eq!(change, Some(Change::Rebased(ms(30))));
        assert!(clock.now - stuck_at >= Throttle::REBASE);
        assert_eq!(throttle.baseline(), Some(ms(30)));
        // Within the threshold of that, the rate grows again.
        sent += 250_000;
        assert!(matches!(
            throttle.busy(ms(31), sent, clock.tick()),
            Some(Change::Faster(_))
        ));
    }

    #[test]
    fn an_unlimited_rate_is_not_halved_before_anything_was_measured() {
        let mut clock = Clock::new();
        let mut throttle = Throttle::new(THRESHOLD);
        throttle.idle(ms(2));
        assert_eq!(throttle.busy(ms(40), 0, clock.tick()), None);
        assert_eq!(
            throttle.busy(ms(40), 8 * MB, clock.tick()),
            Some(Change::Slower(16 * MB))
        );
    }

    #[test]
    fn the_bucket_holds_data_to_the_rate() {
        let start = Instant::now();
        let mut bucket = TokenBucket::default();
        assert_eq!(bucket.take(1_000_000, None, start), Duration::ZERO);
        // At 1 MB/s, 250 kB waits a quarter second; the next one, taken at
        // once, waits for both.
        assert_eq!(bucket.take(250_000, Some(MB), start), ms(250));
        assert_eq!(bucket.take(250_000, Some(MB), start), ms(500));
        // Once their time has passed, nothing is owed.
        let later = start + ms(500);
        assert_eq!(bucket.take(100_000, Some(MB), later), ms(100));
        // A long pause saves up at most 100 ms worth.
        let much_later = later + Duration::from_secs(10);
        assert_eq!(bucket.take(100_000, Some(MB), much_later), Duration::ZERO);
        assert_eq!(bucket.take(100_000, Some(MB), much_later), ms(100));
    }

    #[test]
    fn pieces_are_an_eighth_of_a_second_within_bounds() {
        assert_eq!(chunk(None), MAX_CHUNK);
        assert_eq!(chunk(Some(Throttle::MIN_RATE)), 128 * 1024);
        assert_eq!(chunk(Some(100_000)), MIN_CHUNK);
        assert_eq!(chunk(Some(100 * MB)), MAX_CHUNK);
    }

    #[test]
    fn the_pacer_tells_idle_round_trips_from_busy_ones() {
        let pacer = Pacer::new();
        let start = Instant::now();
        pacer.sample(ms(2), start);
        {
            let _sending = pacer.sending();
            pacer.sample(ms(9), start + ms(250));
            assert_eq!(pacer.status("pc").sending, 1);
        }
        pacer.sample(ms(3), start + ms(500));
        let status = pacer.status("pc");
        assert_eq!(status.idle, [ms(2), ms(3)]);
        assert_eq!(status.busy, [ms(9)]);
        assert_eq!(
            status.recent,
            [(ms(2), false), (ms(9), true), (ms(3), false)]
        );
        assert_eq!(status.sending, 0);
        assert_eq!(status.rtt, Some(ms(3)));
        assert_eq!(status.rate, None);
    }
}
