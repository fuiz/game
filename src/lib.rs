use derive_where::derive_where;
use itertools::Itertools;
use serde::{Deserialize, Serialize};

pub mod constants;

pub mod fuiz;
pub mod game;
pub mod game_id;
pub mod leaderboard;
pub mod names;
pub mod session;
pub mod teams;
pub mod watcher;

#[derive(Debug, Serialize, Clone, derive_more::From)]
pub enum SyncMessage {
    Game(game::SyncMessage),
    MultipleChoice(fuiz::multiple_choice::SyncMessage),
    TypeAnswer(fuiz::type_answer::SyncMessage),
    Order(fuiz::order::SyncMessage),
}

impl SyncMessage {
    pub fn to_message(&self) -> String {
        serde_json::to_string(self).expect("default serializer cannot fail")
    }
}

#[derive(Debug, Serialize, Clone, derive_more::From)]
pub enum UpdateMessage {
    Game(game::UpdateMessage),
    MultipleChoice(fuiz::multiple_choice::UpdateMessage),
    TypeAnswer(fuiz::type_answer::UpdateMessage),
    Order(fuiz::order::UpdateMessage),
}

#[derive(Debug, Clone, derive_more::From, Serialize, Deserialize)]
pub enum AlarmMessage {
    MultipleChoice(fuiz::multiple_choice::AlarmMessage),
    TypeAnswer(fuiz::type_answer::AlarmMessage),
    Order(fuiz::order::AlarmMessage),
}

impl UpdateMessage {
    pub fn to_message(&self) -> String {
        serde_json::to_string(self).expect("default serializer cannot fail")
    }
}

#[derive(Debug, Clone, Serialize)]
#[derive_where(Default)]
pub struct TruncatedVec<T> {
    exact_count: usize,
    items: Vec<T>,
}

impl<T: Clone> TruncatedVec<T> {
    fn new<I: Iterator<Item = T>>(list: I, limit: usize, exact_count: usize) -> Self {
        let items = list.take(limit).collect_vec();
        Self { exact_count, items }
    }

    fn map<F, U>(self, f: F) -> TruncatedVec<U>
    where
        F: Fn(T) -> U,
    {
        TruncatedVec {
            exact_count: self.exact_count,
            items: self.items.into_iter().map(f).collect_vec(),
        }
    }
}
