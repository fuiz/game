//! # Fuiz Game Library
//!
//! This library provides the core game logic for the Fuiz quiz game system.
//! It handles game sessions, player management, different question types,
//! leaderboards, and real-time synchronization between players and hosts.

#![allow(clippy::too_many_arguments)]
#![deny(missing_docs)]
#![deny(rustdoc::missing_crate_level_docs)]

use derive_where::derive_where;
use itertools::Itertools;
use serde::{Deserialize, Serialize};

pub mod constants;

pub mod fuiz;
pub mod game;
pub mod game_id;
pub mod leaderboard;
pub mod names;
pub mod session;
pub mod teams;
pub mod watcher;

/// Messages sent to synchronize state between players and hosts
///
/// This enum represents all possible synchronization messages that can be
/// sent to keep game state consistent across all connected clients.
#[derive(Debug, Serialize, Clone, derive_more::From)]
pub enum SyncMessage {
    /// General game synchronization messages
    Game(game::SyncMessage),
    /// Multiple choice question synchronization
    MultipleChoice(fuiz::multiple_choice::SyncMessage),
    /// Type answer question synchronization
    TypeAnswer(fuiz::type_answer::SyncMessage),
    /// Order question synchronization
    Order(fuiz::order::SyncMessage),
}

impl SyncMessage {
    /// Converts the sync message to a JSON string for transmission
    ///
    /// # Panics
    ///
    /// This method panics if serialization fails, which should never happen
    /// with the default JSON serializer for well-formed data.
    pub fn to_message(&self) -> String {
        serde_json::to_string(self).expect("default serializer cannot fail")
    }
}

/// Messages sent to update specific aspects of the game state
///
/// Update messages are used to notify clients about changes that affect
/// their local view of the game, such as score updates or new questions.
#[derive(Debug, Serialize, Clone, derive_more::From)]
pub enum UpdateMessage {
    /// General game update messages
    Game(game::UpdateMessage),
    /// Multiple choice question updates
    MultipleChoice(fuiz::multiple_choice::UpdateMessage),
    /// Type answer question updates
    TypeAnswer(fuiz::type_answer::UpdateMessage),
    /// Order question updates
    Order(fuiz::order::UpdateMessage),
}

/// Alarm messages for timed events in different question types
///
/// These messages are used to handle time-based events like question
/// timeouts or countdown warnings.
#[derive(Debug, Clone, derive_more::From, Serialize, Deserialize)]
pub enum AlarmMessage {
    /// Multiple choice question alarms
    MultipleChoice(fuiz::multiple_choice::AlarmMessage),
    /// Type answer question alarms
    TypeAnswer(fuiz::type_answer::AlarmMessage),
    /// Order question alarms
    Order(fuiz::order::AlarmMessage),
}

impl UpdateMessage {
    /// Converts the update message to a JSON string for transmission
    ///
    /// # Panics
    ///
    /// This method panics if serialization fails, which should never happen
    /// with the default JSON serializer for well-formed data.
    pub fn to_message(&self) -> String {
        serde_json::to_string(self).expect("default serializer cannot fail")
    }
}

/// A truncated vector that maintains the exact count while limiting displayed items
///
/// This structure is useful for displaying a limited number of items while
/// still showing the total count. For example, showing "10 players" but only
/// displaying the first 5 names.
#[derive(Debug, Clone, Serialize)]
#[derive_where(Default)]
pub struct TruncatedVec<T> {
    /// The exact total count of items
    exact_count: usize,
    /// The truncated list of items (up to the limit)
    items: Vec<T>,
}

impl<T: Clone> TruncatedVec<T> {
    /// Creates a new truncated vector from an iterator
    ///
    /// # Arguments
    ///
    /// * `list` - An iterator over items to include
    /// * `limit` - Maximum number of items to include in the truncated vector
    /// * `exact_count` - The exact total count of items (may be larger than limit)
    ///
    /// # Returns
    ///
    /// A new `TruncatedVec` containing up to `limit` items from the iterator
    fn new<I: Iterator<Item = T>>(list: I, limit: usize, exact_count: usize) -> Self {
        let items = list.take(limit).collect_vec();
        Self { exact_count, items }
    }

    /// Maps a function over the items in the truncated vector
    ///
    /// # Arguments
    ///
    /// * `f` - Function to apply to each item
    ///
    /// # Returns
    ///
    /// A new `TruncatedVec` with the function applied to each item
    fn map<F, U>(self, f: F) -> TruncatedVec<U>
    where
        F: Fn(T) -> U,
    {
        TruncatedVec {
            exact_count: self.exact_count,
            items: self.items.into_iter().map(f).collect_vec(),
        }
    }
}
