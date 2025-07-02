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

/// Phase of the slide
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum SlideState {
    /// Unstarted, exists to distinguish between started and unstarted slide, usually treated the same as [`SlideState::Question`]
    #[default]
    Unstarted,
    /// Showing a question without answers
    Question,
    /// Showing questions and answers for players to answer
    Answers,
    /// Showing correct answers and their statistics
    AnswersResults,
}

type ValidationResult = garde::Result;

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

const MIN_TITLE_LENGTH: usize = crate::config::multiple_choice::MIN_TITLE_LENGTH;
const MIN_INTRODUCE_QUESTION: u64 = crate::config::multiple_choice::MIN_INTRODUCE_QUESTION as u64;
const MIN_TIME_LIMIT: u64 = crate::config::multiple_choice::MIN_TIME_LIMIT as u64;

const MAX_TIME_LIMIT: u64 = crate::config::multiple_choice::MAX_TIME_LIMIT as u64;
const MAX_TITLE_LENGTH: usize = crate::config::multiple_choice::MAX_TITLE_LENGTH;
const MAX_INTRODUCE_QUESTION: u64 = crate::config::multiple_choice::MAX_INTRODUCE_QUESTION as u64;

const MAX_ANSWER_COUNT: usize = crate::config::multiple_choice::MAX_ANSWER_COUNT as usize;

fn validate_introduce_question(val: &Duration) -> ValidationResult {
    validate_duration::<MIN_INTRODUCE_QUESTION, MAX_INTRODUCE_QUESTION>("introduce_question", val)
}

fn validate_time_limit(val: &Duration) -> ValidationResult {
    validate_duration::<MIN_TIME_LIMIT, MAX_TIME_LIMIT>("time_limit", val)
}

#[serde_with::serde_as]
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, serde::Deserialize, Validate)]
pub struct SlideConfig {
    /// The question title, represents what's being asked
    #[garde(length(min = MIN_TITLE_LENGTH, max = MAX_TITLE_LENGTH))]
    title: String,
    /// Accompanying media
    #[garde(dive)]
    media: Option<Media>,
    /// Time before answers get displayed
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
    /// Accompanying answers
    #[garde(length(max = MAX_ANSWER_COUNT))]
    answers: Vec<AnswerChoice>,
}

/// Presenting a multiple choice question that presents a question then the answers with optional accompanying media
#[serde_with::serde_as]
#[skip_serializing_none]
#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct State {
    /// The question title, represents what's being asked
    config: SlideConfig,

    // State
    /// Storage of user answers combined with the time of answering
    user_answers: HashMap<Id, (usize, SystemTime)>,
    /// Instant where answers were first displayed
    answer_start: Option<SystemTime>,
    /// Stage of the slide
    state: SlideState,
}

impl SlideConfig {
    pub fn to_state(&self) -> State {
        State {
            config: self.clone(),
            user_answers: HashMap::new(),
            answer_start: None,
            state: SlideState::Unstarted,
        }
    }
}

/// Utility option with contextual meaning of visibility to the player or the host
#[serde_with::serde_as]
#[skip_serializing_none]
#[derive(Debug, Serialize, Clone)]
pub enum PossiblyHidden<T> {
    Visible(T),
    Hidden,
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
    /// Announcements of the possible answers for the players to choose
    AnswersAnnouncement {
        /// Time before the answering phase ends
        #[serde_as(as = "serde_with::DurationMilliSeconds<u64>")]
        duration: Duration,
        /// Possible answers to choose from
        answers: Vec<PossiblyHidden<TextOrMedia>>,
    },
    /// (HOST ONLY): Number of players who answered the question
    AnswersCount(usize),
    /// Results of the game including correct answers and statistics of how many they got chosen
    AnswersResults {
        /// Same answers for the question displayed
        answers: Vec<TextOrMedia>,
        /// Correctness and statistics about the answers
        results: Vec<AnswerChoiceResult>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum AlarmMessage {
    ProceedFromSlideIntoSlide { index: usize, to: SlideState },
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
        index: usize,
        count: usize,
        question: String,
        media: Option<Media>,
        /// Remaining time for the question to be displayed without its answers
        #[serde_as(as = "serde_with::DurationMilliSeconds<u64>")]
        duration: Duration,
    },
    /// Announcements of the possible answers for the players to choose
    AnswersAnnouncement {
        index: usize,
        count: usize,
        question: String,
        media: Option<Media>,
        /// Remaining time before the answering phase ends
        #[serde_as(as = "serde_with::DurationMilliSeconds<u64>")]
        duration: Duration,
        answers: Vec<PossiblyHidden<TextOrMedia>>,
        answered_count: usize,
    },
    /// Results of the game including correct answers and statistics of how many they got chosen
    AnswersResults {
        index: usize,
        count: usize,
        question: String,
        media: Option<Media>,
        answers: Vec<TextOrMedia>,
        results: Vec<AnswerChoiceResult>,
    },
}

/// Answer choice in the question slide
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnswerChoice {
    pub correct: bool,
    pub content: TextOrMedia,
}

/// Correctness and statistic on how players answered the question
#[derive(Debug, Serialize, Clone)]
pub struct AnswerChoiceResult {
    correct: bool,
    count: usize,
}

impl State {
    pub fn play<
        T: Tunnel,
        F: Fn(Id) -> Option<T>,
        S: FnMut(crate::AlarmMessage, time::Duration),
    >(
        &mut self,
        team_manager: Option<&TeamManager>,
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

    fn calculate_score(
        full_duration: Duration,
        taken_duration: Duration,
        full_points_awarded: u64,
    ) -> u64 {
        (full_points_awarded as f64
            * (1. - (taken_duration.as_secs_f64() / full_duration.as_secs_f64() / 2.)))
            as u64
    }

    fn start_timer(&mut self) {
        self.answer_start = Some(SystemTime::now());
    }

    fn timer(&self) -> SystemTime {
        self.answer_start.unwrap_or(SystemTime::now())
    }

    fn send_question_announcements<
        T: Tunnel,
        F: Fn(Id) -> Option<T>,
        S: FnMut(crate::AlarmMessage, time::Duration),
    >(
        &mut self,
        team_manager: Option<&TeamManager>,
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
                )
            }
        }
    }

    fn send_answers_announcements<
        T: Tunnel,
        F: Fn(Id) -> Option<T>,
        S: FnMut(crate::AlarmMessage, time::Duration),
    >(
        &mut self,
        team_manager: Option<&TeamManager>,
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
                                                        watchers.is_alive(*id, &tunnel_finder)
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
                                                watchers.is_alive(id, &tunnel_finder)
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
            )
        }
    }

    fn change_state(&mut self, before: SlideState, after: SlideState) -> bool {
        if self.state == before {
            self.state = after;

            true
        } else {
            false
        }
    }

    fn state(&self) -> SlideState {
        self.state
    }

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

    fn add_scores<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &self,
        leaderboard: &mut Leaderboard,
        watchers: &Watchers,
        team_manager: Option<&TeamManager>,
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
                    std::iter::repeat(PossiblyHidden::Hidden)
                        .take(self.config.answers.len())
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
                                        .filter(|id| watchers.is_alive(*id, &tunnel_finder))
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
                                .team_index(watcher_id, |id| watchers.is_alive(id, &tunnel_finder))
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
        team_manager: Option<&TeamManager>,
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
                if v < self.config.answers.len() =>
            {
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
        };

        false
    }

    pub fn receive_alarm<
        T: Tunnel,
        F: Fn(Id) -> Option<T>,
        S: FnMut(crate::AlarmMessage, web_time::Duration),
    >(
        &mut self,
        _leaderboard: &mut Leaderboard,
        watchers: &Watchers,
        team_manager: Option<&TeamManager>,
        schedule_message: &mut S,
        tunnel_finder: F,
        message: crate::AlarmMessage,
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
        };

        false
    }
}
