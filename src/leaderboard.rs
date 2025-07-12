//! Leaderboard and scoring functionality
//!
//! This module manages the scoring system for Fuiz games, including
//! tracking points earned by players/teams across multiple questions,
//! maintaining leaderboards, and providing score summaries and statistics.

use itertools::Itertools;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::{TruncatedVec, watcher::Id};

/// Summary of final game statistics and player performance
///
/// This struct contains aggregated data about the completed game,
/// including per-question statistics and individual player scores.
#[derive(Debug, Clone)]
pub struct FinalSummary {
    /// For each slide, tuple of (players who earned points, players who didn't)
    stats: Vec<(usize, usize)>,
    /// For each player, the points they earned on each slide
    mapping: HashMap<Id, Vec<u64>>,
}

/// Serialization helper for Leaderboard struct
#[derive(Deserialize)]
struct LeaderboardSerde {
    points_earned: Vec<Vec<(Id, u64)>>,
}

/// Manages scoring and leaderboard functionality for a game session
///
/// This struct tracks points earned by players across all questions,
/// maintains sorted leaderboards, and provides various scoring views
/// and statistics for the game.
#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(from = "LeaderboardSerde")]
pub struct Leaderboard {
    /// Points earned by each player for each slide/question
    points_earned: Vec<Vec<(Id, u64)>>,

    /// Previous round's scores in descending order (cached)
    #[serde(skip)]
    previous_scores_descending: Vec<(Id, u64)>,
    /// Current scores in descending order (cached)
    #[serde(skip)]
    scores_descending: Vec<(Id, u64)>,
    /// Mapping from player ID to their total score and leaderboard position (cached)
    #[serde(skip)]
    score_and_position: HashMap<Id, (u64, usize)>,
    /// Final game summary (computed once when needed)
    #[serde(skip)]
    final_summary: once_cell_serde::sync::OnceCell<FinalSummary>,
}

impl From<LeaderboardSerde> for Leaderboard {
    /// Reconstructs the Leaderboard from serialized data
    ///
    /// This rebuilds all the cached score data from the points earned data,
    /// which is necessary since the cached fields are not serialized.
    fn from(serde: LeaderboardSerde) -> Self {
        let total_score_mapping = serde
            .points_earned
            .iter()
            .flat_map(|points_earned| points_earned.iter().copied())
            .sorted_by_key(|(id, _)| *id)
            .coalesce(|(id1, points1), (id2, points2)| {
                if id1 == id2 {
                    Ok((id1, points1 + points2))
                } else {
                    Err(((id1, points1), (id2, points2)))
                }
            })
            .collect::<HashMap<_, _>>();

        let previous_total_score_mapping = serde
            .points_earned
            .iter()
            .rev()
            .skip(1)
            .rev()
            .flat_map(|points_earned| points_earned.iter().copied())
            .sorted_by_key(|(id, _)| *id)
            .coalesce(|(id1, points1), (id2, points2)| {
                if id1 == id2 {
                    Ok((id1, points1 + points2))
                } else {
                    Err(((id1, points1), (id2, points2)))
                }
            })
            .collect::<HashMap<_, _>>();

        let scores_descending = total_score_mapping
            .iter()
            .sorted_by_key(|(_, points)| *points)
            .rev()
            .map(|(id, points)| (*id, *points))
            .collect_vec();

        let previous_scores_descending = previous_total_score_mapping
            .iter()
            .sorted_by_key(|(_, points)| *points)
            .rev()
            .map(|(id, points)| (*id, *points))
            .collect_vec();

        let score_and_position = scores_descending
            .iter()
            .enumerate()
            .map(|(i, (id, p))| (*id, (*p, i)))
            .collect();

        Leaderboard {
            points_earned: serde.points_earned,
            previous_scores_descending,
            scores_descending,
            score_and_position,
            final_summary: once_cell_serde::sync::OnceCell::new(),
        }
    }
}

/// Score information for a player or team
///
/// Contains the player's current score and their position in the leaderboard.
/// This information is sent to players so they can see their performance.
#[derive(Debug, Serialize, Clone, Copy)]
pub struct ScoreMessage {
    /// Total points earned by the player or team
    pub points: u64,
    /// Current position in the leaderboard (1-indexed)
    pub position: usize,
}

impl Leaderboard {
    /// Adds new scores for a round and updates leaderboard standings
    ///
    /// Processes the scores earned by players in the latest question/round,
    /// updates total scores, recalculates leaderboard positions, and caches
    /// the sorted standings for efficient access.
    ///
    /// # Arguments
    ///
    /// * `scores` - Slice of (player_id, points_earned) tuples for the round
    ///
    /// # Examples
    ///
    /// ```rust
    /// use fuiz::watcher::Id;
    /// use fuiz::leaderboard::Leaderboard;
    ///
    /// let mut leaderboard = Leaderboard::default();
    /// let player1_id = Id::new();
    /// let player2_id = Id::new();
    /// let player3_id = Id::new();
    /// let scores = [(player1_id, 100), (player2_id, 75), (player3_id, 50)];
    /// leaderboard.add_scores(&scores);
    /// ```
    pub fn add_scores(&mut self, scores: &[(Id, u64)]) {
        let mut summary: HashMap<Id, u64> = self
            .score_and_position
            .iter()
            .map(|(id, (points, _))| (*id, *points))
            .collect();

        for (id, points) in scores {
            *summary.entry(*id).or_default() += points;
        }

        let scores_descending = summary
            .iter()
            .sorted_by_key(|(_, points)| *points)
            .rev()
            .map(|(a, b)| (*a, *b))
            .collect_vec();

        let mapping = scores_descending
            .iter()
            .enumerate()
            .map(|(position, (id, points))| (*id, (*points, position)))
            .collect();

        self.points_earned.push(scores.to_vec());

        self.previous_scores_descending =
            std::mem::replace(&mut self.scores_descending, scores_descending);

        self.score_and_position = mapping;
    }

    /// Returns the current and previous leaderboard standings
    ///
    /// Provides the last two sets of leaderboard standings (current and previous)
    /// as truncated vectors suitable for display to clients. This allows showing
    /// changes in position between rounds.
    ///
    /// # Returns
    ///
    /// An array containing [current_standings, previous_standings] as TruncatedVec
    /// where each entry is (player_id, total_score)
    pub fn last_two_scores_descending(&self) -> [TruncatedVec<(Id, u64)>; 2] {
        const LIMIT: usize = 50;

        [
            TruncatedVec::new(
                self.scores_descending.iter().copied(),
                LIMIT,
                self.scores_descending.len(),
            ),
            TruncatedVec::new(
                self.previous_scores_descending.iter().copied(),
                LIMIT,
                self.previous_scores_descending.len(),
            ),
        ]
    }

    /// Computes comprehensive final game statistics
    ///
    /// Generates detailed statistics about the completed game, including
    /// per-question performance data and individual player score breakdowns.
    /// The statistics can show either real scores or normalized scores (0/1).
    ///
    /// # Arguments
    ///
    /// * `show_real_score` - Whether to show actual point values or binary (0/1) scores
    ///
    /// # Returns
    ///
    /// A FinalSummary containing detailed game statistics and player performance data
    fn compute_final_summary(&self, show_real_score: bool) -> FinalSummary {
        let map_score = |s: u64| {
            if show_real_score { s } else { s.min(1) }
        };

        FinalSummary {
            stats: self
                .points_earned
                .iter()
                .map(|points_earned| {
                    let earned_count = points_earned
                        .iter()
                        .filter(|(_, earned)| *earned > 0)
                        .count();

                    (earned_count, points_earned.len() - earned_count)
                })
                .collect(),
            mapping: self
                .points_earned
                .iter()
                .map(|points_earned| {
                    points_earned
                        .iter()
                        .map(|(id, points)| (*id, map_score(*points)))
                        .collect::<HashMap<_, _>>()
                })
                .enumerate()
                .fold(
                    HashMap::new(),
                    |mut aggregate_score_mapping, (slide_index, slide_score_mapping)| {
                        for (id, points) in slide_score_mapping {
                            aggregate_score_mapping.entry(id).or_default().push(points);
                        }
                        for (_, v) in aggregate_score_mapping.iter_mut() {
                            v.resize(slide_index + 1, 0);
                        }
                        aggregate_score_mapping
                    },
                ),
        }
    }

    /// Gets or computes the final game summary with caching
    ///
    /// Returns the final game summary, computing it if necessary and caching
    /// the result for subsequent calls. This is more efficient than recomputing
    /// statistics multiple times.
    ///
    /// # Arguments
    ///
    /// * `show_real_score` - Whether to show actual point values or binary (0/1) scores
    ///
    /// # Returns
    ///
    /// A reference to the cached FinalSummary
    fn final_summary(&self, show_real_score: bool) -> &FinalSummary {
        self.final_summary
            .get_or_init(|| self.compute_final_summary(show_real_score))
    }

    /// Generates summary statistics for the game host
    ///
    /// Provides aggregated statistics suitable for the game host's view,
    /// including the total number of players and per-question performance
    /// statistics (how many players earned points vs. didn't).
    ///
    /// # Arguments
    ///
    /// * `show_real_score` - Whether to use actual point values or binary scoring
    ///
    /// # Returns
    ///
    /// A tuple of (total_player_count, per_question_stats) where per_question_stats
    /// is a vector of (players_who_earned_points, players_who_didn't) for each question
    pub fn host_summary(&self, show_real_score: bool) -> (usize, Vec<(usize, usize)>) {
        let final_summary = self.final_summary(show_real_score);

        (final_summary.mapping.len(), final_summary.stats.clone())
    }

    /// Generates detailed score breakdown for a specific player
    ///
    /// Provides a player's individual performance across all questions,
    /// showing the points they earned on each question. If the player
    /// didn't participate in all questions, missing scores are filled with zeros.
    ///
    /// # Arguments
    ///
    /// * `id` - The player's unique identifier
    /// * `show_real_score` - Whether to use actual point values or binary scoring
    ///
    /// # Returns
    ///
    /// A vector containing the player's score for each question in order
    pub fn player_summary(&self, id: Id, show_real_score: bool) -> Vec<u64> {
        self.final_summary(show_real_score)
            .mapping
            .get(&id)
            .map_or(vec![0; self.points_earned.len()], std::clone::Clone::clone)
    }

    /// Gets the current score and position for a specific player
    ///
    /// Retrieves a player's current total score and their position in the
    /// leaderboard rankings. This is used for displaying individual player
    /// status and progress.
    ///
    /// # Arguments
    ///
    /// * `watcher_id` - The player's unique identifier
    ///
    /// # Returns
    ///
    /// `Some(ScoreMessage)` containing the player's points and leaderboard position,
    /// or `None` if the player has no recorded scores
    pub fn score(&self, watcher_id: Id) -> Option<ScoreMessage> {
        let (points, position) = self.score_and_position.get(&watcher_id)?;
        Some(ScoreMessage {
            points: *points,
            position: *position,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_leaderboard_add_scores_single_round() {
        let mut leaderboard = Leaderboard::default();
        let id1 = Id::new();
        let id2 = Id::new();
        let id3 = Id::new();

        let scores = [(id1, 100), (id2, 75), (id3, 50)];
        leaderboard.add_scores(&scores);

        // Check that scores are stored
        assert_eq!(leaderboard.points_earned.len(), 1);
        assert_eq!(leaderboard.points_earned[0], scores);

        // Check score messages
        let score1 = leaderboard.score(id1).unwrap();
        assert_eq!(score1.points, 100);
        assert_eq!(score1.position, 0); // First place (0-indexed)

        let score2 = leaderboard.score(id2).unwrap();
        assert_eq!(score2.points, 75);
        assert_eq!(score2.position, 1); // Second place

        let score3 = leaderboard.score(id3).unwrap();
        assert_eq!(score3.points, 50);
        assert_eq!(score3.position, 2); // Third place
    }

    #[test]
    fn test_leaderboard_add_scores_multiple_rounds() {
        let mut leaderboard = Leaderboard::default();
        let id1 = Id::new();
        let id2 = Id::new();

        // First round
        leaderboard.add_scores(&[(id1, 50), (id2, 100)]);

        // After first round, id2 should be first
        assert_eq!(leaderboard.score(id2).unwrap().position, 0);
        assert_eq!(leaderboard.score(id1).unwrap().position, 1);

        // Second round - id1 catches up
        leaderboard.add_scores(&[(id1, 100), (id2, 25)]);

        // After second round, id1 should be first (150 vs 125)
        assert_eq!(leaderboard.score(id1).unwrap().points, 150);
        assert_eq!(leaderboard.score(id1).unwrap().position, 0);
        assert_eq!(leaderboard.score(id2).unwrap().points, 125);
        assert_eq!(leaderboard.score(id2).unwrap().position, 1);
    }

    #[test]
    fn test_leaderboard_tied_scores() {
        let mut leaderboard = Leaderboard::default();
        let id1 = Id::new();
        let id2 = Id::new();

        leaderboard.add_scores(&[(id1, 100), (id2, 100)]);

        // Both should have same score but different positions
        assert_eq!(leaderboard.score(id1).unwrap().points, 100);
        assert_eq!(leaderboard.score(id2).unwrap().points, 100);

        // Positions should be consecutive (0 and 1)
        let pos1 = leaderboard.score(id1).unwrap().position;
        let pos2 = leaderboard.score(id2).unwrap().position;
        assert!(pos1 != pos2);
        assert!(pos1 <= 1 && pos2 <= 1);
    }

    #[test]
    fn test_leaderboard_score_nonexistent_player() {
        let leaderboard = Leaderboard::default();
        let nonexistent_id = Id::new();

        assert!(leaderboard.score(nonexistent_id).is_none());
    }

    #[test]
    fn test_leaderboard_last_two_scores_descending() {
        let mut leaderboard = Leaderboard::default();
        let id1 = Id::new();
        let id2 = Id::new();

        // First round
        leaderboard.add_scores(&[(id1, 50), (id2, 100)]);

        // Second round
        leaderboard.add_scores(&[(id1, 100), (id2, 25)]);

        let [current, previous] = leaderboard.last_two_scores_descending();

        // Current should show id1 first (150 total)
        assert_eq!(current.items()[0].0, id1);
        assert_eq!(current.items()[0].1, 150);
        assert_eq!(current.items()[1].0, id2);
        assert_eq!(current.items()[1].1, 125);

        // Previous should show id2 first (100 total from first round)
        assert_eq!(previous.items()[0].0, id2);
        assert_eq!(previous.items()[0].1, 100);
        assert_eq!(previous.items()[1].0, id1);
        assert_eq!(previous.items()[1].1, 50);
    }

    #[test]
    fn test_leaderboard_host_summary() {
        let mut leaderboard = Leaderboard::default();
        let id1 = Id::new();
        let id2 = Id::new();
        let id3 = Id::new();

        // Round 1: 2 players earn points, 1 doesn't
        leaderboard.add_scores(&[(id1, 100), (id2, 50), (id3, 0)]);

        // Round 2: All 3 players earn points
        leaderboard.add_scores(&[(id1, 75), (id2, 25), (id3, 10)]);

        let (player_count, stats) = leaderboard.host_summary(true);

        assert_eq!(player_count, 3);
        assert_eq!(stats.len(), 2); // Two rounds

        // Round 1: 2 earned points, 1 didn't
        assert_eq!(stats[0], (2, 1));

        // Round 2: 3 earned points, 0 didn't
        assert_eq!(stats[1], (3, 0));
    }

    #[test]
    fn test_leaderboard_host_summary_binary_scoring() {
        let mut leaderboard = Leaderboard::default();
        let id1 = Id::new();
        let id2 = Id::new();

        // Add scores with different values
        leaderboard.add_scores(&[(id1, 100), (id2, 0)]);

        let (player_count, stats) = leaderboard.host_summary(false);

        assert_eq!(player_count, 2);
        assert_eq!(stats[0], (1, 1)); // Only one player earned points (binary)
    }

    #[test]
    fn test_leaderboard_player_summary_real_scoring() {
        let mut leaderboard = Leaderboard::default();
        let id1 = Id::new();
        let id2 = Id::new();

        leaderboard.add_scores(&[(id1, 100), (id2, 50)]);
        leaderboard.add_scores(&[(id1, 25), (id2, 75)]);

        // Test real scoring first (this caches the result)
        let summary1 = leaderboard.player_summary(id1, true);
        assert_eq!(summary1, vec![100, 25]);

        let summary2 = leaderboard.player_summary(id2, true);
        assert_eq!(summary2, vec![50, 75]);
    }

    #[test]
    fn test_leaderboard_player_summary_binary_scoring() {
        let mut leaderboard = Leaderboard::default();
        let id1 = Id::new();
        let id2 = Id::new();

        leaderboard.add_scores(&[(id1, 100), (id2, 0)]);
        leaderboard.add_scores(&[(id1, 0), (id2, 50)]);

        // Test binary scoring first (this caches the result with binary mode)
        let summary1_binary = leaderboard.player_summary(id1, false);
        assert_eq!(summary1_binary, vec![1, 0]); // 100 -> 1, 0 -> 0

        let summary2_binary = leaderboard.player_summary(id2, false);
        assert_eq!(summary2_binary, vec![0, 1]); // 0 -> 0, 50 -> 1
    }

    #[test]
    fn test_leaderboard_player_summary_nonexistent() {
        let mut leaderboard = Leaderboard::default();
        let id1 = Id::new();
        let nonexistent_id = Id::new();

        leaderboard.add_scores(&[(id1, 100)]);
        leaderboard.add_scores(&[(id1, 50)]);

        let summary = leaderboard.player_summary(nonexistent_id, true);
        assert_eq!(summary, vec![0, 0]); // Should return zeros for all rounds
    }

    #[test]
    fn test_leaderboard_serialization_deserialization() {
        let mut original = Leaderboard::default();
        let id1 = Id::new();
        let id2 = Id::new();

        original.add_scores(&[(id1, 100), (id2, 50)]);
        original.add_scores(&[(id1, 25), (id2, 75)]);

        // Serialize
        let serialized = serde_json::to_string(&original).unwrap();

        // Deserialize
        let deserialized: Leaderboard = serde_json::from_str(&serialized).unwrap();

        // Check that scores are preserved
        assert_eq!(deserialized.score(id1).unwrap().points, 125);
        assert_eq!(deserialized.score(id2).unwrap().points, 125);

        // Check that cached data is rebuilt correctly
        let [current, previous] = deserialized.last_two_scores_descending();
        assert_eq!(current.exact_count(), 2);
        assert_eq!(previous.exact_count(), 2);
    }

    #[test]
    fn test_leaderboard_zero_scores() {
        let mut leaderboard = Leaderboard::default();
        let id1 = Id::new();

        leaderboard.add_scores(&[(id1, 0)]);

        let score = leaderboard.score(id1).unwrap();
        assert_eq!(score.points, 0);
        assert_eq!(score.position, 0);
    }

    #[test]
    fn test_score_message_serialization() {
        let score_msg = ScoreMessage {
            points: 150,
            position: 2,
        };

        let serialized = serde_json::to_string(&score_msg).unwrap();
        assert!(serialized.contains("150"));
        assert!(serialized.contains("2"));
    }
}
