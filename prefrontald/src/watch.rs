//! Filesystem watcher → debounced per-project rescans → WS deltas.
//!
//! Watches are added per-directory (never blanket-recursive) so build output
//! can be skipped — `target/` alone would eat thousands of inotify watches
//! and drown the debouncer during a compile. `.git` gets targeted watches
//! (the dir itself + `refs/`) so commits, staging, and branch switches
//! register without the object-store noise.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::Result;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use prefrontal_core::{is_ignored, scan_project, SKIP_DIRS};
use prefrontal_protocol::Event as WireEvent;
use tokio::sync::mpsc;
use tracing::{debug, info};

use crate::AppState;

/// Rescan a project this long after its *last* event — a git checkout or a
/// save-storm collapses into one rescan.
const QUIET: Duration = Duration::from_millis(600);
const MAX_DEPTH: u32 = 8;

struct Raw {
    path: PathBuf,
    created_dir: bool,
}

pub fn spawn(state: Arc<AppState>) -> Result<()> {
    let (tx, rx) = mpsc::unbounded_channel::<Raw>();
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res {
            let created = matches!(ev.kind, notify::EventKind::Create(_));
            for path in ev.paths {
                let created_dir = created && path.is_dir();
                let _ = tx.send(Raw { path, created_dir });
            }
        }
    })?;

    let roots = state.cfg.root_paths();
    let mut watches = 0usize;
    for root in &roots {
        watcher.watch(root, RecursiveMode::NonRecursive)?;
        watches += 1;
        let Ok(entries) = std::fs::read_dir(root) else { continue };
        for entry in entries.filter_map(|e| e.ok()) {
            let name = entry.file_name().to_string_lossy().to_string();
            if !entry.path().is_dir() || is_ignored(&name, &state.cfg) {
                continue;
            }
            watches += add_watches(&mut watcher, &entry.path(), 0);
        }
    }
    info!("watching {watches} directories under {} root(s)", roots.len());

    tokio::spawn(debounce_loop(state, rx, watcher, roots));
    Ok(())
}

fn add_watches(w: &mut RecommendedWatcher, dir: &Path, depth: u32) -> usize {
    if depth > MAX_DEPTH {
        return 0;
    }
    let mut n = 0;
    if w.watch(dir, RecursiveMode::NonRecursive).is_ok() {
        n += 1;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return n };
    for entry in entries.filter_map(|e| e.ok()) {
        let Ok(ft) = entry.file_type() else { continue };
        if !ft.is_dir() || ft.is_symlink() {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if name == ".git" {
            if w.watch(&path, RecursiveMode::NonRecursive).is_ok() {
                n += 1;
            }
            let refs = path.join("refs");
            if refs.is_dir() && w.watch(&refs, RecursiveMode::Recursive).is_ok() {
                n += 1;
            }
            continue;
        }
        if name.starts_with('.') || SKIP_DIRS.contains(&name.as_str()) {
            continue;
        }
        n += add_watches(w, &path, depth + 1);
    }
    n
}

/// Map an event path to the project directory (root's immediate child) it lives in.
fn project_of(path: &Path, roots: &[PathBuf]) -> Option<PathBuf> {
    for root in roots {
        if let Ok(rest) = path.strip_prefix(root) {
            let first = rest.components().next()?;
            return Some(root.join(first.as_os_str()));
        }
    }
    None
}

async fn debounce_loop(
    state: Arc<AppState>,
    mut rx: mpsc::UnboundedReceiver<Raw>,
    mut watcher: RecommendedWatcher,
    roots: Vec<PathBuf>,
) {
    let mut pending: HashMap<PathBuf, Instant> = HashMap::new();
    let mut tick = tokio::time::interval(Duration::from_millis(250));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        tokio::select! {
            raw = rx.recv() => {
                let Some(raw) = raw else { break };
                if raw.created_dir {
                    // brand-new project or fresh subtree — cover it going forward
                    let name = raw.path.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    if !name.starts_with('.') && !SKIP_DIRS.contains(&name.as_str()) {
                        add_watches(&mut watcher, &raw.path, 0);
                    }
                }
                if let Some(proj) = project_of(&raw.path, &roots) {
                    let name = proj.file_name()
                        .map(|n| n.to_string_lossy().to_string())
                        .unwrap_or_default();
                    if !is_ignored(&name, &state.cfg) {
                        pending.insert(proj, Instant::now());
                    }
                }
            }
            _ = tick.tick() => {
                let due: Vec<PathBuf> = pending
                    .iter()
                    .filter(|(_, seen)| seen.elapsed() >= QUIET)
                    .map(|(p, _)| p.clone())
                    .collect();
                for dir in due {
                    pending.remove(&dir);
                    rescan_one(&state, dir).await;
                }
            }
        }
    }
}

async fn rescan_one(state: &Arc<AppState>, dir: PathBuf) {
    let Some(name) = dir.file_name().map(|n| n.to_string_lossy().to_string()) else {
        return;
    };
    let cfg = state.cfg.clone();
    let d = dir.clone();
    let scanned = tokio::task::spawn_blocking(move || {
        d.is_dir().then(|| scan_project(&d, name, &cfg))
    })
    .await
    .ok()
    .flatten();

    match scanned {
        Some(project) => {
            let mut projects = state.projects.write().await;
            match projects.iter_mut().find(|p| p.path == project.path) {
                Some(slot) => {
                    if *slot == project {
                        return; // touched, but nothing the dashboard shows changed
                    }
                    *slot = project.clone();
                }
                None => projects.push(project.clone()),
            }
            projects.sort_by_key(|p| std::cmp::Reverse(p.last_touched_unix));
            drop(projects);
            debug!("project changed: {}", project.name);
            if let Some(search) = state.search.clone() {
                let name = project.name.clone();
                let pdir = dir.clone();
                tokio::task::spawn_blocking(move || {
                    if let Err(e) = search.reindex_project(&name, &pdir) {
                        debug!("reindex {name} failed: {e:#}");
                    }
                });
            }
            let _ = state.tx.send(WireEvent::ProjectChanged { project: Box::new(project) });
        }
        None => {
            let path = dir.to_string_lossy().to_string();
            let mut projects = state.projects.write().await;
            let before = projects.len();
            projects.retain(|p| p.path != path);
            if projects.len() != before {
                drop(projects);
                info!("project removed: {path}");
                if let Some(search) = state.search.clone() {
                    if let Some(name) = dir.file_name().map(|n| n.to_string_lossy().to_string()) {
                        tokio::task::spawn_blocking(move || search.remove_project(&name).ok());
                    }
                }
                let _ = state.tx.send(WireEvent::ProjectRemoved { path });
            }
        }
    }
}
