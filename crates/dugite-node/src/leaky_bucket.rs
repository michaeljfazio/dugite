//! Limit on Patience — the per-peer ChainSync leaky bucket.
//!
//! Port of `Ouroboros.Consensus.Util.LeakyBucket` as configured by
//! `lopBucketConfig` in `MiniProtocol/ChainSync/Client.hs`
//! (release-ouroboros-consensus-3.0.1.0):
//!
//! ```haskell
//! lopBucketConfig gsmState = case (gsmState, csBucketConfig) of
//!   (Syncing, ChainSyncLoPBucketEnabled cfg) -> Config
//!     { capacity = csbcCapacity cfg      -- default 100_000 tokens
//!     , rate = csbcRate cfg              -- default 500 tokens/s
//!     , onEmpty = throwIO EmptyBucket    -- disconnect the peer
//!     , fillOnOverflow = True }
//!   _ -> dummyConfig                     -- PreSyncing / CaughtUp / disabled
//! ```
//!
//! Semantics enforced (each quoted at its method):
//! - the bucket leaks at `rate` while the peer is actively streaming;
//! - it is PAUSED on `MsgAwaitReply` (an at-tip peer consumes no patience)
//!   and resumed on the next `MsgRollForward` / `MsgRollBackward`;
//! - one token is granted per header that STRICTLY advances the peer's best
//!   block number (`checkLoP`: `blockNo hdr > kBestBlockNo`), capped at
//!   capacity (`fillOnOverflow`);
//! - on empty the peer is disconnected (`EmptyBucket`);
//! - a GSM state change reconfigures the bucket and refills it to capacity
//!   (`updateLopBucketConfig` via `cschOnGsmStateChanged`).
//!
//! The level is computed lazily from elapsed time — no background thread.
//! The ChainSync task arms a `sleep_until(empty_deadline)` select branch to
//! catch peers that stop sending entirely (Haskell's leak thread analogue).

use std::time::{Duration, Instant};

/// Per-peer Limit on Patience bucket.
#[derive(Debug)]
pub struct LopBucket {
    /// Token level at `as_of`.
    level: f64,
    as_of: Instant,
    paused: bool,
    capacity: f64,
    /// Tokens per second leaked while active and not paused.
    rate: f64,
    /// `false` = dummy bucket (PreSyncing / CaughtUp / praos): never leaks,
    /// never empties.
    active: bool,
    /// `kBestBlockNo` — highest block number seen from this peer; tokens are
    /// granted only for headers strictly above it.
    best_block_no: Option<u64>,
}

impl LopBucket {
    /// A dummy (inert) bucket — `LeakyBucket.dummyConfig`.
    pub fn dummy(now: Instant) -> Self {
        LopBucket {
            level: 0.0,
            as_of: now,
            paused: false,
            capacity: 0.0,
            rate: 0.0,
            active: false,
            best_block_no: None,
        }
    }

    /// Reconfigure for a GSM state (Haskell `updateLopBucketConfig`):
    /// active in `Syncing` with the configured capacity/rate, dummy
    /// otherwise. Refills to capacity on every reconfiguration (Haskell
    /// `updateConfig` sets the level to the new capacity).
    ///
    /// `kBestBlockNo` is NOT reset — it tracks the peer session, not the
    /// bucket config.
    pub fn reconfigure(&mut self, now: Instant, active: bool, capacity: u64, rate: u64) {
        self.settle(now);
        self.active = active;
        self.capacity = capacity as f64;
        self.rate = rate as f64;
        self.level = self.capacity;
        self.as_of = now;
    }

    /// Settle the lazily-computed level to `now`.
    fn settle(&mut self, now: Instant) {
        if self.active && !self.paused {
            let elapsed = now.saturating_duration_since(self.as_of).as_secs_f64();
            self.level -= elapsed * self.rate;
            // Allow the level to go (slightly) negative — `is_empty`
            // reports it; clamping would mask an elapsed deadline.
        }
        self.as_of = now;
    }

    /// `lbPause` — `MsgAwaitReply` stops the leak ("the LoP clock stops
    /// while waiting at the peer's tip").
    pub fn pause(&mut self, now: Instant) {
        self.settle(now);
        self.paused = true;
    }

    /// `lbResume` — any subsequent `MsgRollForward`/`MsgRollBackward`
    /// restarts the leak.
    pub fn resume(&mut self, now: Instant) {
        self.settle(now);
        self.paused = false;
    }

    /// `checkLoP`: grant one token iff `block_no > kBestBlockNo`, capped at
    /// capacity (`fillOnOverflow = True` caps silently). Updates
    /// `kBestBlockNo` when the token is granted.
    ///
    /// ```haskell
    /// if blockNo hdr > kBestBlockNo
    ///   then lbGrantToken >> pure kis{kBestBlockNo = blockNo hdr}
    ///   else pure kis
    /// ```
    pub fn on_header(&mut self, now: Instant, block_no: u64) {
        self.settle(now);
        let advances = self.best_block_no.map(|b| block_no > b).unwrap_or(true);
        if advances {
            self.best_block_no = Some(block_no);
            if self.active {
                self.level = (self.level + 1.0).min(self.capacity);
            }
        }
    }

    /// Reconcile the pause state with the client-side backpressure flags
    /// (Haskell `pauseBucket` around `checkTime`, Client.hs lines
    /// 1880-1889: "we should not leak tokens as our peer is not
    /// responsible for this waiting time").
    ///
    /// - `throttled` (wire-level backpressure engaged — WE stopped sending
    ///   `MsgRequestNext`): the peer owes us nothing → pause.
    /// - not throttled and not awaiting (`at_tip`): the peer owes us a
    ///   header → leak.
    /// - not throttled but awaiting: leave the `MsgAwaitReply` pause in
    ///   place (Haskell `onMsgAwaitReply`); the next
    ///   `MsgRollForward`/`MsgRollBackward` resumes it.
    pub fn reconcile_backpressure(&mut self, now: Instant, throttled: bool, awaiting: bool) {
        if throttled {
            self.pause(now);
        } else if !awaiting {
            self.resume(now);
        }
    }

    /// True when the active bucket has run dry (`onEmpty` fires —
    /// the peer must be disconnected with `EmptyBucket`).
    pub fn is_empty(&mut self, now: Instant) -> bool {
        if !self.active {
            return false;
        }
        self.settle(now);
        self.level <= 0.0
    }

    /// When the bucket will empty if nothing changes — the ChainSync task
    /// arms a sleep on this deadline so totally-silent peers are caught
    /// (Haskell's leak thread fires exactly at the empty instant).
    /// `None` when inactive or paused (never empties on its own).
    pub fn empty_deadline(&mut self, now: Instant) -> Option<Instant> {
        if !self.active || self.paused {
            return None;
        }
        self.settle(now);
        if self.level <= 0.0 {
            return Some(now);
        }
        if self.rate <= 0.0 {
            return None;
        }
        Some(now + Duration::from_secs_f64(self.level / self.rate))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t0() -> Instant {
        Instant::now()
    }

    fn active_bucket(now: Instant) -> LopBucket {
        let mut b = LopBucket::dummy(now);
        // Upstream defaults: 100_000 tokens @ 500/s = 200 s of patience.
        b.reconfigure(now, true, 100_000, 500);
        b
    }

    #[test]
    fn drains_to_empty_at_capacity_over_rate() {
        let now = t0();
        let mut b = active_bucket(now);
        // 199 s: not yet empty. 201 s: empty.
        assert!(!b.is_empty(now + Duration::from_secs(199)));
        let mut b = active_bucket(now);
        assert!(b.is_empty(now + Duration::from_secs(201)));
    }

    #[test]
    fn grant_only_on_strictly_advancing_block_no() {
        let now = t0();
        let mut b = active_bucket(now);
        // Drain ~100 tokens.
        let later = now + Duration::from_millis(200);
        b.on_header(later, 10); // first header: grants (None → any)
        let lvl_after_first = b.level;
        b.on_header(later, 10); // same block_no: NO token
        assert_eq!(b.level, lvl_after_first);
        b.on_header(later, 9); // lower: NO token
        assert_eq!(b.level, lvl_after_first);
        b.on_header(later, 11); // advances: +1
        assert_eq!(b.level, lvl_after_first + 1.0);
    }

    #[test]
    fn grant_caps_at_capacity() {
        let now = t0();
        let mut b = active_bucket(now);
        // Full bucket + advancing header: stays at capacity
        // (fillOnOverflow caps silently).
        b.on_header(now, 1);
        assert!(b.level <= 100_000.0);
        assert_eq!(b.level, 100_000.0);
    }

    #[test]
    fn reconcile_throttle_pauses_and_release_resumes() {
        // #740: a peer under wire-level backpressure must not be charged
        // patience — 100k tokens @ 500/s would otherwise kill every hot
        // peer ~200s into bulk sync (observed: 237 kills / 34 min).
        let now = t0();
        let mut b = active_bucket(now);
        // Throttle engages 10 s in: 5_000 tokens drained, then frozen.
        b.reconcile_backpressure(now + Duration::from_secs(10), true, false);
        assert_eq!(
            b.empty_deadline(now + Duration::from_secs(10)),
            None,
            "throttled bucket never empties on its own"
        );
        // 10 minutes throttled: still not empty (no leak while paused).
        assert!(!b.is_empty(now + Duration::from_secs(610)));
        // Throttle releases: leak resumes from 95_000 → empty after 190 s.
        let released = now + Duration::from_secs(610);
        b.reconcile_backpressure(released, false, false);
        assert!(!b.is_empty(released + Duration::from_secs(189)));
        assert!(b.is_empty(released + Duration::from_secs(191)));
    }

    #[test]
    fn reconcile_preserves_await_pause_when_not_throttled() {
        // An at-tip peer (MsgAwaitReply) is paused by the await handler;
        // the loop-top reconcile must NOT resume it while still awaiting.
        let now = t0();
        let mut b = active_bucket(now);
        b.pause(now); // MsgAwaitReply
        b.reconcile_backpressure(now + Duration::from_secs(1), false, true);
        assert_eq!(
            b.empty_deadline(now + Duration::from_secs(1)),
            None,
            "await pause survives reconcile"
        );
        // Header arrives (awaiting clears): reconcile resumes the leak.
        b.reconcile_backpressure(now + Duration::from_secs(2), false, false);
        assert!(b.empty_deadline(now + Duration::from_secs(2)).is_some());
    }

    #[test]
    fn reconcile_throttle_dominates_await() {
        // Throttled AND awaiting: paused either way.
        let now = t0();
        let mut b = active_bucket(now);
        b.reconcile_backpressure(now, true, true);
        assert_eq!(b.empty_deadline(now), None);
    }

    #[test]
    fn pause_stops_drain_resume_restarts() {
        let now = t0();
        let mut b = active_bucket(now);
        b.pause(now + Duration::from_secs(10)); // 10s drained: 95_000 left
                                                // 1000 s paused: no further drain.
        assert!(!b.is_empty(now + Duration::from_secs(1_010)));
        let resumed = now + Duration::from_secs(1_010);
        b.resume(resumed);
        // 95_000 tokens / 500 per s = 190 s to empty after resume.
        assert!(!b.is_empty(resumed + Duration::from_secs(189)));
        assert!(b.is_empty(resumed + Duration::from_secs(191)));
    }

    #[test]
    fn reconfigure_refills_and_dummy_never_empties() {
        let now = t0();
        let mut b = active_bucket(now);
        let drained = now + Duration::from_secs(150);
        assert!(!b.is_empty(drained));
        // GSM re-enters Syncing: refilled to capacity.
        b.reconfigure(drained, true, 100_000, 500);
        assert!(!b.is_empty(drained + Duration::from_secs(199)));

        // CaughtUp / PreSyncing / praos: dummy — never empties.
        let mut b = active_bucket(now);
        b.reconfigure(now, false, 0, 0);
        assert!(!b.is_empty(now + Duration::from_secs(100_000)));
        assert_eq!(b.empty_deadline(now + Duration::from_secs(1)), None);
    }

    #[test]
    fn empty_deadline_tracks_level() {
        let now = t0();
        let mut b = active_bucket(now);
        let d = b.empty_deadline(now).expect("active bucket has deadline");
        let secs = d.saturating_duration_since(now).as_secs_f64();
        assert!((secs - 200.0).abs() < 0.5, "expected ~200s, got {secs}");
        b.pause(now);
        assert_eq!(b.empty_deadline(now), None, "paused: no deadline");
    }

    #[test]
    fn best_block_no_survives_reconfiguration() {
        let now = t0();
        let mut b = active_bucket(now);
        b.on_header(now, 100);
        b.reconfigure(now, true, 100_000, 500);
        let lvl = b.level;
        b.on_header(now, 100); // not strictly advancing — no token
        assert_eq!(b.level, lvl);
    }
}
