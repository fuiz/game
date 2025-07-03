//! Game ID generation and management
//!
//! This module provides functionality for generating and managing unique game IDs
//! that are used to identify game sessions. Game IDs are displayed in octal format
//! to make them easier to communicate verbally.

use std::{fmt::Display, num::ParseIntError, str::FromStr};

use enum_map::{Enum, EnumArray};
use serde::{Deserialize, Deserializer, Serialize};

/// Minimum value for generated game IDs (in octal: 10000)
const MIN_VALUE: u16 = 0o10_000;
/// Maximum value for generated game IDs (in octal: 100000)
const MAX_VALUE: u16 = 0o100_000;

/// A unique identifier for a game session
///
/// Game IDs are generated randomly within a specific range and displayed
/// in octal format to make them easier to communicate. The octal format
/// reduces confusion when sharing game IDs verbally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct GameId(u16);

impl GameId {
    /// Creates a new random game ID
    ///
    /// The ID is generated within the valid range to ensure it displays
    /// as a 5-digit octal number for easy communication.
    pub fn new() -> Self {
        Self(fastrand::u16(MIN_VALUE..MAX_VALUE))
    }
}

impl Default for GameId {
    /// Creates a new random game ID (same as `new()`)
    fn default() -> Self {
        Self::new()
    }
}

impl Display for GameId {
    /// Formats the game ID as a 5-digit octal number
    ///
    /// This format makes the ID easier to communicate verbally and
    /// reduces potential confusion with decimal numbers.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{:05o}", self.0)
    }
}

impl Serialize for GameId {
    /// Serializes the game ID as an octal string
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for GameId {
    /// Deserializes a game ID from an octal string
    fn deserialize<D>(deserializer: D) -> Result<GameId, D::Error>
    where
        D: Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;
        GameId::from_str(&s).map_err(|e| serde::de::Error::custom(e.to_string()))
    }
}

impl FromStr for GameId {
    type Err = ParseIntError;

    /// Parses a game ID from an octal string representation
    ///
    /// # Errors
    ///
    /// Returns a `ParseIntError` if the string cannot be parsed as a valid
    /// octal number.
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(u16::from_str_radix(s, 8)?))
    }
}

impl Enum for GameId {
    /// Total number of possible game IDs
    const LENGTH: usize = (MAX_VALUE - MIN_VALUE) as usize;

    /// Creates a game ID from a usize index
    ///
    /// # Panics
    ///
    /// Panics if the value is out of range for the enum.
    fn from_usize(value: usize) -> Self {
        Self(u16::try_from(value).expect("index out of range for Enum::from_usize") + MIN_VALUE)
    }

    /// Converts the game ID to a usize index
    ///
    /// The returned value is clamped to the valid range to prevent
    /// array access violations.
    fn into_usize(self) -> usize {
        usize::from(self.0.saturating_sub(MIN_VALUE)).min(GameId::LENGTH - 1)
    }
}

impl<V> EnumArray<V> for GameId {
    /// Array type for storing values indexed by GameId
    type Array = [V; Self::LENGTH];
}
