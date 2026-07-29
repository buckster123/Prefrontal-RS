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
use prefrontal_core::cortex::CortexClient;
use prefrontal_core::search::SearchIndex;
use prefrontal_core::{scan_all, Config};
use prefrontal_protocol::{
    CortexHit, DocContent, DocEntry, DocWrite, DocWriteResult, Event, Project, SearchHit,
};
use tokio::sync::{broadcast, RwLock};
use tower_http::services::ServeDir;
use tracing::{error, info, warn};

pub struct AppState {
    pub cfg: Config,
    pub projects: RwLock<Vec<Project>>,
    pub tx: broadcast::Sender<Event>,
    /// None when the index couldn't be opened — search degrades, nothing else does.
    pub search: Option<Arc<SearchIndex>>,
    /// Some(...) only when features.cerebro is on; inner None until first use
    /// (the client spawns lazily and respawns after any error).
    pub cortex: Option<Arc<std::sync::Mutex<Option<CortexClient>>>>,
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

    let search = match prefrontal_core::search::default_index_dir() {
        Some(dir) => match prefrontal_core::search::open(&dir) {
            Ok(s) => Some(Arc::new(s)),
            Err(e) => {
                warn!("search index unavailable ({e:#}) — /api/search disabled");
                None
            }
        },
        None => None,
    };

    let cortex = if cfg.features.cerebro && !cfg.cortex.command.is_empty() {
        info!("cortex layer enabled — {}", cfg.cortex.command);
        Some(Arc::new(std::sync::Mutex::new(None)))
    } else {
        None
    };

    let (tx, _) = broadcast::channel(64);
    let state = Arc::new(AppState { cfg, projects: RwLock::new(initial), tx, search, cortex });

    if let Err(e) = watch::spawn(state.clone()) {
        error!("file watcher failed ({e:#}) — dashboard is static; POST /api/rescan to refresh");
    }
    tokio::spawn(build_index(state.clone()));

    let app = Router::new()
        .route("/api/projects", get(list_projects))
        .route("/api/rescan", post(rescan))
        .route("/api/search", get(search_handler))
        .route("/api/cortex", get(cortex_recall))
        .route("/api/cortex/sync", post(cortex_sync))
        .route("/api/docs/{project}", get(list_docs))
        .route("/api/doc/{project}/{*path}", get(read_doc).put(write_doc))
        .route("/raw/{project}/{*path}", get(raw_asset))
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

/// Full index build, once, in the background — the watcher keeps it fresh after.
async fn build_index(state: Arc<AppState>) {
    let Some(search) = state.search.clone() else { return };
    let projects: Vec<(String, String)> = state
        .projects
        .read()
        .await
        .iter()
        .map(|p| (p.name.clone(), p.path.clone()))
        .collect();
    let total = projects.len();
    let started = std::time::Instant::now();
    let docs = tokio::task::spawn_blocking(move || {
        let mut docs = 0usize;
        for (name, path) in projects {
            match search.reindex_project(&name, std::path::Path::new(&path)) {
                Ok(n) => docs += n,
                Err(e) => warn!("indexing {name} failed: {e:#}"),
            }
        }
        docs
    })
    .await
    .unwrap_or(0);
    info!(
        "search index ready — {docs} documents across {total} projects in {:.1}s",
        started.elapsed().as_secs_f32()
    );
}

#[derive(serde::Deserialize)]
struct SearchParams {
    q: String,
    #[serde(default = "default_limit")]
    limit: usize,
}
fn default_limit() -> usize {
    30
}

async fn search_handler(
    axum::extract::Query(params): axum::extract::Query<SearchParams>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<SearchHit>>, ApiError> {
    let Some(search) = state.search.clone() else {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "search index unavailable".into()));
    };
    let dirs: std::collections::HashMap<String, std::path::PathBuf> = state
        .projects
        .read()
        .await
        .iter()
        .map(|p| (p.name.clone(), std::path::PathBuf::from(&p.path)))
        .collect();
    let limit = params.limit.min(100);
    let q = params.q.clone();
    let hits = tokio::task::spawn_blocking(move || {
        prefrontal_core::search::search(&search.index, search.fields, &q, limit, &dirs)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, format!("{e:#}")))?;
    Ok(Json(hits))
}

/// Run a closure against the lazily-spawned cortex client; any error drops
/// the client so the next call respawns fresh.
async fn with_cortex<T, F>(state: &Arc<AppState>, f: F) -> Result<T, ApiError>
where
    T: Send + 'static,
    F: FnOnce(&mut CortexClient) -> anyhow::Result<T> + Send + 'static,
{
    let Some(slot) = state.cortex.clone() else {
        return Err((StatusCode::SERVICE_UNAVAILABLE, "cortex layer disabled".into()));
    };
    let cortex_cfg = state.cfg.cortex.clone();
    tokio::task::spawn_blocking(move || {
        let mut guard = slot.lock().expect("cortex slot poisoned");
        if guard.is_none() {
            *guard = Some(
                CortexClient::spawn(&cortex_cfg)
                    .map_err(|e| (StatusCode::BAD_GATEWAY, format!("{e:#}")))?,
            );
        }
        let client = guard.as_mut().expect("just spawned");
        f(client).map_err(|e| {
            *guard = None; // poisoned pipe or protocol drift — respawn next time
            (StatusCode::BAD_GATEWAY, format!("{e:#}"))
        })
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
}

#[derive(serde::Deserialize)]
struct CortexParams {
    q: String,
}

async fn cortex_recall(
    axum::extract::Query(params): axum::extract::Query<CortexParams>,
    State(state): State<Arc<AppState>>,
) -> Result<Json<Vec<CortexHit>>, ApiError> {
    let top_k = state.cfg.cortex.top_k;
    let q = params.q.clone();
    let hits = with_cortex(&state, move |c| c.recall(&q, top_k)).await?;
    Ok(Json(hits))
}

async fn cortex_sync(
    State(state): State<Arc<AppState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let projects = state.projects.read().await.clone();
    let (created, updated) = with_cortex(&state, move |c| {
        let mut created = 0u32;
        let mut updated = 0u32;
        for p in &projects {
            match c.sync_project(p)? {
                true => created += 1,
                false => updated += 1,
            }
        }
        Ok((created, updated))
    })
    .await?;
    info!("cortex sync: {created} created, {updated} updated");
    Ok(Json(serde_json::json!({ "created": created, "updated": updated })))
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
    // Raw HTML passes through comrak, then ammonia strips anything active
    // (scripts, handlers, iframes) while keeping the img/div/table furniture
    // READMEs actually use for banners. The dashboard origin can write files,
    // so cloned-from-anywhere docs must never execute in it.
    opts.render.r#unsafe = true;
    ammonia::clean(&comrak::markdown_to_html(raw, &opts))
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

/// Read-only project images so docs can show their banners/screenshots.
async fn raw_asset(
    Path((project, path)): Path<(String, String)>,
    State(state): State<Arc<AppState>>,
) -> Result<impl IntoResponse, ApiError> {
    let dir = project_dir(&state, &project).await?;
    let rel = path.clone();
    let bytes = tokio::task::spawn_blocking(move || -> anyhow::Result<Vec<u8>> {
        let p = prefrontal_core::docs::resolve_asset_path(&dir, &rel)?;
        Ok(std::fs::read(&p)?)
    })
    .await
    .map_err(|e| (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()))?
    .map_err(|e| (StatusCode::BAD_REQUEST, format!("{e:#}")))?;

    let mime = match path.rsplit('.').next().map(|e| e.to_lowercase()).as_deref() {
        Some("png") => "image/png",
        Some("jpg") | Some("jpeg") => "image/jpeg",
        Some("gif") => "image/gif",
        Some("svg") => "image/svg+xml",
        Some("webp") => "image/webp",
        Some("ico") => "image/x-icon",
        Some("bmp") => "image/bmp",
        Some("avif") => "image/avif",
        _ => "application/octet-stream",
    };
    Ok((
        [
            (axum::http::header::CONTENT_TYPE, mime),
            (axum::http::header::X_CONTENT_TYPE_OPTIONS, "nosniff"),
        ],
        bytes,
    ))
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
