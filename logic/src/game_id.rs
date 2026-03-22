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
    /// Array type for storing values indexed by `GameId`
    type Array = [V; Self::LENGTH];
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;

    #[test]
    fn test_game_id_new_in_range() {
        for _ in 0..100 {
            let id = GameId::new();
            assert!(id.0 >= MIN_VALUE);
            assert!(id.0 < MAX_VALUE);
        }
    }

    #[test]
    fn test_game_id_display_format() {
        let id = GameId(MIN_VALUE);
        assert_eq!(id.to_string(), "10000");

        let id = GameId(MIN_VALUE + 1);
        assert_eq!(id.to_string(), "10001");

        let id = GameId(MAX_VALUE - 1);
        assert_eq!(id.to_string(), "77777");
    }

    #[test]
    fn test_game_id_from_str() {
        let id = GameId::from_str("10000").unwrap();
        assert_eq!(id.0, MIN_VALUE);

        let id = GameId::from_str("12345").unwrap();
        assert_eq!(id.0, 0o12345);

        let id = GameId::from_str("77777").unwrap();
        assert_eq!(id.0, 0o77777);
    }

    #[test]
    fn test_game_id_from_str_invalid() {
        assert!(GameId::from_str("invalid").is_err());
        assert!(GameId::from_str("888").is_err()); // Invalid octal digit
        assert!(GameId::from_str("").is_err());
    }

    #[test]
    fn test_game_id_serialization() {
        let id = GameId(0o12345);
        let serialized = serde_json::to_string(&id).unwrap();
        assert_eq!(serialized, "\"12345\"");

        let deserialized: GameId = serde_json::from_str(&serialized).unwrap();
        assert_eq!(deserialized, id);
    }

    #[test]
    fn test_game_id_default() {
        let id = GameId::default();
        assert!(id.0 >= MIN_VALUE);
        assert!(id.0 < MAX_VALUE);
    }

    #[test]
    fn test_game_id_enum_conversions() {
        // Test round-trip conversion
        let original = GameId(MIN_VALUE);
        let index = original.into_usize();
        let converted = GameId::from_usize(index);
        assert_eq!(original, converted);

        // Test boundary values
        let max_index = GameId::LENGTH - 1;
        let id_from_max = GameId::from_usize(max_index);
        assert_eq!(id_from_max.into_usize(), max_index);
    }

    #[test]
    fn test_game_id_enum_boundary_clamping() {
        // Test that values outside range are clamped
        let out_of_range = GameId(MAX_VALUE + 100);
        let index = out_of_range.into_usize();
        assert_eq!(index, GameId::LENGTH - 1);
    }

    #[test]
    fn test_game_id_ordering() {
        let id1 = GameId(MIN_VALUE);
        let id2 = GameId(MIN_VALUE + 1);
        let id3 = GameId(MAX_VALUE - 1);

        assert!(id1 < id2);
        assert!(id2 < id3);
        assert!(id1 <= id1);
        assert!(id3 >= id2);
    }

    #[test]
    fn test_game_id_hash_equality() {
        use std::collections::HashMap;

        let id1 = GameId(0o12345);
        let id2 = GameId(0o12345);
        let id3 = GameId(0o54321);

        assert_eq!(id1, id2);
        assert_ne!(id1, id3);

        let mut map = HashMap::new();
        map.insert(id1, "value1");
        map.insert(id3, "value3");

        assert_eq!(map.get(&id2), Some(&"value1"));
        assert_eq!(map.len(), 2);
    }

    #[test]
    #[should_panic(expected = "index out of range for Enum::from_usize")]
    fn test_game_id_from_usize_large_value() {
        // Test that very large values are handled properly
        // This will panic due to the u16::try_from conversion
        GameId::from_usize(usize::MAX);
    }

    #[test]
    fn test_game_id_deserialization_error() {
        // Test deserialization error path for invalid JSON
        let invalid_json = "123"; // Number instead of string
        let result: Result<GameId, _> = serde_json::from_str(invalid_json);
        assert!(result.is_err());
    }

    #[test]
    fn test_game_id_deserialization_parse_error() {
        // Test deserialization error path for invalid octal string
        let invalid_octal = "\"999\""; // Invalid octal digit
        let result: Result<GameId, _> = serde_json::from_str(invalid_octal);
        assert!(result.is_err());
    }
}
