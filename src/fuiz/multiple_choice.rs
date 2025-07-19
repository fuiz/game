//! Multiple choice question implementation
//!
//! This module implements the multiple choice question type for Fuiz games.
//! Multiple choice questions present a question followed by several answer
//! options, allowing players to select one correct answer. The module handles
//! timing, scoring, answer validation, and result presentation.

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
    config::TextOrMedia,
    media::Media,
};

/// Represents the current phase of a multiple choice slide
///
/// Multiple choice questions progress through distinct phases:
/// first showing just the question, then revealing answer options,
/// and finally showing results with statistics.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum SlideState {
    /// Initial state before the slide has started (treated same as Question)
    #[default]
    Unstarted,
    /// Displaying the question without answer options
    Question,
    /// Displaying the question with answer options for player selection
    Answers,
    /// Displaying the results with correct answers and statistics
    AnswersResults,
}

type ValidationResult = garde::Result;

/// Validates that a duration falls within specified bounds
///
/// This helper function ensures that timing parameters for questions
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

/// Validates the duration for introducing a question before showing answers
fn validate_introduce_question(val: &Duration) -> ValidationResult {
    validate_duration::<
        { crate::constants::multiple_choice::MIN_INTRODUCE_QUESTION },
        { crate::constants::multiple_choice::MAX_INTRODUCE_QUESTION },
    >("introduce_question", val)
}

/// Validates the time limit for answering a multiple choice question
fn validate_time_limit(val: &Duration) -> ValidationResult {
    validate_duration::<
        { crate::constants::multiple_choice::MIN_TIME_LIMIT },
        { crate::constants::multiple_choice::MAX_TIME_LIMIT },
    >("time_limit", val)
}

/// Configuration for a multiple choice question slide
///
/// This struct defines all the parameters needed to create and present
/// a multiple choice question, including timing, content, scoring, and
/// the available answer options.
#[serde_with::serde_as]
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, serde::Deserialize, Validate)]
pub struct SlideConfig {
    /// The question text that will be displayed to players
    #[garde(length(min = crate::constants::multiple_choice::MIN_TITLE_LENGTH, max = crate::constants::multiple_choice::MAX_TITLE_LENGTH))]
    title: String,
    /// Optional media content (images, etc.) to accompany the question
    #[garde(dive)]
    media: Option<Media>,
    /// Duration to display the question before revealing answer options
    #[garde(custom(|v, _| validate_introduce_question(v)))]
    #[serde_as(as = "serde_with::DurationMilliSeconds<u64>")]
    introduce_question: Duration,
    /// Duration players have to select their answer once options are revealed
    #[garde(custom(|v, _| validate_time_limit(v)))]
    #[serde_as(as = "serde_with::DurationMilliSeconds<u64>")]
    time_limit: Duration,
    /// Maximum points awarded for a correct answer (decreases linearly over time)
    #[garde(skip)]
    points_awarded: u64,
    /// The available answer choices for this question
    #[garde(length(max = crate::constants::multiple_choice::MAX_ANSWER_COUNT))]
    answers: Vec<AnswerChoice>,
}

/// Runtime state for a multiple choice question during gameplay
///
/// This struct maintains the dynamic state of a multiple choice question
/// as it progresses through its phases, tracking player responses,
/// timing information, and current presentation state.
#[serde_with::serde_as]
#[skip_serializing_none]
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
#[serde_with::serde_as]
#[skip_serializing_none]
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
#[serde_with::serde_as]
#[skip_serializing_none]
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
        #[serde_as(as = "serde_with::DurationMilliSeconds<u64>")]
        duration: Duration,
    },
    /// Announces the answer options for player selection
    AnswersAnnouncement {
        /// Duration before the answering phase ends
        #[serde_as(as = "serde_with::DurationMilliSeconds<u64>")]
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
#[serde_with::serde_as]
#[skip_serializing_none]
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
        #[serde_as(as = "serde_with::DurationMilliSeconds<u64>")]
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
        #[serde_as(as = "serde_with::DurationMilliSeconds<u64>")]
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
        S: FnMut(crate::AlarmMessage, time::Duration),
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

    /// Calculates the score for a player based on how quickly they answered
    ///
    /// The scoring system awards full points for immediate answers and
    /// decreases linearly to half points at the end of the time limit.
    ///
    /// # Arguments
    ///
    /// * `full_duration` - Total time allowed for answering
    /// * `taken_duration` - Time taken by the player to submit their answer
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

    /// Records the current time as the start of the answer phase
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
        S: FnMut(crate::AlarmMessage, time::Duration),
    >(
        &mut self,
        team_manager: Option<&TeamManager<crate::names::NameStyle>>,
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
        S: FnMut(crate::AlarmMessage, time::Duration),
    >(
        &mut self,
        team_manager: Option<&TeamManager<crate::names::NameStyle>>,
        watchers: &Watchers,
        mut schedule_message: S,
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
    /// The current `SlideState` of this multiple choice question
    fn state(&self) -> SlideState {
        self.state
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
            let answer_count = self
                .user_answers
                .iter()
                .map(|(_, (answer, _))| *answer)
                .counts();
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

    /// Calculates and adds scores to the leaderboard based on player answers
    ///
    /// This method evaluates all player answers, calculates scores based on
    /// correctness and response time, and updates the leaderboard. In team mode,
    /// it uses the fastest correct answer from team members.
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
                .map(|(id, (answer, instant))| {
                    let correct = self.config.answers.get(*answer).is_some_and(|x| x.correct);
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
    ///
    /// # Panics
    ///
    /// Panics if the system clock goes backwards while calculating elapsed time
    /// if the state is not one of the expected states.
    ///
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
                duration: self.config.introduce_question
                    - self.timer().elapsed().expect("system clock went backwards"),
            },
            SlideState::Answers => SyncMessage::AnswersAnnouncement {
                index,
                count,
                question: self.config.title.clone(),
                media: self.config.media.clone(),
                duration: {
                    self.config.time_limit
                        - self.timer().elapsed().expect("system clock went backwards")
                },
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
                answered_count: {
                    let left_set: HashSet<_> = watchers
                        .specific_vec(ValueKind::Player, &tunnel_finder)
                        .iter()
                        .map(|(w, _, _)| w.to_owned())
                        .collect();
                    let right_set: HashSet<_> = self.user_answers.keys().copied().collect();
                    left_set.intersection(&right_set).count()
                },
            },
            SlideState::AnswersResults => {
                let answer_count = self
                    .user_answers
                    .iter()
                    .map(|(_, (answer, _))| answer)
                    .counts();

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

    /// Handles incoming messages from participants during the multiple choice question
    ///
    /// This method processes messages from hosts and players, including host commands
    /// to advance the slide and player answer submissions. It manages automatic
    /// progression when all players have answered.
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
        message: &IncomingMessage,
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
                    self.add_scores(leaderboard, watchers, team_manager, tunnel_finder);
                    return true;
                }
            },
            IncomingMessage::Player(IncomingPlayerMessage::IndexAnswer(v))
                if *v < self.config.answers.len() =>
            {
                self.user_answers
                    .insert(watcher_id, (*v, SystemTime::now()));
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
        team_manager: Option<&TeamManager<crate::names::NameStyle>>,
        schedule_message: &mut S,
        tunnel_finder: F,
        message: &crate::AlarmMessage,
        index: usize,
        _count: usize,
    ) -> bool {
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

        false
    }
}

#[cfg(test)]
#[cfg_attr(coverage_nightly, coverage(off))]
mod tests {
    use super::*;
    use crate::fuiz::config::{Fuiz, TextOrMedia};
    use garde::Validate;
    use std::time::Duration;

    fn create_test_slide_config() -> SlideConfig {
        SlideConfig {
            title: "Test Question".to_string(),
            media: None,
            introduce_question: Duration::from_secs(5),
            time_limit: Duration::from_secs(30),
            points_awarded: 1000,
            answers: vec![
                AnswerChoice {
                    correct: true,
                    content: TextOrMedia::Text("Correct Answer".to_string()),
                },
                AnswerChoice {
                    correct: false,
                    content: TextOrMedia::Text("Wrong Answer 1".to_string()),
                },
                AnswerChoice {
                    correct: false,
                    content: TextOrMedia::Text("Wrong Answer 2".to_string()),
                },
            ],
        }
    }

    fn create_test_fuiz() -> Fuiz {
        Fuiz {
            title: "Test Quiz".to_string(),
            slides: vec![crate::fuiz::config::SlideConfig::MultipleChoice(
                create_test_slide_config(),
            )],
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
        // MIN_TITLE_LENGTH is 0, so empty string is actually valid
        config.title = String::new();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn test_slide_config_title_too_long() {
        let mut config = create_test_slide_config();
        config.title = "a".repeat(crate::constants::multiple_choice::MAX_TITLE_LENGTH + 1);
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
            Duration::from_secs(crate::constants::multiple_choice::MAX_INTRODUCE_QUESTION + 1);
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_slide_config_time_limit_too_short() {
        let mut config = create_test_slide_config();
        config.time_limit =
            Duration::from_secs(crate::constants::multiple_choice::MIN_TIME_LIMIT - 1);
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_slide_config_time_limit_too_long() {
        let mut config = create_test_slide_config();
        config.time_limit =
            Duration::from_secs(crate::constants::multiple_choice::MAX_TIME_LIMIT + 1);
        assert!(config.validate().is_err());
    }

    #[test]
    fn test_slide_config_too_many_answers() {
        let mut config = create_test_slide_config();
        config.answers = vec![
            AnswerChoice {
                correct: false,
                content: TextOrMedia::Text("Answer".to_string()),
            };
            crate::constants::multiple_choice::MAX_ANSWER_COUNT + 1
        ];
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
        fuiz.slides =
            vec![
                crate::fuiz::config::SlideConfig::MultipleChoice(create_test_slide_config());
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
    fn test_text_or_media_validation() {
        // Valid text
        let valid_text = TextOrMedia::Text("Valid answer".to_string());
        assert!(valid_text.validate().is_ok());

        // Text too long
        let long_text =
            TextOrMedia::Text("a".repeat(crate::constants::answer_text::MAX_LENGTH + 1));
        assert!(long_text.validate().is_err());

        // Media should validate without errors (garde skip)
        let media = TextOrMedia::Media(crate::fuiz::media::Media::Image(
            crate::fuiz::media::Image::Corkboard {
                id: "a".repeat(crate::constants::corkboard::ID_LENGTH),
                alt: "Test image".to_string(),
            },
        ));
        assert!(media.validate().is_ok());
    }

    #[test]
    fn test_slide_state_default() {
        let state: SlideState = SlideState::default();
        assert_eq!(state, SlideState::Unstarted);
    }

    #[test]
    fn test_possibly_hidden_serialization() {
        let visible = PossiblyHidden::Visible("test".to_string());
        let hidden: PossiblyHidden<String> = PossiblyHidden::Hidden;

        // These should serialize without errors
        let _visible_json = serde_json::to_string(&visible).unwrap();
        let _hidden_json = serde_json::to_string(&hidden).unwrap();
    }

    #[test]
    fn test_validate_duration_functions() {
        // Test introduce_question validation
        let valid_introduce =
            Duration::from_secs(crate::constants::multiple_choice::MIN_INTRODUCE_QUESTION);
        assert!(validate_introduce_question(&valid_introduce).is_ok());

        // MIN_INTRODUCE_QUESTION is 0, so we can't test too short. Test at minimum boundary.
        let invalid_introduce = Duration::from_secs(0);
        assert!(validate_introduce_question(&invalid_introduce).is_ok());

        // Test time_limit validation
        let valid_time_limit =
            Duration::from_secs(crate::constants::multiple_choice::MIN_TIME_LIMIT);
        assert!(validate_time_limit(&valid_time_limit).is_ok());

        let invalid_time_limit =
            Duration::from_secs(crate::constants::multiple_choice::MIN_TIME_LIMIT - 1);
        assert!(validate_time_limit(&invalid_time_limit).is_err());
    }

    #[test]
    fn test_answer_choice_creation() {
        let answer = AnswerChoice {
            correct: true,
            content: TextOrMedia::Text("Test answer".to_string()),
        };

        assert!(answer.correct);
        assert!(matches!(answer.content, TextOrMedia::Text(text) if text == "Test answer"));
    }

    fn create_mock_watchers() -> Watchers {
        let host_id = Id::new();
        let mut watchers = Watchers::with_host_id(host_id);

        let player_id = Id::new();
        watchers
            .add_watcher(
                player_id,
                crate::watcher::Value::Player(crate::watcher::PlayerValue::Individual {
                    name: "TestPlayer".to_string(),
                }),
            )
            .unwrap();

        watchers
    }

    #[derive(Debug, Clone)]
    struct MockTunnel {
        messages:
            std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<crate::UpdateMessage>>>,
        states: std::sync::Arc<std::sync::Mutex<std::collections::VecDeque<crate::SyncMessage>>>,
        closed: std::sync::Arc<std::sync::Mutex<bool>>,
    }

    impl MockTunnel {
        fn new() -> Self {
            Self {
                messages: std::sync::Arc::new(std::sync::Mutex::new(
                    std::collections::VecDeque::new(),
                )),
                states: std::sync::Arc::new(std::sync::Mutex::new(
                    std::collections::VecDeque::new(),
                )),
                closed: std::sync::Arc::new(std::sync::Mutex::new(false)),
            }
        }
    }

    impl crate::session::Tunnel for MockTunnel {
        fn send_message(&self, message: &crate::UpdateMessage) {
            self.messages.lock().unwrap().push_back(message.clone());
        }

        fn send_state(&self, message: &crate::SyncMessage) {
            self.states.lock().unwrap().push_back(message.clone());
        }

        fn close(self) {
            *self.closed.lock().unwrap() = true;
        }
    }

    fn create_mock_tunnel_finder() -> impl Fn(Id) -> Option<MockTunnel> {
        move |_id| Some(MockTunnel::new())
    }

    fn create_mock_leaderboard() -> Leaderboard {
        Leaderboard::default()
    }

    #[test]
    fn test_play_method() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        let watchers = create_mock_watchers();
        let tunnel_finder = create_mock_tunnel_finder();
        let mut schedule_called = false;
        let mut schedule_message = |_msg: crate::AlarmMessage, _duration: std::time::Duration| {
            schedule_called = true;
        };

        state.play(None, &watchers, &mut schedule_message, tunnel_finder, 0, 1);

        assert_eq!(state.state(), SlideState::Question);
        assert!(schedule_called);
    }

    #[test]
    fn test_play_method_with_zero_introduce_time() {
        let mut config = create_test_slide_config();
        config.introduce_question = Duration::from_secs(0);
        let mut state = config.to_state();
        let watchers = create_mock_watchers();
        let tunnel_finder = create_mock_tunnel_finder();
        let mut schedule_called = false;
        let mut schedule_message = |_msg: crate::AlarmMessage, _duration: std::time::Duration| {
            schedule_called = true;
        };

        state.play(None, &watchers, &mut schedule_message, tunnel_finder, 0, 1);

        assert_eq!(state.state(), SlideState::Answers);
        assert!(schedule_called);
    }

    #[test]
    fn test_play_method_schedules_alarm_for_non_zero_introduce_time() {
        let mut config = create_test_slide_config();
        config.introduce_question = Duration::from_secs(10);
        let mut state = config.to_state();
        let watchers = create_mock_watchers();
        let tunnel_finder = create_mock_tunnel_finder();
        let mut scheduled_alarm: Option<crate::AlarmMessage> = None;
        let mut scheduled_duration: Option<std::time::Duration> = None;
        let mut schedule_message = |msg: crate::AlarmMessage, duration: std::time::Duration| {
            scheduled_alarm = Some(msg);
            scheduled_duration = Some(duration);
        };

        // Verify initial state
        assert_eq!(state.state(), SlideState::Unstarted);
        assert!(!config.introduce_question.is_zero());

        state.play(None, &watchers, &mut schedule_message, tunnel_finder, 0, 1);

        // This should hit the else branch at line 468
        assert_eq!(state.state(), SlideState::Question);
        assert!(scheduled_alarm.is_some());
        assert_eq!(scheduled_duration, Some(Duration::from_secs(10)));

        if let Some(crate::AlarmMessage::MultipleChoice(
            AlarmMessage::ProceedFromSlideIntoSlide { index, to },
        )) = scheduled_alarm
        {
            assert_eq!(index, 0);
            assert_eq!(to, SlideState::Answers);
        } else {
            panic!("Expected MultipleChoice alarm message");
        }
    }

    #[test]
    fn test_send_question_announcements_else_branch_explicitly() {
        let mut config = create_test_slide_config();
        config.introduce_question = Duration::from_secs(5);
        let mut state = config.to_state();
        let watchers = create_mock_watchers();
        let tunnel_finder = create_mock_tunnel_finder();
        let mut else_branch_executed = false;
        let mut schedule_message = |_msg: crate::AlarmMessage, _duration: std::time::Duration| {
            else_branch_executed = true;
        };

        // Verify preconditions for hitting the else branch
        assert_eq!(state.state(), SlideState::Unstarted);
        assert!(!config.introduce_question.is_zero());

        // This should execute the else branch (line 458-467, closing at line 468)
        state.send_question_announcements(
            None,
            &watchers,
            &mut schedule_message,
            tunnel_finder,
            0,
            1,
        );

        assert_eq!(state.state(), SlideState::Question);
        assert!(
            else_branch_executed,
            "The else branch at line 468 should have been executed"
        );
    }

    #[test]
    fn test_receive_alarm_method() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        let watchers = create_mock_watchers();
        let tunnel_finder = create_mock_tunnel_finder();
        let mut leaderboard = create_mock_leaderboard();
        let mut schedule_message = |_msg: crate::AlarmMessage, _duration: web_time::Duration| {};

        state.state = SlideState::Question;

        let alarm_message =
            crate::AlarmMessage::MultipleChoice(AlarmMessage::ProceedFromSlideIntoSlide {
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
    fn test_receive_alarm_method_wrong_message_type() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        let watchers = create_mock_watchers();
        let tunnel_finder = create_mock_tunnel_finder();
        let mut leaderboard = create_mock_leaderboard();
        let mut schedule_message = |_msg: crate::AlarmMessage, _duration: web_time::Duration| {};

        state.state = SlideState::Question;

        let alarm_message = crate::AlarmMessage::TypeAnswer(
            crate::fuiz::type_answer::AlarmMessage::ProceedFromSlideIntoSlide {
                index: 0,
                to: crate::fuiz::type_answer::SlideState::Answers,
            },
        );

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
        assert_eq!(state.state(), SlideState::Question);
    }

    #[test]
    fn test_receive_alarm_answers_to_results() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        let watchers = create_mock_watchers();
        let tunnel_finder = create_mock_tunnel_finder();
        let mut leaderboard = create_mock_leaderboard();
        let mut schedule_message = |_msg: crate::AlarmMessage, _duration: web_time::Duration| {};

        state.state = SlideState::Answers;

        let alarm_message =
            crate::AlarmMessage::MultipleChoice(AlarmMessage::ProceedFromSlideIntoSlide {
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
    fn test_receive_alarm_catch_all_case() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        let watchers = create_mock_watchers();
        let tunnel_finder = create_mock_tunnel_finder();
        let mut leaderboard = create_mock_leaderboard();
        let mut schedule_message = |_msg: crate::AlarmMessage, _duration: web_time::Duration| {};

        let initial_state = SlideState::Question;
        state.state = initial_state;

        // Test with SlideState::Unstarted (should hit the catch-all case at line 1064)
        let alarm_message = crate::AlarmMessage::MultipleChoice(
            AlarmMessage::ProceedFromSlideIntoSlide {
                index: 0,
                to: SlideState::Unstarted,
            }
        );

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
        // State should remain unchanged since the catch-all case does nothing
        assert_eq!(state.state(), initial_state);
    }

    #[test]
    fn test_receive_alarm_catch_all_case_with_question() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        let watchers = create_mock_watchers();
        let tunnel_finder = create_mock_tunnel_finder();
        let mut leaderboard = create_mock_leaderboard();
        let mut schedule_message = |_msg: crate::AlarmMessage, _duration: web_time::Duration| {};

        let initial_state = SlideState::Answers;
        state.state = initial_state;

        // Test with SlideState::Question (should hit the catch-all case at line 1064)
        let alarm_message = crate::AlarmMessage::MultipleChoice(
            AlarmMessage::ProceedFromSlideIntoSlide {
                index: 0,
                to: SlideState::Question,
            }
        );

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
        // State should remain unchanged since the catch-all case does nothing
        assert_eq!(state.state(), initial_state);
    }

    #[test]
    fn test_receive_message_host_next_from_unstarted() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        let watchers = create_mock_watchers();
        let tunnel_finder = create_mock_tunnel_finder();
        let mut leaderboard = create_mock_leaderboard();
        let mut schedule_message = |_msg: crate::AlarmMessage, _duration: std::time::Duration| {};

        let message = IncomingMessage::Host(IncomingHostMessage::Next);

        let result = state.receive_message(
            Id::new(),
            &message,
            &mut leaderboard,
            &watchers,
            None,
            &mut schedule_message,
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
        let watchers = create_mock_watchers();
        let tunnel_finder = create_mock_tunnel_finder();
        let mut leaderboard = create_mock_leaderboard();
        let mut schedule_message = |_msg: crate::AlarmMessage, _duration: std::time::Duration| {};

        state.state = SlideState::Question;

        let message = IncomingMessage::Host(IncomingHostMessage::Next);

        let result = state.receive_message(
            Id::new(),
            &message,
            &mut leaderboard,
            &watchers,
            None,
            &mut schedule_message,
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
        let watchers = create_mock_watchers();
        let tunnel_finder = create_mock_tunnel_finder();
        let mut leaderboard = create_mock_leaderboard();
        let mut schedule_message = |_msg: crate::AlarmMessage, _duration: std::time::Duration| {};

        state.state = SlideState::Answers;

        let message = IncomingMessage::Host(IncomingHostMessage::Next);

        let result = state.receive_message(
            Id::new(),
            &message,
            &mut leaderboard,
            &watchers,
            None,
            &mut schedule_message,
            tunnel_finder,
            0,
            1,
        );

        assert!(!result);
        assert_eq!(state.state(), SlideState::AnswersResults);
    }

    #[test]
    fn test_receive_message_host_next_from_results() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        let watchers = create_mock_watchers();
        let tunnel_finder = create_mock_tunnel_finder();
        let mut leaderboard = create_mock_leaderboard();
        let mut schedule_message = |_msg: crate::AlarmMessage, _duration: std::time::Duration| {};

        state.state = SlideState::AnswersResults;

        let message = IncomingMessage::Host(IncomingHostMessage::Next);

        let result = state.receive_message(
            Id::new(),
            &message,
            &mut leaderboard,
            &watchers,
            None,
            &mut schedule_message,
            tunnel_finder,
            0,
            1,
        );

        assert!(result);
        assert_eq!(state.state(), SlideState::AnswersResults);
    }

    #[test]
    fn test_receive_message_player_answer_valid() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        let watchers = create_mock_watchers();
        let tunnel_finder = create_mock_tunnel_finder();
        let mut leaderboard = create_mock_leaderboard();
        let mut schedule_message = |_msg: crate::AlarmMessage, _duration: std::time::Duration| {};

        state.state = SlideState::Answers;
        state.start_timer();

        let player_id = Id::new();
        let message = IncomingMessage::Player(IncomingPlayerMessage::IndexAnswer(0));

        let result = state.receive_message(
            player_id,
            &message,
            &mut leaderboard,
            &watchers,
            None,
            &mut schedule_message,
            tunnel_finder,
            0,
            1,
        );

        assert!(!result);
        assert!(state.user_answers.contains_key(&player_id));
        assert_eq!(state.user_answers[&player_id].0, 0);
    }

    #[test]
    fn test_receive_message_player_answer_invalid_index() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        let watchers = create_mock_watchers();
        let tunnel_finder = create_mock_tunnel_finder();
        let mut leaderboard = create_mock_leaderboard();
        let mut schedule_message = |_msg: crate::AlarmMessage, _duration: std::time::Duration| {};

        state.state = SlideState::Answers;

        let player_id = Id::new();
        let message = IncomingMessage::Player(IncomingPlayerMessage::IndexAnswer(10));

        let result = state.receive_message(
            player_id,
            &message,
            &mut leaderboard,
            &watchers,
            None,
            &mut schedule_message,
            tunnel_finder,
            0,
            1,
        );

        assert!(!result);
        assert!(!state.user_answers.contains_key(&player_id));
    }

    #[test]
    fn test_receive_message_ignore_non_matching_messages() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        let watchers = create_mock_watchers();
        let tunnel_finder = create_mock_tunnel_finder();
        let mut leaderboard = create_mock_leaderboard();
        let mut schedule_message = |_msg: crate::AlarmMessage, _duration: std::time::Duration| {};

        let message =
            IncomingMessage::Player(IncomingPlayerMessage::StringAnswer("text".to_string()));

        let result = state.receive_message(
            Id::new(),
            &message,
            &mut leaderboard,
            &watchers,
            None,
            &mut schedule_message,
            tunnel_finder,
            0,
            1,
        );

        assert!(!result);
        assert_eq!(state.state(), SlideState::Unstarted);
    }

    #[test]
    fn test_receive_message_all_players_answered_triggers_results() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        let mut watchers = create_mock_watchers();
        let mut leaderboard = create_mock_leaderboard();
        let mut schedule_message = |_msg: crate::AlarmMessage, _duration: std::time::Duration| {};

        // Create multiple players
        let player1_id = Id::new();
        let player2_id = Id::new();
        let player3_id = Id::new();

        // Add players to watchers
        watchers
            .add_watcher(
                player1_id,
                crate::watcher::Value::Player(crate::watcher::PlayerValue::Individual {
                    name: "Player1".to_string(),
                }),
            )
            .unwrap();

        watchers
            .add_watcher(
                player2_id,
                crate::watcher::Value::Player(crate::watcher::PlayerValue::Individual {
                    name: "Player2".to_string(),
                }),
            )
            .unwrap();

        watchers
            .add_watcher(
                player3_id,
                crate::watcher::Value::Player(crate::watcher::PlayerValue::Individual {
                    name: "Player3".to_string(),
                }),
            )
            .unwrap();

        // Create tunnel finder that returns tunnels for all players
        let tunnel_finder = move |id: Id| {
            if id == player1_id || id == player2_id || id == player3_id {
                Some(MockTunnel::new())
            } else {
                None
            }
        };

        state.state = SlideState::Answers;
        state.start_timer();

        // First two players answer - should NOT trigger results yet
        let message1 = IncomingMessage::Player(IncomingPlayerMessage::IndexAnswer(0));
        let result1 = state.receive_message(
            player1_id,
            &message1,
            &mut leaderboard,
            &watchers,
            None,
            &mut schedule_message,
            tunnel_finder,
            0,
            1,
        );
        assert!(!result1);
        assert_eq!(state.state(), SlideState::Answers);

        let message2 = IncomingMessage::Player(IncomingPlayerMessage::IndexAnswer(1));
        let result2 = state.receive_message(
            player2_id,
            &message2,
            &mut leaderboard,
            &watchers,
            None,
            &mut schedule_message,
            tunnel_finder,
            0,
            1,
        );
        assert!(!result2);
        assert_eq!(state.state(), SlideState::Answers);

        // Third player answers - should trigger results (line 991)
        let message3 = IncomingMessage::Player(IncomingPlayerMessage::IndexAnswer(2));
        let result3 = state.receive_message(
            player3_id,
            &message3,
            &mut leaderboard,
            &watchers,
            None,
            &mut schedule_message,
            tunnel_finder,
            0,
            1,
        );
        assert!(!result3);
        // This should have triggered send_answers_results and changed state to AnswersResults
        assert_eq!(state.state(), SlideState::AnswersResults);
    }

    #[test]
    fn test_state_message_unstarted() {
        let config = create_test_slide_config();
        let state = config.to_state();
        let watchers = create_mock_watchers();
        let tunnel_finder = create_mock_tunnel_finder();

        let message = state.state_message(
            Id::new(),
            ValueKind::Player,
            None,
            &watchers,
            tunnel_finder,
            0,
            1,
        );

        match message {
            SyncMessage::QuestionAnnouncement {
                index,
                count,
                question,
                ..
            } => {
                assert_eq!(index, 0);
                assert_eq!(count, 1);
                assert_eq!(question, "Test Question");
            }
            _ => panic!("Expected QuestionAnnouncement"),
        }
    }

    #[test]
    fn test_state_message_question() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        let watchers = create_mock_watchers();
        let tunnel_finder = create_mock_tunnel_finder();

        state.state = SlideState::Question;

        let message = state.state_message(
            Id::new(),
            ValueKind::Player,
            None,
            &watchers,
            tunnel_finder,
            0,
            1,
        );

        match message {
            SyncMessage::QuestionAnnouncement {
                index,
                count,
                question,
                ..
            } => {
                assert_eq!(index, 0);
                assert_eq!(count, 1);
                assert_eq!(question, "Test Question");
            }
            _ => panic!("Expected QuestionAnnouncement"),
        }
    }

    #[test]
    fn test_state_message_answers() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        let watchers = create_mock_watchers();
        let tunnel_finder = create_mock_tunnel_finder();

        state.state = SlideState::Answers;
        state.start_timer();

        let message = state.state_message(
            Id::new(),
            ValueKind::Player,
            None,
            &watchers,
            tunnel_finder,
            0,
            1,
        );

        match message {
            SyncMessage::AnswersAnnouncement {
                index,
                count,
                question,
                answers,
                ..
            } => {
                assert_eq!(index, 0);
                assert_eq!(count, 1);
                assert_eq!(question, "Test Question");
                assert_eq!(answers.len(), 3);
            }
            _ => panic!("Expected AnswersAnnouncement"),
        }
    }

    #[test]
    fn test_state_message_answers_results() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        let watchers = create_mock_watchers();
        let tunnel_finder = create_mock_tunnel_finder();

        state.state = SlideState::AnswersResults;

        let message = state.state_message(
            Id::new(),
            ValueKind::Player,
            None,
            &watchers,
            tunnel_finder,
            0,
            1,
        );

        match message {
            SyncMessage::AnswersResults {
                index,
                count,
                question,
                answers,
                results,
                ..
            } => {
                assert_eq!(index, 0);
                assert_eq!(count, 1);
                assert_eq!(question, "Test Question");
                assert_eq!(answers.len(), 3);
                assert_eq!(results.len(), 3);
            }
            _ => panic!("Expected AnswersResults"),
        }
    }

    #[test]
    fn test_state_message_answers_with_team_manager() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        let mut watchers = create_mock_watchers();
        let mut names = crate::names::Names::default();

        // Create team manager and set up teams
        let mut team_manager = create_mock_team_manager();

        let player1_id = Id::new();
        let player2_id = Id::new();
        let player3_id = Id::new();

        // Add players to watchers
        watchers
            .add_watcher(
                player1_id,
                crate::watcher::Value::Player(crate::watcher::PlayerValue::Individual {
                    name: "Player1".to_string(),
                }),
            )
            .unwrap();

        watchers
            .add_watcher(
                player2_id,
                crate::watcher::Value::Player(crate::watcher::PlayerValue::Individual {
                    name: "Player2".to_string(),
                }),
            )
            .unwrap();

        watchers
            .add_watcher(
                player3_id,
                crate::watcher::Value::Player(crate::watcher::PlayerValue::Individual {
                    name: "Player3".to_string(),
                }),
            )
            .unwrap();

        // Set up names
        names.set_name(player1_id, "Player1").unwrap();
        names.set_name(player2_id, "Player2").unwrap();
        names.set_name(player3_id, "Player3").unwrap();

        // Create tunnel finder where only player1 and player2 are alive
        let tunnel_finder = move |id: Id| {
            if id == player1_id || id == player2_id {
                Some(MockTunnel::new()) // These players are "alive"
            } else {
                None // player3 is "dead"
            }
        };

        // Finalize teams and add players
        team_manager.finalize(&mut watchers, &mut names, &tunnel_finder);
        team_manager.add_player(player1_id, &mut watchers);
        team_manager.add_player(player2_id, &mut watchers);
        team_manager.add_player(player3_id, &mut watchers);

        state.state = SlideState::Answers;
        state.start_timer();

        // This should test lines 843-851: the team member filtering in state_message
        let message = state.state_message(
            player1_id,
            ValueKind::Player,
            Some(&team_manager),
            &watchers,
            tunnel_finder,
            0,
            1,
        );

        match message {
            SyncMessage::AnswersAnnouncement {
                index,
                count,
                question,
                answers,
                ..
            } => {
                assert_eq!(index, 0);
                assert_eq!(count, 1);
                assert_eq!(question, "Test Question");
                assert_eq!(answers.len(), 3);
            }
            _ => panic!("Expected AnswersAnnouncement"),
        }
    }

    #[test]
    fn test_state_message_answers_with_team_manager_no_team_members() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        let watchers = create_mock_watchers();
        let tunnel_finder = create_mock_tunnel_finder();

        // Create team manager but don't assign any players to teams
        let team_manager = create_mock_team_manager();

        state.state = SlideState::Answers;
        state.start_timer();

        // This should test the map_or(1, |members| ...) fallback in lines 843-851
        // When team_members returns None, it should fall back to 1
        let message = state.state_message(
            Id::new(),
            ValueKind::Player,
            Some(&team_manager),
            &watchers,
            tunnel_finder,
            0,
            1,
        );

        match message {
            SyncMessage::AnswersAnnouncement {
                index,
                count,
                question,
                answers,
                ..
            } => {
                assert_eq!(index, 0);
                assert_eq!(count, 1);
                assert_eq!(question, "Test Question");
                assert_eq!(answers.len(), 3);
            }
            _ => panic!("Expected AnswersAnnouncement"),
        }
    }

    #[test]
    fn test_get_answers_for_player_host_individual() {
        let config = create_test_slide_config();
        let state = config.to_state();

        let answers = state.get_answers_for_player(Id::new(), ValueKind::Host, 1, 0, false);

        assert_eq!(answers.len(), 3);
        for answer in answers {
            assert!(matches!(answer, PossiblyHidden::Visible(_)));
        }
    }

    #[test]
    fn test_get_answers_for_player_host_team() {
        let config = create_test_slide_config();
        let state = config.to_state();

        let answers = state.get_answers_for_player(Id::new(), ValueKind::Host, 2, 0, true);

        assert_eq!(answers.len(), 3);
        for answer in answers {
            assert!(matches!(answer, PossiblyHidden::Hidden));
        }
    }

    #[test]
    fn test_get_answers_for_player_team_distribution() {
        let config = create_test_slide_config();
        let state = config.to_state();

        let answers_player0 =
            state.get_answers_for_player(Id::new(), ValueKind::Player, 2, 0, true);

        let answers_player1 =
            state.get_answers_for_player(Id::new(), ValueKind::Player, 2, 1, true);

        assert_eq!(answers_player0.len(), 3);
        assert_eq!(answers_player1.len(), 3);

        let visible_count_p0 = answers_player0
            .iter()
            .filter(|a| matches!(a, PossiblyHidden::Visible(_)))
            .count();
        let visible_count_p1 = answers_player1
            .iter()
            .filter(|a| matches!(a, PossiblyHidden::Visible(_)))
            .count();

        assert!(visible_count_p0 > 0);
        assert!(visible_count_p1 > 0);
    }

    #[test]
    fn test_timer_functionality() {
        let config = create_test_slide_config();
        let mut state = config.to_state();

        let timer_before = state.timer();
        state.start_timer();
        let timer_after = state.timer();

        assert!(timer_after >= timer_before);
        assert!(state.answer_start.is_some());
    }

    #[test]
    fn test_add_scores() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        let watchers = create_mock_watchers();
        let tunnel_finder = create_mock_tunnel_finder();
        let mut leaderboard = create_mock_leaderboard();

        state.state = SlideState::Answers;
        state.start_timer();

        let player_id = Id::new();
        state.user_answers.insert(player_id, (0, SystemTime::now()));

        state.add_scores(&mut leaderboard, &watchers, None, tunnel_finder);

        let score = leaderboard.score(player_id);
        assert!(score.is_some());
    }

    #[test]
    fn test_add_scores_with_team_manager() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        let mut watchers = create_mock_watchers();
        let mut names = crate::names::Names::default();
        let tunnel_finder = create_mock_tunnel_finder();
        let mut leaderboard = create_mock_leaderboard();

        // Create team manager and set up teams
        let mut team_manager = create_mock_team_manager();

        let player1_id = Id::new();
        let player2_id = Id::new();

        watchers
            .add_watcher(
                player1_id,
                crate::watcher::Value::Player(crate::watcher::PlayerValue::Individual {
                    name: "Player1".to_string(),
                }),
            )
            .unwrap();

        watchers
            .add_watcher(
                player2_id,
                crate::watcher::Value::Player(crate::watcher::PlayerValue::Individual {
                    name: "Player2".to_string(),
                }),
            )
            .unwrap();

        names.set_name(player1_id, "Player1").unwrap();
        names.set_name(player2_id, "Player2").unwrap();

        // Finalize teams and add players
        team_manager.finalize(&mut watchers, &mut names, &tunnel_finder);
        team_manager.add_player(player1_id, &mut watchers);
        team_manager.add_player(player2_id, &mut watchers);

        state.state = SlideState::Answers;
        state.start_timer();

        // Add answers for both players (using timer to avoid timing issues)
        let answer_time = state.timer();
        state.user_answers.insert(player1_id, (0, answer_time));
        state.user_answers.insert(player2_id, (1, answer_time));

        // This should test line 694: team_manager.get_team(player_id).unwrap_or(player_id)
        state.add_scores(
            &mut leaderboard,
            &watchers,
            Some(&team_manager),
            tunnel_finder,
        );

        // When players are in teams, scores are tracked by team ID
        // Get the team IDs for both players
        let team1_id = team_manager.get_team(player1_id).unwrap_or(player1_id);
        let team2_id = team_manager.get_team(player2_id).unwrap_or(player2_id);

        // Check that scores were added for the team(s)
        let score1 = leaderboard.score(team1_id);
        let score2 = leaderboard.score(team2_id);

        // At least one of the teams should have a score
        assert!(score1.is_some() || score2.is_some());
    }

    #[test]
    fn test_add_scores_with_team_manager_no_team_assignment() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        let watchers = create_mock_watchers();
        let tunnel_finder = create_mock_tunnel_finder();
        let mut leaderboard = create_mock_leaderboard();

        // Create team manager but don't assign any players to teams
        let team_manager = create_mock_team_manager();

        let player_id = Id::new();

        state.state = SlideState::Answers;
        state.start_timer();

        // Add answer using timer to avoid timing issues
        let answer_time = state.timer();
        state.user_answers.insert(player_id, (0, answer_time));

        // This should test the unwrap_or(player_id) part of line 694
        // When get_team returns None, it should fall back to player_id
        state.add_scores(
            &mut leaderboard,
            &watchers,
            Some(&team_manager),
            tunnel_finder,
        );

        let score = leaderboard.score(player_id);
        assert!(score.is_some());
    }

    fn create_mock_team_manager() -> TeamManager<crate::names::NameStyle> {
        TeamManager::new(3, false, crate::names::NameStyle::default())
    }

    #[test]
    fn test_send_answers_announcements_with_team_manager() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        let watchers = create_mock_watchers();
        let tunnel_finder = create_mock_tunnel_finder();
        let team_manager = create_mock_team_manager();
        let mut schedule_called = false;
        let mut schedule_message = |_msg: crate::AlarmMessage, _duration: std::time::Duration| {
            schedule_called = true;
        };

        state.state = SlideState::Question;

        state.send_answers_announcements(
            Some(&team_manager),
            &watchers,
            &mut schedule_message,
            tunnel_finder,
            0,
        );

        assert_eq!(state.state(), SlideState::Answers);
        assert!(schedule_called);
        assert!(state.answer_start.is_some());
    }

    #[test]
    fn test_send_answers_announcements_team_members_filtering() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        let mut watchers = create_mock_watchers();
        let mut names = crate::names::Names::default();
        let mut schedule_message = |_msg: crate::AlarmMessage, _duration: std::time::Duration| {};

        // Create a team manager and add multiple players
        let mut team_manager = create_mock_team_manager();

        let player1_id = Id::new();
        let player2_id = Id::new();
        let player3_id = Id::new();

        watchers
            .add_watcher(
                player1_id,
                crate::watcher::Value::Player(crate::watcher::PlayerValue::Individual {
                    name: "Player1".to_string(),
                }),
            )
            .unwrap();

        watchers
            .add_watcher(
                player2_id,
                crate::watcher::Value::Player(crate::watcher::PlayerValue::Individual {
                    name: "Player2".to_string(),
                }),
            )
            .unwrap();

        watchers
            .add_watcher(
                player3_id,
                crate::watcher::Value::Player(crate::watcher::PlayerValue::Individual {
                    name: "Player3".to_string(),
                }),
            )
            .unwrap();

        // Create tunnel finder that simulates some players being alive and some not
        let tunnel_finder = move |id: Id| {
            if id == player1_id || id == player2_id {
                Some(MockTunnel::new()) // These players are "alive"
            } else {
                None // player3 is "dead" (no tunnel)
            }
        };

        // Set up names for the players
        names.set_name(player1_id, "Player1").unwrap();
        names.set_name(player2_id, "Player2").unwrap();
        names.set_name(player3_id, "Player3").unwrap();

        // Finalize teams (this creates actual teams and assigns players)
        team_manager.finalize(&mut watchers, &mut names, &tunnel_finder);

        // Now add the players to teams
        team_manager.add_player(player1_id, &mut watchers);
        team_manager.add_player(player2_id, &mut watchers);
        team_manager.add_player(player3_id, &mut watchers);

        state.state = SlideState::Question;

        // This should now actually hit the team member filtering logic (lines 516-525)
        state.send_answers_announcements(
            Some(&team_manager),
            &watchers,
            &mut schedule_message,
            tunnel_finder,
            0,
        );

        assert_eq!(state.state(), SlideState::Answers);
    }

    #[test]
    fn test_send_answers_announcements_no_team_members() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        let watchers = create_mock_watchers();
        let tunnel_finder = create_mock_tunnel_finder();
        let team_manager = create_mock_team_manager();
        let mut schedule_message = |_msg: crate::AlarmMessage, _duration: std::time::Duration| {};

        state.state = SlideState::Question;

        // This should handle the case where team_members returns None (no team found)
        state.send_answers_announcements(
            Some(&team_manager),
            &watchers,
            &mut schedule_message,
            tunnel_finder,
            0,
        );

        assert_eq!(state.state(), SlideState::Answers);
    }

    #[test]
    fn test_send_answers_announcements_no_team_manager() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        let watchers = create_mock_watchers();
        let tunnel_finder = create_mock_tunnel_finder();
        let mut schedule_message = |_msg: crate::AlarmMessage, _duration: std::time::Duration| {};

        state.state = SlideState::Question;

        // This should handle the None case for team_manager (lines 527-528)
        state.send_answers_announcements(None, &watchers, &mut schedule_message, tunnel_finder, 0);

        assert_eq!(state.state(), SlideState::Answers);
    }

    #[test]
    fn test_team_member_filtering_with_dead_members() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        let mut watchers = create_mock_watchers();
        let mut names = crate::names::Names::default();
        let mut schedule_message = |_msg: crate::AlarmMessage, _duration: std::time::Duration| {};

        // Create a team manager and add multiple players
        let mut team_manager = create_mock_team_manager();

        let player1_id = Id::new();
        let player2_id = Id::new();
        let player3_id = Id::new();
        let player4_id = Id::new();

        // Add all players to watchers
        for (i, &id) in [player1_id, player2_id, player3_id, player4_id]
            .iter()
            .enumerate()
        {
            watchers
                .add_watcher(
                    id,
                    crate::watcher::Value::Player(crate::watcher::PlayerValue::Individual {
                        name: format!("Player{}", i + 1),
                    }),
                )
                .unwrap();
            names.set_name(id, &format!("Player{}", i + 1)).unwrap();
        }

        // Create tunnel finder where only player1 and player2 are "alive"
        let tunnel_finder = move |id: Id| {
            if id == player1_id || id == player2_id {
                Some(MockTunnel::new()) // These players are "alive"
            } else {
                None // player3 and player4 are "dead"
            }
        };

        // Finalize teams and add players
        team_manager.finalize(&mut watchers, &mut names, &tunnel_finder);
        team_manager.add_player(player1_id, &mut watchers);
        team_manager.add_player(player2_id, &mut watchers);
        team_manager.add_player(player3_id, &mut watchers);
        team_manager.add_player(player4_id, &mut watchers);

        state.state = SlideState::Question;

        // This should exercise the team member filtering logic where some team members are dead
        // The filtering should count only alive members (player1, player2) and ensure max(count, 1)
        state.send_answers_announcements(
            Some(&team_manager),
            &watchers,
            &mut schedule_message,
            tunnel_finder,
            0,
        );

        assert_eq!(state.state(), SlideState::Answers);
    }

    #[test]
    fn test_send_answers_announcements_unassigned_watcher_handling() {
        let config = create_test_slide_config();
        let mut state = config.to_state();
        let mut watchers = create_mock_watchers();
        let tunnel_finder = create_mock_tunnel_finder();
        
        // Add only unassigned watchers to trigger line 545
        let unassigned_id = Id::new();
        let _ = watchers.add_watcher(unassigned_id, crate::watcher::Value::Unassigned);
        
        // No team manager for simplicity
        let team_manager = None;
        
        // Mock schedule_message function
        let mut schedule_message = |_: crate::AlarmMessage, _: std::time::Duration| {};
        
        // Call send_answers_announcements - should handle unassigned watchers by returning None (line 545)
        state.send_answers_announcements(
            team_manager,
            &watchers,
            &mut schedule_message,
            tunnel_finder,
            0,
        );
        
        // Test passes if no panic occurs and unassigned watchers are handled correctly
    }
}
