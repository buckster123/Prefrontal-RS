mod watch;

use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        State,
    },
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use prefrontal_core::{scan_all, Config};
use prefrontal_protocol::{Event, Project};
use tokio::sync::{broadcast, RwLock};
use tower_http::services::ServeDir;
use tracing::{error, info};

pub struct AppState {
    pub cfg: Config,
    pub projects: RwLock<Vec<Project>>,
    pub tx: broadcast::Sender<Event>,
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

    let initial = {
        let cfg = cfg.clone();
        tokio::task::spawn_blocking(move || scan_all(&cfg)).await?
    };
    info!("initial scan: {} projects", initial.len());

    let (tx, _) = broadcast::channel(64);
    let state = Arc::new(AppState { cfg, projects: RwLock::new(initial), tx });

    if let Err(e) = watch::spawn(state.clone()) {
        error!("file watcher failed ({e:#}) — dashboard is static; POST /api/rescan to refresh");
    }

    let app = Router::new()
        .route("/api/projects", get(list_projects))
        .route("/api/rescan", post(rescan))
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

async fn list_projects(State(state): State<Arc<AppState>>) -> Json<Vec<Project>> {
    Json(state.projects.read().await.clone())
}

/// Full rescan on demand — the escape hatch when the watcher can't run
/// (or for a client that wants certainty).
async fn rescan(State(state): State<Arc<AppState>>) -> Json<Vec<Project>> {
    let cfg = state.cfg.clone();
    let fresh = tokio::task::spawn_blocking(move || scan_all(&cfg))
        .await
        .unwrap_or_default();
    *state.projects.write().await = fresh.clone();
    let _ = state.tx.send(Event::Snapshot { projects: fresh.clone() });
    Json(fresh)
}

async fn ws_upgrade(
    ws: WebSocketUpgrade,
    State(state): State<Arc<AppState>>,
) -> impl IntoResponse {
    ws.on_upgrade(move |socket| ws_session(socket, state))
}

async fn ws_session(mut socket: WebSocket, state: Arc<AppState>) {
    // Subscribe before snapshotting so no delta can fall between the two.
    let mut rx = state.tx.subscribe();
    let snapshot = Event::Snapshot { projects: state.projects.read().await.clone() };
    if send_event(&mut socket, &snapshot).await.is_err() {
        return;
    }
    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Ok(ev) => {
                    if send_event(&mut socket, &ev).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    let snap = Event::Snapshot { projects: state.projects.read().await.clone() };
                    if send_event(&mut socket, &snap).await.is_err() {
                        break;
                    }
                }
                Err(broadcast::error::RecvError::Closed) => break,
            },
            msg = socket.recv() => match msg {
                Some(Ok(_)) => {} // clients only listen; drain to notice a close
                _ => break,
            },
        }
    }
}

async fn send_event(socket: &mut WebSocket, ev: &Event) -> Result<(), axum::Error> {
    match serde_json::to_string(ev) {
        Ok(json) => socket.send(Message::Text(json.into())).await,
        Err(_) => Ok(()),
    }
}
