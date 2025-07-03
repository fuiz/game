//! Player and host watcher management
//!
//! This module manages the connections and state of all participants in a game
//! session, including hosts, players, and unassigned connections. It provides
//! functionality for tracking participant types, sending messages, and managing
//! the overall participant lifecycle.

use std::{
    collections::{HashMap, HashSet},
    fmt::Display,
    str::FromStr,
};

use enum_map::{Enum, EnumMap};
use itertools::Itertools;
use serde::{Deserialize, Serialize};
use serde_with::{DeserializeFromStr, SerializeDisplay};
use thiserror::Error;
use uuid::Uuid;

use super::{SyncMessage, UpdateMessage, session::Tunnel};

/// A unique identifier for participants in the game
///
/// Each participant (host, player, or unassigned connection) gets a unique ID
/// that persists throughout their participation in the game session.
#[derive(
    Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, DeserializeFromStr, SerializeDisplay,
)]
pub struct Id(Uuid);

impl Id {
    /// Gets a seed value derived from the ID for deterministic operations
    ///
    /// This method extracts a seed value that can be used for deterministic
    /// random operations tied to this specific ID.
    pub fn _get_seed(&self) -> u64 {
        self.0.as_u64_pair().0
    }

    /// Creates a new random participant ID
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for Id {
    /// Creates a new random participant ID (same as `new()`)
    fn default() -> Self {
        Self::new()
    }
}

impl Display for Id {
    /// Formats the ID as a UUID string
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for Id {
    type Err = uuid::Error;

    /// Parses an ID from a UUID string
    ///
    /// # Errors
    ///
    /// Returns a `uuid::Error` if the string is not a valid UUID.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::from_str(s)?))
    }
}

/// The different types of participants in a game session
///
/// This enum represents the role and state of each participant,
/// determining what actions they can perform and what information
/// they receive.
/// Represents the type and state of a participant in the game
///
/// This enum distinguishes between different participant types and their roles,
/// determining what actions they can perform and what information they receive.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Value {
    /// A connection that hasn't been assigned a role yet
    Unassigned,
    /// The game host who controls the game flow
    Host,
    /// A player participating in the game
    Player(PlayerValue),
}

/// The kind of participant without associated data
///
/// This enum represents just the discriminant of the Value enum,
/// useful for pattern matching and filtering participants by type
/// without needing the associated data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Enum, Serialize, Deserialize)]
pub enum ValueKind {
    /// An unassigned connection
    Unassigned,
    /// A game host
    Host,
    /// A game player
    Player,
}

impl Value {
    /// Returns the kind of this value without the associated data
    ///
    /// # Returns
    ///
    /// The ValueKind corresponding to this Value variant
    pub fn kind(&self) -> ValueKind {
        match self {
            Value::Unassigned => ValueKind::Unassigned,
            Value::Host => ValueKind::Host,
            Value::Player(_) => ValueKind::Player,
        }
    }
}

/// Player-specific data and state
///
/// This enum differentiates between individual players and team players,
/// tracking the necessary information for each type.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PlayerValue {
    /// An individual player not part of a team
    Individual {
        /// The player's chosen name
        name: String,
    },
    /// A player who is part of a team
    Team {
        /// The name of the team
        team_name: String,
        /// The individual player's name within the team
        individual_name: String,
        /// The unique identifier for the team
        team_id: Id,
    },
}

impl PlayerValue {
    /// Gets the individual name of the player
    ///
    /// For individual players, this returns their name.
    /// For team players, this returns their individual name within the team.
    pub fn name(&self) -> &str {
        match self {
            Self::Individual { name } => name,
            Self::Team {
                team_name: _,
                individual_name,
                team_id: _,
            } => individual_name,
        }
    }
}

/// Serialization helper for Watchers struct
#[derive(Deserialize)]
struct WatchersSerde {
    mapping: HashMap<Id, Value>,
}

/// Manages all participants (watchers) in a game session
///
/// This struct tracks all connected participants, their roles, and provides
/// functionality for sending messages, managing state, and organizing
/// participants by type.
#[derive(Default, Serialize, Deserialize)]
#[serde(from = "WatchersSerde")]
pub struct Watchers {
    /// Primary mapping from participant ID to their value/state
    mapping: HashMap<Id, Value>,

    /// Reverse mapping organized by participant type for efficient filtering
    #[serde(skip_serializing)]
    reverse_mapping: EnumMap<ValueKind, HashSet<Id>>,
}

impl From<WatchersSerde> for Watchers {
    /// Reconstructs the Watchers struct from serialized data
    ///
    /// This rebuilds the reverse mapping from the primary mapping,
    /// which is necessary since the reverse mapping is not serialized.
    fn from(serde: WatchersSerde) -> Self {
        let WatchersSerde { mapping } = serde;
        let mut reverse_mapping: EnumMap<ValueKind, HashSet<Id>> = EnumMap::default();
        for (id, value) in mapping.iter() {
            reverse_mapping[value.kind()].insert(*id);
        }
        Self {
            mapping,
            reverse_mapping,
        }
    }
}

/// Errors that can occur when managing watchers
#[derive(Error, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The game has reached the maximum number of allowed players
    #[error("maximum number of players reached")]
    MaximumPlayers,
}

impl Watchers {
    /// Creates a new Watchers instance with a host already assigned
    ///
    /// # Arguments
    ///
    /// * `host_id` - The ID of the host participant
    ///
    /// # Returns
    ///
    /// A new Watchers instance with the specified host already registered
    pub fn with_host_id(host_id: Id) -> Self {
        Self {
            mapping: {
                let mut map = HashMap::default();
                map.insert(host_id, Value::Host);
                map
            },
            reverse_mapping: {
                let mut map: EnumMap<ValueKind, HashSet<Id>> = EnumMap::default();
                map[ValueKind::Host].insert(host_id);
                map
            },
        }
    }

    /// Gets a vector of all participants with their tunnels and values
    ///
    /// # Arguments
    ///
    /// * `tunnel_finder` - Function to retrieve the tunnel for a given ID
    ///
    /// # Returns
    ///
    /// Vector of tuples containing (ID, Tunnel, Value) for all participants
    /// with active tunnels
    pub fn vec<T: Tunnel, F: Fn(Id) -> Option<T>>(&self, tunnel_finder: F) -> Vec<(Id, T, Value)> {
        self.reverse_mapping
            .values()
            .flat_map(|v| v.iter())
            .filter_map(|x| match (tunnel_finder(*x), self.mapping.get(x)) {
                (Some(t), Some(v)) => Some((*x, t, v.to_owned())),
                _ => None,
            })
            .collect_vec()
    }

    /// Gets a vector of participants of a specific type with their tunnels and values
    ///
    /// # Arguments
    ///
    /// * `filter` - The type of participants to include
    /// * `tunnel_finder` - Function to retrieve the tunnel for a given ID
    ///
    /// # Returns
    ///
    /// Vector of tuples containing (ID, Tunnel, Value) for participants
    /// of the specified type with active tunnels
    pub fn specific_vec<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &self,
        filter: ValueKind,
        tunnel_finder: F,
    ) -> Vec<(Id, T, Value)> {
        self.reverse_mapping[filter]
            .iter()
            .filter_map(|x| match (tunnel_finder(*x), self.mapping.get(x)) {
                (Some(t), Some(v)) => Some((*x, t, v.to_owned())),
                _ => None,
            })
            .collect_vec()
    }

    /// Gets the count of participants of a specific type
    ///
    /// # Arguments
    ///
    /// * `filter` - The type of participants to count
    ///
    /// # Returns
    ///
    /// The number of participants of the specified type
    pub fn specific_count(&self, filter: ValueKind) -> usize {
        self.reverse_mapping[filter].len()
    }

    /// Adds a new watcher to the game session
    ///
    /// # Arguments
    ///
    /// * `watcher_id` - The unique ID for the new watcher
    /// * `watcher_value` - The value/role for the new watcher
    ///
    /// # Returns
    ///
    /// `Ok(())` if successful, or `Error::MaximumPlayers` if the game is full
    ///
    /// # Errors
    ///
    /// Returns `Error::MaximumPlayers` if adding this watcher would exceed
    /// the maximum allowed number of participants.
    pub fn add_watcher(&mut self, watcher_id: Id, watcher_value: Value) -> Result<(), Error> {
        let kind = watcher_value.kind();

        if self.mapping.len() >= crate::constants::fuiz::MAX_PLAYER_COUNT {
            return Err(Error::MaximumPlayers);
        }

        self.mapping.insert(watcher_id, watcher_value);
        self.reverse_mapping[kind].insert(watcher_id);

        Ok(())
    }

    /// Updates the value/role of an existing watcher
    ///
    /// This method properly handles moving the watcher between different
    /// type categories if their role changes.
    ///
    /// # Arguments
    ///
    /// * `watcher_id` - The ID of the watcher to update
    /// * `watcher_value` - The new value/role for the watcher
    pub fn update_watcher_value(&mut self, watcher_id: Id, watcher_value: Value) {
        let old_kind = match self.mapping.get(&watcher_id) {
            Some(v) => v.kind(),
            _ => return,
        };
        let new_kind = watcher_value.kind();
        if old_kind != new_kind {
            self.reverse_mapping[old_kind].remove(&watcher_id);
            self.reverse_mapping[new_kind].insert(watcher_id);
        }
        self.mapping.insert(watcher_id, watcher_value);
    }

    /// Gets the value/role of a specific watcher
    ///
    /// # Arguments
    ///
    /// * `watcher_id` - The ID of the watcher to look up
    ///
    /// # Returns
    ///
    /// The watcher's value if they exist, otherwise `None`
    pub fn get_watcher_value(&self, watcher_id: Id) -> Option<Value> {
        self.mapping.get(&watcher_id).map(|v| v.to_owned())
    }

    /// Checks if a watcher exists in the game session
    ///
    /// # Arguments
    ///
    /// * `watcher_id` - The ID of the watcher to check
    ///
    /// # Returns
    ///
    /// `true` if the watcher exists, `false` otherwise
    pub fn has_watcher(&self, watcher_id: Id) -> bool {
        self.mapping.contains_key(&watcher_id)
    }

    /// Checks if a watcher has an active connection
    ///
    /// # Arguments
    ///
    /// * `watcher_id` - The ID of the watcher to check
    /// * `tunnel_finder` - Function to retrieve the tunnel for the watcher
    ///
    /// # Returns
    ///
    /// `true` if the watcher has an active tunnel, `false` otherwise
    pub fn is_alive<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &self,
        watcher_id: Id,
        tunnel_finder: F,
    ) -> bool {
        tunnel_finder(watcher_id).is_some()
    }

    /// Removes a watcher's session and closes their tunnel
    ///
    /// This method finds the watcher's tunnel and properly closes it
    /// to clean up the connection.
    ///
    /// # Arguments
    ///
    /// * `watcher_id` - The ID of the watcher whose session should be removed
    /// * `tunnel_finder` - Function to retrieve the tunnel for the watcher
    pub fn remove_watcher_session<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &mut self,
        watcher_id: &Id,
        tunnel_finder: F,
    ) {
        if let Some(x) = tunnel_finder(*watcher_id) {
            x.close();
        }
    }

    /// Sends an update message to a specific watcher
    ///
    /// # Arguments
    ///
    /// * `message` - The update message to send
    /// * `watcher_id` - The ID of the watcher to send to
    /// * `tunnel_finder` - Function to retrieve the tunnel for the watcher
    pub fn send_message<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &self,
        message: &UpdateMessage,
        watcher_id: Id,
        tunnel_finder: F,
    ) {
        let Some(session) = tunnel_finder(watcher_id) else {
            return;
        };

        session.send_message(message);
    }

    /// Sends a state synchronization message to a specific watcher
    ///
    /// # Arguments
    ///
    /// * `message` - The sync message to send
    /// * `watcher_id` - The ID of the watcher to send to
    /// * `tunnel_finder` - Function to retrieve the tunnel for the watcher
    pub fn send_state<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &self,
        message: &SyncMessage,
        watcher_id: Id,
        tunnel_finder: F,
    ) {
        let Some(session) = tunnel_finder(watcher_id) else {
            return;
        };

        session.send_state(message);
    }

    /// Gets the display name of a watcher
    ///
    /// This only returns a name for player watchers, not hosts or unassigned connections.
    ///
    /// # Arguments
    ///
    /// * `watcher_id` - The ID of the watcher
    ///
    /// # Returns
    ///
    /// The player's name if they are a player, otherwise `None`
    pub fn get_name(&self, watcher_id: Id) -> Option<String> {
        self.get_watcher_value(watcher_id).and_then(|v| match v {
            Value::Player(player_value) => Some(player_value.name().to_owned()),
            _ => None,
        })
    }

    /// Gets the team name of a watcher if they are part of a team
    ///
    /// # Arguments
    ///
    /// * `watcher_id` - The ID of the watcher
    ///
    /// # Returns
    ///
    /// The team name if the watcher is a team player, otherwise `None`
    pub fn get_team_name(&self, watcher_id: Id) -> Option<String> {
        self.get_watcher_value(watcher_id).and_then(|v| match v {
            Value::Player(PlayerValue::Team { team_name, .. }) => Some(team_name),
            _ => None,
        })
    }

    /// Sends personalized messages to all watchers using a sender function
    ///
    /// The sender function is called for each watcher and can return different
    /// messages based on the watcher's ID and type, or None to skip sending.
    ///
    /// # Arguments
    ///
    /// * `sender` - Function that generates messages for each watcher
    /// * `tunnel_finder` - Function to retrieve tunnels for watchers
    pub fn announce_with<S, T: Tunnel, F: Fn(Id) -> Option<T>>(&self, sender: S, tunnel_finder: F)
    where
        S: Fn(Id, ValueKind) -> Option<super::UpdateMessage>,
    {
        for (watcher, session, v) in self.vec(tunnel_finder) {
            let Some(message) = sender(watcher, v.kind()) else {
                continue;
            };

            session.send_message(&message);
        }
    }

    /// Broadcasts an update message to all watchers except unassigned ones
    ///
    /// # Arguments
    ///
    /// * `message` - The update message to broadcast
    /// * `tunnel_finder` - Function to retrieve tunnels for watchers
    pub fn announce<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &self,
        message: &super::UpdateMessage,
        tunnel_finder: F,
    ) {
        self.announce_with(
            |_, value_kind| {
                if matches!(value_kind, ValueKind::Unassigned) {
                    None
                } else {
                    Some(message.to_owned())
                }
            },
            tunnel_finder,
        );
    }

    /// Sends an update message to all watchers of a specific type
    ///
    /// # Arguments
    ///
    /// * `filter` - The type of watchers to send to
    /// * `message` - The update message to send
    /// * `tunnel_finder` - Function to retrieve tunnels for watchers
    pub fn announce_specific<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &self,
        filter: ValueKind,
        message: &super::UpdateMessage,
        tunnel_finder: F,
    ) {
        for (_, session, _) in self.specific_vec(filter, tunnel_finder) {
            session.send_message(message);
        }
    }
}
