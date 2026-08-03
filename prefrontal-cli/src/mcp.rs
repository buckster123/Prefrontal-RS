//! `prefrontal mcp` — an MCP stdio server (newline-delimited JSON-RPC 2.0).
//!
//! Hand-rolled on purpose: the surface is seven tools and four methods, and
//! the daemon isn't required — every tool works from a direct scan, so agents
//! get answers even when nothing else is running. Scans are cached briefly;
//! an agent turn firing several tools shouldn't pay for several scans.

use std::io::{BufRead, Write};
use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::Result;
use prefrontal_core::{scan_all, Config};
use prefrontal_protocol::Project;
use serde_json::{json, Value};

const SCAN_TTL: Duration = Duration::from_secs(10);
const PROTOCOL_VERSION: &str = "2024-11-05";

struct Server {
    cfg: Config,
    cache: Option<(Instant, Vec<Project>)>,
}

pub fn run(cfg: Config) -> Result<()> {
    let mut server = Server { cfg, cache: None };
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let Ok(msg) = serde_json::from_str::<Value>(&line) else { continue };
        let method = msg.get("method").and_then(|m| m.as_str()).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
        let Some(id) = msg.get("id").cloned() else {
            continue; // notification (e.g. notifications/initialized) — no reply
        };
        let response = match server.dispatch(method, &params) {
            Ok(result) => json!({ "jsonrpc": "2.0", "id": id, "result": result }),
            Err((code, message)) => json!({
                "jsonrpc": "2.0", "id": id,
                "error": { "code": code, "message": message }
            }),
        };
        writeln!(stdout, "{response}")?;
        stdout.flush()?;
    }
    Ok(())
}

type RpcError = (i64, String);

impl Server {
    fn dispatch(&mut self, method: &str, params: &Value) -> Result<Value, RpcError> {
        match method {
            "initialize" => {
                let requested = params
                    .get("protocolVersion")
                    .and_then(|v| v.as_str())
                    .unwrap_or(PROTOCOL_VERSION);
                Ok(json!({
                    "protocolVersion": requested,
                    "capabilities": { "tools": {} },
                    "serverInfo": {
                        "name": "prefrontal",
                        "version": env!("CARGO_PKG_VERSION"),
                    }
                }))
            }
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({ "tools": tool_definitions() })),
            "tools/call" => {
                let name = params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or_else(|| json!({}));
                Ok(self.call_tool(name, &args))
            }
            _ => Err((-32601, format!("method not found: {method}"))),
        }
    }

    fn projects(&mut self) -> &[Project] {
        let stale = self
            .cache
            .as_ref()
            .is_none_or(|(at, _)| at.elapsed() > SCAN_TTL);
        if stale {
            self.cache = Some((Instant::now(), scan_all(&self.cfg)));
        }
        &self.cache.as_ref().expect("cache just filled").1
    }

    fn project_dir(&mut self, name: &str) -> Option<PathBuf> {
        self.projects()
            .iter()
            .find(|p| p.name == name)
            .map(|p| PathBuf::from(&p.path))
    }

    /// Tool outcomes are MCP results (with isError for tool-level failures),
    /// never JSON-RPC errors — those are reserved for protocol breakage.
    fn call_tool(&mut self, name: &str, args: &Value) -> Value {
        let s = |key: &str| args.get(key).and_then(|v| v.as_str()).unwrap_or("").to_string();
        let outcome: Result<String, String> = match name {
            "list_projects" => Ok(self.tool_list_projects()),
            "project_status" => self.tool_project_status(&s("project")),
            "where_was_i" => {
                let days = args.get("days").and_then(|v| v.as_u64()).unwrap_or(14);
                Ok(self.tool_where_was_i(days as i64))
            }
            "search" => {
                let limit = args.get("limit").and_then(|v| v.as_u64()).unwrap_or(20) as usize;
                self.tool_search(&s("query"), limit.min(100))
            }
            "list_docs" => self.tool_list_docs(&s("project")),
            "read_doc" => self.tool_read_doc(&s("project"), &s("path")),
            "write_doc" => self.tool_write_doc(&s("project"), &s("path"), &s("content")),
            "colony_status" => self.tool_colony_status(),
            _ => Err(format!("unknown tool: {name}")),
        };
        match outcome {
            Ok(text) => json!({ "content": [{ "type": "text", "text": text }], "isError": false }),
            Err(msg) => json!({ "content": [{ "type": "text", "text": msg }], "isError": true }),
        }
    }

    fn tool_list_projects(&mut self) -> String {
        let rows: Vec<Value> = self
            .projects()
            .iter()
            .map(|p| {
                json!({
                    "name": p.name,
                    "activity": p.activity,
                    "languages": p.languages,
                    "tagline": p.tagline,
                    "branch": p.git.as_ref().and_then(|g| g.branch.clone()),
                    "dirty_files": p.git.as_ref().and_then(|g| g.dirty_files),
                    "health": p.health,
                })
            })
            .collect();
        serde_json::to_string_pretty(&rows).unwrap_or_default()
    }

    fn tool_project_status(&mut self, name: &str) -> Result<String, String> {
        self.projects()
            .iter()
            .find(|p| p.name == name)
            .map(|p| serde_json::to_string_pretty(p).unwrap_or_default())
            .ok_or_else(|| format!("unknown project: {name} (try list_projects)"))
    }

    fn tool_where_was_i(&mut self, days: i64) -> String {
        use chrono::{DateTime, Local};
        let cutoff = Local::now().timestamp() - days * 86_400;
        let mut entries: Vec<(i64, String, String)> = self
            .projects()
            .iter()
            .flat_map(|p| {
                p.git
                    .iter()
                    .flat_map(|g| g.recent_commits.iter())
                    .filter(|c| c.time_unix >= cutoff)
                    .map(move |c| (c.time_unix, p.name.clone(), c.summary.clone()))
            })
            .collect();
        entries.sort_by_key(|(t, _, _)| std::cmp::Reverse(*t));
        if entries.is_empty() {
            return format!("no commits in the last {days} days");
        }
        entries
            .into_iter()
            .map(|(t, project, summary)| {
                let stamp = DateTime::from_timestamp(t, 0)
                    .map(|dt| DateTime::<Local>::from(dt).format("%a %d %b %H:%M").to_string())
                    .unwrap_or_default();
                format!("{stamp}  {project} — {summary}")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn tool_search(&mut self, query: &str, limit: usize) -> Result<String, String> {
        use prefrontal_core::search as ps;
        if query.trim().is_empty() {
            return Err("query must not be empty".into());
        }
        let dir = ps::default_index_dir().ok_or("no data dir")?;
        let (index, fields) =
            ps::open_readonly(&dir).map_err(|e| format!("{e:#}"))?;
        let dirs = self
            .projects()
            .iter()
            .map(|p| (p.name.clone(), PathBuf::from(&p.path)))
            .collect();
        let hits = ps::search(&index, fields, query, limit, &dirs).map_err(|e| format!("{e:#}"))?;
        if hits.is_empty() {
            return Ok(format!("no hits for \"{query}\""));
        }
        Ok(serde_json::to_string_pretty(&hits).unwrap_or_default())
    }

    fn tool_list_docs(&mut self, project: &str) -> Result<String, String> {
        let dir = self
            .project_dir(project)
            .ok_or_else(|| format!("unknown project: {project}"))?;
        let docs = prefrontal_core::list_docs(&dir);
        Ok(serde_json::to_string_pretty(&docs).unwrap_or_default())
    }

    fn tool_read_doc(&mut self, project: &str, path: &str) -> Result<String, String> {
        let dir = self
            .project_dir(project)
            .ok_or_else(|| format!("unknown project: {project}"))?;
        prefrontal_core::read_doc(&dir, path)
            .map(|(raw, _)| raw)
            .map_err(|e| format!("{e:#}"))
    }

    fn tool_colony_status(&mut self) -> Result<String, String> {
        if !self.cfg.colony.enabled {
            return Err("colony panel disabled in the config (colony.enabled = false)".into());
        }
        let cfg = self.cfg.clone();
        let colony = prefrontal_core::colony_status(&cfg, self.projects());
        Ok(serde_json::to_string_pretty(&colony).unwrap_or_default())
    }

    fn tool_write_doc(&mut self, project: &str, path: &str, content: &str) -> Result<String, String> {
        let dir = self
            .project_dir(project)
            .ok_or_else(|| format!("unknown project: {project}"))?;
        let result = prefrontal_core::write_doc(&dir, path, content).map_err(|e| format!("{e:#}"))?;
        Ok(serde_json::to_string_pretty(&result).unwrap_or_default())
    }
}

fn tool_definitions() -> Vec<Value> {
    let obj = |props: Value, required: &[&str]| {
        json!({ "type": "object", "properties": props, "required": required })
    };
    vec![
        json!({
            "name": "list_projects",
            "description": "Every project under the roots: activity state, languages, branch, dirty count, health flags, tagline. Start here to see what exists.",
            "inputSchema": obj(json!({}), &[]),
        }),
        json!({
            "name": "project_status",
            "description": "Full detail for one project, including its recent commits.",
            "inputSchema": obj(json!({ "project": { "type": "string", "description": "project directory name" } }), &["project"]),
        }),
        json!({
            "name": "where_was_i",
            "description": "Cross-project commit timeline, newest first — what the human was working on recently.",
            "inputSchema": obj(json!({ "days": { "type": "integer", "description": "look-back window, default 14" } }), &[]),
        }),
        json!({
            "name": "search",
            "description": "Full-text search across code, docs, and commit messages of ALL projects. Use before writing something that may already exist.",
            "inputSchema": obj(json!({
                "query": { "type": "string" },
                "limit": { "type": "integer", "description": "max hits, default 20" }
            }), &["query"]),
        }),
        json!({
            "name": "list_docs",
            "description": "Markdown/text docs inside a project (README first).",
            "inputSchema": obj(json!({ "project": { "type": "string" } }), &["project"]),
        }),
        json!({
            "name": "read_doc",
            "description": "Raw markdown of one doc.",
            "inputSchema": obj(json!({
                "project": { "type": "string" },
                "path": { "type": "string", "description": "path relative to the project root" }
            }), &["project", "path"]),
        }),
        json!({
            "name": "write_doc",
            "description": "Create or update a markdown note; Prefrontal commits it locally ([prefrontal] prefix, that file only). Never pushes.",
            "inputSchema": obj(json!({
                "project": { "type": "string" },
                "path": { "type": "string" },
                "content": { "type": "string" }
            }), &["project", "path", "content"]),
        }),
        json!({
            "name": "colony_status",
            "description": "Which sibling -RS services are installed on this machine, whether each is live RIGHT NOW (loopback probe), and how to reach it: web UI URL, HTTP API port, MCP server name, or CLI binary. Ask this before assuming a sibling like Cerebro or Imaginarium is (or isn't) running.",
            "inputSchema": obj(json!({}), &[]),
        }),
    ]
}
