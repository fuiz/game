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

#[derive(Debug, Serialize, Deserialize, Clone, Validate)]
pub enum TextOrMedia {
    Media(#[garde(skip)] Media),
    Text(#[garde(length(max = crate::constants::answer_text::MAX_LENGTH))] String),
}

/// A fuiz configuration, title is unused
#[derive(Debug, Serialize, Deserialize, Clone, Validate)]
pub struct Fuiz {
    #[garde(length(max = crate::constants::fuiz::MAX_TITLE_LENGTH))]
    title: String,

    #[garde(length(max = crate::constants::fuiz::MAX_SLIDES_COUNT), dive)]
    pub slides: Vec<SlideConfig>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct CurrentSlide {
    pub index: usize,
    pub state: SlideState,
}

#[derive(Debug, Serialize, Deserialize, Clone, Validate)]
pub enum SlideConfig {
    MultipleChoice(#[garde(dive)] multiple_choice::SlideConfig),
    TypeAnswer(#[garde(dive)] type_answer::SlideConfig),
    Order(#[garde(dive)] order::SlideConfig),
}

impl SlideConfig {
    pub fn to_state(&self) -> SlideState {
        match self {
            Self::MultipleChoice(s) => SlideState::MultipleChoice(s.to_state()),
            Self::TypeAnswer(s) => SlideState::TypeAnswer(s.to_state()),
            Self::Order(s) => SlideState::Order(s.to_state()),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub enum SlideState {
    MultipleChoice(multiple_choice::State),
    TypeAnswer(type_answer::State),
    Order(order::State),
}

impl Fuiz {
    pub fn len(&self) -> usize {
        self.slides.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slides.is_empty()
    }
}

impl SlideState {
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
