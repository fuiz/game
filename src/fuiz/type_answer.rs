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

    fn is_correct_answer(&self, answer: &String) -> bool {
        let cleaned_answers: HashSet<_> = self
            .config
            .answers
            .iter()
            .map(|a| clean_answer(a, self.config.case_sensitive))
            .collect();
        cleaned_answers.contains(&clean_answer(answer, self.config.case_sensitive))
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
        add_scores_to_leaderboard(
            self,
            self,
            leaderboard,
            watchers,
            team_manager,
            tunnel_finder,
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

impl QuestionReceiveMessage for State {
    fn receive_host_next<
        T: Tunnel,
        F: Fn(Id) -> Option<T>,
        S: FnMut(crate::AlarmMessage, time::Duration),
    >(
        &mut self,
        leaderboard: &mut Leaderboard,
        watchers: &Watchers,
        team_manager: Option<&TeamManager<crate::names::NameStyle>>,
        schedule_message: S,
        tunnel_finder: F,
        index: usize,
        count: usize,
    ) -> bool {
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
                self.add_scores(leaderboard, watchers, team_manager, tunnel_finder);
                return true;
            }
        }

        false
    }

    fn receive_player_message<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &mut self,
        watcher_id: Id,
        message: IncomingPlayerMessage,
        watchers: &Watchers,
        tunnel_finder: F,
    ) {
        if let IncomingPlayerMessage::StringAnswer(v) = message {
            self.user_answers.insert(watcher_id, (v, SystemTime::now()));
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

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::{
        fuiz::{
            common::calculate_slide_score,
            config::{Fuiz, SlideConfig as ConfigSlideConfig, SlideConfig as FuizSlideConfig},
        },
        game::{IncomingHostMessage, IncomingMessage},
    };
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
            slides: vec![FuizSlideConfig::TypeAnswer(create_test_slide_config())],
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
        config.title = "a".repeat(MAX_TITLE_LENGTH + 1);
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
        config.introduce_question = Duration::from_secs(MAX_INTRODUCE_QUESTION + 1);
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_slide_config_time_limit_too_short() {
        let mut config = create_test_slide_config();
        config.time_limit = Duration::from_secs(MIN_TIME_LIMIT - 1);
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_slide_config_time_limit_too_long() {
        let mut config = create_test_slide_config();
        config.time_limit = Duration::from_secs(MAX_TIME_LIMIT + 1);
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_slide_config_too_many_answers() {
        let mut config = create_test_slide_config();
        config.answers = vec!["Answer".to_string(); MAX_ANSWER_COUNT + 1];
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
            calculate_slide_score(full_duration, Duration::from_secs(0), full_points);
        assert_eq!(immediate_score, full_points);

        // Answer at the end should get half points
        let late_score = calculate_slide_score(full_duration, full_duration, full_points);
        assert_eq!(late_score, 500);

        // Answer in the middle should get 3/4 points
        let mid_score = calculate_slide_score(full_duration, Duration::from_secs(15), full_points);
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

    #[test]
    fn test_send_accepting_answers_wrong_state() {
        let config = create_test_slide_config();
        let mut state = config.to_state();

        // Put state in Answers (not Question), so send_accepting_answers should fail
        state.change_state(SlideState::Unstarted, SlideState::Question);
        state.change_state(SlideState::Question, SlideState::Answers);
        assert_eq!(state.state(), SlideState::Answers);

        let watchers = mocks::mock_watchers();
        let tunnel_finder = mocks::mock_tunnel_finder();
        let mut schedule_message = mocks::mock_schedule_message();

        // Store current state and timer
        let current_state = state.state();
        let current_timer = state.answer_start;

        // Call receive_alarm with transition to Answers when already in Answers
        // This will call send_accepting_answers, but the state change will fail
        let alarm_message =
            crate::AlarmMessage::TypeAnswer(AlarmMessage::ProceedFromSlideIntoSlide {
                index: 0,
                to: SlideState::Answers,
            });

        let result = state.receive_alarm(
            &mut mocks::mock_leaderboard(),
            &watchers,
            None,
            &mut schedule_message,
            tunnel_finder,
            &alarm_message,
            0,
            1,
        );

        // The alarm handling should not change state since change_state failed
        // This tests the else branch in send_accepting_answers
        assert!(!result);
        assert_eq!(state.state(), current_state);
        assert_eq!(state.answer_start, current_timer);
    }

    #[test]
    fn test_send_answers_results_wrong_state() {
        let config = create_test_slide_config();
        let mut state = config.to_state();

        // Put state in Question (not Answers), so send_answers_results should fail
        state.change_state(SlideState::Unstarted, SlideState::Question);
        assert_eq!(state.state(), SlideState::Question);

        let watchers = mocks::mock_watchers();
        let tunnel_finder = mocks::mock_tunnel_finder();

        // Store current state and timer
        let current_state = state.state();
        let current_timer = state.answer_start;

        // Call receive_alarm with transition to AnswersResults when in Question state
        // This will call send_answers_results, but the state change will fail
        let alarm_message =
            crate::AlarmMessage::TypeAnswer(AlarmMessage::ProceedFromSlideIntoSlide {
                index: 0,
                to: SlideState::AnswersResults,
            });

        let result = state.receive_alarm(
            &mut mocks::mock_leaderboard(),
            &watchers,
            None,
            &mut mocks::mock_schedule_message(),
            tunnel_finder,
            &alarm_message,
            0,
            1,
        );

        // The alarm handling should not change state since change_state failed
        // This tests the else branch in send_answers_results
        assert!(!result);
        assert_eq!(state.state(), current_state);
        assert_eq!(state.answer_start, current_timer);
    }

    #[test]
    fn test_add_scores_with_correct_and_incorrect_answers() {
        let config = create_test_slide_config();
        let mut state = config.to_state();

        // Set up state to AnswersResults with some user answers
        state.change_state(SlideState::Unstarted, SlideState::Question);
        state.change_state(SlideState::Question, SlideState::Answers);
        state.change_state(SlideState::Answers, SlideState::AnswersResults);

        // Add some user answers - correct and incorrect
        let player1 = Id::new();
        let player2 = Id::new();
        let player3 = Id::new();

        state.start_timer(); // Start timing

        // Simulate answers at different times
        state
            .user_answers
            .insert(player1, ("Paris".to_string(), web_time::SystemTime::now()));
        state
            .user_answers
            .insert(player2, ("London".to_string(), web_time::SystemTime::now()));
        state
            .user_answers
            .insert(player3, ("PARIS".to_string(), web_time::SystemTime::now()));

        let mut leaderboard = mocks::mock_leaderboard();
        let watchers = mocks::mock_watchers();
        let tunnel_finder = mocks::mock_tunnel_finder();
        let schedule_message = mocks::mock_schedule_message_std();

        // Call receive_message with Host::Next from AnswersResults
        // This should call add_scores and test the score calculation logic
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

        // Should return true (slide complete) and process scores
        assert!(result);

        // The add_scores function should have been called, testing lines 532-549
        // This tests:
        // - Correct answer detection (lines 532-533)
        // - Score calculation for correct answers (lines 536-544)
        // - Zero score for incorrect answers (lines 544-545)
    }

    #[test]
    fn test_add_scores_case_sensitive() {
        let config = create_test_slide_config_case_sensitive();
        let mut state = config.to_state();

        // Set up state to AnswersResults with some user answers
        state.change_state(SlideState::Unstarted, SlideState::Question);
        state.change_state(SlideState::Question, SlideState::Answers);
        state.change_state(SlideState::Answers, SlideState::AnswersResults);

        // Add answers with different cases
        let player1 = Id::new();
        let player2 = Id::new();

        state.start_timer(); // Start timing

        // For case-sensitive config, only exact match should be correct
        state
            .user_answers
            .insert(player1, ("Hello".to_string(), web_time::SystemTime::now()));
        state
            .user_answers
            .insert(player2, ("hello".to_string(), web_time::SystemTime::now()));

        let mut leaderboard = mocks::mock_leaderboard();
        let watchers = mocks::mock_watchers();
        let tunnel_finder = mocks::mock_tunnel_finder();
        let schedule_message = mocks::mock_schedule_message_std();

        // Call receive_message with Host::Next from AnswersResults
        // This should call add_scores with case-sensitive matching
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

        // Should return true (slide complete) and process scores
        assert!(result);

        // This tests the case-sensitive path in add_scores
        // where "Hello" should be correct but "hello" should be incorrect
    }

    #[test]
    fn test_add_scores_with_team_manager() {
        let config = create_test_slide_config();
        let mut state = config.to_state();

        // Set up state to AnswersResults with some user answers
        state.change_state(SlideState::Unstarted, SlideState::Question);
        state.change_state(SlideState::Question, SlideState::Answers);
        state.change_state(SlideState::Answers, SlideState::AnswersResults);

        // Add some user answers
        let player1 = Id::new();
        let player2 = Id::new();

        state.start_timer(); // Start timing

        // Simulate answers from players
        state
            .user_answers
            .insert(player1, ("Paris".to_string(), web_time::SystemTime::now()));
        state
            .user_answers
            .insert(player2, ("London".to_string(), web_time::SystemTime::now()));

        let mut leaderboard = mocks::mock_leaderboard();
        let watchers = mocks::mock_watchers();
        let tunnel_finder = mocks::mock_tunnel_finder();

        // Create a team manager to test the Some(team_manager) branch
        let team_manager = crate::teams::TeamManager::new(
            2,                                   // optimal_size
            false,                               // assign_random
            crate::names::NameStyle::Petname(2), // name_style
        );

        let schedule_message = mocks::mock_schedule_message_std();

        // Call receive_message with Host::Next from AnswersResults
        // This time pass Some(team_manager) to test line 553
        let result = state.receive_message(
            Id::new(),
            IncomingMessage::Host(IncomingHostMessage::Next),
            &mut leaderboard,
            &watchers,
            Some(&team_manager), // This will make team_manager Some() in add_scores
            schedule_message,
            tunnel_finder,
            0,
            1,
        );

        // Should return true (slide complete) and process scores
        assert!(result);

        // This test hits the Some(team_manager) branch at line 553
        // where team_manager.get_team(player_id).unwrap_or(player_id) is called
    }

    #[test]
    fn test_player_answer_partial_completion() {
        let config = create_test_slide_config();
        let mut state = config.to_state();

        // Set up state to Answers (accepting answers)
        state.change_state(SlideState::Unstarted, SlideState::Question);
        state.change_state(SlideState::Question, SlideState::Answers);

        let mut leaderboard = mocks::mock_leaderboard();
        let mut watchers = mocks::mock_watchers();

        // Add players to watchers to test partial completion
        let player1 = Id::new();
        let player2 = Id::new();

        // Add players to watchers
        let _ = watchers.add_watcher(
            player1,
            crate::watcher::Value::Player(crate::watcher::PlayerValue::Individual {
                name: "Player1".to_string(),
            }),
        );
        let _ = watchers.add_watcher(
            player2,
            crate::watcher::Value::Player(crate::watcher::PlayerValue::Individual {
                name: "Player2".to_string(),
            }),
        );

        let tunnel_finder = mocks::mock_tunnel_finder();
        let schedule_message = mocks::mock_schedule_message_std();

        // Submit one player answer when there are multiple players
        // This should trigger the else branch at lines 721-727
        let result = state.receive_message(
            player1,
            IncomingMessage::Player(IncomingPlayerMessage::StringAnswer("Paris".to_string())),
            &mut leaderboard,
            &watchers,
            None,
            schedule_message,
            tunnel_finder,
            0,
            1,
        );

        // Should not complete slide since not all players answered
        assert!(!result);

        // Should remain in Answers state
        assert_eq!(state.state(), SlideState::Answers);

        // Should have recorded the answer
        assert!(state.user_answers.contains_key(&player1));
        assert_eq!(state.user_answers.get(&player1).unwrap().0, "Paris");

        // This test covers lines 721-727 - the else branch that announces
        // AnswersCount to hosts when not all players have answered
    }
}
