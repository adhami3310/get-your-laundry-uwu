mod machines;

use std::sync::Arc;

use axum::{extract::State, routing::get, Json, Router};
use machines::Machines;
use serde::Serialize;

#[derive(Serialize, Clone)]
struct AppState {
    washers: Arc<Machines<3>>,
    dryers: Arc<Machines<4>>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let state = AppState {
        washers: Machines::new(
            "/dev/ttyUSB1",
            9600,
            [Default::default(), Default::default(), Default::default()],
        )
        .into(),
        dryers: Machines::new(
            "/dev/ttyUSB0",
            9600,
            [
                Default::default(),
                Default::default(),
                Default::default(),
                Default::default(),
            ],
        )
        .into(),
    };

    // build our application with a single route
    let app = Router::new()
        .route("/watch", get(handler))
        .with_state(state);

    // run our app with hyper, listening globally on port 3000
    let listener = tokio::net::TcpListener::bind("0.0.0.0:3000").await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn handler(State(state): State<AppState>) -> Json<AppState> {
    Json(state)
}
