use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use prefrontal_protocol::{Activity, CommitSummary, GitInfo, HealthFlag, Project};

use crate::config::Config;

/// High-churn build output — never watched, never doc-walked.
pub const SKIP_DIRS: &[&str] =
    &["target", "node_modules", "venv", "__pycache__", "dist", "build", ".vite"];

/// Scan every root, newest-touched first.
///
/// Synchronous and unhurried by design — callers on async runtimes wrap it in
/// `spawn_blocking`. Caching/incrementality arrives with the file watcher.
pub fn scan_all(cfg: &Config) -> Vec<Project> {
    let mut projects: Vec<Project> = cfg
        .root_paths()
        .iter()
        .flat_map(|root| scan_root(root, cfg))
        .collect();
    projects.sort_by_key(|p| std::cmp::Reverse(p.last_touched_unix));
    projects
}

fn scan_root(root: &Path, cfg: &Config) -> Vec<Project> {
    let Ok(entries) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            if is_ignored(&name, cfg) {
                return None;
            }
            Some(scan_project(&e.path(), name, cfg))
        })
        .collect()
}

/// Skipped entirely: hidden dirs, configured ignores, and per-project `ignore` overrides.
pub fn is_ignored(name: &str, cfg: &Config) -> bool {
    name.starts_with('.')
        || cfg.ignore.iter().any(|i| i == name)
        || cfg.overrides.get(name).is_some_and(|o| o.ignore)
}

/// Scan one project directory — the watcher's per-change unit of work.
pub fn scan_project(dir: &Path, name: String, cfg: &Config) -> Project {
    let git = git_info(dir, cfg);
    let ovr = cfg.overrides.get(&name);

    let dir_mtime = std::fs::metadata(dir)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let last_touched_unix = git
        .as_ref()
        .and_then(|g| g.last_commit_unix)
        .map_or(dir_mtime, |c| c.max(dir_mtime));

    let activity = ovr
        .and_then(|o| o.status)
        .unwrap_or_else(|| derive_activity(last_touched_unix, cfg));

    let mut health = Vec::new();
    match &git {
        None => health.push(HealthFlag::NoGit),
        Some(g) => {
            if g.remote.is_none() {
                health.push(HealthFlag::NoRemote);
            }
            if g.commit_count == Some(0) {
                health.push(HealthFlag::NeverCommitted);
            }
            if let Some(dirty) = g.dirty_files {
                if dirty >= cfg.thresholds.dirty_pile {
                    health.push(HealthFlag::DirtyPile { count: dirty });
                }
            }
        }
    }

    let tagline = ovr
        .and_then(|o| o.tagline.clone())
        .or_else(|| readme_tagline(dir));

    Project {
        path: dir.to_string_lossy().to_string(),
        languages: detect_languages(dir),
        activity,
        git,
        tagline,
        tags: ovr.map(|o| o.tags.clone()).unwrap_or_default(),
        health,
        last_touched_unix,
        has_readme: dir.join("README.md").is_file(),
        has_claude_md: dir.join("CLAUDE.md").is_file(),
        name,
    }
}

fn derive_activity(last_touched_unix: i64, cfg: &Config) -> Activity {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = (now - last_touched_unix).max(0) / 86_400;
    let t = &cfg.thresholds;
    if days < t.active_days as i64 {
        Activity::Active
    } else if days < t.warm_days as i64 {
        Activity::Warm
    } else if days < t.cold_days as i64 {
        Activity::Cold
    } else {
        Activity::Parked
    }
}

fn git_info(dir: &Path, cfg: &Config) -> Option<GitInfo> {
    if !dir.join(".git").exists() {
        return None;
    }
    let repo = gix::open(dir).ok()?;

    let branch = repo
        .head_name()
        .ok()
        .flatten()
        .map(|n| n.shorten().to_string());

    // One ancestry pass: count everything, collect the timeline window.
    // No early break — merges make the walk only roughly time-ordered.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let cutoff = now - cfg.timeline.days as i64 * 86_400;
    let (last_commit_unix, commit_count, recent_commits) = match repo.head_commit() {
        Ok(commit) => {
            let time = commit.time().ok().map(|t| t.seconds);
            let mut count = 0u32;
            let mut recent: Vec<CommitSummary> = Vec::new();
            if let Ok(walk) = commit.id().ancestors().all() {
                for info in walk.filter_map(Result::ok) {
                    count += 1;
                    let Ok(c) = info.object() else { continue };
                    let Ok(t) = c.time().map(|t| t.seconds) else { continue };
                    if t < cutoff {
                        continue;
                    }
                    let summary = c
                        .message()
                        .ok()
                        .map(|m| m.summary().to_string())
                        .unwrap_or_default();
                    let full = info.id.to_string();
                    recent.push(CommitSummary {
                        id: full.chars().take(8).collect(),
                        summary,
                        time_unix: t,
                    });
                }
            }
            recent.sort_by_key(|c| std::cmp::Reverse(c.time_unix));
            recent.truncate(cfg.timeline.max_per_project as usize);
            (time, Some(count), recent)
        }
        // Unborn HEAD — a repo someone `git init`ed and never committed to.
        Err(_) => (None, Some(0), Vec::new()),
    };

    let remote = repo
        .find_remote("origin")
        .ok()
        .and_then(|r| r.url(gix::remote::Direction::Fetch).map(|u| u.to_string()));

    let dirty_files = repo
        .status(gix::progress::Discard)
        .ok()
        .and_then(|p| p.into_index_worktree_iter(Vec::new()).ok())
        .map(|iter| iter.filter_map(Result::ok).count() as u32);

    Some(GitInfo { branch, last_commit_unix, dirty_files, commit_count, remote, recent_commits })
}

/// Manifest-based detection; checks the project root and one level down
/// (Cargo workspaces, Godot projects nested one deep).
fn detect_languages(dir: &Path) -> Vec<String> {
    let mut langs = Vec::new();
    let here = |f: &str| dir.join(f).is_file();
    let one_deep = |f: &str| {
        std::fs::read_dir(dir).ok().is_some_and(|entries| {
            entries
                .filter_map(|e| e.ok())
                .take(64)
                .any(|e| e.path().join(f).is_file())
        })
    };

    if here("Cargo.toml") || one_deep("Cargo.toml") {
        langs.push("rust".into());
    }
    if here("package.json") {
        langs.push("node".into());
    }
    if here("pyproject.toml") || here("requirements.txt") || here("setup.py") {
        langs.push("python".into());
    }
    if here("project.godot") || one_deep("project.godot") {
        langs.push("godot".into());
    }
    langs
}

/// First `###` heading (house-style taglines) or first plain paragraph line.
fn readme_tagline(dir: &Path) -> Option<String> {
    let raw = std::fs::read_to_string(dir.join("README.md")).ok()?;
    let mut fallback: Option<String> = None;
    for line in raw.lines().take(40) {
        let line = line.trim();
        if let Some(rest) = line.strip_prefix("### ") {
            return Some(clean_md(rest));
        }
        if fallback.is_none()
            && !line.is_empty()
            && !line.starts_with(['#', '<', '!', '[', '>', '-', '|'])
            && !line.starts_with("```")
        {
            fallback = Some(clean_md(line));
        }
    }
    fallback
}

fn clean_md(s: &str) -> String {
    // taglines sometimes sit inside raw-HTML blocks — strip stray tags
    static TAG: std::sync::LazyLock<regex::Regex> =
        std::sync::LazyLock::new(|| regex::Regex::new(r"<[^>]*>").expect("static regex"));
    TAG.replace_all(s, "").trim_matches(['*', '_', ' ']).to_string()
}
