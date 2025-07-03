//! Player name management and validation
//!
//! This module handles the assignment and validation of player names within
//! a game session. It ensures name uniqueness, filters inappropriate content,
//! and maintains bidirectional mappings between player IDs and names.

use std::collections::{HashMap, HashSet, hash_map::Entry};

use rustrict::CensorStr;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::watcher::Id;

/// Serialization helper for Names struct
#[derive(Deserialize)]
struct NamesSerde {
    mapping: HashMap<Id, String>,
}

/// Manages player names and their associations with player IDs
///
/// This struct maintains a bidirectional mapping between player IDs and names,
/// ensuring that names are unique within a game session and meet content
/// and length requirements.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
#[serde(from = "NamesSerde")]
pub struct Names {
    /// Primary mapping from player ID to name
    mapping: HashMap<Id, String>,

    /// Reverse mapping from name to player ID (not serialized)
    #[serde(skip_serializing)]
    reverse_mapping: HashMap<String, Id>,
    /// Set of all existing names for quick uniqueness checks (not serialized)
    #[serde(skip_serializing)]
    existing: HashSet<String>,
}

impl From<NamesSerde> for Names {
    /// Reconstructs the Names struct from serialized data
    ///
    /// This rebuilds the reverse mapping and existing names set from
    /// the primary mapping, which is necessary since these fields
    /// are not serialized.
    fn from(serde: NamesSerde) -> Self {
        let NamesSerde { mapping } = serde;
        let mut reverse_mapping = HashMap::new();
        let mut existing = HashSet::new();
        for (id, name) in mapping.iter() {
            reverse_mapping.insert(name.to_owned(), *id);
            existing.insert(name.to_owned());
        }
        Self {
            mapping,
            reverse_mapping,
            existing,
        }
    }
}

/// Errors that can occur during name validation and assignment
#[derive(Error, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    /// The requested name is already in use by another player
    #[error("name already in-use")]
    Used,
    /// The player already has an assigned name
    #[error("player has an existing name")]
    Assigned,
    /// The name is empty or contains only whitespace
    #[error("name cannot be empty")]
    Empty,
    /// The name contains inappropriate content
    #[error("name is inappropriate")]
    Sinful,
    /// The name exceeds the maximum allowed length
    #[error("name is too long")]
    TooLong,
}

impl Names {
    /// Retrieves the name associated with a player ID
    ///
    /// # Arguments
    ///
    /// * `id` - The player ID to look up
    ///
    /// # Returns
    ///
    /// The player's name if they have one assigned, otherwise `None`
    pub fn get_name(&self, id: &Id) -> Option<String> {
        self.mapping.get(id).map(|s| s.to_owned())
    }

    /// Assigns a name to a player after validation
    ///
    /// This method performs comprehensive validation including length limits,
    /// content filtering, uniqueness checking, and ensures the player doesn't
    /// already have a name assigned.
    ///
    /// # Arguments
    ///
    /// * `id` - The player ID to assign the name to
    /// * `name` - The requested name (will be trimmed of whitespace)
    ///
    /// # Returns
    ///
    /// The cleaned and assigned name on success, or an error describing
    /// why the name was rejected.
    ///
    /// # Errors
    ///
    /// * `Error::TooLong` - Name exceeds 30 characters
    /// * `Error::Empty` - Name is empty after trimming whitespace
    /// * `Error::Sinful` - Name contains inappropriate content
    /// * `Error::Used` - Name is already taken by another player
    /// * `Error::Assigned` - Player already has a name assigned
    pub fn set_name(&mut self, id: Id, name: &str) -> Result<String, Error> {
        if name.len() > 30 {
            return Err(Error::TooLong);
        }
        let name = rustrict::trim_whitespace(name);
        if name.is_empty() {
            return Err(Error::Empty);
        }
        if name.is_inappropriate() {
            return Err(Error::Sinful);
        }
        if !self.existing.insert(name.to_owned()) {
            return Err(Error::Used);
        }
        match self.mapping.entry(id) {
            Entry::Occupied(_) => Err(Error::Assigned),
            Entry::Vacant(v) => {
                v.insert(name.to_owned());
                self.reverse_mapping.insert(name.to_owned(), id);
                Ok(name.to_owned())
            }
        }
    }

    /// Retrieves the player ID associated with a name
    ///
    /// # Arguments
    ///
    /// * `name` - The name to look up
    ///
    /// # Returns
    ///
    /// The player ID if the name is assigned, otherwise `None`
    pub fn get_id(&self, name: &str) -> Option<Id> {
        self.reverse_mapping.get(name).copied()
    }
}
