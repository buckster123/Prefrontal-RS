mod watch;

use std::sync::Arc;

use anyhow::{Context, Result};
use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path, State,
    },
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use prefrontal_core::{scan_all, Config};
use prefrontal_protocol::{DocContent, DocEntry, DocWrite, DocWriteResult, Event, Project};
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
        .route("/api/docs/{project}", get(list_docs))
        .route("/api/doc/{project}/{*path}", get(read_doc).put(write_doc))
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

type ApiError = (StatusCode, String);

/// Projects are addressed by name; the daemon resolves to a path only through
/// its own scan cache — clients never send filesystem paths for projects.
async fn project_dir(state: &Arc<AppState>, name: &str) -> Result<std::path::PathBuf, ApiError> {
    state
        .projects
        .read()
        .await
        .iter()
        .find(|p| p.name == name)
        .map(|p| std::path::PathBuf::from(&p.path))
        .ok_or_else(|| (StatusCode::NOT_FOUND, format!("unknown project: {name}")))
}

fn render_markdown(raw: &str) -> String {
    let mut opts = comrak::Options::default();
    opts.extension.table = true;
    opts.extension.strikethrough = true;
    opts.extension.tasklist = true;
    opts.extension.autolink = true;
    // render.unsafe_ stays false: raw HTML in docs is escaped, not executed
    comrak::markdown_to_html(raw, &opts)
}

async fn list_docs(
    Path(project): Path<String>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<DocEntry>>, ApiError> {
    let dir = project_dir(&state, &project).await?;
    let docs = tokio::task::spawn_blocking(move || prefrontal_core::list_docs(&dir))
        .await
        .unwrap_or_default();
    Ok(Json(docs))
}

async fn read_doc(
    Path((project, path)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<DocContent>, ApiError> {
    let dir = project_dir(&state, &project).await?;
    let rel = path.clone();
    let (raw, modified_unix) =
        tokio::task::spawn_blocking(move || prefrontal_core::read_doc(&dir, &rel))
            .await
            .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    let html = render_markdown(&raw);
    Ok(Json(DocContent { project, path, raw, html, modified_unix }))
}

async fn write_doc(
    Path((project, path)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
    Json(body): Json<DocWrite>,
) -> Result<Json<DocWriteResult>, ApiError> {
    let dir = project_dir(&state, &project).await?;
    let result = tokio::task::spawn_blocking(move || {
        prefrontal_core::write_doc(&dir, &path, &body.content)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;
    // no manual cache poke: the watcher sees the write (and the commit) and
    // pushes the ProjectChanged delta itself
    Ok(Json(result))
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
