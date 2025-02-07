use std::str::FromStr;

use garde::Validate;
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::json;
use wasm_bindgen_futures::wasm_bindgen::JsValue;
use worker::*;

use fuiz::{fuiz::config::Fuiz, game, session::Tunnel, watcher};
use worker_sys::web_sys::Blob;

#[derive(Serialize, Deserialize)]
pub struct GameManagerInstance {
    pub id: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

struct WebSocketTunnel(WebSocket);

impl Tunnel for WebSocketTunnel {
    fn close(self) {
        let _ = self.0.close::<String>(None, None);
    }

    fn send_message(&self, message: &fuiz::UpdateMessage) {
        let message = message.to_message();

        let _ = self.0.send_with_str(message);
    }

    fn send_state(&self, state: &fuiz::SyncMessage) {
        let message = state.to_message();

        let _ = self.0.send_with_str(message);
    }
}

enum LoadingState {
    Loading,
    Done(Option<fuiz::game::Game>),
}

#[durable_object]
pub struct Game {
    game: LoadingState,
    state: State,
    alarm_message: Option<AlarmMessage>,
    env: Env,
}

#[derive(serde::Deserialize, garde::Validate, Serialize)]
struct GameRequest {
    #[garde(dive)]
    config: Fuiz,
    #[garde(dive)]
    options: game::Options,
}

#[derive(Serialize, Deserialize)]
enum AlarmMessage {
    DeleteGame,
    Game(fuiz::AlarmMessage),
}

impl Game {
    async fn load_state(&mut self) {
        if matches!(self.game, LoadingState::Loading) {
            self.game = LoadingState::Done(load_game(&self.state.storage()).await);
            self.alarm_message = self.state.storage().get("alarm").await.ok();
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(transparent)]
struct GameBytes {
    #[serde(with = "serde_bytes")]
    bytes: Vec<u8>,
}

async fn load_game(storage: &worker::durable::Storage) -> Option<fuiz::game::Game> {
    let count = storage.get("count").await.ok()?;

    let mut game_bytes = Vec::new();

    for i in 0..count {
        let array_buffer: Result<GameBytes> = storage.get(&format!("chunk_{}", i)).await;
        match array_buffer {
            Err(e) => {
                console_error!("Error loading chunk: {:?}", e);
                return None;
            }
            Ok(string_chunk) => {
                game_bytes.extend_from_slice(&string_chunk.bytes);
            }
        }
    }

    let game = ciborium::from_reader(game_bytes.as_slice());

    match game {
        Ok(game) => Some(game),
        Err(e) => {
            console_error!("Error deserializing game: {:?}", e);
            None
        }
    }
}

async fn store_game(storage: &mut worker::durable::Storage, game: &fuiz::game::Game) -> Result<()> {
    let mut game_bytes = Vec::new();

    ciborium::into_writer(game, &mut game_bytes).map_err(|e| {
        console_error!("Error serializing game: {:?}", e);
        worker::Error::RustError(e.to_string())
    })?;

    let chunks_of_64kb = game_bytes
        .chunks(64 * 1024)
        .map(|chunk| GameBytes {
            bytes: chunk.to_vec(),
        })
        .collect::<Vec<_>>();

    storage.put("count", &chunks_of_64kb.len()).await?;

    for (i, chunk) in chunks_of_64kb.into_iter().enumerate() {
        if let Err(e) = storage.put(&format!("chunk_{}", i), &chunk).await {
            console_error!("Error storing chunk: {:?}", e);
        }
    }
    Ok(())
}

const GAME_EXPIRY: chrono::Duration = chrono::Duration::hours(1);

#[durable_object]
impl DurableObject for Game {
    fn new(state: State, env: Env) -> Self {
        Self {
            game: LoadingState::Loading,
            state,
            alarm_message: None,
            env,
        }
    }

    async fn alarm(&mut self) -> Result<Response> {
        self.load_state().await;

        let LoadingState::Done(game) = &mut self.game else {
            return Response::empty();
        };

        let alarm_message_to_be_announced = self.alarm_message.take();

        let alarm_message = &mut self.alarm_message;

        let state = &self.state;

        let schedule_message = move |message: fuiz::AlarmMessage, duration: web_time::Duration| {
            let time_in_future = chrono::Utc::now() + duration;

            *alarm_message = Some(AlarmMessage::Game(message));

            let storage = state.storage();

            state.wait_until(async move {
                let _ = storage
                    .set_alarm(ScheduledTime::new(js_sys::Date::new(&JsValue::from_f64(
                        time_in_future.timestamp_millis() as f64,
                    ))))
                    .await;
            })
        };

        match (alarm_message_to_be_announced, game) {
            (Some(AlarmMessage::DeleteGame), _) => {
                self.state.storage().delete_all().await?;
                return Response::ok("");
            }
            (Some(AlarmMessage::Game(message)), Some(game)) => {
                game.receive_alarm(message, schedule_message, |id| {
                    self.state
                        .get_websockets_with_tag(&id.to_string())
                        .first()
                        .map(|ws| WebSocketTunnel(ws.to_owned()))
                });

                store_game(&mut self.state.storage(), game).await?;

                if self.state.storage().get_alarm().await.unwrap().is_none() {
                    self.alarm_message = Some(AlarmMessage::DeleteGame);
                    self.state
                        .storage()
                        .set_alarm(ScheduledTime::new(js_sys::Date::new(&JsValue::from_f64(
                            (chrono::Utc::now() + GAME_EXPIRY).timestamp_millis() as f64,
                        ))))
                        .await?;
                }

                self.state
                    .storage()
                    .put("alarm", &self.alarm_message)
                    .await?;
            }
            _ => {}
        };

        Response::ok("")
    }

    async fn fetch(&mut self, mut req: Request) -> Result<Response> {
        self.load_state().await;

        if req.url()?.path().starts_with("/add") {
            let game_request = req.json::<GameRequest>().await?;

            let host_id = watcher::Id::new();

            self.game = LoadingState::Done(Some(fuiz::game::Game::new(
                game_request.config,
                game_request.options,
                host_id,
            )));
            return Response::ok(host_id.to_string());
        }

        if req.url()?.path().starts_with("/alive") {
            let LoadingState::Done(game) = &mut self.game else {
                return Response::ok("false");
            };

            return Response::ok(
                if game
                    .as_ref()
                    .map(|g| !matches!(g.state, game::State::Done))
                    .unwrap_or(false)
                {
                    "true"
                } else {
                    "false"
                },
            );
        }

        let WebSocketPair { client, server } = WebSocketPair::new()?;

        let claimed_id = req
            .url()?
            .path_segments()
            .and_then(|ps| ps.last())
            .and_then(|s| watcher::Id::from_str(s).to_owned().ok())
            .unwrap_or(watcher::Id::new());

        self.state
            .accept_websocket_with_tags(&server, &[&claimed_id.to_string()]);

        server.serialize_attachment(claimed_id)?;

        Response::from_websocket(client)
    }

    async fn websocket_message(
        &mut self,
        ws: WebSocket,
        message: WebSocketIncomingMessage,
    ) -> Result<()> {
        self.load_state().await;

        {
            let alarm_message = &mut self.alarm_message;
            let state = &self.state;
            let schedule_message =
                move |message: fuiz::AlarmMessage, duration: web_time::Duration| {
                    let time_in_future = chrono::Utc::now() + duration;

                    *alarm_message = Some(AlarmMessage::Game(message));

                    let storage = state.storage();

                    state.wait_until(async move {
                        let _ = storage
                            .set_alarm(ScheduledTime::new(js_sys::Date::new(&JsValue::from_f64(
                                time_in_future.timestamp_millis() as f64,
                            ))))
                            .await;
                    })
                };

            match message {
                WebSocketIncomingMessage::Binary(_) => {}
                WebSocketIncomingMessage::String(s) => {
                    let LoadingState::Done(Some(game)) = &mut self.game else {
                        return Ok(());
                    };

                    let watcher_id = ws.deserialize_attachment::<watcher::Id>()?;

                    if let Ok(message) = serde_json::from_str(s.as_ref()) {
                        match watcher_id {
                            None => match message {
                                game::IncomingMessage::Ghost(
                                    game::IncomingGhostMessage::ClaimId(id),
                                ) if game.watchers.has_watcher(id) => {
                                    ws.serialize_attachment(id)?;

                                    game.update_session(id, |id| {
                                        self.state
                                            .get_websockets_with_tag(&id.to_string())
                                            .first()
                                            .map(|ws| WebSocketTunnel(ws.to_owned()))
                                    });
                                }
                                game::IncomingMessage::Ghost(_) => {
                                    let new_id = watcher::Id::new();

                                    ws.serialize_attachment(new_id)?;

                                    let session = WebSocketTunnel(ws);

                                    session.send_message(
                                        &game::UpdateMessage::IdAssign(new_id).into(),
                                    );

                                    if game
                                        .add_unassigned(new_id, |id| {
                                            self.state
                                                .get_websockets_with_tag(&id.to_string())
                                                .first()
                                                .map(|ws| WebSocketTunnel(ws.to_owned()))
                                        })
                                        .is_err()
                                    {
                                        session.close();
                                    }
                                }
                                _ => {}
                            },
                            Some(watcher_id) => match message {
                                game::IncomingMessage::Ghost(
                                    game::IncomingGhostMessage::DemandId,
                                ) => {
                                    let session = WebSocketTunnel(ws);

                                    session.send_message(
                                        &game::UpdateMessage::IdAssign(watcher_id).into(),
                                    );

                                    if game
                                        .add_unassigned(watcher_id, |id| {
                                            self.state
                                                .get_websockets_with_tag(&id.to_string())
                                                .first()
                                                .map(|ws| WebSocketTunnel(ws.to_owned()))
                                        })
                                        .is_err()
                                    {
                                        session.close();
                                    }

                                    if let Err(e) = self
                                        .env
                                        .service("COUNTER")?
                                        .fetch("https://example.com/player_count", {
                                            Some(RequestInit {
                                                method: Method::Post,
                                                ..RequestInit::default()
                                            })
                                        })
                                        .await
                                    {
                                        console_error!("Error incrementing player count: {:?}", e);
                                    }
                                }
                                game::IncomingMessage::Ghost(_) => {
                                    let session = WebSocketTunnel(ws);

                                    session.send_message(
                                        &game::UpdateMessage::IdAssign(watcher_id).into(),
                                    );

                                    game.update_session(watcher_id, |id| {
                                        self.state
                                            .get_websockets_with_tag(&id.to_string())
                                            .first()
                                            .map(|ws| WebSocketTunnel(ws.to_owned()))
                                    });
                                }
                                message => {
                                    game.receive_message(
                                        watcher_id,
                                        message,
                                        schedule_message,
                                        |id| {
                                            self.state
                                                .get_websockets_with_tag(&id.to_string())
                                                .first()
                                                .map(|ws| WebSocketTunnel(ws.to_owned()))
                                        },
                                    );
                                }
                            },
                        }
                    }
                }
            }
        }

        if self.state.storage().get_alarm().await.unwrap().is_none() {
            self.alarm_message = Some(AlarmMessage::DeleteGame);
            self.state
                .storage()
                .set_alarm(ScheduledTime::new(js_sys::Date::new(&JsValue::from_f64(
                    (chrono::Utc::now() + GAME_EXPIRY).timestamp_millis() as f64,
                ))))
                .await?;
        }

        if let LoadingState::Done(game) = &self.game {
            store_game(&mut self.state.storage(), game.as_ref().unwrap()).await?;
            self.state
                .storage()
                .put("alarm", &self.alarm_message)
                .await?;
        }

        Ok(())
    }

    async fn websocket_close(
        &mut self,
        ws: WebSocket,
        _code: usize,
        _reason: String,
        _was_clean: bool,
    ) -> Result<()> {
        let LoadingState::Done(Some(game)) = &mut self.game else {
            return Ok(());
        };

        let Some(watcher_id) = ws.deserialize_attachment::<watcher::Id>()? else {
            return Ok(());
        };

        game.watchers.remove_watcher_session(&watcher_id, |id| {
            self.state
                .get_websockets_with_tag(&id.to_string())
                .first()
                .map(|ws| WebSocketTunnel(ws.to_owned()))
        });

        Ok(())
    }
}

async fn fetch_instance(game_manager: Fetcher, game_id: &str) -> Option<GameManagerInstance> {
    let response = game_manager
        .fetch(&format!("https://example.com/{}", game_id), None)
        .await
        .ok()?;

    let game_manager_instance =
        serde_json::from_slice(&response.into_body().collect().await.ok()?.to_bytes()).ok()?;

    Some(game_manager_instance)
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    console_error_panic_hook::set_once();

    let router = Router::new();

    router
        .get("/hello", |_, _| Response::ok("Hello World!"))
        .post_async("/add", |mut req, ctx| async move {
            let game_request = req.json::<GameRequest>().await?;

            if let Err(e) = game_request.validate() {
                return Response::error(e.to_string(), 400);
            }

            let game_namespace = ctx.durable_object("GAME")?;

            let internal_id = game_namespace.unique_id()?;

            let game_manager_instance = GameManagerInstance {
                id: internal_id.to_string(),
                created_at: chrono::Utc::now(),
            };

            let game_manager = ctx.service("GAME_MANAGER")?;

            let arr = js_sys::Array::new();
            arr.push(&JsValue::from_str(&serde_json::to_string(
                &game_manager_instance,
            )?));

            let request = http::Request::builder()
                .method("POST")
                .uri("http://example.com")
                .header("content-type", "application/json")
                .body(Body::new(Blob::new_with_str_sequence(&arr)?.stream()))?;

            let response = game_manager.fetch_request(request).await?;

            let bytes = response.into_body().collect().await?.to_bytes().to_vec();

            let game_id = serde_json::from_slice::<String>(&bytes)
                .map_err(|e| Error::RustError(e.to_string()))?;

            let stub = internal_id.get_stub()?;

            let watcher_id = stub
                .fetch_with_request(Request::new_with_init(
                    "http://fake_url.com/add",
                    &RequestInit {
                        body: Some(JsValue::from_str(
                            &serde_json::to_string(&game_request).expect("serializer failed"),
                        )),
                        headers: {
                            let mut headers = Headers::new();
                            headers.append("content-type", "application/json")?;
                            headers
                        },
                        cf: CfProperties::default(),
                        method: Method::Post,
                        redirect: RequestRedirect::Follow,
                    },
                )?)
                .await?
                .text()
                .await?;

            Response::from_json(&json!({
                "watcher_id": watcher_id,
                "game_id": game_id
            }))
        })
        .get_async("/watch/:gameid/:watcherid", |req, ctx| async move {
            let Some(id) = ctx.param("gameid") else {
                return Response::error("Bad Request", 400);
            };

            let Some(game_instance) = fetch_instance(ctx.service("GAME_MANAGER")?, id).await else {
                return Response::error("Not Found", 404);
            };

            let game_stub = ctx
                .durable_object("GAME")?
                .id_from_string(&game_instance.id)?
                .get_stub()?;

            game_stub.fetch_with_request(req).await
        })
        .get_async("/watch/:gameid/", |req, ctx| async move {
            let Some(id) = ctx.param("gameid") else {
                return Response::error("Bad Request", 400);
            };

            let Some(game_instance) = fetch_instance(ctx.service("GAME_MANAGER")?, id).await else {
                return Response::error("Not Found", 404);
            };

            let namespace = ctx.durable_object("GAME")?;

            let stub = namespace.id_from_string(&game_instance.id)?.get_stub()?;

            stub.fetch_with_request(req).await
        })
        .get_async("/alive/:gameid", |req, ctx| async move {
            let Some(id) = ctx.param("gameid") else {
                return Response::error("Bad Request", 400);
            };

            let Some(game_instance) = fetch_instance(ctx.service("GAME_MANAGER")?, id).await else {
                return Response::ok("false");
            };

            let namespace = ctx.durable_object("GAME")?;

            let stub = namespace.id_from_string(&game_instance.id)?.get_stub()?;

            stub.fetch_with_request(req).await
        })
        .options("/add", |_, _| Response::ok(""))
        .run(req, env)
        .await?
        .with_cors(
            &Cors::default()
                .with_max_age(86400)
                .with_allowed_headers(["*"])
                .with_origins(vec!["https://fuiz.us"])
                .with_methods(vec![Method::Get, Method::Post, Method::Options]),
        )
}
