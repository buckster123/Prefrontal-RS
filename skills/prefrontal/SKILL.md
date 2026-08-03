---
name: prefrontal
description: Query and update the user's project garden via Prefrontal-RS — USE BEFORE writing any function/tool/code that might already exist in their projects root, when asked "where was I", "what was I working on", "which projects...", "do we have...", project status/health questions, or to save a note/idea into a project. Works via MCP tools, CLI, or REST.
---

# Prefrontal — the project-garden brain

Prefrontal-RS indexes every project under the user's projects root (git state,
activity, docs, full-text + symbol search, commit timeline). It answers three
questions: **Where was I? What's rotting? Didn't I already write this?**
Always check it before writing something that may already exist.

## Pick your surface (in order of preference)

1. **MCP tools** (if `mcp__prefrontal__*` are loaded): `list_projects`,
   `project_status`, `where_was_i`, `search`, `list_docs`, `read_doc`,
   `write_doc`, `colony_status`. Daemon-independent — they work even if
   nothing is running.
2. **CLI** (daemon not required):
   ```sh
   prefrontal status              # every project: state, branch, dirty, flags
   prefrontal health              # only flagged projects (rot check)
   prefrontal timeline            # where-was-I, commits grouped by day
   prefrontal find <terms…>       # full-text + symbols across everything
   prefrontal colony              # -RS siblings: installed / live / reach
   prefrontal recall <words…>     # semantic recall (optional cortex layer)
   ```
3. **REST** — daemon at `http://127.0.0.1:7320`
   (check: `curl -sf 127.0.0.1:7320/api/projects`):
   `GET /api/projects` · `GET /api/colony` · `GET /api/search?q=&limit=`
   · `GET /api/docs/{project}` · `GET/PUT /api/doc/{project}/{path}`
   · `GET /api/cortex?q=` (503 = feature off) · `POST /api/rescan`.
   Full reference: `docs/API.md` in the repo.

## Knowledge you need

- **Projects are addressed by directory name** — get names from
  `list_projects` first; never guess or send filesystem paths.
- **Search** covers code, docs, commit messages, AND symbol cards (one doc per
  `fn`/`class`/`struct` declaration — a name query ranks the declaration
  first). Hit kinds: `code | doc | commit | symbol`; file hits carry
  `path:line`. Empty result means it genuinely isn't there — a file watcher
  keeps the index live.
- **`search` is lexical; `recall`/`/api/cortex` is semantic** (vague,
  memory-shaped queries). Recall requires the optional CerebroCortex layer
  (`features.cerebro`); a 503 means it's off — fall back to `search`.
- **`write_doc` / PUT doc**: creates or updates `.md`/`.markdown`/`.txt` only,
  relative paths, no `..`. It **auto-commits locally** with message
  `[prefrontal] note: <path>` — pathspec-scoped, so it can never sweep up other
  staged work — and **never pushes**. In a non-git folder it saves and honestly
  reports `committed: false`.
- **Activity states** (derived): active <7d, warm <30d, cold <180d, parked
  beyond; `archived` only via config override. **Health flags**: `no_git`,
  `no_remote`, `never_committed`, `dirty_pile(n)` — surface these when the
  user risks losing work.
- **`colony_status` answers "is <sibling> running?"** for the -RS family
  (Cerebro, Imaginarium, ApexRouter, Callosum, …): installed (checkout,
  binary, or answering port — independent ORs), live right now (loopback
  probe), and how to reach each (web UI URL / API port / MCP name / CLI).
  Check it before assuming a sibling service is up or absent.
- Config: `~/.config/prefrontal/config.toml` (central — no per-project
  dotfiles). Dashboard: `http://127.0.0.1:7320`.

## Patterns

- "Do we already have X?" → `search` X, check symbol hits first, then code/doc.
- "Where was I / catch me up" → `where_was_i` (or `timeline`); lead with the
  most recent 1–2 projects, not the full dump.
- Filing a design/idea for project P → `write_doc` to
  `design/ideas/<topic>.md` or `notes/<date>-<topic>.md`; report the commit id.
- Before recommending "create/init/setup" of anything, check `project_status`
  — it may exist, parked.
