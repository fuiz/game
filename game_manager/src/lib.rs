use fuiz::game_id::GameId;
use serde::{Deserialize, Serialize};
use worker::*;

const GAME_EXPIRY: chrono::Duration = chrono::Duration::hours(24);

#[durable_object]
struct GameManager {
    state: State,
    env: Env,
}

impl GameManager {
    async fn get_game(&mut self, game_id: &str) -> Result<GameManagerInstance> {
        let game = self
            .state
            .storage()
            .get::<GameManagerInstance>(game_id)
            .await?;

        if game.created_at + GAME_EXPIRY < chrono::Utc::now() {
            self.state.storage().delete(game_id).await?;
            return Err(Error::RustError("Game not found".to_string()));
        }

        Ok(game)
    }
}

#[durable_object]
impl DurableObject for GameManager {
    fn new(state: State, env: Env) -> Self {
        Self { state, env }
    }

    async fn fetch(&mut self, mut req: Request) -> Result<Response> {
        match req.method() {
            Method::Get => {
                let url = req.url()?;

                let game_id = url.path_segments().and_then(|ps| ps.last());

                if let Some(game_id) = game_id {
                    let game = self.get_game(game_id).await;

                    if let Ok(game) = game {
                        Response::from_json(&game)
                    } else {
                        Response::error("Not Found", 404)
                    }
                } else {
                    Response::error("Bad Request", 400)
                }
            }
            Method::Post => {
                let game_instance = req.json::<GameManagerInstance>().await?;

                loop {
                    let random_game_id = GameId::new();

                    let game = self.get_game(&random_game_id.to_string()).await;

                    if game.is_err() {
                        self.state
                            .storage()
                            .put(&random_game_id.to_string(), &game_instance)
                            .await?;

                        if let Err(e) = self
                            .env
                            .service("COUNTER")?
                            .fetch("https://example.com/game_count", {
                                let mut request_init = RequestInit::default();
                                request_init.method = Method::Post;
                                Some(request_init)
                            })
                            .await
                        {
                            console_error!("Failed to increment game count: {:?}", e);
                        }

                        break Response::from_json(&random_game_id);
                    } else {
                        continue;
                    }
                }
            }
            _ => Response::error("Method Not Allowed", 405),
        }
    }
}

#[derive(Serialize, Deserialize)]
pub struct GameManagerInstance {
    pub id: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    console_error_panic_hook::set_once();

    let game_manager = env
        .durable_object("GAME_MANAGER")?
        .id_from_name("default")?
        .get_stub()?;

    game_manager.fetch_with_request(req).await
}
