use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
    routing::get,
    Json, Router,
};
use prefrontal_core::{scan_all, Config};
use prefrontal_protocol::{Event, Project};
use tower_http::services::ServeDir;
use tracing::info;

struct AppState {
    cfg: Config,
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "prefrontald=info,tower_http=warn".into()),
        )
        .init();

    let cfg = Config::load()?;
    let bind = cfg.server.bind.clone();
    let ui_dir = cfg.server.ui_dir.clone();
    let state = Arc::new(AppState { cfg });

    let app = Router::new()
        .route("/api/projects", get(list_projects))
        .route("/ws", get(ws_upgrade))
        .fallback_service(ServeDir::new(&ui_dir))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .with_context(|| format!("binding {bind}"))?;
    info!("prefrontald up — http://{bind} (ui from {ui_dir}/)");
    axum::serve(listener, app).await?;
    Ok(())
}

/// Scan-per-request for now; the phase-1 watcher replaces this with a warm
/// cache + delta pushes over /ws.
async fn scan_blocking(state: &Arc<AppState>) -> Vec<Project> {
    let cfg = state.cfg.clone();
    tokio::task::spawn_blocking(move || scan_all(&cfg))
        .await
        .unwrap_or_default()
}

async fn list_projects(State(state): State<Arc<AppState>>) -> Json<Vec<Project>> {
    Json(scan_blocking(&state).await)
}

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_session(socket, state))
}

async fn ws_session(mut socket: WebSocket, state: Arc<AppState>) {
    let snapshot = Event::Snapshot { projects: scan_blocking(&state).await };
    if let Ok(json) = serde_json::to_string(&snapshot) {
        if socket.send(Message::Text(json.into())).await.is_err() {
            return;
        }
    }
    // Hold the connection; ProjectChanged deltas land here once the watcher exists.
    while let Some(Ok(_)) = socket.recv().await {}
}
