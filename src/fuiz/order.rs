//! Order/ranking question implementation
//!
//! This module implements the order question type for Fuiz games.
//! Order questions present a set of items that players must arrange
//! in a specific sequence. Players drag and drop or reorder items
//! to match the correct ordering, and scoring is based on how close
//! their arrangement is to the correct order.

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

/// Represents the current phase of an order question slide
///
/// Order questions progress through phases: first showing the question
/// and items to be ordered, then accepting player arrangements,
/// and finally showing the correct order with comparison results.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum SlideState {
    /// Initial state before the slide has started (treated same as Question)
    #[default]
    Unstarted,
    /// Displaying the question and items without ordering interface
    Question,
    /// Accepting item arrangements from players
    Answers,
    /// Displaying results with correct order and player comparisons
    AnswersResults,
}

type ValidationResult = garde::Result;

/// Validates that a duration falls within specified bounds
///
/// This helper function ensures that timing parameters for order questions
/// fall within acceptable ranges as defined by the game constants.
///
/// # Arguments
///
/// * `field` - Name of the field being validated (for error messages)
/// * `val` - The duration value to validate
///
/// # Returns
///
/// `Ok(())` if the duration is valid, `Err` with descriptive message if not
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

/// Validates the time limit for answering an order question
fn validate_time_limit(val: &Duration) -> ValidationResult {
    validate_duration::<
        { crate::constants::order::MIN_TIME_LIMIT },
        { crate::constants::order::MAX_TIME_LIMIT },
    >("time_limit", val)
}

/// Validates the duration for introducing a question before showing items
fn validate_introduce_question(val: &Duration) -> ValidationResult {
    validate_duration::<
        { crate::constants::order::MIN_INTRODUCE_QUESTION },
        { crate::constants::order::MAX_INTRODUCE_QUESTION },
    >("introduce_question", val)
}

/// Labels for the ordering axis in an order question
///
/// These labels help players understand what the ordering represents,
/// such as "Earliest" to "Latest" or "Smallest" to "Largest".
#[skip_serializing_none]
#[derive(Debug, Clone, Default, Serialize, serde::Deserialize, Validate)]
pub struct AxisLabels {
    /// Label for the start/left end of the ordering axis
    #[garde(length(chars, max = crate::constants::order::MAX_LABEL_LENGTH))]
    from: Option<String>,
    /// Label for the end/right end of the ordering axis
    #[garde(length(chars, max = crate::constants::order::MAX_LABEL_LENGTH))]
    to: Option<String>,
}

/// Configuration for an order question slide
///
/// Contains all the settings and content for a single order question,
/// including the question text, media, timing, items to be ordered,
/// and axis labels for the ordering interface.
#[serde_with::serde_as]
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, serde::Deserialize, Validate)]
pub struct SlideConfig {
    /// The question title, represents what's being asked
    #[garde(length(chars, min = crate::constants::order::MIN_TITLE_LENGTH, max = crate::constants::order::MAX_TITLE_LENGTH))]
    title: String,
    /// Accompanying media
    #[garde(dive)]
    media: Option<Media>,
    /// Time before the question is displayed
    #[garde(custom(|v, _| validate_introduce_question(v)))]
    #[serde_as(as = "serde_with::DurationMilliSeconds<u64>")]
    introduce_question: Duration,
    /// Time where players can answer the question
    #[garde(custom(|v, _| validate_time_limit(v)))]
    #[serde_as(as = "serde_with::DurationMilliSeconds<u64>")]
    time_limit: Duration,
    /// Maximum number of points awarded the question, decreases linearly to half the amount by the end of the slide
    #[garde(skip)]
    points_awarded: u64,
    /// Accompanying answers in the correct order
    #[garde(length(max = crate::constants::order::MAX_ANSWER_COUNT),
        inner(length(chars, max = crate::constants::answer_text::MAX_LENGTH))
    )]
    answers: Vec<String>,
    /// From and to labels for the order
    #[garde(dive)]
    axis_labels: AxisLabels,
}

/// Runtime state for an order question during gameplay
///
/// This struct maintains the dynamic state of an order question as it
/// progresses through its phases, tracking player arrangements, timing
/// information, shuffled item order, and current presentation state.
#[serde_with::serde_as]
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct State {
    /// The configuration this state was created from
    config: SlideConfig,

    // Runtime State
    /// Items in shuffled order as presented to players
    shuffled_answers: Vec<String>,
    /// Player arrangements with submission timestamps
    user_answers: HashMap<Id, (Vec<String>, SystemTime)>,
    /// Time when the ordering interface was first displayed
    answer_start: Option<SystemTime>,
    /// Current phase of the slide presentation
    state: SlideState,
}

impl SlideConfig {
    /// Creates a new runtime state from this configuration
    ///
    /// This method initializes a fresh state for gameplay, setting up
    /// empty answer tracking, unshuffled items, and the initial unstarted phase.
    ///
    /// # Returns
    ///
    /// A new `State` ready for gameplay
    pub fn to_state(&self) -> State {
        State {
            config: self.clone(),
            shuffled_answers: Vec::new(),
            user_answers: HashMap::new(),
            answer_start: None,
            state: SlideState::Unstarted,
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
    },
    /// Announcement of the question with its answers
    AnswersAnnouncement {
        /// Labels for the axis
        axis_labels: AxisLabels,
        /// Answers in a shuffled order
        answers: Vec<String>,
        /// Time where players can answer the question
        #[serde_as(as = "serde_with::DurationMilliSeconds<u64>")]
        duration: Duration,
    },
    /// (HOST ONLY): Number of players who answered the question
    AnswersCount(usize),
    /// Results of the game including correct answers and statistics of how many they got chosen
    AnswersResults {
        /// Correct answers
        answers: Vec<String>,
        /// Statistics of how many players got it right and wrong
        results: (usize, usize),
    },
}

/// Alarm messages for timed events in order questions
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
    },
    /// Announcement of the question with its answers
    AnswersAnnouncement {
        /// Index of the current slide
        index: usize,
        /// Total number of slides in the game
        count: usize,
        /// The question text being asked
        question: String,
        /// Labels for the ordering axis
        axis_labels: AxisLabels,
        /// Optional media content accompanying the question
        media: Option<Media>,
        /// Items to be ordered in shuffled arrangement
        answers: Vec<String>,
        /// Time where players can answer the question
        #[serde_as(as = "serde_with::DurationMilliSeconds<u64>")]
        duration: Duration,
    },
    /// Results of the game including correct answers and statistics of how many they got chosen
    AnswersResults {
        /// Index of the current slide
        index: usize,
        /// Total number of slides in the game
        count: usize,
        /// The question text that was asked
        question: String,
        /// Labels for the ordering axis
        axis_labels: AxisLabels,
        /// Optional media content that accompanied the question
        media: Option<Media>,
        /// Items in the correct order
        answers: Vec<String>,
        /// Statistics: (correct_count, incorrect_count)
        results: (usize, usize),
    },
}

impl State {
    /// Starts the order slide by sending initial question announcements
    ///
    /// This method initiates the question flow by transitioning to the question phase
    /// and announcing the question to all participants. It schedules the transition
    /// to the ordering phase based on the configured introduction duration.
    ///
    /// # Arguments
    ///
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

    /// Calculates the score for a player based on how quickly they submitted their arrangement
    ///
    /// The scoring system awards full points for immediate submissions and
    /// decreases linearly to half points at the end of the time limit.
    ///
    /// # Arguments
    ///
    /// * `full_duration` - Total time allowed for ordering
    /// * `taken_duration` - Time taken by the player to submit their arrangement
    /// * `full_points_awarded` - Maximum points possible for this question
    ///
    /// # Returns
    ///
    /// The calculated score (between half and full points)
    fn calculate_score(
        full_duration: Duration,
        taken_duration: Duration,
        full_points_awarded: u64,
    ) -> u64 {
        (full_points_awarded as f64
            * (1. - (taken_duration.as_secs_f64() / full_duration.as_secs_f64() / 2.)))
            as u64
    }

    /// Records the current time as the start of the ordering phase
    fn start_timer(&mut self) {
        self.answer_start = Some(SystemTime::now());
    }

    /// Returns the start time of the current phase
    ///
    /// # Returns
    ///
    /// The `SystemTime` when the current phase started, or current time if not set
    fn timer(&self) -> SystemTime {
        self.answer_start.unwrap_or(SystemTime::now())
    }

    /// Sends the initial question announcement to all participants
    ///
    /// This method handles the transition from Unstarted to Question state,
    /// announcing the question text and media without revealing the items to order.
    /// It schedules the transition to the ordering phase or immediately proceeds
    /// if no introduction time is configured.
    ///
    /// # Arguments
    ///
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
                    watchers,
                    tunnel_finder,
                    schedule_message,
                    index,
                    count,
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

    /// Transitions to the ordering phase and reveals shuffled items
    ///
    /// This method handles the transition from Question to Answers state,
    /// shuffling the items and revealing them to participants for ordering.
    /// It starts the ordering timer and schedules the transition to results.
    ///
    /// # Arguments
    ///
    /// * `watchers` - Connection manager for all participants
    /// * `tunnel_finder` - Function to find communication tunnels for participants
    /// * `schedule_message` - Function to schedule delayed messages for timing
    /// * `index` - Current slide index in the game
    /// * `_count` - Total number of slides in the game (unused)
    ///
    /// # Type Parameters
    ///
    /// * `T` - Type implementing the Tunnel trait for participant communication
    /// * `F` - Function type for finding tunnels by participant ID
    /// * `S` - Function type for scheduling alarm messages
    fn send_answers_announcements<
        T: Tunnel,
        F: Fn(Id) -> Option<T>,
        S: FnMut(crate::AlarmMessage, time::Duration),
    >(
        &mut self,
        watchers: &Watchers,
        tunnel_finder: F,
        mut schedule_message: S,
        index: usize,
        _count: usize,
    ) {
        if self.change_state(SlideState::Question, SlideState::Answers) {
            self.shuffled_answers.clone_from(&self.config.answers);
            fastrand::shuffle(&mut self.shuffled_answers);

            self.start_timer();

            watchers.announce(
                &UpdateMessage::AnswersAnnouncement {
                    axis_labels: self.config.axis_labels.clone(),
                    answers: self.shuffled_answers.clone(),
                    duration: self.config.time_limit,
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
    /// This method provides safe state transitions by checking that the current
    /// state matches the expected "before" state before changing to the "after" state.
    ///
    /// # Arguments
    ///
    /// * `before` - Expected current state
    /// * `after` - Target state to transition to
    ///
    /// # Returns
    ///
    /// `true` if the transition was successful, `false` if the current state didn't match
    fn change_state(&mut self, before: SlideState, after: SlideState) -> bool {
        if self.state == before {
            self.state = after;

            true
        } else {
            false
        }
    }

    /// Returns the current state of the slide
    ///
    /// # Returns
    ///
    /// The current `SlideState` of this order question
    fn state(&self) -> SlideState {
        self.state
    }

    /// Sends the results showing correct order and player statistics
    ///
    /// This method handles the transition from Answers to `AnswersResults` state,
    /// revealing the correct order and showing statistics about player responses.
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
            let correct_count = self
                .user_answers
                .iter()
                .filter(|(_, (answers, _))| answers == &self.config.answers)
                .count();

            watchers.announce(
                &UpdateMessage::AnswersResults {
                    answers: self.config.answers.iter().cloned().collect_vec(),
                    results: (correct_count, self.user_answers.len() - correct_count),
                }
                .into(),
                tunnel_finder,
            );
        }
    }

    /// Calculates and adds scores to the leaderboard based on player arrangements
    ///
    /// This method evaluates all player arrangements against the correct order,
    /// calculates scores based on correctness and response time, and updates the
    /// leaderboard. In team mode, it uses the best score from team members.
    ///
    /// # Arguments
    ///
    /// * `leaderboard` - Mutable reference to the game leaderboard
    /// * `watchers` - Connection manager for all participants
    /// * `team_manager` - Optional team manager for team-based games
    /// * `tunnel_finder` - Function to find communication tunnels for participants
    ///
    /// # Type Parameters
    ///
    /// * `T` - Type implementing the Tunnel trait for participant communication
    /// * `F` - Function type for finding tunnels by participant ID
    fn add_scores<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &self,
        leaderboard: &mut Leaderboard,
        watchers: &Watchers,
        team_manager: Option<&TeamManager<crate::names::NameStyle>>,
        tunnel_finder: F,
    ) {
        let starting_instant = self.timer();

        leaderboard.add_scores(
            &self
                .user_answers
                .iter()
                .map(|(id, (answers, instant))| {
                    let correct = answers == &self.config.answers;
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
                    )
                })
                .into_grouping_map_by(|(id, _)| {
                    let player_id = *id;
                    match &team_manager {
                        Some(team_manager) => team_manager.get_team(player_id).unwrap_or(player_id),
                        None => player_id,
                    }
                })
                .min_by_key(|_, (_, score)| *score)
                .into_iter()
                .map(|(id, (_, score))| (id, score))
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

    /// Generates a synchronization message for a participant joining during the question
    ///
    /// This method creates the appropriate sync message based on the current slide state,
    /// allowing newly connected participants to see the current question state with
    /// correct timing and item arrangement.
    ///
    /// # Arguments
    ///
    /// * `_watcher_id` - ID of the participant to synchronize (unused)
    /// * `_watcher_kind` - Type of participant (unused)
    /// * `_team_manager` - Optional team manager for team-based games (unused)
    /// * `_watchers` - Connection manager for all participants (unused)
    /// * `_tunnel_finder` - Function to find communication tunnels (unused)
    /// * `index` - Current slide index in the game
    /// * `count` - Total number of slides in the game
    ///
    /// # Returns
    ///
    /// A `SyncMessage` appropriate for the current state
    ///
    /// # Type Parameters
    ///
    /// * `T` - Type implementing the Tunnel trait for participant communication
    /// * `F` - Function type for finding tunnels by participant ID
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
            },
            SlideState::Answers => SyncMessage::AnswersAnnouncement {
                index,
                count,
                question: self.config.title.clone(),
                axis_labels: self.config.axis_labels.clone(),
                media: self.config.media.clone(),
                answers: self.shuffled_answers.clone(),
                duration: self.config.time_limit
                    - self.timer().elapsed().expect("system clock went backwards"),
            },
            SlideState::AnswersResults => SyncMessage::AnswersResults {
                index,
                count,
                question: self.config.title.clone(),
                axis_labels: self.config.axis_labels.clone(),
                media: self.config.media.clone(),
                answers: self.config.answers.clone(),
                results: {
                    let correct_count = self
                        .user_answers
                        .iter()
                        .filter(|(_, (answers, _))| answers == &self.config.answers)
                        .count();
                    (correct_count, self.user_answers.len() - correct_count)
                },
            },
        }
    }

    /// Handles incoming messages from participants during the order question
    ///
    /// This method processes messages from hosts and players, including host commands
    /// to advance the slide and player arrangement submissions. It manages automatic
    /// progression when all players have submitted their arrangements.
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
                    self.send_answers_announcements(
                        watchers,
                        tunnel_finder,
                        schedule_message,
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
            IncomingMessage::Player(IncomingPlayerMessage::StringArrayAnswer(v)) => {
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

    /// Handles scheduled alarm messages for timed state transitions
    ///
    /// This method processes alarm messages that trigger automatic transitions
    /// between slide states at predetermined times, such as moving from question
    /// display to item ordering or from ordering to results.
    ///
    /// # Arguments
    ///
    /// * `_leaderboard` - Mutable reference to the game leaderboard (unused)
    /// * `watchers` - Connection manager for all participants
    /// * `_team_manager` - Optional team manager for team-based games (unused)
    /// * `schedule_message` - Function to schedule delayed messages for timing
    /// * `tunnel_finder` - Function to find communication tunnels for participants
    /// * `message` - The alarm message to process
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
        if let crate::AlarmMessage::Order(AlarmMessage::ProceedFromSlideIntoSlide {
            index: _,
            to,
        }) = message
        {
            match to {
                SlideState::Answers => {
                    self.send_answers_announcements(
                        watchers,
                        tunnel_finder,
                        schedule_message,
                        index,
                        count,
                    );
                }
                SlideState::AnswersResults => {
                    self.send_answers_results(watchers, tunnel_finder);
                }
                _ => {}
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
    use garde::Validate;
    use std::time::Duration;

    fn create_test_slide_config() -> SlideConfig {
        SlideConfig {
            title: "Order these items".to_string(),
            media: None,
            introduce_question: Duration::from_secs(3),
            time_limit: Duration::from_secs(45),
            points_awarded: 1000,
            answers: vec![
                "First".to_string(),
                "Second".to_string(),
                "Third".to_string(),
                "Fourth".to_string(),
            ],
            axis_labels: AxisLabels {
                from: Some("Start".to_string()),
                to: Some("End".to_string()),
            },
        }
    }

    fn create_test_fuiz() -> Fuiz {
        Fuiz {
            title: "Test Order Quiz".to_string(),
            slides: vec![ConfigSlideConfig::Order(create_test_slide_config())],
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
        config.title = "a".repeat(crate::constants::order::MAX_TITLE_LENGTH + 1);
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
            Duration::from_secs(crate::constants::order::MAX_INTRODUCE_QUESTION + 1);
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_slide_config_time_limit_too_short() {
        let mut config = create_test_slide_config();
        config.time_limit = Duration::from_secs(crate::constants::order::MIN_TIME_LIMIT - 1);
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_slide_config_time_limit_too_long() {
        let mut config = create_test_slide_config();
        config.time_limit = Duration::from_secs(crate::constants::order::MAX_TIME_LIMIT + 1);
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_slide_config_too_many_answers() {
        let mut config = create_test_slide_config();
        config.answers = vec!["Answer".to_string(); crate::constants::order::MAX_ANSWER_COUNT + 1];
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_slide_config_answer_too_long() {
        let mut config = create_test_slide_config();
        config.answers = vec!["a".repeat(crate::constants::answer_text::MAX_LENGTH + 1)];
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_axis_labels_validation() {
        let mut labels = AxisLabels {
            from: Some("a".repeat(crate::constants::order::MAX_LABEL_LENGTH + 1)),
            to: Some("Valid".to_string()),
        };
        assert!(labels.validate().is_err());

        labels.from = Some("Valid".to_string());
        labels.to = Some("a".repeat(crate::constants::order::MAX_LABEL_LENGTH + 1));
        assert!(labels.validate().is_err());

        labels.to = Some("Valid".to_string());
        assert!(labels.validate().is_ok());
    }

    #[test]
    fn test_axis_labels_default() {
        let labels: AxisLabels = AxisLabels::default();
        assert!(labels.from.is_none());
        assert!(labels.to.is_none());
    }

    #[test]
    fn test_slide_config_to_state() {
        let config = create_test_slide_config();
        let state = config.to_state();

        assert_eq!(state.state, SlideState::Unstarted);
        assert!(state.user_answers.is_empty());
        assert!(state.answer_start.is_none());
        assert!(state.shuffled_answers.is_empty());
        assert_eq!(state.config.title, config.title);
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
            ConfigSlideConfig::Order(create_test_slide_config());
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
        let full_duration = Duration::from_secs(45);
        let full_points = 1000;

        // Immediate answer should get full points
        let immediate_score =
            State::calculate_score(full_duration, Duration::from_secs(0), full_points);
        assert_eq!(immediate_score, full_points);

        // Answer at the end should get half points
        let late_score = State::calculate_score(full_duration, full_duration, full_points);
        assert_eq!(late_score, 500);

        // Answer in the middle should get 3/4 points
        let mid_score = State::calculate_score(full_duration, Duration::from_secs(22), full_points);
        assert!(mid_score > 700 && mid_score < 800); // Approximate due to rounding
    }

    #[test]
    fn test_slide_state_default() {
        let state: SlideState = SlideState::default();
        assert_eq!(state, SlideState::Unstarted);
    }

    #[test]
    fn test_validate_duration_functions() {
        // Test introduce_question validation
        let valid_introduce = Duration::from_secs(crate::constants::order::MIN_INTRODUCE_QUESTION);
        assert!(validate_introduce_question(&valid_introduce).is_ok());

        // MIN_INTRODUCE_QUESTION is 0, so we can't test too short. Test at minimum boundary.
        let invalid_introduce = Duration::from_secs(0);
        assert!(validate_introduce_question(&invalid_introduce).is_ok());

        // Test time_limit validation
        let valid_time_limit = Duration::from_secs(crate::constants::order::MIN_TIME_LIMIT);
        assert!(validate_time_limit(&valid_time_limit).is_ok());

        let invalid_time_limit = Duration::from_secs(crate::constants::order::MIN_TIME_LIMIT - 1);
        assert!(validate_time_limit(&invalid_time_limit).is_err());
    }

    #[test]
    fn test_answer_ordering_and_comparison() {
        let config = create_test_slide_config();
        let state = config.to_state();

        // Test correct order
        let correct_order = vec![
            "First".to_string(),
            "Second".to_string(),
            "Third".to_string(),
            "Fourth".to_string(),
        ];
        assert_eq!(correct_order, state.config.answers);

        // Test wrong order
        let wrong_order = vec![
            "Fourth".to_string(),
            "Third".to_string(),
            "Second".to_string(),
            "First".to_string(),
        ];
        assert_ne!(wrong_order, state.config.answers);
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
        assert_eq!(state.config.axis_labels.from, config.axis_labels.from);
        assert_eq!(state.config.axis_labels.to, config.axis_labels.to);
    }

    #[test]
    fn test_slide_config_serialization() {
        let config = create_test_slide_config();

        // Test that the config can be serialized and deserialized
        let serialized = serde_json::to_string(&config).unwrap();
        let deserialized: SlideConfig = serde_json::from_str(&serialized).unwrap();

        assert_eq!(config.title, deserialized.title);
        assert_eq!(config.answers, deserialized.answers);
    }

    #[test]
    fn test_message_serialization() {
        let update_msg = UpdateMessage::QuestionAnnouncement {
            index: 0,
            count: 5,
            question: "Test question".to_string(),
            media: None,
            duration: Duration::from_secs(10),
        };

        // Should serialize without errors
        let _serialized = serde_json::to_string(&update_msg).unwrap();

        let sync_msg = SyncMessage::AnswersResults {
            index: 0,
            count: 5,
            question: "Test question".to_string(),
            axis_labels: AxisLabels::default(),
            media: None,
            answers: vec!["A".to_string(), "B".to_string()],
            results: (5, 3),
        };

        // Should serialize without errors
        let _serialized = serde_json::to_string(&sync_msg).unwrap();
    }
}
