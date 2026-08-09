//! How a game is laid out in Durable Object storage.
//!
//! A game is a snapshot plus the entries logged since it. Writing the whole
//! game on every message would cost bytes proportional to the room size on
//! every one of a room-sized number of messages; appending one small entry
//! instead keeps the per-message write flat, and the snapshot bounds how much
//! has to be replayed after an eviction.
//!
//! Keys:
//!
//! | key             | holds                                            |
//! | --------------- | ------------------------------------------------ |
//! | `count`         | how many chunks the snapshot spans               |
//! | `chunk_{i}`     | the snapshot, in 64 KiB pieces                   |
//! | `snapshot_seq`  | the sequence number the snapshot covers          |
//! | `connected`     | watchers holding a socket when it was taken      |
//! | `log_{seq}`     | one [`Entry`], zero-padded so keys sort by seq   |
//!
//! Entries a snapshot has superseded are left where they are. A delete bills
//! the same as a write on the key-value backend, so paying to remove them
//! would hand back the saving; `snapshot_seq` tells a load where to start
//! reading instead, and the expiry alarm's `delete_all` clears the lot at once.
//!
//! [`crate::game`] owns the object's lifecycle and decides which events to
//! record. This module owns only where the bytes go.

use std::cell::Cell;

use fuiz::{
    tick::Tick,
    wal::{Entry, Event, Replay},
    watcher,
};
use rustc_hash::FxHashSet;
use serde::{Deserialize, Serialize};
use worker::*;

/// Entries appended between snapshots. Each entry is one small write; a
/// snapshot is one write per 64 KiB of state, so the interval trades a rarer
/// large write against a longer replay on wake.
const SNAPSHOT_INTERVAL: u64 = 128;

/// Size of the pieces a snapshot is split into, under the 128 KiB ceiling on a
/// single stored value.
const CHUNK: usize = 64 * 1024;

const COUNT: &str = "count";
const SNAPSHOT_SEQ: &str = "snapshot_seq";
const CONNECTED: &str = "connected";
/// Keys are zero-padded so lexical order is sequence order, which is what lets
/// a listing start just past the snapshot.
const LOG_PREFIX: &str = "log_";

fn log_key(seq: u64) -> String {
    format!("{LOG_PREFIX}{seq:020}")
}

/// A blob of bytes stored under one key.
///
/// Transparent so the stored value is the byte string itself, and routed
/// through `serde_bytes` so it lands as bytes rather than an array of numbers.
#[derive(Serialize, Deserialize)]
#[serde(transparent)]
struct Blob {
    #[serde(with = "serde_bytes")]
    bytes: Vec<u8>,
}

/// An encoded game, waiting to be written.
pub struct Snapshot(Vec<u8>);

/// Tracks where a game's history has got to, and reads and writes it.
///
/// Holds no game of its own: the object keeps that in memory and hands it over
/// when there is something to persist.
pub struct Journal {
    /// Sequence number of the last entry appended.
    seq: Cell<u64>,
    /// Entries appended since the last snapshot.
    since_snapshot: Cell<u64>,
}

impl Journal {
    /// A journal for an object that has not read storage yet.
    pub fn new() -> Self {
        Self {
            seq: Cell::new(0),
            since_snapshot: Cell::new(0),
        }
    }

    /// Reads the snapshot and folds in every entry logged after it.
    ///
    /// Returns `None` when there is no snapshot, which means either a game
    /// that has not been created yet or one already swept away.
    pub async fn load(&self, storage: &Storage) -> Option<fuiz::game::Game> {
        let mut game = read_snapshot(storage).await?;

        let snapshot_seq: u64 = storage.get(SNAPSHOT_SEQ).await.ok().flatten().unwrap_or(0);
        let connected: FxHashSet<watcher::Id> = storage.get(CONNECTED).await.ok().flatten().unwrap_or_default();

        let mut replay = Replay::from_snapshot(snapshot_seq, connected);
        for entry in read_log(storage, snapshot_seq).await {
            replay.step(&mut game, &entry);
        }

        self.seq.set(replay.applied());
        self.since_snapshot.set(replay.applied() - snapshot_seq);

        Some(game)
    }

    /// Appends one entry, under the next sequence number.
    ///
    /// # Errors
    ///
    /// Returns an error if the entry cannot be encoded or the write fails, so
    /// the object can surface a message it did not manage to make durable.
    pub async fn append(&self, storage: &Storage, tick: Tick, event: Event) -> Result<()> {
        let seq = self.seq.get() + 1;

        let mut bytes = Vec::new();
        ciborium::into_writer(&Entry { seq, tick, event }, &mut bytes).map_err(|e| {
            console_error!("Error serializing log entry: {:?}", e);
            Error::RustError(e.to_string())
        })?;

        storage.put(&log_key(seq), &Blob { bytes }).await?;

        self.seq.set(seq);
        self.since_snapshot.set(self.since_snapshot.get() + 1);

        Ok(())
    }

    /// Whether enough entries have piled up to be worth a fresh snapshot.
    pub fn snapshot_due(&self) -> bool {
        self.since_snapshot.get() >= SNAPSHOT_INTERVAL
    }

    /// Encodes a game ready for [`Self::write_snapshot`].
    ///
    /// Separate from the write so the caller can let go of its borrow of the
    /// game before awaiting storage: holding one across an await risks a panic
    /// should the object re-enter and want the game mutably.
    ///
    /// # Errors
    ///
    /// Returns an error if the game cannot be encoded.
    pub fn encode(game: &fuiz::game::Game) -> Result<Snapshot> {
        let mut bytes = Vec::new();
        ciborium::into_writer(game, &mut bytes).map_err(|e| {
            console_error!("Error serializing game: {:?}", e);
            Error::RustError(e.to_string())
        })?;

        Ok(Snapshot(bytes))
    }

    /// Writes the whole game out and marks the log prefix it now covers.
    ///
    /// `connected` is the watchers currently holding a socket. Game logic asks
    /// its tunnel finder who is reachable and persists some of what it decides
    /// from the answer, so a replay starting here has to begin from the same
    /// set rather than guess at it.
    ///
    /// # Errors
    ///
    /// Returns an error if a write fails.
    pub async fn write_snapshot(
        &self,
        storage: &Storage,
        snapshot: Snapshot,
        connected: &FxHashSet<watcher::Id>,
    ) -> Result<()> {
        let chunks: Vec<Blob> = snapshot
            .0
            .chunks(CHUNK)
            .map(|chunk| Blob { bytes: chunk.to_vec() })
            .collect();

        storage.put(COUNT, &chunks.len()).await?;
        for (index, chunk) in chunks.into_iter().enumerate() {
            if let Err(e) = storage.put(&format!("chunk_{index}"), &chunk).await {
                console_error!("Error storing chunk: {:?}", e);
            }
        }

        storage.put(CONNECTED, connected).await?;
        storage.put(SNAPSHOT_SEQ, &self.seq.get()).await?;
        self.since_snapshot.set(0);

        Ok(())
    }

    /// Forgets where the history had got to, after storage has been wiped.
    ///
    /// Without this the sequence would carry on past entries that no longer
    /// exist, and a later load would find them with no snapshot to build on.
    pub fn reset(&self) {
        self.seq.set(0);
        self.since_snapshot.set(0);
    }
}

/// Reassembles the snapshot from its chunks.
async fn read_snapshot(storage: &Storage) -> Option<fuiz::game::Game> {
    let count: usize = storage.get(COUNT).await.ok()??;

    let mut bytes = Vec::new();
    for index in 0..count {
        match storage.get::<Blob>(&format!("chunk_{index}")).await {
            Ok(Some(chunk)) => bytes.extend_from_slice(&chunk.bytes),
            Ok(None) => {
                console_error!("Chunk {} not found", index);
                return None;
            }
            Err(e) => {
                console_error!("Error loading chunk: {:?}", e);
                return None;
            }
        }
    }

    match ciborium::from_reader(bytes.as_slice()) {
        Ok(game) => Some(game),
        Err(e) => {
            console_error!("Error deserializing game: {:?}", e);
            None
        }
    }
}

/// Reads back the unbroken run of entries starting just after `after`.
///
/// Stops at the first entry that will not decode and at the first gap in the
/// sequence, returning only the prefix before it. Replaying across a hole
/// would produce a game missing whatever that entry did while still looking
/// well-formed, which is worse than coming up short: a game rebuilt from a
/// short prefix is one clients can be resynchronised against, whereas one
/// rebuilt across a hole silently contradicts what they were already told.
async fn read_log(storage: &Storage, after: u64) -> Vec<Entry> {
    let start = log_key(after + 1);
    let options = ListOptions::new().prefix(LOG_PREFIX).start(&start);

    let listed = match storage.list_with_options(options).await {
        Ok(listed) => listed,
        Err(e) => {
            console_error!("Error listing log: {:?}", e);
            return Vec::new();
        }
    };

    let mut entries: Vec<Entry> = Vec::with_capacity(listed.size() as usize);
    let mut truncated = false;
    listed.for_each(&mut |value, _key| {
        if truncated {
            return;
        }
        match serde_wasm_bindgen::from_value::<Blob>(value)
            .map_err(|e| e.to_string())
            .and_then(|stored| ciborium::from_reader::<Entry, _>(stored.bytes.as_slice()).map_err(|e| e.to_string()))
        {
            Ok(entry) => entries.push(entry),
            Err(e) => {
                console_error!("Error decoding log entry, replaying up to it only: {:?}", e);
                truncated = true;
            }
        }
    });

    // The zero-padded key already puts a listing in sequence order; sorting
    // keeps that from being something the caller has to know.
    entries.sort_by_key(|entry| entry.seq);

    // A missing key leaves the same hole a bad decode does, so cut there too.
    let contiguous = entries
        .iter()
        .zip(after + 1..)
        .take_while(|(entry, expected)| entry.seq == *expected)
        .count();
    if contiguous < entries.len() {
        console_error!(
            "Log gap at {}, replaying {} of {} entries",
            after + 1 + contiguous as u64,
            contiguous,
            entries.len()
        );
        entries.truncate(contiguous);
    }

    entries
}
