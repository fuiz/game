mod clashmap;
mod game_manager;

use actix_web::{
    get,
    middleware::Logger,
    post,
    web::{self, Data},
    App, HttpRequest, HttpResponse, HttpServer, Responder,
};
use fuiz::game::{IncomingGhostMessage, Options};
use fuiz::{
    fuiz::config::Fuiz,
    game::{IncomingMessage, UpdateMessage},
    game_id::GameId,
    session::Tunnel,
    watcher::Id,
};
use futures_util::StreamExt;
use game_manager::GameManager;
use garde::Validate;
use serde_json::json;
use std::{
    env,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, OnceLock,
    },
    time::Duration,
};

use crate::game_manager::GameVanish;

extern crate pretty_env_logger;
#[macro_use]
extern crate log;

#[derive(Clone)]
pub struct Session {
    session: actix_ws::Session,
}

impl Session {
    pub fn new(session: actix_ws::Session) -> Self {
        Self { session }
    }
}

type MessageScheduler = Box<dyn Fn(fuiz::AlarmMessage, Duration)>;

impl fuiz::session::Tunnel for Session {
    fn send_message(&self, message: &fuiz::UpdateMessage) {
        let mut session = self.session.clone();

        let message = message.to_message();

        actix_web::rt::spawn(async move {
            let _ = session.text(message).await;
        });
    }

    fn send_state(&self, state: &fuiz::SyncMessage) {
        let mut session = self.session.clone();

        let message = state.to_message();

        actix_web::rt::spawn(async move {
            let _ = session.text(message).await;
        });
    }

    fn close(self) {
        actix_web::rt::spawn(async move {
            let _ = self.session.close(None).await;
        });
    }
}

struct AppState {
    game_manager: GameManager,
}

#[derive(serde::Deserialize, Validate)]
struct GameRequest {
    #[garde(dive)]
    config: Fuiz,
    #[garde(dive)]
    options: Options,
}

#[post("/add")]
async fn add(
    data: Data<AppState>,
    request: garde_actix_web::web::Json<GameRequest>,
) -> impl Responder {
    let GameRequest { config, options } = request.into_inner();

    let host_id = Id::new();
    let game_id = data.game_manager.add_game(config, options, host_id);

    let stale_data = data;

    // Stale Detection
    actix_web::rt::spawn(async move {
        loop {
            actix_web::rt::time::sleep(Duration::from_secs(60)).await;
            match stale_data.game_manager.alive_check(game_id) {
                Ok(true) => continue,
                Ok(false) => {
                    info!("clearing, {}", game_id);
                    stale_data.game_manager.remove_game(game_id);
                }
                _ => break,
            }
        }
    });

    Ok::<_, GameVanish>(web::Json(json!({
        "game_id": game_id,
        "watcher_id": host_id
    })))
}

#[get("/alive/{game_id}")]
async fn alive(data: web::Data<AppState>, game_id: web::Path<GameId>) -> impl Responder {
    data.game_manager
        .exists(game_id.into_inner())
        .is_ok()
        .to_string()
}

#[get("/count")]
async fn count(data: web::Data<AppState>) -> impl Responder {
    let (current, since_restart) = data.game_manager.count();
    web::Json(json!({
        "current": current,
        "since_restart": since_restart
    }))
}

fn websocket_heartbeat_verifier(mut session: actix_ws::Session) -> impl Fn(bytes::Bytes) -> bool {
    let latest_value = Arc::new(AtomicU64::new(0));

    let sender_latest_value = latest_value.clone();
    actix_web::rt::spawn(async move {
        loop {
            actix_web::rt::time::sleep(Duration::from_secs(5)).await;
            let new_value = fastrand::u64(0..u64::MAX);
            sender_latest_value.store(new_value, Ordering::SeqCst);
            if session.ping(&new_value.to_ne_bytes()).await.is_err() {
                break;
            }
        }
    });

    move |bytes: bytes::Bytes| {
        let last_value = latest_value.load(Ordering::SeqCst);
        if let Ok(actual_bytes) = bytes.into_iter().collect::<Vec<_>>().try_into() {
            if u64::from_ne_bytes(actual_bytes) == last_value {
                return false;
            }
        }
        true
    }
}

#[get("/watch/{game_id}/{watcher_id}")]
async fn watch(
    data: web::Data<AppState>,
    req: HttpRequest,
    body: web::Payload,
    params: web::Path<(GameId, String)>,
) -> Result<HttpResponse, actix_web::Error> {
    let (response, mut session, mut msg_stream) = actix_ws::handle(&req, body)?;

    let (game_id, _) = *params;

    data.game_manager.exists(game_id)?;

    let own_session = Session::new(session.clone());

    let mismatch = websocket_heartbeat_verifier(session.clone());

    let data_thread = data.clone();

    actix_web::rt::spawn(async move {
        let schedule_thread = data_thread.clone();

        let schedule_message: Arc<OnceLock<MessageScheduler>> = Default::default();

        let thread_schedule_message = schedule_message.clone();

        let temp_schedule_message = move |alarm_message: fuiz::AlarmMessage, duration: Duration| {
            let schedule_thread = schedule_thread.clone();
            let schedule_message = thread_schedule_message.clone();
            actix_web::rt::spawn(async move {
                actix_web::rt::time::sleep(duration).await;
                let _ = schedule_thread.game_manager.receive_alarm(
                    game_id,
                    alarm_message,
                    |alarm, duration| {
                        schedule_message.get().expect("schedule is unintialized")(alarm, duration)
                    },
                );
            });
        };

        schedule_message
            .as_ref()
            .get_or_init(|| Box::new(temp_schedule_message));

        let mut watcher_id = None;
        while let Some(Ok(msg)) = msg_stream.next().await {
            if data.game_manager.exists(game_id).is_err() {
                break;
            }

            match msg {
                actix_ws::Message::Pong(bytes) => {
                    if mismatch(bytes) {
                        break;
                    }
                }
                actix_ws::Message::Ping(bytes) => {
                    if session.pong(&bytes).await.is_err() {
                        break;
                    }
                }
                actix_ws::Message::Text(s) => {
                    if let Ok(message) = serde_json::from_str(s.as_ref()) {
                        match watcher_id {
                            None => match message {
                                IncomingMessage::Ghost(IncomingGhostMessage::ClaimId(id))
                                    if matches!(
                                        data_thread.game_manager.watcher_exists(game_id, id),
                                        Ok(true)
                                    ) =>
                                {
                                    data_thread.game_manager.set_tunnel(id, own_session.clone());

                                    if data_thread
                                        .game_manager
                                        .update_session(game_id, id)
                                        .is_err()
                                    {
                                        break;
                                    }

                                    watcher_id = Some(id);
                                }
                                IncomingMessage::Ghost(_) => {
                                    let new_id = Id::new();
                                    watcher_id = Some(new_id);

                                    own_session
                                        .send_message(&UpdateMessage::IdAssign(new_id).into());

                                    data_thread
                                        .game_manager
                                        .set_tunnel(new_id, own_session.clone());

                                    match data_thread.game_manager.add_unassigned(game_id, new_id) {
                                        Err(_) | Ok(Err(_)) => {
                                            own_session.clone().close();
                                        }
                                        _ => {}
                                    }
                                }
                                _ => {}
                            },
                            Some(watcher_id) => match message {
                                IncomingMessage::Ghost(_) => {}
                                message => {
                                    let data_thread = data_thread.clone();
                                    let schedule_message = schedule_message.clone();
                                    actix_web::rt::spawn(async move {
                                        let _ = data_thread.game_manager.receive_message(
                                            game_id,
                                            watcher_id,
                                            message,
                                            |alarm, duration| {
                                                schedule_message
                                                    .get()
                                                    .expect("schedule is unintialized")(
                                                    alarm, duration,
                                                )
                                            },
                                        );
                                    });
                                }
                            },
                        }
                    }
                }
                _ => break,
            }
        }

        if let Some(watcher_id) = watcher_id {
            data.game_manager.remove_tunnel(watcher_id);

            let _ = data.game_manager.remove_watcher_session(watcher_id);
        }
        session.close(None).await.ok();
    });

    Ok(response)
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    pretty_env_logger::init();

    let app_state = web::Data::new(AppState {
        game_manager: GameManager::default(),
    });

    HttpServer::new(move || {
        let app = App::new()
            .wrap(Logger::default())
            .app_data(app_state.clone())
            .route("/hello", web::get().to(|| async { "Hello World!" }))
            .service(alive)
            .service(add)
            .service(count)
            .service(watch);

        if cfg!(feature = "expose-network") {
            let cors = actix_cors::Cors::default()
                .allowed_origin(&env::var("NETWORK_ORIGIN").unwrap_or_else(|_| "*".to_string()))
                .allowed_methods(vec!["GET", "POST"])
                .allowed_headers(vec![
                    actix_web::http::header::AUTHORIZATION,
                    actix_web::http::header::ACCEPT,
                ])
                .allowed_header(actix_web::http::header::CONTENT_TYPE);
            app.wrap(cors)
        } else {
            let cors = actix_cors::Cors::permissive();
            app.wrap(cors)
        }
    })
    .bind((
        if cfg!(feature = "expose-network") {
            "127.0.0.1"
        } else {
            "0.0.0.0"
        },
        env::var("PORT")
            .ok()
            .and_then(|port| port.parse::<u16>().ok())
            .unwrap_or(8080),
    ))?
    .run()
    .await
}
