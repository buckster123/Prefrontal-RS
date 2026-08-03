# Prefrontal-RS — Project Charter

> **Executive function as a service.** A fully local, live-updating dashboard over `~/Projects`
> — the first stop after being AFK. An AuDHD prosthetic named after the hardware it's prosthetizing.

*Charter locked 2026-07-29. Changes to locked decisions get a dated entry in the Decisions Log below.*

---

## The problem

~48 projects in `~/Projects` across several eras (active Rust cluster, Quest/VR-Godot era,
Python LLM-infra era). Three recurring failure modes this tool exists to kill:

1. **Lost thread** — back from AFK, no idea where work stopped or in which project.
2. **Lost work** — uncommitted piles and remoteless repos nobody is watching
   (found on day one: 44 dirty files + no remote in one repo, 107 commits with no remote in another,
   9 folders with no git at all).
3. **Lost code** — "I've already written this somewhere" → re-implementing instead of finding
   the existing function buried in a forgotten project.

## What it is

A daemon that watches the projects root, understands git, and serves a live dashboard:
browse projects, read/edit/create markdown in-app, search code and docs across everything,
and expose the same brain to agents via MCP + CLI.

## What it is not (non-goals)

- Not a kanban/PM tool, no tickets, no time tracking.
- No cloud, no accounts, no telemetry. `127.0.0.1` only.
- Not an IDE. Editing is for notes/docs; code editing stays in real editors.
- No Electron, no bundled Chromium. The web UI is plain static assets served by the daemon.

---

## Locked decisions

| # | Decision | Rationale |
|---|----------|-----------|
| D1 | **Name: Prefrontal-RS** | Slots into the brain-region series (Occipital, CerebroCortex); the prefrontal cortex *is* executive function. |
| D2 | **House architecture**: daemon + shared protocol crate + thin frontends over WebSocket | Same shape as ApexOS-RS (`agentd` / `apexos-protocol` / `ui-slint`). Proven in-ecosystem. |
| D3 | **Web frontend is the daily driver**; Slint frontend is a later, ApexOS-native port | Main use is on the laptop via browser. Rich markdown editing is web-territory. |
| D4 | **Slint frontend is text-only where it must be** | No markdown widget in Slint; dashboard/browse/read is its wheelhouse, editing stays web-side. We compromise in Slint, never in web. |
| D5 | **Central config**, not per-project dotfiles | One file owned by the dashboard (`~/.config/prefrontal/config.toml`). Projects stay unpolluted; works on repos you don't own. |
| D6 | **CerebroCortex-RS integration is optional and feature-flagged** (`features.cerebro`, default off) | Must be useful to people without a Cerebro on their system if this goes public. Core search (tantivy + tree-sitter) has zero Cerebro dependency. |
| D7 | **Pure-Rust backend** (`gix`, not libgit2; tantivy; tree-sitter) | ApexOS ethos: one toolchain, no C library linking pain. |
| D8 | **Zero config to first paint** | Point it at a root (default `~/Projects`), get a dashboard. Overrides are opt-in polish. |
| D9 | **The dash commits notes itself** — local commit always (`[prefrontal]` prefix), push is optional/configurable | Idea-capture must not depend on remembering to commit. |

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│ prefrontald                                             │
│  scanner ── git (gix) ── watcher (notify)               │
│  index (tantivy) ── symbols (tree-sitter)               │
│  notes engine (md + auto-commit)                        │
│  [feature: cerebro] ── CerebroCortex-RS client          │
│  axum: REST + WS (:7320) + static ui-web                │
└──────────────┬──────────────────────┬───────────────────┘
               │ ws/http              │ ws
        ┌──────┴──────┐        ┌──────┴──────┐
        │   ui-web    │        │  ui-slint   │  (phase 5)
        │ daily driver│        │ ApexOS-native│
        └─────────────┘        └─────────────┘
   prefrontal (CLI) ── same core as lib + MCP stdio server
```

- **`prefrontal-protocol`** — wire/domain types (`Project`, `Activity`, `HealthFlag`, `Event`).
  Frontends deserialize into the *same* enum the daemon serializes from — no string matching.
- **`prefrontal-core`** — config, scanner, git logic as a library. Daemon *and* CLI consume it,
  so the CLI works even when the daemon isn't running.
- **`prefrontald`** — the daemon. Port **7320** ("PFC" on a phone keypad).
- **`prefrontal-cli`** — binary `prefrontal`. Human subcommands now, MCP stdio server in phase 4.
- **`ui-web`** — static assets, no build step, no CDN. Vendored libs only (CodeMirror later).

## Derived project model

Everything below is computed, never hand-maintained:

- **Activity** (from last commit / fs mtime, thresholds configurable):
  `active` < 7d · `warm` < 30d · `cold` < 180d · `parked` ≥ 180d · `archived` (manual override only)
- **Health flags**: `no_git` · `no_remote` · `never_committed` · `dirty_pile` (≥ 10 dirty files)
- **Languages** from manifests (Cargo.toml, package.json, pyproject/requirements, project.godot — root + one level deep)
- **Tagline** from README (first `###` heading or first paragraph), overridable centrally

## Phases

| Phase | Scope | Done when |
|-------|-------|-----------|
| **1. Pulse** | Scanner, health flags, activity states, web dashboard with card grid + health panel + "where was I" timeline (cross-project recent commits). WS live updates via file watcher. | Opening `localhost:7320` after a week AFK answers "where was I" in one screen. |
| **2. Notes** | Markdown render (view any md in any project), edit + create notes, auto-commit with `[prefrontal]` prefix. `plan_drafts/` and `docs/` become first-class citizens. | Idea captured in the dash lands as a local commit without touching a terminal. |
| **3. Recall** | tantivy full-text over code/docs/commit messages; tree-sitter symbol index (`fn`/`struct`/`class` cards across all projects). Incremental via watcher. | "resampler" finds `fn resample_audio` in a project you forgot existed, in <1s. |
| **4. Agents** | MCP stdio server in the CLI: `project_list`, `project_status`, `where_was_i`, `search_code`, `find_symbol`, `read_doc`, `write_doc`. | A Claude session answers "do we already have X?" from Prefrontal instead of grepping. |
| **5. Slint** | `ui-slint` over the same WS protocol: dashboard, browse, read (text-rendered md). No editing. | Runs on a pure ApexOS setup with no browser. |
| **6. Cortex** *(optional, feature-flag)* | Ingest project summaries + docs into CerebroCortex-RS; semantic "that thing where I…" queries alongside lexical search. | Vague memory queries beat grep. |

## Open questions (not blocking phase 1)

- Local-LLM "you were mid-way through X" resume blurbs (ecosystem inference stack) — nice-to-have, phase 6+.
- Multiple roots (e.g. add `~/Projects-archive`) — config supports a list from day one, UI grouping TBD.
- Push policy for auto-committed notes (never / ask / always-per-project) — default **never push**, revisit in phase 2.

## Decisions log

- **2026-07-29** — Charter locked (D1–D9). Scaffold created.
- **2026-07-29** — Phase 1 "Pulse" complete: watcher + WS deltas, health drawer,
  where-was-I timeline (web + `prefrontal timeline`), WS auto-reconnect.
- **2026-07-29** — Phase 2 "Notes" complete: doc list/read/write API (traversal-safe,
  comrak server-side render), project panel UI (browse/read/edit/create, Ctrl+S),
  pathspec-scoped auto-commit `[prefrontal] note: <path>` — first note committed
  through the flow was `notes/phase-3-ideas.md`, by the feature itself.
- **2026-07-29** — Phase 2 polish: `/raw` asset route (docs show their images),
  relative .md links navigate in-panel.
- **2026-07-29** — Phase 3 "Recall" core shipped: tantivy full-text over code +
  docs + commit messages (index at `~/.local/share/prefrontal/index`, built in
  background at startup, kept live by the watcher; ~23k docs / 47 projects /
  ~17s build / ~3ms queries). `/api/search`, live search UI under the filter
  box, `prefrontal find`. **Still open in phase 3:** tree-sitter symbol cards
  (structured fn/struct/class search) — full-text already finds symbols by text.
- **2026-07-29** — Phase 3 complete: symbol cards shipped. **Amendment:** extraction
  is pure-Rust regex per language (rust/py/js·ts/gd/go/sh), not tree-sitter —
  tree-sitter's C runtime is exactly the linking pattern D7 avoids; revisit only
  if precision demands real parsing. ~230k symbol docs (name+kind indexed,
  signature+line stored); a name query ranks the declaration above its file.
- **2026-07-29** — Phase 5 "Slint" shipped: `ui-slint` crate (binary `prefrontal-ui`)
  over the same WS/REST surface — dashboard with activity dots + filter,
  where-was-I pane, per-project doc reader (plain text per D4, no editing).
  Same palette as ui-web, WS reconnect with backoff. Default winit backend;
  add `backend-linuxkms-noseat` for pure-ApexOS KMS/DRM.
- **2026-07-29** — Phase 6 "Cortex" complete (feature-flagged per D6, default off):
  `[cortex]` config points at any cerebro MCP binary; Prefrontal speaks MCP as a
  *client* (mirror of phase 4's server). One semantic summary per project,
  tag-deduped upsert (`prefrontal` + `project:<name>`, visibility shared),
  recall via `/api/cortex?q=`, the 🧠 semantic-recall UI section, and
  `prefrontal recall` / `cortex-sync`. Validated: "the project where the agent
  browses the web politely" → Occipital-RS top hit.
- **2026-07-29** — Phase 4 "Agents" complete: `prefrontal mcp` — hand-rolled MCP
  stdio server (newline-delimited JSON-RPC, no SDK dep), 7 tools
  (`list_projects`, `project_status`, `where_was_i`, `search`, `list_docs`,
  `read_doc`, `write_doc`), daemon-independent (direct scan, 10s cache).
  Registered for Claude Code via `~/Projects/.mcp.json` (release binary).
- **2026-08-03** — Colony panel shipped (idea → survey → live checks → code,
  same day: `docs/ideas/colony-panel.md` holds the receipts). Built-in roster
  of the twelve -RS siblings in `core/colony.rs`; detection is an OR of
  independent signals — checkout under a scan root, binary in PATH/known
  install dirs, open loopback port — because dev installs proved any one can
  be the whole story (apexrouter: live on `:8888`, no checkout). `[colony]`
  config: enabled / probe interval / port overrides — ports only, hosts
  hard-wired to 127.0.0.1 so probing can never be configured into LAN
  scanning. Daemon sweeps (15 s default), suppresses equal sweeps via
  protocol `PartialEq`, broadcasts `colony` WS events; connect sequence is
  now Snapshot + Colony. Surfaces: `/api/colony`, ui-web drawer,
  `prefrontal colony`, MCP `colony_status` (8th tool), SDK `colony()`.
  NeuralSymphony-RS stays outside the roster deliberately (planning stage).
