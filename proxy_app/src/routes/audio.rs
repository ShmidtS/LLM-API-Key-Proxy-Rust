use axum::{Router, response::Json, routing::post};
use serde_json::{Value, json};

use crate::state::AppState;

pub fn router() -> Router<AppState> {
    Router::new()
        .route("/v1/audio/speech", post(speech))
        .route("/v1/audio/transcriptions", post(transcriptions))
}

async fn speech() -> Json<Value> {
    Json(json!({"object": "audio.speech", "data": {}}))
}

async fn transcriptions() -> Json<Value> {
    Json(json!({"object": "audio.transcription", "text": ""}))
}
