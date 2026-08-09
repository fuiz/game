//! Write-ahead log: the events a game can be rebuilt from.
//!
//! Persisting a game by re-serializing the whole [`Game`] on every message
//! costs bytes proportional to the room size, on every one of a room-sized
//! number of messages. This module is the other half of the alternative:
//! append one small entry per message, snapshot occasionally, and rebuild
//! state after an eviction by replaying the entries the snapshot doesn't
//! already cover.
//!
//! [`apply`] is the single place an event reaches the game, used by the live
//! path and by [`Replay`] alike, so the two cannot drift apart. What makes
//! that sound is [`crate::tick::Tick`]: every clock reading and random draw a
//! handler consumes arrives as an argument and is recorded in the [`Entry`],
//! so applying the same entry twice lands in the same state.
//!
//! Replay reproduces the tunnel bookkeeping too. Game logic asks the tunnel
//! finder whether a watcher is reachable, and some of what it decides from
//! that is persisted (which players a team draws from, most importantly). A
//! replay therefore cannot hand out tunnels indiscriminately: [`Replay`]
//! tracks which watchers are connected from the connect and disconnect events
//! in the log, and answers exactly as the live object would have.

use rustc_hash::FxHashSet;
use serde::{Deserialize, Serialize};

use crate::{
    AlarmMessage, SyncMessage, UpdateMessage,
    fuiz::config::ScheduleMessageFn,
    game::{Game, IncomingMessage},
    session::Tunnel,
    tick::Tick,
    watcher::Id,
};

/// A single durable step in a game's history.
///
/// Deliberately coarser than the raw websocket frame: the object resolves a
/// frame into one of these using state the game does not own (which socket
/// carries which id, whether a returning watcher is known), and logs the
/// decision rather than the input. Replay then needs none of that context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Event {
    /// A new watcher joined and was registered as unassigned.
    Joined(Id),
    /// A known watcher reconnected and reclaimed their id.
    Rejoined(Id),
    /// A watcher sent a message.
    Received(Id, IncomingMessage),
    /// A scheduled alarm fired.
    Alarm(AlarmMessage),
    /// A watcher's connection closed.
    Left(Id),
}

/// One log record: an event, the nondeterministic inputs it consumed, and its
/// position in the sequence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Entry {
    /// Position in the log. Monotonic from 1, and the key entries are stored
    /// under, so a snapshot can name the prefix it already covers.
    pub seq: u64,
    /// The clock reading and seed the handler ran with.
    pub tick: Tick,
    /// What happened.
    pub event: Event,
}

/// A tunnel that accepts messages and drops them.
///
/// Replay re-runs handlers that would ordinarily push to clients. Those
/// clients either are not there or have already seen the messages, so the
/// output goes nowhere; only the state changes matter.
#[derive(Clone, Copy, Debug)]
pub struct Sink;

impl Tunnel for Sink {
    fn send_message(&self, _message: &UpdateMessage) {}
    fn send_state(&self, _state: &SyncMessage) {}
    fn close(self) {}
}

/// Applies one event to `game`.
///
/// The live path calls this so that what it does and what a replay does are
/// the same code. `tunnel_finder` is the caller's: live handling passes the
/// real sockets, [`Replay`] passes [`Sink`] for whoever was connected at the
/// time.
/// # Errors
///
/// Returns the admission error when a join or rejoin is refused, because the
/// game is locked or full, so the caller can tell the client why. The event is
/// still a legitimate part of the history: replaying it refuses again, which
/// is what keeps the rebuilt state honest.
pub fn apply<F, S>(
    game: &mut Game,
    event: &Event,
    tick: Tick,
    schedule_message: S,
    tunnel_finder: F,
) -> Result<(), crate::watcher::Error>
where
    F: crate::session::TunnelFinder,
    S: ScheduleMessageFn,
{
    match event {
        Event::Joined(id) => game.add_unassigned(*id, tick, tunnel_finder)?,
        Event::Rejoined(id) => game.rejoin(*id, tick, tunnel_finder)?,
        Event::Received(id, message) => {
            game.receive_message(*id, message.clone(), schedule_message, tick, tunnel_finder);
        }
        Event::Alarm(message) => {
            game.receive_alarm(message, schedule_message, tick, tunnel_finder);
        }
        Event::Left(id) => {
            game.watcher_left(*id, tunnel_finder);
        }
    }

    Ok(())
}

/// Rebuilds a game by replaying entries onto a starting state.
///
/// Feed it the snapshot's game (or a fresh one) and then every entry after the
/// snapshot, in order. Entries at or before the snapshot's sequence are
/// skipped, so re-applying a prefix after a crash between writing a snapshot
/// and dropping the entries it covers is harmless.
pub struct Replay {
    /// Watchers with a live connection, tracked so the replay's tunnel finder
    /// answers what the live object's did.
    connected: FxHashSet<Id>,
    /// Highest sequence already folded in.
    applied: u64,
}

impl Replay {
    /// Starts a replay that treats everything up to and including `applied` as
    /// already reflected in the game it will be handed.
    #[must_use]
    pub fn from_snapshot(applied: u64, connected: FxHashSet<Id>) -> Self {
        Self { connected, applied }
    }

    /// The sequence number most recently folded in.
    #[must_use]
    pub fn applied(&self) -> u64 {
        self.applied
    }

    /// The watchers currently holding a connection.
    #[must_use]
    pub fn connected(&self) -> &FxHashSet<Id> {
        &self.connected
    }

    /// Folds one entry into `game`, or skips it if the snapshot already covers
    /// it. Returns whether it was applied.
    pub fn step(&mut self, game: &mut Game, entry: &Entry) -> bool {
        if entry.seq <= self.applied {
            return false;
        }

        match &entry.event {
            Event::Joined(id) | Event::Rejoined(id) => {
                self.connected.insert(*id);
            }
            Event::Left(id) => {
                self.connected.remove(id);
            }
            Event::Received(..) | Event::Alarm(_) => {}
        }

        // Borrowed by the finder below, so the set is read while the game is
        // mutated; taking a copy of the handle keeps both borrows immutable.
        let connected = &self.connected;
        // A refused join is part of the history, not a replay failure: the
        // live object refused it too, and the state reflects that.
        let _ = apply(
            game,
            &entry.event,
            entry.tick,
            |_: AlarmMessage, _: std::time::Duration| {},
            |id: Id| connected.contains(&id).then_some(Sink),
        );

        self.applied = entry.seq;
        true
    }
}
