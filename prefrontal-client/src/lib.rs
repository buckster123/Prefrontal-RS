//! Tiny SDK for the Prefrontal-RS daemon.
//!
//! Everything the dashboard can do, from your own code — the client wraps the
//! REST surface and the WebSocket event stream, deserializing into the same
//! [`prefrontal_protocol`] types the daemon serializes from.
//!
//! ```no_run
//! # async fn demo() -> anyhow::Result<()> {
//! let pf = prefrontal_client::Prefrontal::default();
//! for p in pf.projects().await? {
//!     println!("{:<24} {:?}", p.name, p.activity);
//! }
//! for hit in pf.search("debounce", 10).await? {
//!     println!("{}:{} {}", hit.path, hit.line.unwrap_or(0), hit.snippet);
//! }
//! # Ok(()) }
//! ```

use anyhow::{Context, Result};
use futures_util::{Stream, StreamExt};
pub use prefrontal_protocol::{
    Activity, ColonyStatus, CommitSummary, CortexHit, DocContent, DocEntry, DocWriteResult,
    Event, GitCommitDetail, GitCommitRequest, GitDiff, GitFile, GitInfo, GitOpResult, GitPaths,
    GitRef, GitStashRequest, GitStatus, GitSwitchRequest, GitTreeEntry, HealthFlag, Project,
    SearchHit, Sibling, SiblingSurface,
};

pub struct Prefrontal {
    base: String,
    http: reqwest::Client,
}

impl Default for Prefrontal {
    fn default() -> Self {
        Self::connect("http://127.0.0.1:7320")
    }
}

impl Prefrontal {
    pub fn connect(base: impl Into<String>) -> Self {
        Self {
            base: base.into().trim_end_matches('/').to_string(),
            http: reqwest::Client::new(),
        }
    }

    async fn get_json<T: serde::de::DeserializeOwned>(&self, path: &str) -> Result<T> {
        let url = format!("{}{path}", self.base);
        let res = self.http.get(&url).send().await.context("daemon unreachable")?;
        anyhow::ensure!(res.status().is_success(), "{} → {}", url, res.status());
        Ok(res.json().await?)
    }

    /// Every project, newest-touched first.
    pub async fn projects(&self) -> Result<Vec<Project>> {
        self.get_json("/api/projects").await
    }

    /// Force a full rescan and return the fresh state.
    pub async fn rescan(&self) -> Result<Vec<Project>> {
        let res = self
            .http
            .post(format!("{}/api/rescan", self.base))
            .send()
            .await
            .context("daemon unreachable")?;
        Ok(res.json().await?)
    }

    /// Full-text search over code, docs, commit messages, and symbols.
    pub async fn search(&self, query: &str, limit: usize) -> Result<Vec<SearchHit>> {
        self.get_json(&format!(
            "/api/search?q={}&limit={limit}",
            urlencode(query)
        ))
        .await
    }

    /// Semantic recall via the optional CerebroCortex layer.
    /// Errors with the daemon's 503 message when the feature is off.
    pub async fn recall(&self, query: &str) -> Result<Vec<CortexHit>> {
        self.get_json(&format!("/api/cortex?q={}", urlencode(query))).await
    }

    /// The -RS colony as seen from the daemon's machine: installed, live,
    /// and how to reach each sibling. 503s when the panel is disabled.
    pub async fn colony(&self) -> Result<ColonyStatus> {
        self.get_json("/api/colony").await
    }

    /// Markdown/text docs of one project, README first.
    pub async fn docs(&self, project: &str) -> Result<Vec<DocEntry>> {
        self.get_json(&format!("/api/docs/{}", urlencode(project))).await
    }

    /// One doc: raw markdown + server-rendered HTML.
    pub async fn read_doc(&self, project: &str, path: &str) -> Result<DocContent> {
        self.get_json(&format!(
            "/api/doc/{}/{}",
            urlencode(project),
            encode_path(path)
        ))
        .await
    }

    pub async fn git_status(&self, project: &str) -> Result<GitStatus> {
        self.get_json(&format!("/api/git/{}/status", urlencode(project)))
            .await
    }

    pub async fn git_diff(
        &self,
        project: &str,
        path: &str,
        cached: bool,
        rev: Option<&str>,
    ) -> Result<GitDiff> {
        let mut q = format!(
            "/api/git/{}/diff?path={}&cached={cached}",
            urlencode(project),
            urlencode(path)
        );
        if let Some(r) = rev {
            q.push_str(&format!("&rev={}", urlencode(r)));
        }
        self.get_json(&q).await
    }

    pub async fn git_log(&self, project: &str, limit: u32, skip: u32) -> Result<Vec<CommitSummary>> {
        self.get_json(&format!(
            "/api/git/{}/log?limit={limit}&skip={skip}",
            urlencode(project)
        ))
        .await
    }

    pub async fn git_commit(&self, project: &str, id: &str) -> Result<GitCommitDetail> {
        self.get_json(&format!(
            "/api/git/{}/commit/{}",
            urlencode(project),
            urlencode(id)
        ))
        .await
    }

    pub async fn git_refs(&self, project: &str) -> Result<Vec<GitRef>> {
        self.get_json(&format!("/api/git/{}/refs", urlencode(project)))
            .await
    }

    pub async fn git_tree(
        &self,
        project: &str,
        rev: Option<&str>,
        path: Option<&str>,
    ) -> Result<Vec<GitTreeEntry>> {
        let mut q = format!("/api/git/{}/tree", urlencode(project));
        let mut sep = '?';
        if let Some(r) = rev {
            q.push_str(&format!("{sep}rev={}", urlencode(r)));
            sep = '&';
        }
        if let Some(p) = path {
            q.push_str(&format!("{sep}path={}", urlencode(p)));
        }
        self.get_json(&q).await
    }

    pub async fn git_file(&self, project: &str, path: &str, rev: Option<&str>) -> Result<GitFile> {
        let mut q = format!(
            "/api/git/{}/file?path={}",
            urlencode(project),
            urlencode(path)
        );
        if let Some(r) = rev {
            q.push_str(&format!("&rev={}", urlencode(r)));
        }
        self.get_json(&q).await
    }

    /// Create or update a doc; the daemon commits it locally (never pushes).
    pub async fn write_doc(
        &self,
        project: &str,
        path: &str,
        content: &str,
    ) -> Result<DocWriteResult> {
        let url = format!(
            "{}/api/doc/{}/{}",
            self.base,
            urlencode(project),
            encode_path(path)
        );
        let res = self
            .http
            .put(url)
            .json(&serde_json::json!({ "content": content }))
            .send()
            .await
            .context("daemon unreachable")?;
        anyhow::ensure!(res.status().is_success(), "write failed: {}", res.status());
        Ok(res.json().await?)
    }

    /// Live event stream: a `Snapshot` on connect, then `ProjectChanged` /
    /// `ProjectRemoved` deltas as the filesystem moves.
    pub async fn events(&self) -> Result<impl Stream<Item = Event>> {
        let ws_url = format!(
            "{}/ws",
            self.base.replacen("http", "ws", 1) // http→ws, https→wss
        );
        let (stream, _) = tokio_tungstenite::connect_async(&ws_url)
            .await
            .context("websocket connect")?;
        let (_, read) = stream.split();
        Ok(read.filter_map(|msg| async {
            let text = msg.ok()?.into_text().ok()?;
            serde_json::from_str::<Event>(&text).ok()
        }))
    }
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn encode_path(path: &str) -> String {
    path.split('/').map(urlencode).collect::<Vec<_>>().join("/")
}
