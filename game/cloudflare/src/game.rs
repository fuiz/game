use std::{
    cell::{Cell, RefCell, RefMut},
    str::FromStr,
    time::Duration,
};

use serde::{Deserialize, Serialize};
use worker::*;

use fuiz::tick::Tick;
use fuiz::wal::Event;
use fuiz::{
    game,
    session::Tunnel,
    watcher::{self},
};
use rustc_hash::FxHashSet;

use crate::journal::Journal;

#[derive(Debug, serde::Deserialize, garde::Validate, Serialize)]
#[garde(context(fuiz::settings::Settings))]
pub struct GameRequest {
    #[garde(dive)]
    pub config: fuiz::fuiz::config::Fuiz,
    #[garde(dive)]
    pub options: fuiz::game::Options,
}

struct WebSocketTunnel(WebSocket);

impl WebSocketTunnel {
    /// Closes without consuming the tunnel, for callers holding it by
    /// reference. [`Tunnel::close`] takes ownership, which the admission path
    /// cannot give up while it still needs the socket to report the refusal.
    fn close_tunnel(&self) {
        let _ = self.0.close::<String>(None, None);
    }
}

impl Tunnel for WebSocketTunnel {
    fn close(self) {
        self.close_tunnel();
    }

    fn send_message(&self, message: &fuiz::UpdateMessage) {
        let message = serde_json::to_string(message).expect("Failed to serialize message");

        let _ = self.0.send_with_str(message);
    }

    fn send_state(&self, state: &fuiz::SyncMessage) {
        let message = serde_json::to_string(state).expect("Failed to serialize state");

        let _ = self.0.send_with_str(message);
    }
}

#[durable_object]
pub struct Game {
    game: RefCell<Option<fuiz::game::Game>>,
    alarm_message: RefCell<Option<AlarmMessage>>,
    /// Where this game's history has got to in storage.
    journal: Journal,
    /// Whether storage has been read since this instance was constructed. A
    /// game that legitimately does not exist yet reads as `None`, so the
    /// absence of a game is not on its own a reason to read again.
    loaded: Cell<bool>,
    state: State,
    env: Env,
}

#[derive(Serialize, Deserialize)]
enum AlarmMessage {
    DeleteGame,
    Game(fuiz::AlarmMessage),
}

/// How long a game with nothing scheduled is kept before it is swept away.
const GAME_EXPIRY: Duration = Duration::from_hours(1);

impl Game {
    /// Loads the game the object is responsible for.
    async fn load_state(&self) {
        if self.loaded.get() {
            return;
        }
        self.loaded.set(true);

        let storage = self.state.storage();

        // Read before the game: an alarm can fire against storage that no
        // longer holds one, and the expiry sweep still has to run.
        self.alarm_message.replace(storage.get("alarm").await.ok().flatten());

        self.game.replace(self.journal.load(&storage).await);
    }

    /// Runs an event against the game and appends it to the log.
    ///
    /// This is the only path that mutates a live game, so every change the
    /// object makes is one a replay can make again. Returns `None` when there
    /// is no game to apply it to, otherwise whether the watcher was admitted.
    async fn record(&self, event: Event) -> Result<Option<std::result::Result<(), watcher::Error>>> {
        let tick = Tick::sample();
        let mut scheduled = None;

        let Some(admission) = self.with_mut_game(|game| {
            fuiz::wal::apply(
                game,
                &event,
                tick,
                |message: fuiz::AlarmMessage, duration: Duration| scheduled = Some((message, duration)),
                self.tunnel_finder(),
            )
        }) else {
            return Ok(None);
        };

        // The event is already in the live game and already on its way to
        // clients. If it could not be appended, fold it into a snapshot rather
        // than leave it in memory alone: the next scheduled snapshot could be
        // a hundred messages away, and an eviction before then would lose a
        // change players have already seen.
        if let Err(error) = self.journal.append(&self.state.storage(), tick, event).await {
            console_error!("Error appending log entry, snapshotting instead: {:?}", error);
            self.snapshot().await?;
        }

        self.update_alarm(scheduled).await?;

        if self.journal.snapshot_due() {
            self.snapshot().await?;
        }

        Ok(Some(admission))
    }

    /// Rewrites the alarm bookkeeping the way the pre-log object did: a
    /// scheduled transition when the game asked for one, otherwise the expiry
    /// sweep, so an abandoned game still cleans itself up.
    async fn update_alarm(&self, scheduled: Option<(fuiz::AlarmMessage, Duration)>) -> Result<()> {
        let storage = self.state.storage();

        if let Some((message, duration)) = scheduled {
            self.alarm_message.replace(Some(AlarmMessage::Game(message)));
            storage.set_alarm(duration).await?;
        } else if storage.get_alarm().await.unwrap_or(None).is_none() {
            self.alarm_message.replace(Some(AlarmMessage::DeleteGame));
            storage.set_alarm(GAME_EXPIRY).await?;
        } else {
            return Ok(());
        }

        storage.put("alarm", &self.alarm_message).await?;
        Ok(())
    }

    /// Hands the live game to the journal to be written out whole.
    ///
    /// Encoded first so the borrow is released before the write awaits.
    async fn snapshot(&self) -> Result<()> {
        let Some(snapshot) = self.borrow_game().map(|game| Journal::encode(&game)).transpose()? else {
            return Ok(());
        };

        self.journal
            .write_snapshot(&self.state.storage(), snapshot, &self.connected())
            .await
    }

    /// Records a join or rejoin, telling the client and dropping the socket if
    /// the game turned them away.
    async fn admit(&self, event: Event, session: &WebSocketTunnel) -> Result<()> {
        if let Some(Err(error)) = self.record(event).await? {
            session.send_message(&game::UpdateMessage::CannotJoin(error).into());
            session.close_tunnel();
        }

        Ok(())
    }

    /// The watchers currently holding a socket, which is what the game's
    /// tunnel finder answers from and therefore what a replay has to start
    /// from.
    fn connected(&self) -> FxHashSet<watcher::Id> {
        self.state
            .get_websockets()
            .into_iter()
            .filter_map(|socket| socket.deserialize_attachment::<watcher::Id>().ok().flatten())
            .collect()
    }
}

impl Game {
    fn borrow_game_mut(&self) -> Option<RefMut<'_, fuiz::game::Game>> {
        let game = RefMut::filter_map(self.game.borrow_mut(), std::option::Option::as_mut);

        game.ok()
    }

    fn borrow_game(&self) -> Option<std::cell::Ref<'_, fuiz::game::Game>> {
        let game = std::cell::Ref::filter_map(self.game.borrow(), std::option::Option::as_ref);

        game.ok()
    }

    fn with_mut_game<F, R>(&self, f: F) -> Option<R>
    where
        F: FnOnce(&mut fuiz::game::Game) -> R,
    {
        self.borrow_game_mut().map(|mut game| f(&mut game))
    }

    fn tunnel_finder(&self) -> impl Fn(watcher::Id) -> Option<WebSocketTunnel> + '_ {
        |id| {
            self.state
                .get_websockets_with_tag(&id.to_string())
                .first()
                .map(|ws| WebSocketTunnel(ws.to_owned()))
        }
    }

    async fn increment_player_count(&self) -> Result<()> {
        self.env
            .service("COUNTER")?
            .fetch("https://example.com/player_count", {
                Some(RequestInit {
                    method: Method::Post,
                    ..RequestInit::default()
                })
            })
            .await?;

        Ok(())
    }
}

impl DurableObject for Game {
    fn new(state: State, env: Env) -> Self {
        Self {
            game: None.into(),
            alarm_message: None.into(),
            journal: Journal::new(),
            loaded: Cell::new(false),
            state,
            env,
        }
    }

    async fn alarm(&self) -> Result<Response> {
        self.load_state().await;

        let alarm_message_to_be_announced = self.alarm_message.take();

        match alarm_message_to_be_announced {
            Some(AlarmMessage::DeleteGame) => {
                self.state.storage().delete_all().await?;

                // The snapshot and the log went with it, so a straggling
                // message must not append against the sequence they were under.
                self.game.replace(None);
                self.journal.reset();

                return Response::ok("");
            }
            Some(AlarmMessage::Game(message)) => {
                self.record(Event::Alarm(message)).await?;
            }
            _ => {}
        }

        Response::ok("")
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        self.load_state().await;

        if req.url()?.path().starts_with("/add") {
            let game_request = req.json::<GameRequest>().await?;

            let host_id = watcher::Id::new();

            let settings = fuiz::settings::Settings::default();
            self.game.replace(Some(fuiz::game::Game::new(
                game_request.config,
                game_request.options,
                host_id,
                &settings,
            )));
            self.loaded.set(true);

            // The starting state has to reach storage before any entry does:
            // replay rebuilds from a snapshot, and there is no other way to
            // recover the config and the host's id.
            self.snapshot().await?;

            // Arm the sweep straight away. A game whose host never opens the
            // websocket records no message, so nothing else would ever set an
            // alarm and the snapshot just written would sit there for good.
            self.update_alarm(None).await?;

            return Response::ok(host_id.to_string());
        }

        if req.url()?.path().starts_with("/alive") {
            let Some(game) = self.borrow_game() else {
                return Response::ok("false");
            };

            return Response::ok(if matches!(game.state, game::State::Done) {
                "false"
            } else {
                "true"
            });
        }

        let WebSocketPair { client, server } = WebSocketPair::new()?;

        let claimed_id = req
            .url()?
            .path_segments()
            .and_then(|mut ps| ps.next_back())
            .and_then(|s| watcher::Id::from_str(s).clone().ok())
            .unwrap_or(watcher::Id::new());

        close_connections_with_tag(&self.state, &claimed_id);
        self.state
            .accept_websocket_with_tags(&server, &[&claimed_id.to_string()]);
        server.serialize_attachment(claimed_id)?;

        Response::from_websocket(client)
    }

    async fn websocket_message(&self, ws: WebSocket, message: WebSocketIncomingMessage) -> Result<()> {
        self.load_state().await;

        {
            let WebSocketIncomingMessage::String(serialized_message) = message else {
                return Ok(());
            };

            let Ok(message) = serde_json::from_str(serialized_message.as_ref()) else {
                return Ok(());
            };

            let watcher_id = ws.deserialize_attachment::<watcher::Id>()?;

            if let Some(watcher_id) = watcher_id {
                match message {
                    game::IncomingMessage::Ghost(game::IncomingGhostMessage::DemandId) => {
                        close_connections_with_tag_except_one(&self.state, &watcher_id, &ws);
                        let session = WebSocketTunnel(ws);

                        session.send_message(&game::UpdateMessage::IdAssign(watcher_id).into());

                        self.admit(Event::Joined(watcher_id), &session).await?;

                        if let Err(e) = self.increment_player_count().await {
                            console_error!("Error incrementing player count: {:?}", e);
                        }
                    }
                    game::IncomingMessage::Ghost(_) => {
                        close_connections_with_tag_except_one(&self.state, &watcher_id, &ws);

                        let session = WebSocketTunnel(ws);

                        session.send_message(&game::UpdateMessage::IdAssign(watcher_id).into());

                        self.admit(Event::Rejoined(watcher_id), &session).await?;
                    }
                    message => {
                        self.record(Event::Received(watcher_id, message)).await?;
                    }
                }
            } else {
                let game::IncomingMessage::Ghost(ghost_message) = message else {
                    return Ok(());
                };

                // Whether the id is one the game already knows decides between
                // reclaiming it and handing out a fresh one. That lookup reads
                // game state the log does not carry, so the branch is resolved
                // here and only the resulting event is recorded.
                let known = matches!(ghost_message, game::IncomingGhostMessage::ClaimId(id)
                    if self.borrow_game().is_some_and(|game| game.watchers.has_watcher(id)));

                if let game::IncomingGhostMessage::ClaimId(id) = ghost_message
                    && known
                {
                    close_connections_with_tag(&self.state, &id);
                    ws.serialize_attachment(id)?;

                    let session = WebSocketTunnel(ws);
                    self.admit(Event::Rejoined(id), &session).await?;
                } else {
                    let new_id = watcher::Id::new();

                    ws.serialize_attachment(new_id)?;

                    let session = WebSocketTunnel(ws);
                    session.send_message(&game::UpdateMessage::IdAssign(new_id).into());

                    self.admit(Event::Joined(new_id), &session).await?;
                }
            }
        }

        Ok(())
    }

    async fn websocket_close(&self, ws: WebSocket, _code: usize, _reason: String, _was_clean: bool) -> Result<()> {
        let Some(watcher_id) = ws.deserialize_attachment::<watcher::Id>()? else {
            return Ok(());
        };

        self.load_state().await;

        self.record(Event::Left(watcher_id)).await?;

        Ok(())
    }
}

fn close_connections_with_tag_except_one(state: &State, tag: &watcher::Id, ws: &WebSocket) {
    state
        .get_websockets_with_tag(&tag.to_string())
        .into_iter()
        .filter(|web_socket| web_socket != ws)
        .for_each(close_web_socket);
}

fn close_connections_with_tag(state: &State, tag: &watcher::Id) {
    state
        .get_websockets_with_tag(&tag.to_string())
        .into_iter()
        .for_each(close_web_socket);
}

#[allow(clippy::needless_pass_by_value)]
fn close_web_socket(web_socket: WebSocket) {
    let _ = web_socket.close(Some(4141), None::<String>);
}
