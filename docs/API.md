# Prefrontal-RS — API reference

Everything the dashboard does goes through this surface, so everything the
dashboard does, your code can do too. Four ways in:

| Surface | Transport | For |
|---|---|---|
| REST + WebSocket | `http://127.0.0.1:7320` | apps, scripts, the web UI |
| `prefrontal-client` | Rust crate | Rust programs (thin wrapper over REST/WS) |
| `prefrontal` CLI | terminal | humans in a shell |
| `prefrontal mcp` | MCP stdio | AI agents |

All types below live in the `prefrontal-protocol` crate — every surface
serializes/deserializes the same structs. The daemon binds loopback only.

---

## REST

### `GET /api/projects` → `Project[]`

Every project, newest-touched first, served from the warm cache (the file
watcher keeps it current; no scan happens per request).

```jsonc
{
  "name": "Occipital-RS",
  "path": "/home/you/Projects/Occipital-RS",
  "languages": ["rust"],
  "activity": "active",            // active | warm | cold | parked | archived
  "git": {
    "branch": "main",
    "last_commit_unix": 1785340000,
    "dirty_files": 0,
    "commit_count": 45,
    "remote": "git@github.com:you/Occipital-RS.git",
    "recent_commits": [ { "id": "9cf767e6", "summary": "…", "time_unix": 0 } ]
  },
  "tagline": "The agent's reading cortex…",
  "tags": [],
  "health": [ { "flag": "dirty_pile", "count": 44 } ],  // no_git | no_remote | never_committed | dirty_pile
  "last_touched_unix": 1785340000,
  "has_readme": true,
  "has_claude_md": true
}
```

### `POST /api/rescan` → `Project[]`

Full rescan now; also broadcasts a fresh `snapshot` to every WS client.
The escape hatch — normally the watcher makes this unnecessary.

### `GET /api/search?q=<query>&limit=<n>` → `SearchHit[]`

Full-text over code, docs, commit messages, **and symbol cards** (one tiny
document per `fn`/`struct`/`class`/`def` declaration). `limit` caps at 100,
default 30.

```jsonc
{
  "project": "Prefrontal-RS",
  "path": "prefrontald/src/watch.rs",  // file path, or short commit id for kind=commit
  "kind": "symbol",                    // code | doc | commit | symbol
  "line": 110,                         // 1-based; null for commits
  "snippet": "async fn debounce_loop(",
  "score": 14.2
}
```

### `GET /api/cortex?q=<query>` → `CortexHit[]`

Semantic recall through the optional CerebroCortex layer. `503` when
`features.cerebro` is off — treat that as "feature absent", not an error.

### `POST /api/cortex/sync` → `{ created, updated }`

Upsert one semantic summary per project into the cortex (tag-deduped:
`prefrontal` + `project:<name>`).

### Docs & notes

| Route | Returns |
|---|---|
| `GET /api/docs/{project}` | `DocEntry[]` — md/markdown/txt files, README first |
| `GET /api/doc/{project}/{path}` | `DocContent` — `raw` + sanitized `html` |
| `PUT /api/doc/{project}/{path}` body `{"content": "…"}` | `DocWriteResult` |
| `GET /raw/{project}/{path}` | image bytes (doc-referenced assets) |

Writes auto-commit **that file only** (`git commit -- <path>`, message
`[prefrontal] note: <path>`) and never push. `DocWriteResult` reports honestly:

```jsonc
{ "saved": true, "committed": false, "commit_id": null, "detail": "not a git repository" }
```

Path rules (all doc/raw routes): relative, no `..`, extension allow-listed,
symlink-escape checked. Projects are addressed by name and resolved only
through the daemon's own scan — clients never send filesystem paths.

---

## WebSocket — `GET /ws`

JSON frames, tagged by `type`:

| Frame | When |
|---|---|
| `{"type":"snapshot","projects":[…]}` | on connect, and after `/api/rescan` |
| `{"type":"project_changed","project":{…}}` | a project's visible state changed on disk |
| `{"type":"project_removed","path":"…"}` | a project directory disappeared |

Deltas are debounced (~600 ms of quiet per project) and suppressed when a
rescan changes nothing the dashboard shows. On reconnect, the fresh snapshot
covers anything missed — clients never need replay logic.

---

## Rust SDK — `prefrontal-client`

```rust
let pf = prefrontal_client::Prefrontal::default();      // 127.0.0.1:7320
let projects = pf.projects().await?;
let hits = pf.search("resample", 10).await?;
let doc = pf.read_doc("Prefrontal-RS", "docs/CHARTER.md").await?;
pf.write_doc("my-project", "notes/idea.md", "# it begins").await?;  // auto-commits

let mut events = std::pin::pin!(pf.events().await?);    // live deltas
while let Some(ev) = events.next().await { /* … */ }
```

See `prefrontal-client/examples/watch.rs`.

---

## MCP — `prefrontal mcp`

Stdio server, newline-delimited JSON-RPC 2.0, daemon-independent (tools scan
directly, cached 10 s). Register:

```sh
claude mcp add prefrontal -- /path/to/prefrontal mcp
```

| Tool | Args | Returns |
|---|---|---|
| `list_projects` | — | every project, compact JSON |
| `project_status` | `project` | full detail incl. recent commits |
| `where_was_i` | `days?` (14) | commit timeline, newest first |
| `search` | `query`, `limit?` (20) | `SearchHit[]` |
| `list_docs` | `project` | doc paths |
| `read_doc` | `project`, `path` | raw markdown |
| `write_doc` | `project`, `path`, `content` | write + local auto-commit result |

Tool failures come back as MCP `isError` results with a helpful message;
JSON-RPC errors are reserved for protocol breakage.

---

## CLI

```sh
prefrontal status [--json]   # table of everything
prefrontal health            # only flagged projects (rot check)
prefrontal timeline          # where was I, grouped by day
prefrontal find <terms…>     # full-text + symbols
prefrontal recall <words…>   # semantic (needs features.cerebro)
prefrontal cortex-sync       # upsert project summaries into the cortex
prefrontal mcp               # serve MCP on stdio
```

The CLI scans directly — it works with the daemon stopped (search reads the
shared index, built by the daemon).

---

## Configuration

`~/.config/prefrontal/config.toml`, all fields optional — see
[`config.example.toml`](../config.example.toml) for the annotated reference.
Highlights: `roots` (scan targets), `[thresholds]` (activity + dirty-pile),
`[timeline]` (window/cap), `[overrides.<name>]` (pin status, tags, hide),
`features.cerebro` + `[cortex]` (semantic layer).

The search index lives at `~/.local/share/prefrontal/index` and is a cache:
schema changes wipe and rebuild it automatically.
