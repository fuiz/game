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
    fuiz::config::SlideAction,
    leaderboard::Leaderboard,
    session::Tunnel,
    teams::TeamManager,
    watcher::{Id, ValueKind, Watchers},
};

use super::{
    super::constants::type_answer::*,
    super::game::IncomingPlayerMessage,
    common::{
        AnswerHandler, QuestionReceiveMessage, SlideStateManager, SlideTimer,
        add_scores_to_leaderboard, all_players_answered, get_answered_count, validate_duration,
    },
    media::Media,
};

// Re-export SlideState publicly so other modules can use it
pub use super::common::SlideState;

#[serde_with::serde_as]
#[skip_serializing_none]
/// Configuration for a type answer slide
///
/// Contains all the settings and content for a single type answer question,
/// including the question text, media, timing, and acceptable answers.
#[derive(Debug, Clone, Serialize, serde::Deserialize, Validate)]
pub struct SlideConfig {
    /// The question title, represents what's being asked
    #[garde(length(chars, min = MIN_TITLE_LENGTH, max = MAX_TITLE_LENGTH))]
    title: String,
    /// Accompanying media
    #[garde(dive)]
    media: Option<Media>,
    /// Time before the answers are displayed
    #[garde(custom(validate_duration::<MIN_INTRODUCE_QUESTION, MAX_INTRODUCE_QUESTION>))]
    #[serde_as(as = "serde_with::DurationMilliSeconds<u64>")]
    #[serde(default)]
    introduce_question: Duration,
    /// Time where players can answer the question
    #[garde(custom(validate_duration::<MIN_TIME_LIMIT, MAX_TIME_LIMIT>))]
    #[serde_as(as = "serde_with::DurationMilliSeconds<u64>")]
    time_limit: Duration,
    /// Maximum number of points awarded the question, decreases linearly to half the amount by the end of the slide
    #[garde(skip)]
    points_awarded: u64,
    /// List of acceptable text answers for this question
    #[garde(length(max = MAX_ANSWER_COUNT),
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
    /// The set of cleaned player answers
    cleaned_answers: HashSet<String>,
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
            cleaned_answers: self
                .answers
                .iter()
                .map(|a| clean_answer(a, self.case_sensitive))
                .collect(),
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

impl SlideStateManager for State {
    fn state(&self) -> SlideState {
        self.state
    }

    fn change_state(&mut self, before: SlideState, after: SlideState) -> bool {
        if self.state == before {
            self.state = after;
            true
        } else {
            false
        }
    }
}

impl SlideTimer for State {
    fn answer_start(&self) -> Option<SystemTime> {
        self.answer_start
    }

    fn set_answer_start(&mut self, time: Option<SystemTime>) {
        self.answer_start = time;
    }
}

impl AnswerHandler<String> for State {
    fn user_answers(&self) -> &HashMap<Id, (String, SystemTime)> {
        &self.user_answers
    }

    fn user_answers_mut(&mut self) -> &mut HashMap<Id, (String, SystemTime)> {
        &mut self.user_answers
    }

    fn transform_answer(&self, answer: String) -> String {
        clean_answer(&answer, self.config.case_sensitive)
    }

    fn is_correct_answer(&self, answer: &String) -> bool {
        self.cleaned_answers.contains(answer)
    }

    fn max_points(&self) -> u64 {
        self.config.points_awarded
    }

    fn time_limit(&self) -> Duration {
        self.config.time_limit
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
        S: FnOnce(crate::AlarmMessage, time::Duration),
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
        S: FnOnce(crate::AlarmMessage, time::Duration),
    >(
        &mut self,
        watchers: &Watchers,
        schedule_message: S,
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
        S: FnOnce(crate::AlarmMessage, time::Duration),
    >(
        &mut self,
        watchers: &Watchers,
        schedule_message: S,
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
                    answers: self.cleaned_answers.iter().cloned().collect_vec(),
                    results: self.answer_counts().into_iter().collect_vec(),
                    case_sensitive: self.config.case_sensitive,
                }
                .into(),
                tunnel_finder,
            );
        }
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
                duration: self.config.introduce_question - self.elapsed(),
                accept_answers: false,
            },
            SlideState::Answers => SyncMessage::QuestionAnnouncement {
                index,
                count,
                question: self.config.title.clone(),
                media: self.config.media.clone(),
                duration: self.config.time_limit - self.elapsed(),
                accept_answers: true,
            },
            SlideState::AnswersResults => SyncMessage::AnswersResults {
                index,
                count,
                question: self.config.title.clone(),
                media: self.config.media.clone(),
                answers: self.cleaned_answers.iter().cloned().collect_vec(),
                results: self.answer_counts().into_iter().collect_vec(),
                case_sensitive: self.config.case_sensitive,
            },
        }
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
    /// * A `SlideAction` indicating whether to stay on the current slide or advance
    pub fn receive_alarm<
        T: Tunnel,
        F: Fn(Id) -> Option<T>,
        S: FnOnce(crate::AlarmMessage, web_time::Duration),
    >(
        &mut self,
        _leaderboard: &mut Leaderboard,
        watchers: &Watchers,
        _team_manager: Option<&TeamManager<crate::names::NameStyle>>,
        schedule_message: S,
        tunnel_finder: F,
        message: &crate::AlarmMessage,
        index: usize,
        count: usize,
    ) -> SlideAction<S> {
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

        SlideAction::Stay
    }
}

impl QuestionReceiveMessage for State {
    fn receive_host_next<
        T: Tunnel,
        F: Fn(Id) -> Option<T>,
        S: FnOnce(crate::AlarmMessage, time::Duration),
    >(
        &mut self,
        leaderboard: &mut Leaderboard,
        watchers: &Watchers,
        team_manager: Option<&TeamManager<crate::names::NameStyle>>,
        schedule_message: S,
        tunnel_finder: F,
        index: usize,
        count: usize,
    ) -> SlideAction<S> {
        match self.state() {
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
                add_scores_to_leaderboard(
                    self,
                    self,
                    leaderboard,
                    watchers,
                    team_manager,
                    tunnel_finder,
                );
                return SlideAction::Next { schedule_message };
            }
        }

        SlideAction::Stay
    }

    fn receive_player_message<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &mut self,
        watcher_id: Id,
        message: IncomingPlayerMessage,
        watchers: &Watchers,
        tunnel_finder: F,
    ) {
        if let IncomingPlayerMessage::StringAnswer(v) = message {
            self.record_answer(watcher_id, v);
            if all_players_answered(self, watchers, &tunnel_finder) {
                self.send_answers_results(watchers, &tunnel_finder);
            } else {
                watchers.announce_specific(
                    ValueKind::Host,
                    &UpdateMessage::AnswersCount(get_answered_count(
                        self,
                        watchers,
                        &tunnel_finder,
                    ))
                    .into(),
                    &tunnel_finder,
                );
            }
        }
    }
}
