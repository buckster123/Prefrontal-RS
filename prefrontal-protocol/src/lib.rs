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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// Commits ahead of the upstream tracking branch, if one is set.
    /// Filled when cheap; the Repo pane's `GitStatus` is the source of truth.
    #[serde(default)]
    pub ahead: Option<u32>,
    /// Commits behind the upstream tracking branch, if one is set.
    #[serde(default)]
    pub behind: Option<u32>,
    /// Commits within the timeline window (config: days/cap), newest first.
    /// The "where was I" view derives from these — per project, so the
    /// watcher's per-project deltas keep the merged timeline live for free.
    #[serde(default)]
    pub recent_commits: Vec<CommitSummary>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CommitSummary {
    /// Abbreviated hex id.
    pub id: String,
    /// First line of the commit message.
    pub summary: String,
    pub time_unix: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

/// `PartialEq` lets the daemon suppress broadcasts for rescans where nothing
/// the dashboard shows actually changed.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// One markdown/text file inside a project, path relative to the project root.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DocEntry {
    pub path: String,
    pub size: u64,
    pub modified_unix: i64,
}

/// A doc served for viewing: raw source plus server-rendered HTML.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocContent {
    pub project: String,
    pub path: String,
    pub raw: String,
    /// comrak output with raw HTML escaped — safe to inject as innerHTML.
    pub html: String,
    pub modified_unix: i64,
}

/// Body of a doc write (create or update).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocWrite {
    pub content: String,
}

/// What happened to a saved doc. `saved` is always true on a 200 —
/// commit state is reported honestly rather than pretended (charter D9:
/// local commit always *attempted*, never push).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DocWriteResult {
    pub saved: bool,
    pub committed: bool,
    /// Short hash when committed.
    pub commit_id: Option<String>,
    /// Why it didn't commit (not a repo, identity unset, no changes…).
    pub detail: Option<String>,
}

/// One full-text search result.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    pub project: String,
    /// File path for code/doc hits; abbreviated commit id for commit hits.
    pub path: String,
    /// "code" | "doc" | "commit"
    pub kind: String,
    /// 1-based line of the first term match, for file hits.
    pub line: Option<u32>,
    pub snippet: String,
    pub score: f32,
}

/// One semantic-recall result from the (optional) CerebroCortex layer.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CortexHit {
    pub content: String,
    pub agent_id: String,
    pub tags: Vec<String>,
    pub score: Option<f64>,
}

/// How a colony sibling is primarily reached on this machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SiblingSurface {
    /// Serves a browser UI — `url` is the way in.
    WebUi,
    /// HTTP API without a UI.
    HttpApi,
    /// MCP-first — reach it by its MCP server name.
    Mcp,
    /// A CLI binary.
    Cli,
    /// A native app; nothing to connect to.
    Native,
    /// Nothing runnable on a host at all (bare metal, templates).
    NoRuntime,
}

/// One member of the -RS colony as seen from this machine. The detection
/// signals are independent ORs — a sibling can be live with no checkout
/// (binary installs) or checked out and dormant. `installed` is derived:
/// `checkout.is_some() || binary.is_some() || live == Some(true)`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sibling {
    pub name: String,
    /// One-liner, from the built-in roster — never derived from READMEs.
    pub tagline: String,
    pub surface: SiblingSurface,
    /// Effective loopback port (config override or roster default).
    pub port: Option<u16>,
    /// `http://127.0.0.1:<port>/` when the sibling serves a browser UI.
    pub url: Option<String>,
    /// MCP server name agents can reach it by, when it speaks MCP.
    pub mcp: Option<String>,
    /// Source checkout path under a scan root, if present.
    pub checkout: Option<String>,
    /// Installed binary path, if one was found in a known dir.
    pub binary: Option<String>,
    /// `Some(true)` = the port answered just now (401/403 counts — a rejection
    /// proves liveness); `None` = no port to probe.
    pub live: Option<bool>,
    /// The sibling's lander page — where "not installed" points.
    pub lander: String,
}

/// The whole colony, one probe sweep. Compare `siblings` (not the timestamp)
/// to decide whether anything worth broadcasting changed.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct ColonyStatus {
    pub siblings: Vec<Sibling>,
    pub checked_unix: i64,
}

/// Frames pushed over the daemon's WebSocket. Connect sequence is
/// `Snapshot` then (when the colony panel is enabled) `Colony` — together
/// they cover all gaps, so clients still need zero replay logic.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Event {
    /// Full state, sent on connect and after a full rescan.
    Snapshot { projects: Vec<Project> },
    /// A single project changed on disk. (Boxed: keeps the enum small — this
    /// variant is ~10× the size of the others; serde sees straight through.)
    ProjectChanged { project: Box<Project> },
    /// A project directory disappeared from the root.
    ProjectRemoved { path: String },
    /// Colony sweep result — sent on connect and whenever a sibling's state
    /// actually changed (equal sweeps are suppressed, same as rescans).
    Colony { colony: ColonyStatus },
}

/// How one side of a path (index or worktree) differs from its pair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitChange {
    None,
    Modified,
    Added,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Untracked,
    Unmerged,
}

/// One path in a working-tree status listing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitEntry {
    pub path: String,
    /// HEAD ↔ index.
    pub index: GitChange,
    /// Index ↔ worktree.
    pub worktree: GitChange,
    pub conflicted: bool,
}

/// One stash entry (`stash@{n}`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStash {
    pub index: u32,
    pub message: String,
}

/// Full working-tree snapshot for the Repo pane. Not attached to `Project`
/// — dirty paths stay off the garden WS snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GitStatus {
    pub branch: Option<String>,
    pub detached: bool,
    pub upstream: Option<String>,
    pub ahead: Option<u32>,
    pub behind: Option<u32>,
    pub merging: bool,
    pub rebasing: bool,
    /// True when `[git] allow_push` is on — Fetch/Push are live.
    pub allow_push: bool,
    pub entries: Vec<GitEntry>,
    pub stashes: Vec<GitStash>,
}

/// A unified diff for one path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitDiff {
    pub path: String,
    pub cached: bool,
    pub rev: Option<String>,
    pub patch: String,
    pub binary: bool,
    pub truncated: bool,
}

/// One file touched by a commit (patch lives on `GET .../diff?rev=`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommitFile {
    pub path: String,
    pub status: GitChange,
}

/// Author + body + file list for one commit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommitDetail {
    pub id: String,
    pub short_id: String,
    pub summary: String,
    pub body: Option<String>,
    pub author: String,
    pub author_email: String,
    pub time_unix: i64,
    pub parents: Vec<String>,
    pub files: Vec<GitCommitFile>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitRefKind {
    Local,
    Remote,
    Tag,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitRef {
    pub name: String,
    pub kind: GitRefKind,
    pub current: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitTreeKind {
    File,
    Dir,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitTreeEntry {
    pub path: String,
    pub kind: GitTreeKind,
    pub size: Option<u64>,
}

/// A file at a revision (or `WORKTREE` for the on-disk path).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitFile {
    pub path: String,
    pub rev: String,
    pub text: String,
    pub binary: bool,
    pub truncated: bool,
}

/// Honest result of an allowlisted porcelain write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitOpResult {
    pub ok: bool,
    pub detail: Option<String>,
    pub commit_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitPaths {
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitCommitRequest {
    pub message: String,
    #[serde(default)]
    pub paths: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitSwitchRequest {
    pub name: String,
    #[serde(default)]
    pub create: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitStashAction {
    Push,
    Pop,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GitStashRequest {
    pub action: GitStashAction,
    pub message: Option<String>,
}
