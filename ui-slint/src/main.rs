//! Native Prefrontal frontend: the same WS/REST surface ui-web speaks,
//! rendered by Slint. Reading only — editing stays web-side (charter D4).

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::Result;
use futures_util::StreamExt;
use prefrontal_protocol::{Event, Project};
use slint::{ComponentHandle, ModelRc, VecModel, Weak};

slint::include_modules!();

const BASE: &str = "http://127.0.0.1:7320";
const WS: &str = "ws://127.0.0.1:7320/ws";

#[derive(Default)]
struct Shared {
    projects: Vec<Project>,
    filter: String,
}

fn main() -> Result<()> {
    let ui = MainWindow::new()?;
    let shared = Arc::new(Mutex::new(Shared::default()));
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;

    rt.spawn(ws_loop(ui.as_weak(), shared.clone()));

    ui.on_filter_edited({
        let shared = shared.clone();
        let weak = ui.as_weak();
        move |text| {
            shared.lock().expect("shared").filter = text.to_string();
            refresh(&weak, &shared);
        }
    });

    ui.on_project_clicked({
        let weak = ui.as_weak();
        let handle = rt.handle().clone();
        move |name| {
            let name = name.to_string();
            let weak = weak.clone();
            handle.spawn(open_project(weak, name));
        }
    });

    ui.on_doc_clicked({
        let weak = ui.as_weak();
        let handle = rt.handle().clone();
        move |path| {
            let weak2 = weak.clone();
            let path = path.to_string();
            let project = weak
                .upgrade()
                .map(|ui| ui.get_open_project().to_string())
                .unwrap_or_default();
            handle.spawn(async move {
                load_doc(&weak2, &project, &path).await;
            });
        }
    });

    ui.on_back({
        let weak = ui.as_weak();
        move || {
            if let Some(ui) = weak.upgrade() {
                ui.set_open_project("".into());
                ui.set_doc_path("".into());
                ui.set_doc_content("".into());
            }
        }
    });

    ui.run()?;
    Ok(())
}

/// Connect, stream events, reconnect with backoff — the snapshot on
/// reconnect covers anything missed, same contract as ui-web.
async fn ws_loop(weak: Weak<MainWindow>, shared: Arc<Mutex<Shared>>) {
    let mut backoff = Duration::from_secs(1);
    loop {
        if let Ok((stream, _)) = tokio_tungstenite::connect_async(WS).await {
            backoff = Duration::from_secs(1);
            set_connected(&weak, true);
            let (_, mut read) = stream.split();
            while let Some(Ok(msg)) = read.next().await {
                if let Ok(text) = msg.into_text() {
                    if let Ok(event) = serde_json::from_str::<Event>(&text) {
                        apply(&shared, event);
                        refresh(&weak, &shared);
                    }
                }
            }
        }
        set_connected(&weak, false);
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(Duration::from_secs(15));
    }
}

fn apply(shared: &Arc<Mutex<Shared>>, event: Event) {
    let mut s = shared.lock().expect("shared");
    match event {
        Event::Snapshot { projects } => s.projects = projects,
        Event::ProjectChanged { project } => {
            match s.projects.iter_mut().find(|p| p.path == project.path) {
                Some(slot) => *slot = *project,
                None => s.projects.push(*project),
            }
            s.projects.sort_by_key(|p| std::cmp::Reverse(p.last_touched_unix));
        }
        Event::ProjectRemoved { path } => s.projects.retain(|p| p.path != path),
    }
}

fn set_connected(weak: &Weak<MainWindow>, connected: bool) {
    let _ = weak.upgrade_in_event_loop(move |ui| ui.set_connected(connected));
}

fn refresh(weak: &Weak<MainWindow>, shared: &Arc<Mutex<Shared>>) {
    let (projects, timeline, stats) = {
        let s = shared.lock().expect("shared");
        let filter = s.filter.to_lowercase();
        let visible: Vec<ProjectItem> = s
            .projects
            .iter()
            .filter(|p| {
                filter.is_empty()
                    || p.name.to_lowercase().contains(&filter)
                    || p.tagline.as_deref().unwrap_or("").to_lowercase().contains(&filter)
                    || p.languages.join(" ").contains(&filter)
            })
            .map(project_item)
            .collect();

        let mut entries: Vec<(i64, TimelineItem)> = s
            .projects
            .iter()
            .flat_map(|p| {
                p.git.iter().flat_map(|g| g.recent_commits.iter()).map(move |c| {
                    (c.time_unix, TimelineItem {
                        stamp: stamp(c.time_unix).into(),
                        project: p.name.clone().into(),
                        summary: c.summary.clone().into(),
                    })
                })
            })
            .collect();
        entries.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
        let timeline: Vec<TimelineItem> =
            entries.into_iter().take(200).map(|(_, item)| item).collect();

        let flagged = s.projects.iter().filter(|p| !p.health.is_empty()).count();
        let active = s
            .projects
            .iter()
            .filter(|p| matches!(p.activity, prefrontal_protocol::Activity::Active))
            .count();
        let stats = format!(
            "{} projects · {} active · {} flagged",
            s.projects.len(),
            active,
            flagged
        );
        (visible, timeline, stats)
    };
    let _ = weak.upgrade_in_event_loop(move |ui| {
        ui.set_projects(ModelRc::from(std::rc::Rc::new(VecModel::from(projects))));
        ui.set_timeline(ModelRc::from(std::rc::Rc::new(VecModel::from(timeline))));
        ui.set_stats(stats.into());
    });
}

fn project_item(p: &Project) -> ProjectItem {
    let git = p.git.as_ref();
    let flags = p
        .health
        .iter()
        .map(|f| match f {
            prefrontal_protocol::HealthFlag::NoGit => "no-git".to_string(),
            prefrontal_protocol::HealthFlag::NoRemote => "no-remote".to_string(),
            prefrontal_protocol::HealthFlag::NeverCommitted => "never-committed".to_string(),
            prefrontal_protocol::HealthFlag::DirtyPile { count } => format!("dirty-pile({count})"),
        })
        .collect::<Vec<_>>()
        .join(" ");
    ProjectItem {
        name: p.name.clone().into(),
        tagline: p.tagline.clone().unwrap_or_default().into(),
        activity: activity_str(p.activity).into(),
        branch: git.and_then(|g| g.branch.clone()).unwrap_or_default().into(),
        dirty: git.and_then(|g| g.dirty_files).unwrap_or(0) as i32,
        ago: ago(p.last_touched_unix).into(),
        langs: p.languages.join(" · ").into(),
        flags: flags.into(),
    }
}

fn activity_str(a: prefrontal_protocol::Activity) -> &'static str {
    use prefrontal_protocol::Activity::*;
    match a {
        Active => "active",
        Warm => "warm",
        Cold => "cold",
        Parked => "parked",
        Archived => "archived",
    }
}

fn ago(unix: i64) -> String {
    let now = chrono::Utc::now().timestamp();
    let days = (now - unix).max(0) / 86_400;
    match days {
        0 => "today".into(),
        1 => "1d ago".into(),
        d if d < 60 => format!("{d}d ago"),
        d => format!("{}mo ago", d / 30),
    }
}

fn stamp(unix: i64) -> String {
    use chrono::{DateTime, Local};
    DateTime::from_timestamp(unix, 0)
        .map(|dt| DateTime::<Local>::from(dt).format("%a %H:%M").to_string())
        .unwrap_or_default()
}

async fn open_project(weak: Weak<MainWindow>, name: String) {
    let docs: Vec<prefrontal_protocol::DocEntry> =
        fetch_json(&format!("{BASE}/api/docs/{name}")).await.unwrap_or_default();
    let first = docs.first().map(|d| d.path.clone());
    let items: Vec<DocItem> = docs
        .into_iter()
        .map(|d| DocItem { path: d.path.into() })
        .collect();
    {
        let name = name.clone();
        let _ = weak.upgrade_in_event_loop(move |ui| {
            ui.set_open_project(name.into());
            ui.set_docs(ModelRc::from(std::rc::Rc::new(VecModel::from(items))));
            ui.set_doc_content("".into());
            ui.set_doc_path("".into());
        });
    }
    if let Some(path) = first {
        load_doc(&weak, &name, &path).await;
    }
}

async fn load_doc(weak: &Weak<MainWindow>, project: &str, path: &str) {
    let url = format!(
        "{BASE}/api/doc/{}/{}",
        project,
        path.split('/')
            .map(urlencode)
            .collect::<Vec<_>>()
            .join("/")
    );
    let content = fetch_json::<prefrontal_protocol::DocContent>(&url)
        .await
        .map(|d| d.raw)
        .unwrap_or_else(|| "could not load doc — is prefrontald running?".into());
    let path = path.to_string();
    let _ = weak.upgrade_in_event_loop(move |ui| {
        ui.set_doc_path(path.into());
        ui.set_doc_content(content.into());
    });
}

async fn fetch_json<T: serde::de::DeserializeOwned>(url: &str) -> Option<T> {
    reqwest::get(url).await.ok()?.json().await.ok()
}

fn urlencode(segment: &str) -> String {
    segment
        .bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}
