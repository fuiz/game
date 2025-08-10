//! Shared traits and common functionality for slide implementations
//!
//! This module contains traits and helper functions that are common across
//! different question types (multiple_choice, type_answer, order), reducing
//! code duplication and providing consistent behavior.

use std::{
    collections::{HashMap, HashSet},
    time::Duration,
};

use itertools::Itertools;
use serde::{Deserialize, Serialize};
use web_time::SystemTime;

use crate::{
    leaderboard::Leaderboard,
    session::Tunnel,
    teams::TeamManager,
    watcher::{Id, ValueKind, Watchers},
};

/// Common slide states shared by all question types
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum SlideState {
    /// Initial state before the slide has started
    #[default]
    Unstarted,
    /// Displaying the question without answers
    Question,
    /// Accepting answers from players
    Answers,
    /// Displaying results with correct answers and statistics
    AnswersResults,
}

/// Validation result type for duration validation
type ValidationResult = garde::Result;

/// Validates that a duration falls within specified bounds.
///
/// This is a custom validation function for use with the `garde` crate.
/// It checks if the duration in seconds is within the inclusive range
/// defined by `MIN_SECONDS` and `MAX_SECONDS`.
///
/// # Arguments
///
/// * `val` - The `Duration` to validate.
/// * `_ctx` - The validation context (unused).
///
/// # Generics
///
/// * `MIN_SECONDS` - The minimum allowed duration in seconds (inclusive).
/// * `MAX_SECONDS` - The maximum allowed duration in seconds (inclusive).
///
/// # Errors
///
/// Returns a `garde::Error` if the duration is outside the specified bounds.
pub fn validate_duration<const MIN_SECONDS: u64, const MAX_SECONDS: u64>(
    val: &Duration,
    _ctx: &(),
) -> ValidationResult {
    if (MIN_SECONDS..=MAX_SECONDS).contains(&val.as_secs()) {
        Ok(())
    } else {
        Err(garde::Error::new(format!(
            "outside of bounds [{MIN_SECONDS},{MAX_SECONDS}]",
        )))
    }
}

/// Trait for basic slide state management functionality
pub trait SlideStateManager {
    /// Get the current slide state
    fn state(&self) -> SlideState;

    /// Attempt to change state from one to another
    /// Returns true if successful, false if current state doesn't match expected
    fn change_state(&mut self, before: SlideState, after: SlideState) -> bool;
}

/// Trait for slide timer management
pub trait SlideTimer {
    /// Get the answer start time, or None if not started
    fn answer_start(&self) -> Option<SystemTime>;

    /// Set the answer start time, or None if not started
    fn set_answer_start(&mut self, time: Option<SystemTime>);

    /// Start the timer by setting the current time
    fn start_timer(&mut self) {
        self.set_answer_start(Some(SystemTime::now()));
    }

    /// Get the timer start time, or current time if not set
    fn timer(&self) -> SystemTime {
        self.answer_start().unwrap_or(SystemTime::now())
    }

    /// Get the elapsed time since the timer started
    fn elapsed(&self) -> Duration {
        self.timer().elapsed().unwrap_or_default()
    }
}

/// Calculate score based on timing - shared function used by all slide types
pub fn calculate_slide_score(
    full_duration: Duration,
    taken_duration: Duration,
    full_points_awarded: u64,
) -> u64 {
    (full_points_awarded as f64
        * (1. - (taken_duration.as_secs_f64() / full_duration.as_secs_f64() / 2.))) as u64
}

/// Trait for slides that handle answers and scoring
pub trait AnswerHandler<AnswerType> {
    /// Get user answers with timestamps
    fn user_answers(&self) -> &HashMap<Id, (AnswerType, SystemTime)>;

    /// Get mutable user answers
    fn user_answers_mut(&mut self) -> &mut HashMap<Id, (AnswerType, SystemTime)>;

    /// Check if an answer is correct
    fn is_correct_answer(&self, answer: &AnswerType) -> bool;

    /// Get the maximum points for this slide
    fn max_points(&self) -> u64;

    /// Get the time limit for answers
    fn time_limit(&self) -> Duration;
}

/// Helper function to add scores to leaderboard (common across all slide types)
pub fn add_scores_to_leaderboard<T, F, AnswerType>(
    slide: &impl AnswerHandler<AnswerType>,
    timer: &impl SlideTimer,
    leaderboard: &mut Leaderboard,
    watchers: &Watchers,
    team_manager: Option<&TeamManager<crate::names::NameStyle>>,
    tunnel_finder: F,
) where
    T: Tunnel,
    F: Fn(Id) -> Option<T>,
    AnswerType: Clone,
{
    let starting_instant = timer.timer();

    leaderboard.add_scores(
        &slide
            .user_answers()
            .iter()
            .map(|(id, (answer, instant))| {
                let correct = slide.is_correct_answer(answer);
                (
                    *id,
                    if correct {
                        calculate_slide_score(
                            slide.time_limit(),
                            instant.duration_since(starting_instant).unwrap_or_default(),
                            slide.max_points(),
                        )
                    } else {
                        0
                    },
                    instant,
                )
            })
            .into_grouping_map_by(|(id, _, _)| {
                let player_id = *id;
                match &team_manager {
                    Some(team_manager) => team_manager.get_team(player_id).unwrap_or(player_id),
                    None => player_id,
                }
            })
            .min_by_key(|_, (_, _, instant)| *instant)
            .into_iter()
            .map(|(id, (_, score, _))| (id, score))
            .chain(
                {
                    match &team_manager {
                        Some(team_manager) => team_manager.all_ids(),
                        None => watchers
                            .specific_vec(ValueKind::Player, tunnel_finder)
                            .into_iter()
                            .map(|(x, _, _)| x)
                            .collect_vec(),
                    }
                }
                .into_iter()
                .map(|id| (id, 0)),
            )
            .unique_by(|(id, _)| *id)
            .collect_vec(),
    );
}

/// Helper function to check if all players have answered
pub fn all_players_answered<T, F, AnswerType>(
    slide: &impl AnswerHandler<AnswerType>,
    watchers: &Watchers,
    tunnel_finder: &F,
) -> bool
where
    T: Tunnel,
    F: Fn(Id) -> Option<T>,
{
    let left_set: HashSet<_> = watchers
        .specific_vec(ValueKind::Player, tunnel_finder)
        .iter()
        .map(|(w, _, _)| w.to_owned())
        .collect();
    let right_set: HashSet<_> = slide.user_answers().keys().copied().collect();
    left_set.is_subset(&right_set)
}

/// Helper function to get answered player count
pub fn get_answered_count<T, F, AnswerType>(
    slide: &impl AnswerHandler<AnswerType>,
    watchers: &Watchers,
    tunnel_finder: &F,
) -> usize
where
    T: Tunnel,
    F: Fn(Id) -> Option<T>,
{
    let left_set: HashSet<_> = watchers
        .specific_vec(ValueKind::Player, tunnel_finder)
        .iter()
        .map(|(w, _, _)| w.to_owned())
        .collect();
    let right_set: HashSet<_> = slide.user_answers().keys().copied().collect();
    left_set.intersection(&right_set).count()
}

/// Common interface for all question types to handle incoming messages
///
/// This trait abstracts the message handling logic that is common across
/// all question types, allowing for uniform treatment of different slide types.
pub trait QuestionReceiveMessage {
    /// Handle host "Next" command
    ///
    /// This method processes the host's request to advance to the next phase
    /// or complete the slide.
    ///
    /// # Arguments
    ///
    /// * `leaderboard` - Mutable reference to the game leaderboard
    /// * `watchers` - Connection manager for all participants
    /// * `team_manager` - Optional team manager for team-based games
    /// * `schedule_message` - Function to schedule delayed messages for timing
    /// * `tunnel_finder` - Function to find communication tunnels for participants
    /// * `index` - Current slide index in the game
    /// * `count` - Total number of slides in the game
    ///
    /// # Returns
    ///
    /// `true` if the slide is complete and should advance to the next slide, `false` otherwise
    ///
    /// # Type Parameters
    ///
    /// * `T` - Type implementing the Tunnel trait for participant communication
    /// * `F` - Function type for finding tunnels by participant ID
    /// * `S` - Function type for scheduling alarm messages
    fn receive_host_next<
        T: Tunnel,
        F: Fn(Id) -> Option<T>,
        S: FnMut(crate::AlarmMessage, Duration),
    >(
        &mut self,
        leaderboard: &mut Leaderboard,
        watchers: &Watchers,
        team_manager: Option<&TeamManager<crate::names::NameStyle>>,
        schedule_message: S,
        tunnel_finder: F,
        index: usize,
        count: usize,
    ) -> bool;

    /// Handle player messages
    ///
    /// This method processes player-specific messages like answer submissions
    /// and other player interactions.
    ///
    /// # Arguments
    ///
    /// * `watcher_id` - ID of the player sending the message
    /// * `message` - The player message to process
    /// * `leaderboard` - Mutable reference to the game leaderboard
    /// * `watchers` - Connection manager for all participants
    /// * `team_manager` - Optional team manager for team-based games
    /// * `schedule_message` - Function to schedule delayed messages for timing
    /// * `tunnel_finder` - Function to find communication tunnels for participants
    /// * `index` - Current slide index in the game
    /// * `count` - Total number of slides in the game
    ///
    /// # Type Parameters
    ///
    /// * `T` - Type implementing the Tunnel trait for participant communication
    /// * `F` - Function type for finding tunnels by participant ID
    /// * `S` - Function type for scheduling alarm messages
    fn receive_player_message<
        T: Tunnel,
        F: Fn(Id) -> Option<T>,
        S: FnMut(crate::AlarmMessage, Duration),
    >(
        &mut self,
        watcher_id: Id,
        message: crate::game::IncomingPlayerMessage,
        leaderboard: &mut Leaderboard,
        watchers: &Watchers,
        team_manager: Option<&TeamManager<crate::names::NameStyle>>,
        schedule_message: S,
        tunnel_finder: F,
        index: usize,
        count: usize,
    );

    /// Combined message handler that delegates to specific handlers
    ///
    /// This method provides a unified interface for handling both host and player
    /// messages by delegating to the appropriate specific handler method.
    ///
    /// # Arguments
    ///
    /// * `watcher_id` - ID of the participant sending the message
    /// * `message` - The incoming message to process
    /// * `leaderboard` - Mutable reference to the game leaderboard
    /// * `watchers` - Connection manager for all participants
    /// * `team_manager` - Optional team manager for team-based games
    /// * `schedule_message` - Function to schedule delayed messages for timing
    /// * `tunnel_finder` - Function to find communication tunnels for participants
    /// * `index` - Current slide index in the game
    /// * `count` - Total number of slides in the game
    ///
    /// # Returns
    ///
    /// `true` if the slide is complete and should advance to the next slide, `false` otherwise
    ///
    /// # Type Parameters
    ///
    /// * `T` - Type implementing the Tunnel trait for participant communication
    /// * `F` - Function type for finding tunnels by participant ID
    /// * `S` - Function type for scheduling alarm messages
    fn receive_message<
        T: Tunnel,
        F: Fn(Id) -> Option<T>,
        S: FnMut(crate::AlarmMessage, Duration),
    >(
        &mut self,
        watcher_id: Id,
        message: crate::game::IncomingMessage,
        leaderboard: &mut Leaderboard,
        watchers: &Watchers,
        team_manager: Option<&TeamManager<crate::names::NameStyle>>,
        schedule_message: S,
        tunnel_finder: F,
        index: usize,
        count: usize,
    ) -> bool {
        match message {
            crate::game::IncomingMessage::Host(crate::game::IncomingHostMessage::Next) => self
                .receive_host_next(
                    leaderboard,
                    watchers,
                    team_manager,
                    schedule_message,
                    tunnel_finder,
                    index,
                    count,
                ),
            crate::game::IncomingMessage::Player(player_message) => {
                self.receive_player_message(
                    watcher_id,
                    player_message,
                    leaderboard,
                    watchers,
                    team_manager,
                    schedule_message,
                    tunnel_finder,
                    index,
                    count,
                );
                false
            }
            crate::game::IncomingMessage::Host(
                crate::game::IncomingHostMessage::Index(_)
                | crate::game::IncomingHostMessage::Lock(_),
            )
            | crate::game::IncomingMessage::Ghost(_)
            | crate::game::IncomingMessage::Unassigned(_) => false,
        }
    }
}
