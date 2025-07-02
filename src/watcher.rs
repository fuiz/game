use std::{
    collections::{HashMap, HashSet},
    fmt::Display,
    str::FromStr,
};

use enum_map::{Enum, EnumMap};
use itertools::Itertools;
use kinded::Kinded;
use serde::{Deserialize, Serialize};
use serde_with::{DeserializeFromStr, SerializeDisplay};
use thiserror::Error;
use uuid::Uuid;

use super::{session::Tunnel, SyncMessage, UpdateMessage};

#[derive(
    Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, DeserializeFromStr, SerializeDisplay,
)]
pub struct Id(Uuid);

impl Id {
    pub fn _get_seed(&self) -> u64 {
        self.0.as_u64_pair().0
    }

    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for Id {
    fn default() -> Self {
        Self::new()
    }
}

impl Display for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl FromStr for Id {
    type Err = uuid::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(Self(Uuid::from_str(s)?))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Kinded, Serialize, Deserialize)]
#[kinded(derive(Hash, Enum, Serialize, Deserialize))]
pub enum Value {
    Unassigned,
    Host,
    Player(PlayerValue),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PlayerValue {
    Individual {
        name: String,
    },
    Team {
        team_name: String,
        individual_name: String,
        team_id: Id,
    },
}

impl PlayerValue {
    pub fn name(&self) -> &str {
        match self {
            Self::Individual { name } => name,
            Self::Team {
                team_name: _,
                individual_name,
                team_id: _,
            } => individual_name,
        }
    }
}

#[derive(Deserialize)]
struct WatchersSerde {
    mapping: HashMap<Id, Value>,
}

#[derive(Default, Serialize, Deserialize)]
#[serde(from = "WatchersSerde")]
pub struct Watchers {
    mapping: HashMap<Id, Value>,

    #[serde(skip_serializing)]
    reverse_mapping: EnumMap<ValueKind, HashSet<Id>>,
}

impl From<WatchersSerde> for Watchers {
    fn from(serde: WatchersSerde) -> Self {
        let WatchersSerde { mapping } = serde;
        let mut reverse_mapping: EnumMap<ValueKind, HashSet<Id>> = EnumMap::default();
        for (id, value) in mapping.iter() {
            reverse_mapping[value.kind()].insert(*id);
        }
        Self {
            mapping,
            reverse_mapping,
        }
    }
}

#[derive(Error, Serialize, Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    #[error("maximum number of players reached")]
    MaximumPlayers,
}

impl Watchers {
    pub fn with_host_id(host_id: Id) -> Self {
        Self {
            mapping: {
                let mut map = HashMap::default();
                map.insert(host_id, Value::Host);
                map
            },
            reverse_mapping: {
                let mut map: EnumMap<ValueKind, HashSet<Id>> = EnumMap::default();
                map[ValueKind::Host].insert(host_id);
                map
            },
        }
    }

    pub fn vec<T: Tunnel, F: Fn(Id) -> Option<T>>(&self, tunnel_finder: F) -> Vec<(Id, T, Value)> {
        self.reverse_mapping
            .values()
            .flat_map(|v| v.iter())
            .filter_map(|x| match (tunnel_finder(*x), self.mapping.get(x)) {
                (Some(t), Some(v)) => Some((*x, t, v.to_owned())),
                _ => None,
            })
            .collect_vec()
    }

    pub fn specific_vec<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &self,
        filter: ValueKind,
        tunnel_finder: F,
    ) -> Vec<(Id, T, Value)> {
        self.reverse_mapping[filter]
            .iter()
            .filter_map(|x| match (tunnel_finder(*x), self.mapping.get(x)) {
                (Some(t), Some(v)) => Some((*x, t, v.to_owned())),
                _ => None,
            })
            .collect_vec()
    }

    pub fn specific_count(&self, filter: ValueKind) -> usize {
        self.reverse_mapping[filter].len()
    }

    pub fn add_watcher(&mut self, watcher_id: Id, watcher_value: Value) -> Result<(), Error> {
        let kind = watcher_value.kind();

        if self.mapping.len() >= crate::config::fuiz::MAX_PLAYER_COUNT {
            return Err(Error::MaximumPlayers);
        }

        self.mapping.insert(watcher_id, watcher_value);
        self.reverse_mapping[kind].insert(watcher_id);

        Ok(())
    }

    pub fn update_watcher_value(&mut self, watcher_id: Id, watcher_value: Value) {
        let old_kind = match self.mapping.get(&watcher_id) {
            Some(v) => v.kind(),
            _ => return,
        };
        let new_kind = watcher_value.kind();
        if old_kind != new_kind {
            self.reverse_mapping[old_kind].remove(&watcher_id);
            self.reverse_mapping[new_kind].insert(watcher_id);
        }
        self.mapping.insert(watcher_id, watcher_value);
    }

    pub fn get_watcher_value(&self, watcher_id: Id) -> Option<Value> {
        self.mapping.get(&watcher_id).map(|v| v.to_owned())
    }

    pub fn has_watcher(&self, watcher_id: Id) -> bool {
        self.mapping.contains_key(&watcher_id)
    }

    pub fn is_alive<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &self,
        watcher_id: Id,
        tunnel_finder: F,
    ) -> bool {
        tunnel_finder(watcher_id).is_some()
    }

    pub fn remove_watcher_session<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &mut self,
        watcher_id: &Id,
        tunnel_finder: F,
    ) {
        if let Some(x) = tunnel_finder(*watcher_id) {
            x.close();
        }
    }

    pub fn send_message<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &self,
        message: &UpdateMessage,
        watcher_id: Id,
        tunnel_finder: F,
    ) {
        let Some(session) = tunnel_finder(watcher_id) else {
            return;
        };

        session.send_message(message);
    }

    pub fn send_state<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &self,
        message: &SyncMessage,
        watcher_id: Id,
        tunnel_finder: F,
    ) {
        let Some(session) = tunnel_finder(watcher_id) else {
            return;
        };

        session.send_state(message);
    }

    pub fn get_name(&self, watcher_id: Id) -> Option<String> {
        self.get_watcher_value(watcher_id).and_then(|v| match v {
            Value::Player(player_value) => Some(player_value.name().to_owned()),
            _ => None,
        })
    }

    pub fn get_team_name(&self, watcher_id: Id) -> Option<String> {
        self.get_watcher_value(watcher_id).and_then(|v| match v {
            Value::Player(PlayerValue::Team { team_name, .. }) => Some(team_name),
            _ => None,
        })
    }

    pub fn announce_with<S, T: Tunnel, F: Fn(Id) -> Option<T>>(&self, sender: S, tunnel_finder: F)
    where
        S: Fn(Id, ValueKind) -> Option<super::UpdateMessage>,
    {
        for (watcher, session, v) in self.vec(tunnel_finder) {
            let Some(message) = sender(watcher, v.kind()) else {
                continue;
            };

            session.send_message(&message);
        }
    }

    pub fn announce<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &self,
        message: &super::UpdateMessage,
        tunnel_finder: F,
    ) {
        self.announce_with(
            |_, value_kind| {
                if matches!(value_kind, ValueKind::Unassigned) {
                    None
                } else {
                    Some(message.to_owned())
                }
            },
            tunnel_finder,
        );
    }

    pub fn announce_specific<T: Tunnel, F: Fn(Id) -> Option<T>>(
        &self,
        filter: ValueKind,
        message: &super::UpdateMessage,
        tunnel_finder: F,
    ) {
        for (_, session, _) in self.specific_vec(filter, tunnel_finder) {
            session.send_message(message);
        }
    }
}
