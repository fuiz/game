use derive_where::derive_where;
use enum_map::EnumMap;
use parking_lot::{MappedRwLockReadGuard, RwLockReadGuard};
use serde::Serialize;
use thiserror::Error;

use self::{
    fuiz::config::Fuiz,
    game::{Game, IncomingMessage},
    game_id::GameId,
    session::Tunnel,
    watcher::Id,
};

pub mod fuiz;
pub mod game;
pub mod game_id;
pub mod leaderboard;
pub mod names;
pub mod session;
pub mod watcher;

#[derive(Debug, Serialize, Clone, derive_more::From)]
pub enum SyncMessage {
    Game(game::SyncMessage),
    Bingo(fuiz::bingo::SyncMessage),
    MultipleChoice(fuiz::multiple_choice::SyncMessage),
}

impl SyncMessage {
    pub fn to_message(&self) -> String {
        serde_json::to_string(self).expect("default serializer cannot fail")
    }
}

#[derive(Debug, Serialize, Clone, derive_more::From)]
pub enum UpdateMessage {
    Game(game::UpdateMessage),
    Bingo(fuiz::bingo::UpdateMessage),
    MultipleChoice(fuiz::multiple_choice::UpdateMessage),
}

impl UpdateMessage {
    pub fn to_message(&self) -> String {
        serde_json::to_string(self).expect("default serializer cannot fail")
    }
}

#[derive_where(Debug, Default)]
struct SharedGame<T: Tunnel>(parking_lot::RwLock<Option<Box<Game<T>>>>);

impl<T: Tunnel> SharedGame<T> {
    pub fn read(&self) -> Option<MappedRwLockReadGuard<'_, Game<T>>> {
        RwLockReadGuard::try_map(self.0.read(), std::option::Option::as_ref)
            .ok()
            .and_then(|x| {
                if x.state().is_done() {
                    None
                } else {
                    Some(MappedRwLockReadGuard::map(x, unbox_box::BoxExt::unbox_ref))
                }
            })
    }
}

#[derive_where(Debug, Default)]
pub struct GameManager<T: Tunnel> {
    games: EnumMap<GameId, SharedGame<T>>,
}

#[derive(Debug, Error)]
#[error("game does not exist")]
pub struct GameVanish {}

impl actix_web::error::ResponseError for GameVanish {}

impl<T: Tunnel> GameManager<T> {
    pub fn add_game(&self, fuiz: Fuiz) -> GameId {
        let shared_game = Box::new(Game::new(fuiz));

        loop {
            let game_id = GameId::new();

            let Some(mut game) = self.games[game_id].0.try_write() else {
                continue;
            };

            if game.is_none() {
                *game = Some(shared_game);
                return game_id;
            }
        }
    }

    pub fn reserve_host(&self, game_id: GameId, watcher_id: Id) -> Result<(), GameVanish> {
        self.get_game(game_id)?.reserve_host(watcher_id);
        Ok(())
    }

    pub fn add_unassigned(
        &self,
        game_id: GameId,
        watcher_id: Id,
        new_session: T,
    ) -> Result<Result<(), watcher::Error>, GameVanish> {
        Ok(self
            .get_game(game_id)?
            .add_unassigned(watcher_id, new_session))
    }

    pub fn alive_check(&self, game_id: GameId) -> Result<bool, GameVanish> {
        let game = self.get_game(game_id)?;
        Ok(!matches!(game.state(), game::State::Done)
            && game.updated().elapsed() <= std::time::Duration::from_secs(60 * 5))
    }

    pub fn watcher_exists(&self, game_id: GameId, watcher_id: Id) -> Result<bool, GameVanish> {
        Ok(self.get_game(game_id)?.has_watcher(watcher_id))
    }

    pub async fn receive_message(
        &self,
        game_id: GameId,
        watcher_id: Id,
        message: IncomingMessage,
    ) -> Result<(), GameVanish> {
        self.get_game(game_id)?
            .receive_message(watcher_id, message)
            .await;
        Ok(())
    }

    pub fn remove_watcher_session(
        &self,
        game_id: GameId,
        watcher_id: Id,
    ) -> Result<(), GameVanish> {
        self.get_game(game_id)?.remove_watcher_session(watcher_id);
        Ok(())
    }

    pub fn exists(&self, game_id: GameId) -> Result<(), GameVanish> {
        let _ = self.get_game(game_id)?;

        Ok(())
    }

    pub fn update_session(
        &self,
        game_id: GameId,
        watcher_id: Id,
        new_session: T,
    ) -> Result<(), GameVanish> {
        self.get_game(game_id)?
            .update_session(watcher_id, new_session);

        Ok(())
    }

    pub fn get_game(
        &self,
        game_id: GameId,
    ) -> Result<MappedRwLockReadGuard<'_, Game<T>>, GameVanish> {
        self.games[game_id].read().ok_or(GameVanish {})
    }

    pub fn remove_game(&self, game_id: GameId) {
        let mut game = self.games[game_id].0.write();
        if let Some(ongoing_game) = game.take() {
            ongoing_game.mark_as_done();
        }
    }
}
