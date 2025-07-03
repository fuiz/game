//! Fuiz configuration and question management
//!
//! This module defines the core configuration structures for Fuiz games,
//! including the main `Fuiz` struct, slide configurations, and the runtime
//! state management for different question types. It provides the central
//! coordination layer that manages question flow and state transitions.

use web_time;

use garde::Validate;
use serde::{Deserialize, Serialize};

use crate::{
    leaderboard::Leaderboard,
    session::Tunnel,
    teams::TeamManager,
    watcher::{Id, ValueKind, Watchers},
    AlarmMessage, SyncMessage,
};

use super::{super::game::IncomingMessage, media::Media, multiple_choice, order, type_answer};

/// Represents content that can be either text or media
///
/// This enum allows questions and answers to include either plain text
/// or rich media content like images, providing flexibility in question design.
#[derive(Debug, Serialize, Deserialize, Clone, Validate)]
pub enum TextOrMedia {
    /// Media content (images, etc.)
    Media(#[garde(skip)] Media),
    /// Plain text content with length validation
    Text(#[garde(length(max = crate::constants::answer_text::MAX_LENGTH))] String),
}

/// A complete Fuiz configuration containing all questions and settings
///
/// This is the main configuration structure that defines an entire quiz game,
/// including the title and all slides/questions that will be presented to players.
#[derive(Debug, Serialize, Deserialize, Clone, Validate)]
pub struct Fuiz {
    /// The title of the Fuiz game (currently unused in gameplay)
    #[garde(length(max = crate::constants::fuiz::MAX_TITLE_LENGTH))]
    title: String,

    /// The collection of slides/questions in the game
    #[garde(length(max = crate::constants::fuiz::MAX_SLIDES_COUNT), dive)]
    pub slides: Vec<SlideConfig>,
}

/// Represents a currently active slide with its runtime state
///
/// This struct tracks which slide is currently being presented and
/// maintains its runtime state for player interactions and timing.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CurrentSlide {
    /// The index of the current slide in the slides vector
    pub index: usize,
    /// The runtime state of the current slide
    pub state: SlideState,
}

/// Configuration for a single slide/question
///
/// This enum represents the different types of questions that can be
/// included in a Fuiz game. Each variant contains the specific configuration
/// for that question type, including timing, content, and scoring parameters.
#[derive(Debug, Serialize, Deserialize, Clone, Validate)]
pub enum SlideConfig {
    /// A multiple choice question with predefined answer options
    MultipleChoice(#[garde(dive)] multiple_choice::SlideConfig),
    /// A type answer question where players enter free text
    TypeAnswer(#[garde(dive)] type_answer::SlideConfig),
    /// An order question where players arrange items in sequence
    Order(#[garde(dive)] order::SlideConfig),
}

impl SlideConfig {
    /// Converts this configuration into a runtime state
    ///
    /// This method creates the initial runtime state for a slide based on
    /// its configuration, preparing it for active gameplay.
    ///
    /// # Returns
    ///
    /// A new `SlideState` initialized from this configuration
    pub fn to_state(&self) -> SlideState {
        match self {
            Self::MultipleChoice(s) => SlideState::MultipleChoice(s.to_state()),
            Self::TypeAnswer(s) => SlideState::TypeAnswer(s.to_state()),
            Self::Order(s) => SlideState::Order(s.to_state()),
        }
    }
}

/// Runtime state for a slide during active gameplay
///
/// This enum represents the active state of a slide while it's being
/// presented to players. It maintains timing information, player responses,
/// and current phase information for each question type.
#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum SlideState {
    /// Runtime state for a multiple choice question
    MultipleChoice(multiple_choice::State),
    /// Runtime state for a type answer question
    TypeAnswer(type_answer::State),
    /// Runtime state for an order question
    Order(order::State),
}

impl Fuiz {
    /// Returns the number of slides in this Fuiz
    ///
    /// # Returns
    ///
    /// The total number of slides/questions in the game
    pub fn len(&self) -> usize {
        self.slides.len()
    }

    /// Checks if this Fuiz contains any slides
    ///
    /// # Returns
    ///
    /// `true` if there are no slides, `false` if there are slides
    pub fn is_empty(&self) -> bool {
        self.slides.is_empty()
    }
}

impl SlideState {
    /// Starts playing this slide and manages its lifecycle
    ///
    /// This method initiates the slide presentation, handles timing,
    /// and coordinates with the scheduling system for timed events.
    /// It delegates to the specific implementation for each question type.
    ///
    /// # Arguments
    ///
    /// * `team_manager` - Optional team manager for team-based games
    /// * `watchers` - The watchers manager for sending messages to participants
    /// * `schedule_message` - Function to schedule timed alarm messages
    /// * `tunnel_finder` - Function to find communication tunnels for participants
    /// * `index` - The current slide index
    /// * `count` - The total number of slides
    pub fn play<T: Tunnel, F: Fn(Id) -> Option<T>, S: FnMut(AlarmMessage, web_time::Duration)>(
        &mut self,
        team_manager: Option<&TeamManager>,
        watchers: &Watchers,
        schedule_message: S,
        tunnel_finder: F,
        index: usize,
        count: usize,
    ) {
        match self {
            Self::MultipleChoice(s) => {
                s.play(
                    team_manager,
                    watchers,
                    schedule_message,
                    tunnel_finder,
                    index,
                    count,
                );
            }
            Self::TypeAnswer(s) => {
                s.play(watchers, schedule_message, tunnel_finder, index, count);
            }
            Self::Order(s) => {
                s.play(watchers, schedule_message, tunnel_finder, index, count);
            }
        }
    }

    /// Processes an incoming message for this slide
    ///
    /// This method handles player and host messages during slide presentation,
    /// including answer submissions, host controls, and other interactions.
    /// It delegates to the specific implementation for each question type.
    ///
    /// # Arguments
    ///
    /// * `leaderboard` - The game's leaderboard for score tracking
    /// * `watchers` - The watchers manager for participant communication
    /// * `team_manager` - Optional team manager for team-based games
    /// * `schedule_message` - Function to schedule timed alarm messages
    /// * `watcher_id` - ID of the participant sending the message
    /// * `tunnel_finder` - Function to find communication tunnels
    /// * `message` - The incoming message to process
    /// * `index` - The current slide index
    /// * `count` - The total number of slides
    ///
    /// # Returns
    ///
    /// `true` if the message was successfully processed, `false` otherwise
    pub fn receive_message<
        T: Tunnel,
        F: Fn(Id) -> Option<T>,
        S: FnMut(AlarmMessage, web_time::Duration),
    >(
        &mut self,
        leaderboard: &mut Leaderboard,
        watchers: &Watchers,
        team_manager: Option<&TeamManager>,
        schedule_message: S,
        watcher_id: Id,
        tunnel_finder: F,
        message: IncomingMessage,
        index: usize,
        count: usize,
    ) -> bool {
        match self {
            Self::MultipleChoice(s) => s.receive_message(
                watcher_id,
                message,
                leaderboard,
                watchers,
                team_manager,
                schedule_message,
                tunnel_finder,
                index,
                count,
            ),
            Self::TypeAnswer(s) => s.receive_message(
                watcher_id,
                message,
                leaderboard,
                watchers,
                team_manager,
                schedule_message,
                tunnel_finder,
                index,
                count,
            ),
            Self::Order(s) => s.receive_message(
                watcher_id,
                message,
                leaderboard,
                watchers,
                team_manager,
                schedule_message,
                tunnel_finder,
                index,
                count,
            ),
        }
    }

    /// Generates a state synchronization message for a specific participant
    ///
    /// This method creates a sync message that allows a participant to
    /// synchronize their view with the current state of the slide.
    /// It's used when participants connect or reconnect during gameplay.
    ///
    /// # Arguments
    ///
    /// * `watcher_id` - ID of the participant requesting synchronization
    /// * `watcher_kind` - The type of participant (host, player, etc.)
    /// * `team_manager` - Optional team manager for team-based games
    /// * `watchers` - The watchers manager for participant information
    /// * `tunnel_finder` - Function to find communication tunnels
    /// * `index` - The current slide index
    /// * `count` - The total number of slides
    ///
    /// # Returns
    ///
    /// A `SyncMessage` containing the current slide state information
    pub fn state_message<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &self,
        watcher_id: Id,
        watcher_kind: ValueKind,
        team_manager: Option<&TeamManager>,
        watchers: &Watchers,
        tunnel_finder: F,
        index: usize,
        count: usize,
    ) -> SyncMessage {
        match self {
            Self::MultipleChoice(s) => SyncMessage::MultipleChoice(s.state_message(
                watcher_id,
                watcher_kind,
                team_manager,
                watchers,
                tunnel_finder,
                index,
                count,
            )),
            Self::TypeAnswer(s) => SyncMessage::TypeAnswer(s.state_message(
                watcher_id,
                watcher_kind,
                team_manager,
                watchers,
                tunnel_finder,
                index,
                count,
            )),
            Self::Order(s) => SyncMessage::Order(s.state_message(
                watcher_id,
                watcher_kind,
                team_manager,
                watchers,
                tunnel_finder,
                index,
                count,
            )),
        }
    }

    /// Processes a scheduled alarm message for this slide
    ///
    /// This method handles timed events that were previously scheduled,
    /// such as transitioning between slide phases, timing out answers,
    /// or triggering automatic state changes. It delegates to the specific
    /// implementation for each question type.
    ///
    /// # Arguments
    ///
    /// * `leaderboard` - The game's leaderboard for score tracking
    /// * `watchers` - The watchers manager for participant communication
    /// * `team_manager` - Optional team manager for team-based games
    /// * `schedule_message` - Function to schedule additional timed messages
    /// * `tunnel_finder` - Function to find communication tunnels
    /// * `message` - The alarm message being processed
    /// * `index` - The current slide index
    /// * `count` - The total number of slides
    ///
    /// # Returns
    ///
    /// `true` if the alarm was successfully processed, `false` otherwise
    pub fn receive_alarm<
        T: Tunnel,
        F: Fn(Id) -> Option<T>,
        S: FnMut(AlarmMessage, web_time::Duration),
    >(
        &mut self,
        leaderboard: &mut Leaderboard,
        watchers: &Watchers,
        team_manager: Option<&TeamManager>,
        schedule_message: &mut S,
        tunnel_finder: F,
        message: AlarmMessage,
        index: usize,
        count: usize,
    ) -> bool {
        match self {
            Self::MultipleChoice(s) => s.receive_alarm(
                leaderboard,
                watchers,
                team_manager,
                schedule_message,
                tunnel_finder,
                message,
                index,
                count,
            ),
            Self::TypeAnswer(s) => s.receive_alarm(
                leaderboard,
                watchers,
                team_manager,
                schedule_message,
                tunnel_finder,
                message,
                index,
                count,
            ),
            Self::Order(s) => s.receive_alarm(
                leaderboard,
                watchers,
                team_manager,
                schedule_message,
                tunnel_finder,
                message,
                index,
                count,
            ),
        }
    }
}
