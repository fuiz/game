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
