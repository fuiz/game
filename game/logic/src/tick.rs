//! The nondeterministic inputs a single message handler is allowed to consume.
//!
//! Game logic never reads the clock or the global RNG directly on a path that
//! mutates state. It receives a [`Tick`] instead, and every such value is
//! recorded in the write-ahead log next to the message it accompanied. Feeding
//! the same message and the same `Tick` back in therefore produces the same
//! state, which is what makes log replay a faithful reconstruction rather than
//! an approximation.
//!
//! Read-only paths (the "time remaining" figures in sync messages, say) may
//! still call [`Timestamp::now`] directly: nothing they compute is persisted,
//! so a replay is free to arrive at a different answer.

use crate::time::Timestamp;

#[cfg(feature = "serializable")]
use serde::{Deserialize, Serialize};

/// A sampled clock reading plus a random seed.
///
/// Copy, and small enough to pass by value through the slide dispatch chain.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serializable", derive(Serialize, Deserialize))]
pub struct Tick {
    /// Wall-clock reading for everything this message stores.
    now: Timestamp,
    /// Seed for any randomness this message draws.
    seed: u64,
}

impl Tick {
    /// Samples the real clock and a fresh seed. Live message handling only.
    #[must_use]
    pub fn sample() -> Self {
        Self {
            now: Timestamp::now(),
            seed: fastrand::u64(..),
        }
    }

    /// Rebuilds the tick a log entry recorded.
    #[must_use]
    pub fn new(now: Timestamp, seed: u64) -> Self {
        Self { now, seed }
    }

    /// The instant every timestamp written during this message should carry.
    #[must_use]
    pub fn now(self) -> Timestamp {
        self.now
    }

    /// The seed this message's randomness draws from.
    #[must_use]
    pub fn seed(self) -> u64 {
        self.seed
    }

    /// A generator seeded for this message. Two calls yield generators that
    /// produce the same sequence, so a handler drawing randomness twice must
    /// reuse one generator rather than asking for a second.
    #[must_use]
    pub fn rng(self) -> fastrand::Rng {
        fastrand::Rng::with_seed(self.seed)
    }
}

impl Default for Tick {
    /// Samples, so `Tick::default()` in a test behaves like live handling.
    fn default() -> Self {
        Self::sample()
    }
}
