//! Multiple choice question implementation
//!
//! This module implements the multiple choice question type for Fuiz games.
//! Multiple choice questions present a question followed by several answer
//! options, allowing players to select one correct answer. The module handles
//! timing, scoring, answer validation, and result presentation.

use std::{
    collections::HashMap,
    time::{self, Duration},
};

use garde::Validate;
use itertools::Itertools;
use serde::{Deserialize, Serialize};
use serde_with::DurationMilliSeconds;
use web_time::SystemTime;

use crate::{
    fuiz::config::SlideAction,
    leaderboard::Leaderboard,
    session::Tunnel,
    teams::TeamManager,
    watcher::{Id, ValueKind, Watchers},
};

use super::{
    super::constants::multiple_choice::*,
    super::game::IncomingPlayerMessage,
    common::{
        AnswerHandler, QuestionReceiveMessage, SlideStateManager, SlideTimer,
        add_scores_to_leaderboard, all_players_answered, get_answered_count, validate_duration,
    },
    config::TextOrMedia,
    media::Media,
};

// Re-export SlideState publicly from slide_traits
pub use super::common::SlideState;

/// Configuration for a multiple choice question slide
///
/// This struct defines all the parameters needed to create and present
/// a multiple choice question, including timing, content, scoring, and
/// the available answer options.
#[derive(Debug, Clone, Serialize, serde::Deserialize, Validate)]
pub struct SlideConfig {
    /// The question text that will be displayed to players
    #[garde(length(min = MIN_TITLE_LENGTH, max = MAX_TITLE_LENGTH))]
    title: String,
    /// Optional media content (images, etc.) to accompany the question
    #[garde(dive)]
    media: Option<Media>,
    /// Duration to display the question before revealing answer options
    #[garde(custom(validate_duration::<MIN_INTRODUCE_QUESTION, MAX_INTRODUCE_QUESTION>))]
    #[serde(with = "serde_with::As::<DurationMilliSeconds<u64>>")]
    introduce_question: Duration,
    /// Duration players have to select their answer once options are revealed
    #[garde(custom(validate_duration::<MIN_TIME_LIMIT, MAX_TIME_LIMIT>))]
    #[serde(with = "serde_with::As::<DurationMilliSeconds<u64>>")]
    time_limit: Duration,
    /// Maximum points awarded for a correct answer (decreases linearly over time)
    #[garde(skip)]
    points_awarded: u64,
    /// The available answer choices for this question
    #[garde(length(max = MAX_ANSWER_COUNT))]
    answers: Vec<AnswerChoice>,
}

/// Runtime state for a multiple choice question during gameplay
///
/// This struct maintains the dynamic state of a multiple choice question
/// as it progresses through its phases, tracking player responses,
/// timing information, and current presentation state.
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct State {
    /// The configuration this state was created from
    /// The configuration this state was created from
    config: SlideConfig,

    // Runtime State
    /// Stores player answers along with the timestamp when they were submitted
    user_answers: HashMap<Id, (usize, SystemTime)>,
    /// The time when answer options were first displayed to players
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
            user_answers: HashMap::new(),
            answer_start: None,
            state: SlideState::Unstarted,
        }
    }
}

/// Utility type for conditionally hiding content based on viewer permissions
///
/// This enum allows content to be visible to some participants (like hosts)
/// while being hidden from others (like players) until the appropriate time.
#[derive(Debug, Serialize, Clone)]
pub enum PossiblyHidden<T> {
    /// Content is visible to the recipient
    Visible(T),
    /// Content is hidden from the recipient
    Hidden,
}

/// Update messages sent to participants during multiple choice questions
///
/// These messages inform participants about changes in the question state,
/// such as when new phases begin or when results become available.
/// They are sent to participants who already have some context about the slide.
#[derive(Debug, Serialize, Clone)]
pub enum UpdateMessage {
    /// Announces the question without revealing answer options
    QuestionAnnouncement {
        /// Index of the current slide (0-based)
        index: usize,
        /// Total number of slides in the game
        count: usize,
        /// The question text being asked
        question: String,
        /// Optional media content accompanying the question
        media: Option<Media>,
        /// Duration before answer options will be revealed
        #[serde(with = "serde_with::As::<DurationMilliSeconds<u64>>")]
        duration: Duration,
    },
    /// Announces the answer options for player selection
    AnswersAnnouncement {
        /// Duration before the answering phase ends
        #[serde(with = "serde_with::As::<DurationMilliSeconds<u64>>")]
        duration: Duration,
        /// Answer options (may be hidden from some participants)
        answers: Vec<PossiblyHidden<TextOrMedia>>,
    },
    /// (HOST ONLY) Reports the number of players who have submitted answers
    AnswersCount(usize),
    /// Shows the results with correct answers and response statistics
    AnswersResults {
        /// All answer options for the question
        answers: Vec<TextOrMedia>,
        /// Results showing correctness and selection statistics
        results: Vec<AnswerChoiceResult>,
    },
}

/// Alarm messages for timed events in multiple choice questions
///
/// These messages are used internally to trigger state transitions
/// at scheduled times during question presentation.
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

/// Synchronization messages for participants joining during multiple choice questions
///
/// These messages provide complete state information to participants who
/// connect or reconnect during a question, allowing them to synchronize
/// their view with the current state. Similar to UpdateMessage but includes
/// additional context needed for synchronization.
#[derive(Debug, Serialize, Clone)]
pub enum SyncMessage {
    /// Synchronizes the question announcement phase
    QuestionAnnouncement {
        /// Index of the current slide
        index: usize,
        /// Total number of slides in the game
        count: usize,
        /// The question text being asked
        question: String,
        /// Optional media content accompanying the question
        media: Option<Media>,
        /// Remaining time before answer options will be revealed
        #[serde(with = "serde_with::As::<DurationMilliSeconds<u64>>")]
        duration: Duration,
    },
    /// Synchronizes the answer selection phase
    AnswersAnnouncement {
        /// Index of the current slide
        index: usize,
        /// Total number of slides in the game
        count: usize,
        /// The question text being asked
        question: String,
        /// Optional media content accompanying the question
        media: Option<Media>,
        /// Remaining time before the answering phase ends
        #[serde(with = "serde_with::As::<DurationMilliSeconds<u64>>")]
        duration: Duration,
        /// Answer options (may be hidden from some participants)
        answers: Vec<PossiblyHidden<TextOrMedia>>,
        /// Number of players who have already answered
        answered_count: usize,
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
        /// All answer options for the question
        answers: Vec<TextOrMedia>,
        /// Results showing correctness and selection statistics
        results: Vec<AnswerChoiceResult>,
    },
}

/// Represents a single answer option in a multiple choice question
///
/// Each answer choice contains the content to display and whether
/// it is a correct answer to the question.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnswerChoice {
    /// Whether this answer choice is correct
    pub correct: bool,
    /// The content of this answer choice (text or media)
    pub content: TextOrMedia,
}

/// Contains correctness information and statistics for an answer choice
///
/// This struct is used in results display to show whether each answer
/// option was correct and how many players selected it.
#[derive(Debug, Serialize, Clone)]
pub struct AnswerChoiceResult {
    /// Whether this answer choice was correct
    correct: bool,
    /// Number of players who selected this answer choice
    count: usize,
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

impl AnswerHandler<usize> for State {
    fn user_answers(&self) -> &HashMap<Id, (usize, SystemTime)> {
        &self.user_answers
    }

    fn user_answers_mut(&mut self) -> &mut HashMap<Id, (usize, SystemTime)> {
        &mut self.user_answers
    }

    fn is_correct_answer(&self, answer: &usize) -> bool {
        self.config.answers.get(*answer).is_some_and(|x| x.correct)
    }

    fn max_points(&self) -> u64 {
        self.config.points_awarded
    }

    fn time_limit(&self) -> Duration {
        self.config.time_limit
    }
}

impl State {
    /// Starts the multiple choice slide by sending initial question announcements
    ///
    /// This method initiates the question flow by transitioning to the question phase
    /// and announcing the question to all participants. It schedules the transition
    /// to the answer phase based on the configured introduction duration.
    ///
    /// # Arguments
    ///
    /// * `team_manager` - Optional team manager for team-based games
    /// * `watchers` - Connection manager for all participants
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
    pub fn play<
        T: Tunnel,
        F: Fn(Id) -> Option<T>,
        S: FnOnce(crate::AlarmMessage, time::Duration),
    >(
        &mut self,
        team_manager: Option<&TeamManager<crate::names::NameStyle>>,
        watchers: &Watchers,
        schedule_message: S,
        tunnel_finder: F,
        index: usize,
        count: usize,
    ) {
        self.send_question_announcements(
            team_manager,
            watchers,
            schedule_message,
            tunnel_finder,
            index,
            count,
        );
    }

    /// Sends the initial question announcement to all participants
    ///
    /// This method handles the transition from Unstarted to Question state,
    /// announcing the question text and media without revealing answer options.
    /// It schedules the transition to the answer phase or immediately proceeds
    /// if no introduction time is configured.
    ///
    /// # Arguments
    ///
    /// * `team_manager` - Optional team manager for team-based games
    /// * `watchers` - Connection manager for all participants
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
    fn send_question_announcements<
        T: Tunnel,
        F: Fn(Id) -> Option<T>,
        S: FnOnce(crate::AlarmMessage, time::Duration),
    >(
        &mut self,
        team_manager: Option<&TeamManager<crate::names::NameStyle>>,
        watchers: &Watchers,
        schedule_message: S,
        tunnel_finder: F,
        index: usize,
        count: usize,
    ) {
        if self.change_state(SlideState::Unstarted, SlideState::Question) {
            watchers.announce(
                &UpdateMessage::QuestionAnnouncement {
                    index,
                    count,
                    question: self.config.title.clone(),
                    media: self.config.media.clone(),
                    duration: self.config.introduce_question,
                }
                .into(),
                &tunnel_finder,
            );

            if self.config.introduce_question.is_zero() {
                self.send_answers_announcements(
                    team_manager,
                    watchers,
                    schedule_message,
                    tunnel_finder,
                    index,
                );
            } else {
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
    }

    /// Transitions to the answer selection phase and reveals answer options
    ///
    /// This method handles the transition from Question to Answers state,
    /// revealing answer options to participants and starting the answer timer.
    /// In team mode, answer options are distributed among team members to
    /// encourage collaboration.
    ///
    /// # Arguments
    ///
    /// * `team_manager` - Optional team manager for team-based games
    /// * `watchers` - Connection manager for all participants
    /// * `schedule_message` - Function to schedule delayed messages for timing
    /// * `tunnel_finder` - Function to find communication tunnels for participants
    /// * `index` - Current slide index in the game
    ///
    /// # Type Parameters
    ///
    /// * `T` - Type implementing the Tunnel trait for participant communication
    /// * `F` - Function type for finding tunnels by participant ID
    /// * `S` - Function type for scheduling alarm messages
    fn send_answers_announcements<
        T: Tunnel,
        F: Fn(Id) -> Option<T>,
        S: FnOnce(crate::AlarmMessage, time::Duration),
    >(
        &mut self,
        team_manager: Option<&TeamManager<crate::names::NameStyle>>,
        watchers: &Watchers,
        schedule_message: S,
        tunnel_finder: F,
        index: usize,
    ) {
        if self.change_state(SlideState::Question, SlideState::Answers) {
            self.start_timer();

            watchers.announce_with(
                |id, kind| match kind {
                    ValueKind::Host | ValueKind::Player => Some(
                        UpdateMessage::AnswersAnnouncement {
                            duration: self.config.time_limit,
                            answers: self.get_answers_for_player(
                                id,
                                kind,
                                {
                                    match &team_manager {
                                        Some(team_manager) => {
                                            team_manager.team_members(id).map_or(1, |members| {
                                                members
                                                    .into_iter()
                                                    .filter(|id| {
                                                        Watchers::is_alive(*id, &tunnel_finder)
                                                    })
                                                    .count()
                                                    .max(1)
                                            })
                                        }
                                        None => 1,
                                    }
                                },
                                {
                                    match &team_manager {
                                        Some(team_manager) => team_manager
                                            .team_index(id, |id| {
                                                Watchers::is_alive(id, &tunnel_finder)
                                            })
                                            .unwrap_or(0),
                                        None => 0,
                                    }
                                },
                                team_manager.is_some(),
                            ),
                        }
                        .into(),
                    ),
                    ValueKind::Unassigned => None,
                },
                &tunnel_finder,
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

    /// Sends the results showing correct answers and player response statistics
    ///
    /// This method handles the transition from Answers to `AnswersResults` state,
    /// revealing the correct answers and showing statistics about how players responded.
    ///
    /// # Arguments
    ///
    /// * `watchers` - Connection manager for all participants
    /// * `tunnel_finder` - Function to find communication tunnels for participants
    ///
    /// # Type Parameters
    ///
    /// * `T` - Type implementing the Tunnel trait for participant communication
    /// * `F` - Function type for finding tunnels by participant ID
    fn send_answers_results<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &mut self,
        watchers: &Watchers,
        tunnel_finder: F,
    ) {
        if self.change_state(SlideState::Answers, SlideState::AnswersResults) {
            let answer_count = self.answer_counts();
            watchers.announce(
                &UpdateMessage::AnswersResults {
                    answers: self
                        .config
                        .answers
                        .iter()
                        .map(|a| a.content.clone())
                        .collect_vec(),
                    results: self
                        .config
                        .answers
                        .iter()
                        .enumerate()
                        .map(|(i, a)| AnswerChoiceResult {
                            correct: a.correct,
                            count: *answer_count.get(&i).unwrap_or(&0),
                        })
                        .collect_vec(),
                }
                .into(),
                tunnel_finder,
            );
        }
    }

    /// Determines which answer options should be visible to a specific participant
    ///
    /// In individual games, players see all answer options. In team games, answer
    /// options are distributed among team members to encourage collaboration.
    /// Hosts see all options in individual mode but none in team mode.
    ///
    /// # Arguments
    ///
    /// * `_id` - The participant's ID (currently unused)
    /// * `watcher_kind` - The type of participant (host, player, unassigned)
    /// * `team_size` - Number of active members in the participant's team
    /// * `team_index` - The participant's index within their team
    /// * `is_team` - Whether this is a team-based game
    ///
    /// # Returns
    ///
    /// A vector of answer options, some potentially hidden based on game mode and participant role
    fn get_answers_for_player(
        &self,
        _id: Id,
        watcher_kind: ValueKind,
        team_size: usize,
        team_index: usize,
        is_team: bool,
    ) -> Vec<PossiblyHidden<TextOrMedia>> {
        match watcher_kind {
            ValueKind::Host | ValueKind::Unassigned => {
                if is_team {
                    std::iter::repeat_n(PossiblyHidden::Hidden, self.config.answers.len())
                        .collect_vec()
                } else {
                    self.config
                        .answers
                        .iter()
                        .map(|answer_choice| PossiblyHidden::Visible(answer_choice.content.clone()))
                        .collect_vec()
                }
            }
            ValueKind::Player => match self.config.answers.len() {
                0 => Vec::new(),
                answer_count => {
                    let adjusted_team_index = (team_index % team_size) % answer_count;

                    self.config
                        .answers
                        .iter()
                        .enumerate()
                        .map(|(answer_index, answer_choice)| {
                            if answer_index % team_size == adjusted_team_index {
                                PossiblyHidden::Visible(answer_choice.content.clone())
                            } else {
                                PossiblyHidden::Hidden
                            }
                        })
                        .collect_vec()
                }
            },
        }
    }

    /// Generates a synchronization message for a participant joining during the question
    ///
    /// This method creates the appropriate sync message based on the current slide state,
    /// allowing newly connected participants to see the current question state with
    /// correct timing and answer visibility.
    ///
    /// # Arguments
    ///
    /// * `watcher_id` - ID of the participant to synchronize
    /// * `watcher_kind` - Type of participant (host, player, unassigned)
    /// * `team_manager` - Optional team manager for team-based games
    /// * `watchers` - Connection manager for all participants
    /// * `tunnel_finder` - Function to find communication tunnels for participants
    /// * `index` - Current slide index in the game
    /// * `count` - Total number of slides in the game
    ///
    /// # Returns
    ///
    /// A `SyncMessage` appropriate for the current state and participant type
    ///
    /// # Type Parameters
    ///
    /// * `T` - Type implementing the Tunnel trait for participant communication
    /// * `F` - Function type for finding tunnels by participant ID
    pub fn state_message<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &self,
        watcher_id: Id,
        watcher_kind: ValueKind,
        team_manager: Option<&TeamManager<crate::names::NameStyle>>,
        watchers: &Watchers,
        tunnel_finder: F,
        index: usize,
        count: usize,
    ) -> SyncMessage {
        match self.state() {
            SlideState::Unstarted | SlideState::Question => SyncMessage::QuestionAnnouncement {
                index,
                count,
                question: self.config.title.clone(),
                media: self.config.media.clone(),
                duration: self
                    .config
                    .introduce_question
                    .saturating_sub(self.elapsed()),
            },
            SlideState::Answers => SyncMessage::AnswersAnnouncement {
                index,
                count,
                question: self.config.title.clone(),
                media: self.config.media.clone(),
                duration: self.config.time_limit.saturating_sub(self.elapsed()),
                answers: self.get_answers_for_player(
                    watcher_id,
                    watcher_kind,
                    {
                        match &team_manager {
                            Some(team_manager) => {
                                team_manager.team_members(watcher_id).map_or(1, |members| {
                                    members
                                        .into_iter()
                                        .filter(|id| Watchers::is_alive(*id, &tunnel_finder))
                                        .collect_vec()
                                        .len()
                                        .max(1)
                                })
                            }
                            None => 1,
                        }
                    },
                    {
                        match &team_manager {
                            Some(team_manager) => team_manager
                                .team_index(watcher_id, |id| Watchers::is_alive(id, &tunnel_finder))
                                .unwrap_or(0),
                            None => 0,
                        }
                    },
                    team_manager.is_some(),
                ),
                answered_count: get_answered_count(self, watchers, &tunnel_finder),
            },
            SlideState::AnswersResults => {
                let answer_count = self.answer_counts();

                SyncMessage::AnswersResults {
                    index,
                    count,
                    question: self.config.title.clone(),
                    media: self.config.media.clone(),
                    answers: self
                        .config
                        .answers
                        .iter()
                        .map(|a| a.content.clone())
                        .collect_vec(),
                    results: self
                        .config
                        .answers
                        .iter()
                        .enumerate()
                        .map(|(i, a)| AnswerChoiceResult {
                            correct: a.correct,
                            count: *answer_count.get(&i).unwrap_or(&0),
                        })
                        .collect_vec(),
                }
            }
        }
    }

    /// Handles scheduled alarm messages for timed state transitions
    ///
    /// This method processes alarm messages that trigger automatic transitions
    /// between slide states at predetermined times, such as moving from question
    /// display to answer selection or from answers to results.
    ///
    /// # Arguments
    ///
    /// * `_leaderboard` - Mutable reference to the game leaderboard (unused)
    /// * `watchers` - Connection manager for all participants
    /// * `team_manager` - Optional team manager for team-based games
    /// * `schedule_message` - Function to schedule delayed messages for timing
    /// * `tunnel_finder` - Function to find communication tunnels for participants
    /// * `message` - The alarm message to process
    /// * `index` - Current slide index in the game
    /// * `_count` - Total number of slides in the game (unused)
    ///
    /// # Returns
    ///
    /// A `SlideAction` indicating whether to stay on the current slide or advance
    ///
    /// # Type Parameters
    ///
    /// * `T` - Type implementing the Tunnel trait for participant communication
    /// * `F` - Function type for finding tunnels by participant ID
    /// * `S` - Function type for scheduling alarm messages
    pub(crate) fn receive_alarm<
        T: Tunnel,
        F: Fn(Id) -> Option<T>,
        S: FnOnce(crate::AlarmMessage, std::time::Duration),
    >(
        &mut self,
        _leaderboard: &mut Leaderboard,
        watchers: &Watchers,
        team_manager: Option<&TeamManager<crate::names::NameStyle>>,
        schedule_message: S,
        tunnel_finder: F,
        message: &crate::AlarmMessage,
        index: usize,
        _count: usize,
    ) -> SlideAction<S> {
        if let crate::AlarmMessage::MultipleChoice(AlarmMessage::ProceedFromSlideIntoSlide {
            index: _,
            to,
        }) = message
        {
            match to {
                SlideState::Answers => {
                    self.send_answers_announcements(
                        team_manager,
                        watchers,
                        schedule_message,
                        tunnel_finder,
                        index,
                    );
                }
                SlideState::AnswersResults => self.send_answers_results(watchers, tunnel_finder),
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
                    team_manager,
                    watchers,
                    schedule_message,
                    tunnel_finder,
                    index,
                    count,
                );
            }
            SlideState::Question => {
                self.send_answers_announcements(
                    team_manager,
                    watchers,
                    schedule_message,
                    tunnel_finder,
                    index,
                );
            }
            SlideState::Answers => self.send_answers_results(watchers, tunnel_finder),
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
        if let IncomingPlayerMessage::IndexAnswer(v) = message
            && v < self.config.answers.len()
        {
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
