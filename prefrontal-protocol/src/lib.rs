//! Wire/domain types shared by the daemon, CLI, and every frontend.
//!
//! Frontends deserialize into the SAME types the daemon serializes from —
//! no hand-rolled string matching (same trick as apexos-protocol).

use serde::{Deserialize, Serialize};

/// Derived from last-touched time (thresholds configurable); `Archived` only via override.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Activity {
    Active,
    Warm,
    Cold,
    Parked,
    Archived,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GitInfo {
    /// Current branch, if HEAD is on one.
    pub branch: Option<String>,
    /// Unix seconds of the last commit; `None` on an unborn HEAD.
    pub last_commit_unix: Option<i64>,
    /// Uncommitted paths (staged + worktree + untracked); `None` = could not determine.
    pub dirty_files: Option<u32>,
    /// Total commits reachable from HEAD.
    pub commit_count: Option<u32>,
    /// Fetch URL of `origin`, if any.
    pub remote: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "flag", rename_all = "snake_case")]
pub enum HealthFlag {
    /// Not a git repository at all.
    NoGit,
    /// A repo with no `origin` — one disk failure from gone.
    NoRemote,
    /// A repo with zero commits (possibly with dirty files piling up).
    NeverCommitted,
    /// Uncommitted files at/above the configured threshold.
    DirtyPile { count: u32 },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Project {
    pub name: String,
    pub path: String,
    /// Manifest-detected: "rust", "node", "python", "godot", …
    pub languages: Vec<String>,
    pub activity: Activity,
    /// `None` for un-git'ed folders (which is itself a health flag).
    pub git: Option<GitInfo>,
    /// From README (first `###` or first paragraph) or a config override.
    pub tagline: Option<String>,
    /// From config overrides only — never derived.
    pub tags: Vec<String>,
    pub health: Vec<HealthFlag>,
    /// Unix seconds — max(last commit, top-level dir mtime). Sort key.
    pub last_touched_unix: i64,
    pub has_readme: bool,
    pub has_claude_md: bool,
}

/// Frames pushed over the daemon's WebSocket.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Full state, sent on connect (and after rescans until delta events land).
    Snapshot { projects: Vec<Project> },
    /// A single project changed on disk (phase 1: file watcher).
    ProjectChanged { project: Project },
}
