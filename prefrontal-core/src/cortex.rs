//! CerebroCortex integration (charter D6/phase 6) — an MCP stdio *client*,
//! the mirror twin of `prefrontal mcp`. Prefrontal ingests one semantic
//! summary per project (tag-deduped: `project:<name>` updates in place) and
//! answers "that thing where I…" queries via cortex recall, alongside — never
//! instead of — the lexical index.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

use anyhow::{bail, Context, Result};
use prefrontal_protocol::{CortexHit, Project};
use serde_json::{json, Value};

use crate::config::CortexConfig;

pub struct CortexClient {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    next_id: u64,
    cfg: CortexConfig,
}

impl Drop for CortexClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl CortexClient {
    pub fn spawn(cfg: &CortexConfig) -> Result<Self> {
        if cfg.command.is_empty() {
            bail!("cortex.command is not configured");
        }
        let mut child = Command::new(&cfg.command)
            .args(&cfg.args)
            .envs(&cfg.env)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .with_context(|| format!("spawning {}", cfg.command))?;
        let stdin = child.stdin.take().context("cortex stdin")?;
        let stdout = child.stdout.take().context("cortex stdout")?;
        let mut client = Self {
            child,
            stdin,
            reader: BufReader::new(stdout),
            next_id: 0,
            cfg: cfg.clone(),
        };
        client.request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "clientInfo": { "name": "prefrontal", "version": env!("CARGO_PKG_VERSION") }
            }),
        )?;
        writeln!(client.stdin, r#"{{"jsonrpc":"2.0","method":"notifications/initialized"}}"#)?;
        client.stdin.flush()?;
        Ok(client)
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.next_id += 1;
        let id = self.next_id;
        let msg = json!({ "jsonrpc": "2.0", "id": id, "method": method, "params": params });
        writeln!(self.stdin, "{msg}")?;
        self.stdin.flush()?;
        let mut line = String::new();
        loop {
            line.clear();
            if self.reader.read_line(&mut line)? == 0 {
                bail!("cortex closed the pipe");
            }
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            if v.get("id").and_then(|i| i.as_u64()) != Some(id) {
                continue; // stray notification or log line
            }
            if let Some(err) = v.get("error") {
                bail!("cortex rpc error: {err}");
            }
            return Ok(v.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    /// tools/call returning the text payload; tool-level isError becomes Err.
    fn call(&mut self, tool: &str, arguments: Value) -> Result<String> {
        let result =
            self.request("tools/call", json!({ "name": tool, "arguments": arguments }))?;
        let text = result
            .get("content")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("text"))
            .and_then(|t| t.as_str())
            .unwrap_or("")
            .to_string();
        if result.get("isError").and_then(|e| e.as_bool()).unwrap_or(false) {
            bail!("cortex tool {tool} failed: {text}");
        }
        Ok(text)
    }

    pub fn recall(&mut self, query: &str, top_k: u32) -> Result<Vec<CortexHit>> {
        let text = self.call("recall", json!({ "query": query, "top_k": top_k }))?;
        let rows: Vec<Value> = serde_json::from_str(&text).unwrap_or_default();
        Ok(rows
            .iter()
            .map(|row| {
                // rows arrive as {memory: {...}, score?} or flat — read both
                let memory = row.get("memory").unwrap_or(row);
                let get_s = |k: &str| {
                    memory.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string()
                };
                CortexHit {
                    content: get_s("content"),
                    agent_id: get_s("agent_id"),
                    tags: memory
                        .get("tags")
                        .and_then(|t| t.as_array())
                        .map(|a| {
                            a.iter()
                                .filter_map(|v| v.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default(),
                    score: ["score", "similarity", "relevance"]
                        .iter()
                        .find_map(|k| row.get(k).and_then(|v| v.as_f64())),
                }
            })
            .collect())
    }

    /// Upsert one project summary. Returns true if a new memory was created.
    pub fn sync_project(&mut self, project: &Project) -> Result<bool> {
        let project_tag = format!("project:{}", project.name);
        let existing = self.call(
            "find_by_tags",
            json!({ "tags": ["prefrontal", project_tag], "limit": 1 }),
        )?;
        let rows: Vec<Value> = serde_json::from_str(&existing).unwrap_or_default();
        let existing_id = rows.first().and_then(|row| {
            ["id", "memory_id"]
                .iter()
                .find_map(|k| {
                    row.get(k)
                        .or_else(|| row.get("memory").and_then(|m| m.get(k)))
                        .and_then(|v| v.as_str())
                })
                .map(String::from)
        });

        let content = summarize(project);
        match existing_id {
            Some(memory_id) => {
                self.call(
                    "update_memory",
                    json!({ "memory_id": memory_id, "content": content }),
                )?;
                Ok(false)
            }
            None => {
                self.call(
                    "remember",
                    json!({
                        "content": content,
                        "tags": ["prefrontal", project_tag],
                        "memory_type": "semantic",
                        "agent_id": self.cfg.agent_id,
                        // project facts are for every agent in the house
                        "visibility": "shared",
                    }),
                )?;
                Ok(true)
            }
        }
    }
}

/// The semantic card a project becomes inside the cortex.
fn summarize(p: &Project) -> String {
    let mut s = format!("PROJECT {}", p.name);
    if let Some(t) = &p.tagline {
        s.push_str(&format!(" — {t}"));
    }
    s.push_str(&format!(
        "\nStatus: {:?}, languages: [{}], path: {}",
        p.activity,
        p.languages.join(", "),
        p.path
    ));
    if let Some(g) = &p.git {
        if let Some(b) = &g.branch {
            s.push_str(&format!("\nBranch {b}"));
        }
        if let Some(c) = g.commit_count {
            s.push_str(&format!(", {c} commits"));
        }
        if let Some(d) = g.dirty_files {
            if d > 0 {
                s.push_str(&format!(", {d} uncommitted files"));
            }
        }
        if !g.recent_commits.is_empty() {
            s.push_str("\nRecent work:");
            for c in g.recent_commits.iter().take(8) {
                s.push_str(&format!("\n- {}", c.summary));
            }
        }
    }
    if !p.health.is_empty() {
        s.push_str(&format!("\nHealth flags: {:?}", p.health));
    }
    s
}
