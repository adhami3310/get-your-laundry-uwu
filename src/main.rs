mod machines;

use std::{sync::Arc, time::Instant};

use axum::{
    extract::State,
    routing::{get, post},
    Json, Router,
};
use itertools::Itertools;
use machines::{EmailRequest, Machines, Thresholds};
use serde::{Deserialize, Serialize};
use tower_http::services::ServeDir;

#[derive(Serialize, Clone)]
struct AppState {
    #[serde(serialize_with = "crate::serialize_instant_to_duration_since")]
    #[serde(rename = "uptime")]
    server_started: Instant,
    washers: Arc<Machines<3>>,
    dryers: Arc<Machines<4>>,
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let state = AppState {
        server_started: Instant::now(),
        washers: Machines::new(
            "Washer",
            "/dev/ttyUSB1",
            9600,
            [
                Thresholds::with_status(machines::PowerStatus::Broken),
                Thresholds::default(),
                Thresholds::default(),
            ],
        )
        .into(),
        dryers: Machines::new(
            "Dryer",
            "/dev/ttyUSB0",
            9600,
            [
                Thresholds::default(),
                Thresholds::default(),
                Thresholds::default(),
                Thresholds::default(),
            ],
        )
        .into(),
    };

    // build our application with a single route
    let app = Router::new()
        .route("/watch", get(handler))
        .route("/notify", post(email))
        .nest_service("/", ServeDir::new("public"))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:3000")
        .await
        .unwrap();
    axum::serve(listener, app).await.unwrap();
}

async fn handler(State(state): State<AppState>) -> Json<AppState> {
    Json(state)
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
enum MachineType {
    Washer,
    Dryer,
}

#[derive(Deserialize)]
struct MachineRequest {
    pub index: usize,
    pub machine: MachineType,
}

#[derive(Deserialize)]
struct NotifyEmail {
    pub kerb: String,
    pub machines: Vec<MachineRequest>,
}

async fn email(State(state): State<AppState>, Json(payload): Json<NotifyEmail>) {
    let NotifyEmail { kerb, machines } = payload;

    let (dryers, washers): (Vec<_>, Vec<_>) =
        machines.into_iter().partition_map(|m| match m.machine {
            MachineType::Dryer => itertools::Either::Left(m.index),
            MachineType::Washer => itertools::Either::Right(m.index),
        });

    state.dryers.add_request(EmailRequest {
        kerb: kerb.clone(),
        indicies: dryers,
    });
    state.washers.add_request(EmailRequest {
        kerb,
        indicies: washers,
    });
}

pub fn serialize_instant_to_duration_since<S>(
    last_updated: &Instant,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
{
    serde_millis::serialize(&Instant::now().duration_since(*last_updated), serializer)
}
