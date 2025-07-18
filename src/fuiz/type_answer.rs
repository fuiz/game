//! Type answer (free text) question implementation
//!
//! This module implements the type answer question type for Fuiz games.
//! Type answer questions present a question and allow players to submit
//! free text responses. The system supports multiple acceptable answers
//! and uses fuzzy matching to determine correctness.

use std::{
    collections::{HashMap, HashSet},
    time::{self, Duration},
};

use garde::Validate;
use itertools::Itertools;
use serde::{Deserialize, Serialize};
use serde_with::skip_serializing_none;
use web_time::SystemTime;

use crate::{
    leaderboard::Leaderboard,
    session::Tunnel,
    teams::TeamManager,
    watcher::{Id, ValueKind, Watchers},
};

use super::{
    super::game::{IncomingHostMessage, IncomingMessage, IncomingPlayerMessage},
    media::Media,
};

/// Represents the current phase of a type answer slide
///
/// Type answer questions progress through phases similar to multiple choice:
/// first showing the question, then accepting text input from players,
/// and finally showing results with accepted answers.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum SlideState {
    /// Initial state before the slide has started (treated same as Question)
    #[default]
    Unstarted,
    /// Displaying the question without input field
    Question,
    /// Accepting text input from players
    Answers,
    /// Displaying results with correct answers and statistics
    AnswersResults,
}

type ValidationResult = garde::Result;

/// Validates that a duration falls within specified bounds
///
/// # Arguments
/// * `field` - Name of the field being validated for error messages
/// * `val` - Duration to validate
///
/// # Returns
/// * `Ok(())` if the duration is within bounds
/// * `Err(garde::Error)` if the duration is outside bounds
fn validate_duration<const MIN_SECONDS: u64, const MAX_SECONDS: u64>(
    field: &'static str,
    val: &Duration,
) -> ValidationResult {
    if (MIN_SECONDS..=MAX_SECONDS).contains(&val.as_secs()) {
        Ok(())
    } else {
        Err(garde::Error::new(format!(
            "{field} is outside of the bounds [{MIN_SECONDS},{MAX_SECONDS}]",
        )))
    }
}

/// Validates that a time limit is within acceptable bounds for type answer questions
fn validate_time_limit(val: &Duration) -> ValidationResult {
    validate_duration::<
        { crate::constants::type_answer::MIN_TIME_LIMIT },
        { crate::constants::type_answer::MAX_TIME_LIMIT },
    >("time_limit", val)
}

/// Validates that the question introduction duration is within acceptable bounds
fn validate_introduce_question(val: &Duration) -> ValidationResult {
    validate_duration::<
        { crate::constants::type_answer::MIN_INTRODUCE_QUESTION },
        { crate::constants::type_answer::MAX_INTRODUCE_QUESTION },
    >("introduce_question", val)
}

#[serde_with::serde_as]
#[skip_serializing_none]
/// Configuration for a type answer slide
///
/// Contains all the settings and content for a single type answer question,
/// including the question text, media, timing, and acceptable answers.
#[derive(Debug, Clone, Serialize, serde::Deserialize, Validate)]
pub struct SlideConfig {
    /// The question title, represents what's being asked
    #[garde(length(chars, min = crate::constants::type_answer::MIN_TITLE_LENGTH, max = crate::constants::type_answer::MAX_TITLE_LENGTH))]
    title: String,
    /// Accompanying media
    #[garde(dive)]
    media: Option<Media>,
    /// Time before the answers are displayed
    #[garde(custom(|v, _| validate_introduce_question(v)))]
    #[serde_as(as = "serde_with::DurationMilliSeconds<u64>")]
    #[serde(default)]
    introduce_question: Duration,
    /// Time where players can answer the question
    #[garde(custom(|v, _| validate_time_limit(v)))]
    #[serde_as(as = "serde_with::DurationMilliSeconds<u64>")]
    time_limit: Duration,
    /// Maximum number of points awarded the question, decreases linearly to half the amount by the end of the slide
    #[garde(skip)]
    points_awarded: u64,
    /// List of acceptable text answers for this question
    #[garde(length(max = crate::constants::type_answer::MAX_ANSWER_COUNT),
        inner(length(chars, max = crate::constants::answer_text::MAX_LENGTH))
    )]
    answers: Vec<String>,
    /// Whether answer matching should be case-sensitive
    #[garde(skip)]
    #[serde(default)]
    case_sensitive: bool,
}

/// Runtime state for a type answer slide
///
/// Tracks the current state of the slide including player answers,
/// timing information, and the current phase of the question.
#[serde_with::serde_as]
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct State {
    /// The configuration this state was created from
    config: SlideConfig,

    // Runtime State
    /// Player text answers with submission timestamps
    user_answers: HashMap<Id, (String, SystemTime)>,
    /// Time when text input was first enabled for players
    answer_start: Option<SystemTime>,
    /// Current phase of the slide presentation
    state: SlideState,
}

impl SlideConfig {
    /// Creates a new runtime state from this configuration
    ///
    /// This method initializes a fresh state for gameplay, setting up
    /// empty answer tracking and the initial unstarted phase.
    ///
    /// # Returns
    ///
    /// A new `State` ready for gameplay
    pub fn to_state(&self) -> State {
        State {
            config: self.clone(),
            user_answers: HashMap::default(),
            answer_start: Option::default(),
            state: SlideState::default(),
        }
    }
}

/// Messages sent to the listeners to update their pre-existing state with the slide state
#[serde_with::serde_as]
#[skip_serializing_none]
#[derive(Debug, Serialize, Clone)]
pub enum UpdateMessage {
    /// Announcement of the question without its answers
    QuestionAnnouncement {
        /// Index of the slide (0-indexing)
        index: usize,
        /// Total count of slides
        count: usize,
        /// Question text (i.e. what's being asked)
        question: String,
        /// Accompanying media
        media: Option<Media>,
        /// Time before answers will be release
        #[serde_as(as = "serde_with::DurationMilliSeconds<u64>")]
        duration: Duration,
        /// Accept answers from players
        accept_answers: bool,
    },
    /// (HOST ONLY): Number of players who answered the question
    AnswersCount(usize),
    /// Results of the game including correct answers and statistics of how many they got chosen
    AnswersResults {
        /// Correct answers
        answers: Vec<String>,
        /// Statistics of how many times each answer was chosen
        results: Vec<(String, usize)>,
        /// Case-sensitive check for answers
        case_sensitive: bool,
    },
}

/// Messages used for scheduled state transitions in type answer slides
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AlarmMessage {
    /// Triggers a transition from one slide state to another
    ProceedFromSlideIntoSlide {
        /// Index of the slide being transitioned
        index: usize,
        /// Target state to transition to
        to: SlideState,
    },
}

/// Messages sent to the listeners who lack preexisting state to synchronize their state.
///
/// See [`UpdateMessage`] for explaination of these fields.
#[serde_with::serde_as]
#[skip_serializing_none]
#[derive(Debug, Serialize, Clone)]
pub enum SyncMessage {
    /// Announcement of the question without its answers
    QuestionAnnouncement {
        /// Index of the current slide
        index: usize,
        /// Total number of slides in the game
        count: usize,
        /// The question text being asked
        question: String,
        /// Optional media content accompanying the question
        media: Option<Media>,
        /// Remaining time for the question to be displayed without its answers
        #[serde_as(as = "serde_with::DurationMilliSeconds<u64>")]
        duration: Duration,
        /// Whether to accept text answers from players
        accept_answers: bool,
    },
    /// Results of the game including correct answers and statistics of how many they got chosen
    AnswersResults {
        /// Index of the current slide
        index: usize,
        /// Total number of slides in the game
        count: usize,
        /// The question text that was asked
        question: String,
        /// Optional media content that accompanied the question
        media: Option<Media>,
        /// Correct answers for this question
        answers: Vec<String>,
        /// Statistics of player submissions: (answer_text, count)
        results: Vec<(String, usize)>,
        /// Whether the answer matching was case-sensitive
        case_sensitive: bool,
    },
}

/// Normalizes an answer string for comparison
///
/// # Arguments
/// * `answer` - The answer string to clean
/// * `case_sensitive` - Whether to preserve case sensitivity
///
/// # Returns
/// * Cleaned answer string (trimmed and optionally lowercased)
fn clean_answer(answer: &str, case_sensitive: bool) -> String {
    if case_sensitive {
        answer.trim().to_string()
    } else {
        answer.trim().to_lowercase()
    }
}

impl State {
    /// Starts the type answer slide by sending initial question announcements
    ///
    /// # Arguments
    /// * `watchers` - Connection manager for players and hosts
    /// * `schedule_message` - Function to schedule delayed messages
    /// * `tunnel_finder` - Function to find communication tunnels for specific watchers
    /// * `index` - Current slide index
    /// * `count` - Total number of slides
    pub fn play<
        T: Tunnel,
        F: Fn(Id) -> Option<T>,
        S: FnMut(crate::AlarmMessage, time::Duration),
    >(
        &mut self,
        watchers: &Watchers,
        schedule_message: S,
        tunnel_finder: F,
        index: usize,
        count: usize,
    ) {
        self.send_question_announcements(watchers, schedule_message, tunnel_finder, index, count);
    }

    /// Calculates the score for a player based on how quickly they answered
    ///
    /// Score decreases linearly from full points to half points over the duration.
    ///
    /// # Arguments
    /// * `full_duration` - Total time allowed for the question
    /// * `taken_duration` - Time taken by the player to answer
    /// * `full_points_awarded` - Maximum points possible for this question
    ///
    /// # Returns
    /// * Points awarded (between half and full points)
    fn calculate_score(
        full_duration: Duration,
        taken_duration: Duration,
        full_points_awarded: u64,
    ) -> u64 {
        (full_points_awarded as f64
            * (1. - (taken_duration.as_secs_f64() / full_duration.as_secs_f64() / 2.)))
            as u64
    }

    /// Starts the timer for the current phase of the slide
    fn start_timer(&mut self) {
        self.answer_start = Some(SystemTime::now());
    }

    /// Returns the start time of the current phase
    ///
    /// # Returns
    /// * `SystemTime` when the current phase started, or current time if not set
    fn timer(&self) -> SystemTime {
        self.answer_start.unwrap_or(SystemTime::now())
    }

    /// Sends the initial question announcement to all watchers
    ///
    /// This method handles the transition from Unstarted to Question state,
    /// announcing the question text and media before accepting answers.
    ///
    /// # Arguments
    /// * `watchers` - Connection manager for players and hosts
    /// * `schedule_message` - Function to schedule delayed messages
    /// * `tunnel_finder` - Function to find communication tunnels
    /// * `index` - Current slide index
    /// * `count` - Total number of slides
    fn send_question_announcements<
        T: Tunnel,
        F: Fn(Id) -> Option<T>,
        S: FnMut(crate::AlarmMessage, time::Duration),
    >(
        &mut self,
        watchers: &Watchers,
        mut schedule_message: S,
        tunnel_finder: F,
        index: usize,
        count: usize,
    ) {
        if self.change_state(SlideState::Unstarted, SlideState::Question) {
            if self.config.introduce_question.is_zero() {
                self.send_accepting_answers(
                    watchers,
                    schedule_message,
                    tunnel_finder,
                    index,
                    count,
                );
                return;
            }

            self.start_timer();

            watchers.announce(
                &UpdateMessage::QuestionAnnouncement {
                    index,
                    count,
                    question: self.config.title.clone(),
                    media: self.config.media.clone(),
                    duration: self.config.introduce_question,
                    accept_answers: false,
                }
                .into(),
                tunnel_finder,
            );

            schedule_message(
                AlarmMessage::ProceedFromSlideIntoSlide {
                    index,
                    to: SlideState::Answers,
                }
                .into(),
                self.config.introduce_question,
            );
        }
    }

    /// Transitions to accepting answers from players
    ///
    /// This method handles the transition from Question to Answers state,
    /// enabling the answer input field and starting the answer timer.
    ///
    /// # Arguments
    /// * `watchers` - Connection manager for players and hosts
    /// * `schedule_message` - Function to schedule delayed messages
    /// * `tunnel_finder` - Function to find communication tunnels
    /// * `index` - Current slide index
    /// * `count` - Total number of slides
    fn send_accepting_answers<
        T: Tunnel,
        F: Fn(Id) -> Option<T>,
        S: FnMut(crate::AlarmMessage, time::Duration),
    >(
        &mut self,
        watchers: &Watchers,
        mut schedule_message: S,
        tunnel_finder: F,
        index: usize,
        count: usize,
    ) {
        if self.change_state(SlideState::Question, SlideState::Answers) {
            self.start_timer();

            watchers.announce(
                &UpdateMessage::QuestionAnnouncement {
                    index,
                    count,
                    question: self.config.title.clone(),
                    media: self.config.media.clone(),
                    duration: self.config.time_limit,
                    accept_answers: true,
                }
                .into(),
                tunnel_finder,
            );

            schedule_message(
                AlarmMessage::ProceedFromSlideIntoSlide {
                    index,
                    to: SlideState::AnswersResults,
                }
                .into(),
                self.config.time_limit,
            );
        }
    }

    /// Attempts to transition from one slide state to another
    ///
    /// # Arguments
    /// * `before` - Expected current state
    /// * `after` - Target state
    ///
    /// # Returns
    /// * `true` if transition was successful, `false` if current state didn't match expected state
    fn change_state(&mut self, before: SlideState, after: SlideState) -> bool {
        if self.state == before {
            self.state = after;

            true
        } else {
            false
        }
    }

    /// Returns the current state of the slide
    fn state(&self) -> SlideState {
        self.state
    }

    /// Sends the results showing correct answers and player statistics
    ///
    /// This method handles the transition from Answers to `AnswersResults` state,
    /// revealing the correct answers and showing statistics about player responses.
    ///
    /// # Arguments
    /// * `watchers` - Connection manager for players and hosts
    /// * `tunnel_finder` - Function to find communication tunnels
    fn send_answers_results<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &mut self,
        watchers: &Watchers,
        tunnel_finder: F,
    ) {
        if self.change_state(SlideState::Answers, SlideState::AnswersResults) {
            watchers.announce(
                &UpdateMessage::AnswersResults {
                    answers: self
                        .config
                        .answers
                        .iter()
                        .map(|answer| clean_answer(answer, self.config.case_sensitive))
                        .collect_vec(),
                    results: self
                        .user_answers
                        .iter()
                        .map(|(_, (answer, _))| clean_answer(answer, self.config.case_sensitive))
                        .counts()
                        .into_iter()
                        .map(|(i, c)| (i.clone(), c))
                        .collect_vec(),
                    case_sensitive: self.config.case_sensitive,
                }
                .into(),
                tunnel_finder,
            );
        }
    }

    /// Calculates and adds scores to the leaderboard based on player answers
    ///
    /// This method evaluates all player answers against the correct answers,
    /// calculates scores based on correctness and timing, and updates the leaderboard.
    /// In team mode, it takes the best answer time for each team.
    ///
    /// # Arguments
    /// * `leaderboard` - Mutable reference to the game leaderboard
    /// * `watchers` - Connection manager for players and hosts
    /// * `team_manager` - Optional team manager for team-based games
    /// * `tunnel_finder` - Function to find communication tunnels
    fn add_scores<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &self,
        leaderboard: &mut Leaderboard,
        watchers: &Watchers,
        team_manager: Option<&TeamManager<crate::names::NameStyle>>,
        tunnel_finder: F,
    ) {
        let starting_instant = self.timer();

        let cleaned_answers: HashSet<_> = self
            .config
            .answers
            .iter()
            .map(|answer| clean_answer(answer, self.config.case_sensitive))
            .collect();

        leaderboard.add_scores(
            &self
                .user_answers
                .iter()
                .map(|(id, (answer, instant))| {
                    let correct =
                        cleaned_answers.contains(&clean_answer(answer, self.config.case_sensitive));
                    (
                        *id,
                        if correct {
                            State::calculate_score(
                                self.config.time_limit,
                                instant
                                    .duration_since(starting_instant)
                                    .expect("future is past the past"),
                                self.config.points_awarded,
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

    /// Generates a synchronization message for a newly connected watcher
    ///
    /// # Arguments
    /// * `_watcher_id` - ID of the connecting watcher
    /// * `_watcher_kind` - Type of watcher (player/host)
    /// * `_team_manager` - Optional team manager for team-based games
    /// * `_watchers` - Connection manager
    /// * `_tunnel_finder` - Function to find communication tunnels
    /// * `index` - Current slide index
    /// * `count` - Total number of slides
    ///
    /// # Returns
    /// * Appropriate sync message based on current slide state
    ///
    /// # Panics
    ///
    /// Panics if the system clock goes backwards while calculating elapsed time
    /// if the state is not one of the expected states.
    ///
    pub fn state_message<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &self,
        _watcher_id: Id,
        _watcher_kind: ValueKind,
        _team_manager: Option<&TeamManager<crate::names::NameStyle>>,
        _watchers: &Watchers,
        _tunnel_finder: F,
        index: usize,
        count: usize,
    ) -> SyncMessage {
        match self.state() {
            SlideState::Unstarted | SlideState::Question => SyncMessage::QuestionAnnouncement {
                index,
                count,
                question: self.config.title.clone(),
                media: self.config.media.clone(),
                duration: self.config.introduce_question
                    - self.timer().elapsed().expect("system clock went backwards"),
                accept_answers: false,
            },
            SlideState::Answers => SyncMessage::QuestionAnnouncement {
                index,
                count,
                question: self.config.title.clone(),
                media: self.config.media.clone(),
                duration: self.config.time_limit
                    - self.timer().elapsed().expect("system clock went backwards"),
                accept_answers: true,
            },
            SlideState::AnswersResults => SyncMessage::AnswersResults {
                index,
                count,
                question: self.config.title.clone(),
                media: self.config.media.clone(),
                answers: self
                    .config
                    .answers
                    .iter()
                    .map(|answer| clean_answer(answer, self.config.case_sensitive))
                    .collect_vec(),
                results: self
                    .user_answers
                    .iter()
                    .map(|(_, (answer, _))| clean_answer(answer, self.config.case_sensitive))
                    .counts()
                    .into_iter()
                    .map(|(i, c)| (i.clone(), c))
                    .collect_vec(),
                case_sensitive: self.config.case_sensitive,
            },
        }
    }

    /// Handles incoming messages from players and hosts
    ///
    /// # Arguments
    /// * `watcher_id` - ID of the sender
    /// * `message` - The incoming message
    /// * `leaderboard` - Mutable reference to the game leaderboard
    /// * `watchers` - Connection manager
    /// * `team_manager` - Optional team manager for team-based games
    /// * `schedule_message` - Function to schedule delayed messages
    /// * `tunnel_finder` - Function to find communication tunnels
    /// * `index` - Current slide index
    /// * `count` - Total number of slides
    ///
    /// # Returns
    /// * `true` if the slide is complete and should advance, `false` otherwise
    pub fn receive_message<
        T: Tunnel,
        F: Fn(Id) -> Option<T>,
        S: FnMut(crate::AlarmMessage, time::Duration),
    >(
        &mut self,
        watcher_id: Id,
        message: IncomingMessage,
        leaderboard: &mut Leaderboard,
        watchers: &Watchers,
        team_manager: Option<&TeamManager<crate::names::NameStyle>>,
        schedule_message: S,
        tunnel_finder: F,
        index: usize,
        count: usize,
    ) -> bool {
        match message {
            IncomingMessage::Host(IncomingHostMessage::Next) => match self.state() {
                SlideState::Unstarted => {
                    self.send_question_announcements(
                        watchers,
                        schedule_message,
                        tunnel_finder,
                        index,
                        count,
                    );
                }
                SlideState::Question => {
                    self.send_accepting_answers(
                        watchers,
                        schedule_message,
                        tunnel_finder,
                        index,
                        count,
                    );
                }
                SlideState::Answers => {
                    self.send_answers_results(watchers, tunnel_finder);
                }
                SlideState::AnswersResults => {
                    self.add_scores(leaderboard, watchers, team_manager, tunnel_finder);
                    return true;
                }
            },
            IncomingMessage::Player(IncomingPlayerMessage::StringAnswer(v)) => {
                self.user_answers.insert(watcher_id, (v, SystemTime::now()));
                let left_set: HashSet<_> = watchers
                    .specific_vec(ValueKind::Player, &tunnel_finder)
                    .iter()
                    .map(|(w, _, _)| w.to_owned())
                    .collect();
                let right_set: HashSet<_> = self.user_answers.keys().copied().collect();
                if left_set.is_subset(&right_set) {
                    self.send_answers_results(watchers, &tunnel_finder);
                } else {
                    watchers.announce_specific(
                        ValueKind::Host,
                        &UpdateMessage::AnswersCount(left_set.intersection(&right_set).count())
                            .into(),
                        &tunnel_finder,
                    );
                }
            }
            _ => (),
        }

        false
    }

    /// Handles scheduled alarm messages for state transitions
    ///
    /// # Arguments
    /// * `_leaderboard` - Mutable reference to the game leaderboard
    /// * `watchers` - Connection manager
    /// * `_team_manager` - Optional team manager for team-based games
    /// * `schedule_message` - Function to schedule delayed messages
    /// * `tunnel_finder` - Function to find communication tunnels
    /// * `message` - The alarm message to handle
    /// * `index` - Current slide index
    /// * `count` - Total number of slides
    ///
    /// # Returns
    /// * `true` if the slide is complete and should advance, `false` otherwise
    pub fn receive_alarm<
        T: Tunnel,
        F: Fn(Id) -> Option<T>,
        S: FnMut(crate::AlarmMessage, web_time::Duration),
    >(
        &mut self,
        _leaderboard: &mut Leaderboard,
        watchers: &Watchers,
        _team_manager: Option<&TeamManager<crate::names::NameStyle>>,
        schedule_message: &mut S,
        tunnel_finder: F,
        message: &crate::AlarmMessage,
        index: usize,
        count: usize,
    ) -> bool {
        if let crate::AlarmMessage::TypeAnswer(AlarmMessage::ProceedFromSlideIntoSlide {
            index: _,
            to,
        }) = message
        {
            match to {
                SlideState::Answers => {
                    self.send_accepting_answers(
                        watchers,
                        schedule_message,
                        tunnel_finder,
                        index,
                        count,
                    );
                }
                SlideState::AnswersResults => {
                    self.send_answers_results(watchers, tunnel_finder);
                }
                _ => (),
            }
        }

        false
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::fuiz::config::{Fuiz, SlideConfig as ConfigSlideConfig};
    use core::panic;
    use garde::Validate;
    use std::time::Duration;

    fn create_test_slide_config() -> SlideConfig {
        SlideConfig {
            title: "What is the capital of France?".to_string(),
            media: None,
            introduce_question: Duration::from_secs(2),
            time_limit: Duration::from_secs(30),
            points_awarded: 1000,
            answers: vec![
                "Paris".to_string(),
                "paris".to_string(),
                "PARIS".to_string(),
            ],
            case_sensitive: false,
        }
    }

    fn create_test_slide_config_case_sensitive() -> SlideConfig {
        SlideConfig {
            title: "Type the exact word: Hello".to_string(),
            media: None,
            introduce_question: Duration::from_secs(0),
            time_limit: Duration::from_secs(20),
            points_awarded: 500,
            answers: vec!["Hello".to_string()],
            case_sensitive: true,
        }
    }

    fn create_test_fuiz() -> Fuiz {
        Fuiz {
            title: "Test Type Answer Quiz".to_string(),
            slides: vec![ConfigSlideConfig::TypeAnswer(create_test_slide_config())],
        }
    }

    #[test]
    fn test_slide_config_validation() {
        let config = create_test_slide_config();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_slide_config_title_too_short() {
        let mut config = create_test_slide_config();
        // MIN_TITLE_LENGTH is 0, so we can't test too short. Test at minimum boundary.
        config.title = String::new();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_slide_config_title_too_long() {
        let mut config = create_test_slide_config();
        config.title = "a".repeat(crate::constants::type_answer::MAX_TITLE_LENGTH + 1);
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_slide_config_introduce_question_too_short() {
        let mut config = create_test_slide_config();
        // MIN_INTRODUCE_QUESTION is 0, so we can't test too short. Test at minimum boundary.
        config.introduce_question = Duration::from_secs(0);
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_slide_config_introduce_question_too_long() {
        let mut config = create_test_slide_config();
        config.introduce_question =
            Duration::from_secs(crate::constants::type_answer::MAX_INTRODUCE_QUESTION + 1);
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_slide_config_time_limit_too_short() {
        let mut config = create_test_slide_config();
        config.time_limit = Duration::from_secs(crate::constants::type_answer::MIN_TIME_LIMIT - 1);
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_slide_config_time_limit_too_long() {
        let mut config = create_test_slide_config();
        config.time_limit = Duration::from_secs(crate::constants::type_answer::MAX_TIME_LIMIT + 1);
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_slide_config_too_many_answers() {
        let mut config = create_test_slide_config();
        config.answers =
            vec!["Answer".to_string(); crate::constants::type_answer::MAX_ANSWER_COUNT + 1];
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_slide_config_answer_too_long() {
        let mut config = create_test_slide_config();
        config.answers = vec!["a".repeat(crate::constants::answer_text::MAX_LENGTH + 1)];
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_slide_config_to_state() {
        let config = create_test_slide_config();
        let state = config.to_state();

        assert_eq!(state.state, SlideState::Unstarted);
        assert!(state.user_answers.is_empty());
        assert!(state.answer_start.is_none());
        assert_eq!(state.config.title, config.title);
        assert_eq!(state.config.case_sensitive, config.case_sensitive);
    }

    #[test]
    fn test_fuiz_config_validation() {
        let fuiz = create_test_fuiz();
        assert!(fuiz.validate().is_ok());
    }

    #[test]
    fn test_fuiz_len_and_empty() {
        let fuiz = create_test_fuiz();
        assert_eq!(fuiz.len(), 1);
        assert!(!fuiz.is_empty());

        let empty_fuiz = Fuiz {
            title: "Empty".to_string(),
            slides: vec![],
        };
        assert_eq!(empty_fuiz.len(), 0);
        assert!(empty_fuiz.is_empty());
    }

    #[test]
    fn test_fuiz_title_too_long() {
        let mut fuiz = create_test_fuiz();
        fuiz.title = "a".repeat(crate::constants::fuiz::MAX_TITLE_LENGTH + 1);
        assert!(fuiz.validate().is_err());
    }

    #[test]
    fn test_fuiz_too_many_slides() {
        let mut fuiz = create_test_fuiz();
        fuiz.slides = vec![
            ConfigSlideConfig::TypeAnswer(create_test_slide_config());
            crate::constants::fuiz::MAX_SLIDES_COUNT + 1
        ];
        assert!(fuiz.validate().is_err());
    }

    #[test]
    fn test_state_change() {
        let config = create_test_slide_config();
        let mut state = config.to_state();

        // Test successful state change
        assert!(state.change_state(SlideState::Unstarted, SlideState::Question));
        assert_eq!(state.state(), SlideState::Question);

        // Test failed state change (wrong current state)
        assert!(!state.change_state(SlideState::Unstarted, SlideState::Answers));
        assert_eq!(state.state(), SlideState::Question);
    }

    #[test]
    fn test_calculate_score() {
        let full_duration = Duration::from_secs(30);
        let full_points = 1000;

        // Immediate answer should get full points
        let immediate_score =
            State::calculate_score(full_duration, Duration::from_secs(0), full_points);
        assert_eq!(immediate_score, full_points);

        // Answer at the end should get half points
        let late_score = State::calculate_score(full_duration, full_duration, full_points);
        assert_eq!(late_score, 500);

        // Answer in the middle should get 3/4 points
        let mid_score = State::calculate_score(full_duration, Duration::from_secs(15), full_points);
        assert_eq!(mid_score, 750);
    }

    #[test]
    fn test_clean_answer_case_insensitive() {
        assert_eq!(clean_answer("  Paris  ", false), "paris");
        assert_eq!(clean_answer("PARIS", false), "paris");
        assert_eq!(clean_answer("pArIs", false), "paris");
        assert_eq!(clean_answer("   hello WORLD   ", false), "hello world");
    }

    #[test]
    fn test_clean_answer_case_sensitive() {
        assert_eq!(clean_answer("  Paris  ", true), "Paris");
        assert_eq!(clean_answer("PARIS", true), "PARIS");
        assert_eq!(clean_answer("pArIs", true), "pArIs");
        assert_eq!(clean_answer("   Hello World   ", true), "Hello World");
    }

    #[test]
    fn test_slide_state_default() {
        let state: SlideState = SlideState::default();
        assert_eq!(state, SlideState::Unstarted);
    }

    #[test]
    fn test_validate_duration_functions() {
        // Test introduce_question validation
        let valid_introduce =
            Duration::from_secs(crate::constants::type_answer::MIN_INTRODUCE_QUESTION);
        assert!(validate_introduce_question(&valid_introduce).is_ok());

        // MIN_INTRODUCE_QUESTION is 0, so we can't test too short. Test at minimum boundary.
        let invalid_introduce = Duration::from_secs(0);
        assert!(validate_introduce_question(&invalid_introduce).is_ok());

        // Test time_limit validation
        let valid_time_limit = Duration::from_secs(crate::constants::type_answer::MIN_TIME_LIMIT);
        assert!(validate_time_limit(&valid_time_limit).is_ok());

        let invalid_time_limit =
            Duration::from_secs(crate::constants::type_answer::MIN_TIME_LIMIT - 1);
        assert!(validate_time_limit(&invalid_time_limit).is_err());
    }

    #[test]
    fn test_case_sensitivity_behavior() {
        let case_insensitive_config = create_test_slide_config();
        let case_sensitive_config = create_test_slide_config_case_sensitive();

        assert!(!case_insensitive_config.case_sensitive);
        assert!(case_sensitive_config.case_sensitive);

        // Verify the configuration is applied to the state
        let case_insensitive_state = case_insensitive_config.to_state();
        let case_sensitive_state = case_sensitive_config.to_state();

        assert!(!case_insensitive_state.config.case_sensitive);
        assert!(case_sensitive_state.config.case_sensitive);
    }

    #[test]
    fn test_config_to_state_conversion() {
        let config = create_test_slide_config();
        let state = config.to_state();

        // Verify the state is properly initialized from config
        assert_eq!(state.config.title, config.title);
        assert_eq!(state.config.answers, config.answers);
        assert_eq!(state.config.time_limit, config.time_limit);
        assert_eq!(state.config.points_awarded, config.points_awarded);
        assert_eq!(state.config.case_sensitive, config.case_sensitive);
        assert_eq!(state.config.introduce_question, config.introduce_question);
    }

    #[test]
    fn test_slide_config_serialization() {
        let config = create_test_slide_config();

        // Test that the config can be serialized and deserialized
        let serialized = serde_json::to_string(&config).unwrap();
        let deserialized: SlideConfig = serde_json::from_str(&serialized).unwrap();

        assert_eq!(config.title, deserialized.title);
        assert_eq!(config.answers, deserialized.answers);
        assert_eq!(config.case_sensitive, deserialized.case_sensitive);
    }

    #[test]
    fn test_message_serialization() {
        let update_msg = UpdateMessage::QuestionAnnouncement {
            index: 0,
            count: 5,
            question: "Test question".to_string(),
            media: None,
            duration: Duration::from_secs(10),
            accept_answers: true,
        };

        // Should serialize without errors
        let _serialized = serde_json::to_string(&update_msg).unwrap();

        let sync_msg = SyncMessage::AnswersResults {
            index: 0,
            count: 5,
            question: "Test question".to_string(),
            media: None,
            answers: vec!["A".to_string(), "B".to_string()],
            results: vec![("A".to_string(), 3), ("B".to_string(), 2)],
            case_sensitive: false,
        };

        // Should serialize without errors
        let _serialized = serde_json::to_string(&sync_msg).unwrap();
    }

    #[test]
    fn test_answer_validation_logic() {
        let config = create_test_slide_config();

        // Test that case insensitive matching should work
        let cleaned_answers: std::collections::HashSet<_> = config
            .answers
            .iter()
            .map(|answer| clean_answer(answer, config.case_sensitive))
            .collect();

        // All variations should resolve to "paris"
        assert!(cleaned_answers.contains("paris"));
        assert_eq!(cleaned_answers.len(), 1); // All variations collapse to one answer

        // Test case sensitive config
        let case_sensitive_config = create_test_slide_config_case_sensitive();
        let case_sensitive_answers: std::collections::HashSet<_> = case_sensitive_config
            .answers
            .iter()
            .map(|answer| clean_answer(answer, case_sensitive_config.case_sensitive))
            .collect();

        assert!(case_sensitive_answers.contains("Hello"));
        assert_eq!(case_sensitive_answers.len(), 1);
    }

    #[test]
    fn test_config_defaults() {
        // Test that introduce_question defaults to 0 when using serde default
        let json = r#"{"title":"Test","time_limit":30000,"points_awarded":100,"answers":["test"],"case_sensitive":false}"#;
        let config: SlideConfig = serde_json::from_str(json).unwrap();

        assert_eq!(config.introduce_question, Duration::from_secs(0));
        assert!(!config.case_sensitive);
    }

    // Mock implementations for testing
    mod mocks {
        use crate::leaderboard::Leaderboard;
        use crate::session::Tunnel;
        use crate::watcher::{Id, Watchers};
        use std::sync::{Arc, Mutex};

        #[derive(Debug, Clone)]
        pub struct MockTunnel {
            pub messages: Arc<Mutex<Vec<String>>>,
        }

        impl Tunnel for MockTunnel {
            fn send_message(&self, message: &crate::UpdateMessage) {
                if let Ok(serialized) = serde_json::to_string(message) {
                    self.messages.lock().unwrap().push(serialized);
                }
            }

            fn send_state(&self, state: &crate::SyncMessage) {
                if let Ok(serialized) = serde_json::to_string(state) {
                    self.messages.lock().unwrap().push(serialized);
                }
            }

            fn close(self) {
                // Mock implementation - no-op
            }
        }

        pub fn mock_watchers() -> Watchers {
            Watchers::with_host_id(Id::new())
        }

        pub fn mock_leaderboard() -> Leaderboard {
            Leaderboard::default()
        }

        pub fn mock_tunnel_finder() -> impl Fn(Id) -> Option<MockTunnel> {
            move |_id| {
                Some(MockTunnel {
                    messages: Arc::new(Mutex::new(Vec::new())),
                })
            }
        }

        pub fn mock_schedule_message() -> impl FnMut(crate::AlarmMessage, web_time::Duration) {
            move |_message, _duration| {}
        }

        pub fn mock_schedule_message_std() -> impl FnMut(crate::AlarmMessage, std::time::Duration) {
            move |_message, _duration| {}
        }
    }

    #[test]
    fn test_receive_alarm_proceed_to_answers() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        state.change_state(SlideState::Unstarted, SlideState::Question);

        let mut leaderboard = mocks::mock_leaderboard();
        let watchers = mocks::mock_watchers();
        let tunnel_finder = mocks::mock_tunnel_finder();
        let mut schedule_message = mocks::mock_schedule_message();

        let alarm_message =
            crate::AlarmMessage::TypeAnswer(AlarmMessage::ProceedFromSlideIntoSlide {
                index: 0,
                to: SlideState::Answers,
            });

        let result = state.receive_alarm(
            &mut leaderboard,
            &watchers,
            None,
            &mut schedule_message,
            tunnel_finder,
            &alarm_message,
            0,
            1,
        );

        assert!(!result);
        assert_eq!(state.state(), SlideState::Answers);
    }

    #[test]
    fn test_receive_alarm_proceed_to_answers_results() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        state.change_state(SlideState::Unstarted, SlideState::Question);
        state.change_state(SlideState::Question, SlideState::Answers);

        let mut leaderboard = mocks::mock_leaderboard();
        let watchers = mocks::mock_watchers();
        let tunnel_finder = mocks::mock_tunnel_finder();
        let mut schedule_message = mocks::mock_schedule_message();

        let alarm_message =
            crate::AlarmMessage::TypeAnswer(AlarmMessage::ProceedFromSlideIntoSlide {
                index: 0,
                to: SlideState::AnswersResults,
            });

        let result = state.receive_alarm(
            &mut leaderboard,
            &watchers,
            None,
            &mut schedule_message,
            tunnel_finder,
            &alarm_message,
            0,
            1,
        );

        assert!(!result);
        assert_eq!(state.state(), SlideState::AnswersResults);
    }

    #[test]
    fn test_receive_alarm_unhandled_target_state() {
        let config = create_test_slide_config();
        let mut state = config.to_state();

        let mut leaderboard = mocks::mock_leaderboard();
        let watchers = mocks::mock_watchers();
        let tunnel_finder = mocks::mock_tunnel_finder();
        let mut schedule_message = mocks::mock_schedule_message();

        let alarm_message =
            crate::AlarmMessage::TypeAnswer(AlarmMessage::ProceedFromSlideIntoSlide {
                index: 0,
                to: SlideState::Question,
            });

        let original_state = state.state();

        let result = state.receive_alarm(
            &mut leaderboard,
            &watchers,
            None,
            &mut schedule_message,
            tunnel_finder,
            &alarm_message,
            0,
            1,
        );

        assert!(!result);
        assert_eq!(state.state(), original_state);
    }

    #[test]
    fn test_receive_alarm_non_type_answer_message() {
        let config = create_test_slide_config();
        let mut state = config.to_state();

        let mut leaderboard = mocks::mock_leaderboard();
        let watchers = mocks::mock_watchers();
        let tunnel_finder = mocks::mock_tunnel_finder();
        let mut schedule_message = mocks::mock_schedule_message();

        let alarm_message = crate::AlarmMessage::MultipleChoice(
            crate::fuiz::multiple_choice::AlarmMessage::ProceedFromSlideIntoSlide {
                index: 0,
                to: crate::fuiz::multiple_choice::SlideState::Answers,
            },
        );

        let original_state = state.state();

        let result = state.receive_alarm(
            &mut leaderboard,
            &watchers,
            None,
            &mut schedule_message,
            tunnel_finder,
            &alarm_message,
            0,
            1,
        );

        assert!(!result);
        assert_eq!(state.state(), original_state);
    }

    #[test]
    fn test_receive_message_host_next_from_unstarted() {
        let config = create_test_slide_config();
        let mut state = config.to_state();

        let mut leaderboard = mocks::mock_leaderboard();
        let watchers = mocks::mock_watchers();
        let tunnel_finder = mocks::mock_tunnel_finder();
        let schedule_message = mocks::mock_schedule_message_std();

        let result = state.receive_message(
            Id::new(),
            IncomingMessage::Host(IncomingHostMessage::Next),
            &mut leaderboard,
            &watchers,
            None,
            schedule_message,
            tunnel_finder,
            0,
            1,
        );

        assert!(!result);
        assert_eq!(state.state(), SlideState::Question);
    }

    #[test]
    fn test_receive_message_host_next_from_question() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        state.change_state(SlideState::Unstarted, SlideState::Question);

        let mut leaderboard = mocks::mock_leaderboard();
        let watchers = mocks::mock_watchers();
        let tunnel_finder = mocks::mock_tunnel_finder();
        let schedule_message = mocks::mock_schedule_message_std();

        let result = state.receive_message(
            Id::new(),
            IncomingMessage::Host(IncomingHostMessage::Next),
            &mut leaderboard,
            &watchers,
            None,
            schedule_message,
            tunnel_finder,
            0,
            1,
        );

        assert!(!result);
        assert_eq!(state.state(), SlideState::Answers);
    }

    #[test]
    fn test_receive_message_host_next_from_answers() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        state.change_state(SlideState::Unstarted, SlideState::Question);
        state.change_state(SlideState::Question, SlideState::Answers);

        let mut leaderboard = mocks::mock_leaderboard();
        let watchers = mocks::mock_watchers();
        let tunnel_finder = mocks::mock_tunnel_finder();
        let schedule_message = mocks::mock_schedule_message_std();

        let result = state.receive_message(
            Id::new(),
            IncomingMessage::Host(IncomingHostMessage::Next),
            &mut leaderboard,
            &watchers,
            None,
            schedule_message,
            tunnel_finder,
            0,
            1,
        );

        assert!(!result);
        assert_eq!(state.state(), SlideState::AnswersResults);
    }

    #[test]
    fn test_receive_message_host_next_from_answers_results() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        state.change_state(SlideState::Unstarted, SlideState::Question);
        state.change_state(SlideState::Question, SlideState::Answers);
        state.change_state(SlideState::Answers, SlideState::AnswersResults);

        let mut leaderboard = mocks::mock_leaderboard();
        let watchers = mocks::mock_watchers();
        let tunnel_finder = mocks::mock_tunnel_finder();
        let schedule_message = mocks::mock_schedule_message_std();

        let result = state.receive_message(
            Id::new(),
            IncomingMessage::Host(IncomingHostMessage::Next),
            &mut leaderboard,
            &watchers,
            None,
            schedule_message,
            tunnel_finder,
            0,
            1,
        );

        assert!(result);
    }

    #[test]
    fn test_receive_message_player_string_answer() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        state.change_state(SlideState::Unstarted, SlideState::Question);
        state.change_state(SlideState::Question, SlideState::Answers);

        let mut leaderboard = mocks::mock_leaderboard();
        let watchers = mocks::mock_watchers();
        let tunnel_finder = mocks::mock_tunnel_finder();
        let schedule_message = mocks::mock_schedule_message_std();

        let player_id = Id::new();
        let result = state.receive_message(
            player_id,
            IncomingMessage::Player(IncomingPlayerMessage::StringAnswer("Paris".to_string())),
            &mut leaderboard,
            &watchers,
            None,
            schedule_message,
            tunnel_finder,
            0,
            1,
        );

        assert!(!result);
        assert!(state.user_answers.contains_key(&player_id));
        assert_eq!(state.user_answers.get(&player_id).unwrap().0, "Paris");
    }

    #[test]
    fn test_receive_message_unhandled_message() {
        let config = create_test_slide_config();
        let mut state = config.to_state();

        let mut leaderboard = mocks::mock_leaderboard();
        let watchers = mocks::mock_watchers();
        let tunnel_finder = mocks::mock_tunnel_finder();
        let schedule_message = mocks::mock_schedule_message_std();

        let original_state = state.state();

        let result = state.receive_message(
            Id::new(),
            IncomingMessage::Player(IncomingPlayerMessage::IndexAnswer(42)),
            &mut leaderboard,
            &watchers,
            None,
            schedule_message,
            tunnel_finder,
            0,
            1,
        );

        assert!(!result);
        assert_eq!(state.state(), original_state);
    }

    #[test]
    fn test_state_message_unstarted_state() {
        let config = create_test_slide_config();
        let state = config.to_state();

        let watchers = mocks::mock_watchers();
        let tunnel_finder = mocks::mock_tunnel_finder();

        let sync_msg = state.state_message(
            Id::new(),
            ValueKind::Player,
            None,
            &watchers,
            tunnel_finder,
            0,
            1,
        );

        let SyncMessage::QuestionAnnouncement {
            index,
            count,
            question,
            media,
            duration,
            accept_answers,
        } = sync_msg
        else {
            panic!("Expected QuestionAnnouncement");
        };

        assert_eq!(index, 0);
        assert_eq!(count, 1);
        assert_eq!(question, config.title);
        assert!(media.is_none());
        assert!(duration <= config.introduce_question);
        assert!(!accept_answers);
    }

    #[test]
    fn test_state_message_question_state() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        state.change_state(SlideState::Unstarted, SlideState::Question);

        let watchers = mocks::mock_watchers();
        let tunnel_finder = mocks::mock_tunnel_finder();

        let sync_msg = state.state_message(
            Id::new(),
            ValueKind::Player,
            None,
            &watchers,
            tunnel_finder,
            0,
            1,
        );

        let SyncMessage::QuestionAnnouncement {
            index,
            count,
            question,
            media,
            duration,
            accept_answers,
        } = sync_msg
        else {
            panic!("Expected QuestionAnnouncement");
        };

        assert_eq!(index, 0);
        assert_eq!(count, 1);
        assert_eq!(question, config.title);
        assert!(media.is_none());
        assert!(duration <= config.introduce_question);
        assert!(!accept_answers);
    }

    #[test]
    fn test_state_message_answers_state() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        state.change_state(SlideState::Unstarted, SlideState::Question);
        state.change_state(SlideState::Question, SlideState::Answers);

        let watchers = mocks::mock_watchers();
        let tunnel_finder = mocks::mock_tunnel_finder();

        let sync_msg = state.state_message(
            Id::new(),
            ValueKind::Player,
            None,
            &watchers,
            tunnel_finder,
            0,
            1,
        );

        let SyncMessage::QuestionAnnouncement {
            index,
            count,
            question,
            media,
            duration,
            accept_answers,
        } = sync_msg
        else {
            panic!("Expected QuestionAnnouncement");
        };

        assert_eq!(index, 0);
        assert_eq!(count, 1);
        assert_eq!(question, config.title);
        assert!(media.is_none());
        assert!(duration <= config.time_limit);
        assert!(accept_answers);
    }

    #[test]
    fn test_state_message_answers_results_state() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        state.change_state(SlideState::Unstarted, SlideState::Question);
        state.change_state(SlideState::Question, SlideState::Answers);
        state.change_state(SlideState::Answers, SlideState::AnswersResults);

        state.user_answers.insert(
            Id::new(),
            ("Paris".to_string(), web_time::SystemTime::now()),
        );
        state.user_answers.insert(
            Id::new(),
            ("London".to_string(), web_time::SystemTime::now()),
        );

        let watchers = mocks::mock_watchers();
        let tunnel_finder = mocks::mock_tunnel_finder();

        let sync_msg = state.state_message(
            Id::new(),
            ValueKind::Player,
            None,
            &watchers,
            tunnel_finder,
            0,
            1,
        );

        let SyncMessage::AnswersResults {
            index,
            count,
            question,
            media,
            answers,
            results,
            case_sensitive,
        } = sync_msg
        else {
            panic!("Expected AnswersResults, got {:?}", sync_msg);
        };
        assert_eq!(index, 0);
        assert_eq!(count, 1);
        assert_eq!(question, config.title);
        assert!(media.is_none());
        assert_eq!(answers.len(), 3); // Config has 3 answers that all normalize to "paris"
        assert!(answers.iter().all(|a| a == "paris"));
        assert_eq!(results.len(), 2);
        assert!(!case_sensitive);
    }

    #[test]
    fn test_state_message_case_sensitive() {
        let config = create_test_slide_config_case_sensitive();
        let mut state = config.to_state();
        state.change_state(SlideState::Unstarted, SlideState::Question);
        state.change_state(SlideState::Question, SlideState::Answers);
        state.change_state(SlideState::Answers, SlideState::AnswersResults);

        state.user_answers.insert(
            Id::new(),
            ("Hello".to_string(), web_time::SystemTime::now()),
        );
        state.user_answers.insert(
            Id::new(),
            ("hello".to_string(), web_time::SystemTime::now()),
        );

        let watchers = mocks::mock_watchers();
        let tunnel_finder = mocks::mock_tunnel_finder();

        let sync_msg = state.state_message(
            Id::new(),
            ValueKind::Player,
            None,
            &watchers,
            tunnel_finder,
            0,
            1,
        );

        let SyncMessage::AnswersResults {
            index,
            count,
            question,
            media,
            answers,
            results,
            case_sensitive,
        } = sync_msg
        else {
            panic!("Expected AnswersResults, got {:?}", sync_msg);
        };
        assert_eq!(index, 0);
        assert_eq!(count, 1);
        assert_eq!(question, config.title);
        assert!(media.is_none());
        assert_eq!(answers, vec!["Hello"]);
        assert_eq!(results.len(), 2);
        assert!(case_sensitive);
    }

    #[test]
    fn test_play_from_unstarted_with_introduce_question() {
        let config = create_test_slide_config();
        let mut state = config.to_state();

        let watchers = mocks::mock_watchers();
        let tunnel_finder = mocks::mock_tunnel_finder();
        let schedule_message = mocks::mock_schedule_message_std();

        // Verify initial state
        assert_eq!(state.state(), SlideState::Unstarted);
        assert!(state.answer_start.is_none());

        state.play(&watchers, schedule_message, tunnel_finder, 0, 1);

        // Should transition to Question state and start timer
        assert_eq!(state.state(), SlideState::Question);
        assert!(state.answer_start.is_some());
    }

    #[test]
    fn test_play_from_unstarted_zero_introduce_question() {
        let mut config = create_test_slide_config();
        config.introduce_question = Duration::from_secs(0);
        let mut state = config.to_state();

        let watchers = mocks::mock_watchers();
        let tunnel_finder = mocks::mock_tunnel_finder();
        let schedule_message = mocks::mock_schedule_message_std();

        // Verify initial state
        assert_eq!(state.state(), SlideState::Unstarted);

        state.play(&watchers, schedule_message, tunnel_finder, 0, 1);

        // Should skip Question and go directly to Answers state
        assert_eq!(state.state(), SlideState::Answers);
        assert!(state.answer_start.is_some());
    }

    #[test]
    fn test_play_from_non_unstarted_state() {
        let config = create_test_slide_config();
        let mut state = config.to_state();

        // Change state away from Unstarted
        state.change_state(SlideState::Unstarted, SlideState::Question);

        let watchers = mocks::mock_watchers();
        let tunnel_finder = mocks::mock_tunnel_finder();
        let schedule_message = mocks::mock_schedule_message_std();

        // Store current state
        let current_state = state.state();
        let current_timer = state.answer_start;

        state.play(&watchers, schedule_message, tunnel_finder, 0, 1);

        // Should not change anything since not in Unstarted state
        assert_eq!(state.state(), current_state);
        assert_eq!(state.answer_start, current_timer);
    }

    #[test]
    fn test_play_with_different_slide_indices() {
        let config = create_test_slide_config();
        let mut state = config.to_state();

        let watchers = mocks::mock_watchers();
        let tunnel_finder = mocks::mock_tunnel_finder();
        let schedule_message = mocks::mock_schedule_message_std();

        state.play(&watchers, schedule_message, tunnel_finder, 5, 10);

        // Should work with any index/count values
        assert_eq!(state.state(), SlideState::Question);
        assert!(state.answer_start.is_some());
    }

    #[test]
    fn test_play_timer_behavior() {
        let config = create_test_slide_config();
        let mut state = config.to_state();

        let watchers = mocks::mock_watchers();
        let tunnel_finder = mocks::mock_tunnel_finder();
        let schedule_message = mocks::mock_schedule_message_std();

        // Record time before play
        let before_play = web_time::SystemTime::now();

        state.play(&watchers, schedule_message, tunnel_finder, 0, 1);

        // Timer should be set to approximately current time
        let timer = state.timer();
        assert!(timer >= before_play);
        assert!(timer <= web_time::SystemTime::now());
    }

    #[test]
    fn test_play_with_media() {
        let mut config = create_test_slide_config();
        config.media = Some(crate::fuiz::media::Media::Image(
            crate::fuiz::media::Image::Corkboard {
                id: "testid123".to_string(),
                alt: "Test image".to_string(),
            },
        ));
        let mut state = config.to_state();

        let watchers = mocks::mock_watchers();
        let tunnel_finder = mocks::mock_tunnel_finder();
        let schedule_message = mocks::mock_schedule_message_std();

        state.play(&watchers, schedule_message, tunnel_finder, 0, 1);

        // Should work with media present
        assert_eq!(state.state(), SlideState::Question);
        assert!(state.answer_start.is_some());
    }
}
