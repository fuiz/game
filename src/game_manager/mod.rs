use std::{
    ops::{Deref, DerefMut},
    sync::atomic::AtomicUsize,
    time::Duration,
};

use enum_map::EnumMap;
use jiden::StateSaver;
use parking_lot::{
    MappedRwLockReadGuard, MappedRwLockWriteGuard, RwLockReadGuard, RwLockWriteGuard,
};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::{clashmap::ClashMap, Session};

use fuiz::{
    fuiz::config::Fuiz,
    game::{self, Game, IncomingMessage, Options},
    game_id::GameId,
    watcher::{self, Id},
    AlarmMessage,
};

#[derive(Debug, Default)]
struct SharedGame(parking_lot::RwLock<Option<Box<Game>>>);

impl SharedGame {
    pub fn read(&self) -> Option<MappedRwLockReadGuard<'_, Game>> {
        RwLockReadGuard::try_map(self.0.read(), std::option::Option::as_ref)
            .ok()
            .and_then(|x| {
                if matches!(x.state, game::State::Done) {
                    None
                } else {
                    Some(MappedRwLockReadGuard::map(x, Box::deref))
                }
            })
    }

    pub fn write(&self) -> Option<MappedRwLockWriteGuard<'_, Game>> {
        RwLockWriteGuard::try_map(self.0.write(), std::option::Option::as_mut)
            .ok()
            .and_then(|x| {
                if matches!(x.state, game::State::Done) {
                    None
                } else {
                    Some(MappedRwLockWriteGuard::map(x, Box::deref_mut))
                }
            })
    }

    pub fn write_done(&self) -> Option<MappedRwLockWriteGuard<'_, Game>> {
        RwLockWriteGuard::try_map(self.0.write(), std::option::Option::as_mut)
            .ok()
            .map(|x| MappedRwLockWriteGuard::map(x, Box::deref_mut))
    }
}

#[derive(Default, Debug, Serialize, Deserialize)]
pub struct Statistics {
    all_games: AtomicUsize,
    game_count: AtomicUsize,
}

pub struct GameManager {
    games: EnumMap<GameId, SharedGame>,
    statistics: Statistics,
    state_saver: StateSaver<Statistics>,
    watcher_mapping: ClashMap<Id, Session>,
}

impl Default for GameManager {
    fn default() -> Self {
        let state_saver = StateSaver::new("stats.txt");
        Self {
            games: EnumMap::default(),
            statistics: state_saver.state().unwrap_or_default(),
            state_saver,
            watcher_mapping: ClashMap::default(),
        }
    }
}

#[derive(Debug, Error)]
#[error("game does not exist")]
pub struct GameVanish {}

impl actix_web::error::ResponseError for GameVanish {}

impl GameManager {
    pub fn add_game(&self, fuiz: Fuiz, options: Options, host_id: Id) -> GameId {
        let shared_game = Box::new(Game::new(fuiz, options, host_id));

        loop {
            let game_id = GameId::new();

            let Some(mut game) = self.games[game_id].0.try_write() else {
                continue;
            };

            if game.is_none() {
                *game = Some(shared_game);
                self.statistics
                    .game_count
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.statistics
                    .all_games
                    .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                self.state_saver.save(&self.statistics);

                return game_id;
            }
        }
    }

    fn tunnel_finder(&self, watcher_id: Id) -> Option<Session> {
        self.watcher_mapping.get(&watcher_id)
    }

    pub fn set_tunnel(&self, watcher_id: Id, tunnel: Session) -> Option<Session> {
        self.watcher_mapping.insert(watcher_id, tunnel)
    }

    pub fn remove_tunnel(&self, watcher_id: Id) -> Option<Session> {
        self.watcher_mapping.remove(&watcher_id).map(|(_, s)| s)
    }

    pub fn add_unassigned(
        &self,
        game_id: GameId,
        watcher_id: Id,
    ) -> Result<Result<(), watcher::Error>, GameVanish> {
        Ok(self
            .get_game_mut(game_id)?
            .add_unassigned(watcher_id, |id| self.tunnel_finder(id)))
    }

    pub fn alive_check(&self, game_id: GameId) -> Result<bool, GameVanish> {
        let game = self.get_done_game_mut(game_id)?;
        Ok(!matches!(game.state, game::State::Done))
    }

    pub fn watcher_exists(&self, game_id: GameId, watcher_id: Id) -> Result<bool, GameVanish> {
        Ok(self.get_game(game_id)?.watchers.has_watcher(watcher_id))
    }

    pub fn receive_message<F: Fn(AlarmMessage, Duration)>(
        &self,
        game_id: GameId,
        watcher_id: Id,
        message: IncomingMessage,
        schedule_message: F,
    ) -> Result<(), GameVanish> {
        self.get_game_mut(game_id)?
            .receive_message(watcher_id, message, schedule_message, |id| {
                self.tunnel_finder(id)
            });
        Ok(())
    }

    pub fn receive_alarm<F: Fn(AlarmMessage, Duration)>(
        &self,
        game_id: GameId,
        alarm_message: AlarmMessage,
        schedule_message: F,
    ) -> Result<(), GameVanish> {
        self.get_game_mut(game_id)?
            .receive_alarm(&alarm_message, schedule_message, |id| {
                self.tunnel_finder(id)
            });
        Ok(())
    }

    pub fn remove_watcher_session(&self, watcher_id: Id) -> Result<(), GameVanish> {
        fuiz::watcher::Watchers::remove_watcher_session(watcher_id, |id| self.tunnel_finder(id));
        Ok(())
    }

    pub fn exists(&self, game_id: GameId) -> Result<(), GameVanish> {
        let _ = self.get_game(game_id)?;

        Ok(())
    }

    pub fn update_session(&self, game_id: GameId, watcher_id: Id) -> Result<(), GameVanish> {
        self.get_game_mut(game_id)?
            .update_session(watcher_id, |id| self.tunnel_finder(id));

        Ok(())
    }

    pub fn get_game(&self, game_id: GameId) -> Result<MappedRwLockReadGuard<'_, Game>, GameVanish> {
        self.games[game_id].read().ok_or(GameVanish {})
    }

    pub fn get_game_mut(
        &self,
        game_id: GameId,
    ) -> Result<MappedRwLockWriteGuard<'_, Game>, GameVanish> {
        self.games[game_id].write().ok_or(GameVanish {})
    }

    pub fn get_done_game_mut(
        &self,
        game_id: GameId,
    ) -> Result<MappedRwLockWriteGuard<'_, Game>, GameVanish> {
        self.games[game_id].write_done().ok_or(GameVanish {})
    }

    pub fn remove_game(&self, game_id: GameId) {
        let mut game = self.games[game_id].0.write();
        if let Some(mut ongoing_game) = game.take() {
            self.statistics
                .game_count
                .fetch_sub(1, std::sync::atomic::Ordering::SeqCst);
            self.state_saver.save(&self.statistics);
            ongoing_game.mark_as_done(|id| self.tunnel_finder(id));
        }
    }

    pub fn count(&self) -> (usize, usize) {
        (
            self.statistics
                .game_count
                .load(std::sync::atomic::Ordering::SeqCst),
            self.statistics
                .all_games
                .load(std::sync::atomic::Ordering::SeqCst),
        )
    }
}
