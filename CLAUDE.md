# CLAUDE.md — Prefrontal-RS maintainer's brief

You are working on Prefrontal-RS: a fully local, live dashboard over the
user's projects root. Built idea-to-public in one session (2026-07-29); this
file is that session's expertise, distilled. Read `docs/CHARTER.md` before
any non-trivial change — **the decisions log (D1–D9 + dated entries) is
binding**; amend it with a dated entry when a decision changes, never
silently.

## Architecture in one breath

`prefrontald` (axum, :7320 — "PFC" on a phone keypad) holds a warm cache of
`Project`s, fed by a gix scanner and a notify watcher, and broadcasts WS
deltas. `prefrontal-core` owns all logic (scan/config/docs/search/cortex) so
the CLI works daemon-less. `prefrontal-protocol` is the wire: **frontends
deserialize the same enums the daemon serializes — never string-match, never
invent shapes**. Frontends: `ui-web` (daily driver, static, no build step, no
CDN), `ui-slint` (reading surface ONLY per D4 — no editing, no markdown
rendering), `prefrontal-cli` (humans + `mcp` stdio server for agents),
`prefrontal-client` (SDK).

## Invariants — break these and you've broken the product

- **Localhost only.** Nothing ever binds beyond 127.0.0.1; no telemetry.
- **Path inputs are hostile.** Every project-relative path goes through
  `docs::resolve_rel_path`: relative, no `..`, extension allow-listed,
  symlink-escape checked. Projects are addressed by *name*, resolved only
  through the scan cache — clients never send filesystem paths.
- **Note commits are pathspec-scoped** (`git add -- <file> && git commit
  -- <file>`, message `[prefrontal] note: <path>`) so they can NEVER sweep up
  staged work. Never push. Report `committed: false` honestly with the reason.
- **The index is a cache, never truth** — schema mismatch wipes and rebuilds
  (`search::open`). Any schema change is therefore safe but costs a rebuild.
- **Central config only** (`~/.config/prefrontal/config.toml`); no
  per-project dotfiles, ever (D5).
- **Cortex is optional** (D6): `features.cerebro` off ⇒ every cortex path is
  dark and lexical search never notices. `/api/cortex` 503s, UI goes quiet.
- **Rendered HTML is sanitized**: comrak with `render.r#unsafe = true` piped
  through `ammonia::clean` — banners/img/div survive, scripts and handlers
  must not. The dashboard origin can write files; a hostile README must never
  execute in it.
- **Pure Rust, no C linking** (D7): gix not libgit2, regex symbols not
  tree-sitter, comrak not a JS renderer. The one sanctioned exception is the
  system `git` shell-out for note commits (identity/hooks for free) — and
  Slint, confined to `ui-slint` (see LICENSE note).
- **Typography**: body text is system sans (~1.5–1.6 line-height); monospace
  strictly inside code blocks. This is an accessibility decision (owner's
  eyes), not taste.

## Where things live / how they work

- `core/scan.rs` — scanner. Activity derives from last-touch (7/30/180d);
  health flags: no_git, no_remote, never_committed, dirty_pile. Tagline =
  first `###` or first paragraph of README, HTML-stripped. `SKIP_DIRS` is THE
  shared skip list (watcher + docs walk + indexer).
- `prefrontald/watch.rs` — per-directory watches (NEVER blanket-recursive:
  `target/` would eat inotify), `.git` watched surgically (dir non-recursive
  + `refs/` recursive → commits/branch-switches register without object-store
  noise). 600 ms quiet-period debounce per project; equal rescans are
  suppressed via protocol `PartialEq`. ~4k watches on a 47-project garden.
- `core/search.rs` — one tantivy index: files (content indexed, not stored —
  snippets/line numbers read from disk at query time), commit summaries
  (1000/project via gix walk, stored), symbol cards (regex per language, one
  tiny doc per declaration; name queries outrank files naturally). Index dir:
  `~/.local/share/prefrontal/index`.
- `core/cortex.rs` — MCP stdio *client* (mirror of `cli/mcp.rs`'s server).
  Spawns `[cortex] command` (cerebro-mcp). Upserts are tag-deduped
  (`prefrontal` + `project:<name>`, shared visibility). Client respawns after
  any pipe error (daemon holds it in `Mutex<Option<_>>`).
- `cli/mcp.rs` — hand-rolled MCP server, newline JSON-RPC. Tool failures are
  MCP `isError` results with helpful text; JSON-RPC errors only for protocol
  breakage. Scans cached 10 s per process.
- WS contract: snapshot on connect covers all gaps — clients need zero replay
  logic. Keep it that way.

## Sharp edges met and filed down (don't rediscover these)

- axum 0.8: routes are `/{param}` and `/{*path}`, not `:param`.
- comrak 0.54: the field is `render.r#unsafe` (raw identifier).
- tantivy 0.26: `TopDocs::with_limit(n).order_by_score()`.
- gix 0.86: unborn HEAD ⇒ `head_commit()` errs ⇒ that's `never_committed`;
  dirty count via `status(...).into_index_worktree_iter(Vec::new())`.
- CSS: an author `display:` beats the `hidden` attribute — every element that
  toggles `hidden` and declares its own display needs a `[hidden]{display:none}`
  guard. **Render-test UI changes; curl is not enough.**
- **The MCP registration points at `target/release/prefrontal`** (in
  `~/Projects/.mcp.json`). After changing the CLI/MCP surface, run
  `cargo build --release` or agents run a stale binary.

## Workflow

```sh
cargo build --workspace && cargo clippy --workspace   # clippy-zero policy
pkill -x prefrontald; ./target/debug/prefrontald &    # run from repo root (ui-web resolves via cwd)
curl -s 127.0.0.1:7320/api/projects | head            # smoke
```

Verification style: prove features on real data (this repo flagged its own
`no-git` at birth; the first note ever committed was its own phase-3 ideas
file). WS testing: python3 + `websockets` (see session pattern: connect,
mutate a file, assert the delta). House commit voice: story-telling subject
lines; significant work updates `docs/CHARTER.md`'s log. README banners are
generated with Imaginarium-RS and credited by job id.

## Roadmap seeds (charter-consistent, unscheduled)

`notes/phase-3-ideas.md` holds search follow-ups; open questions live at the
bottom of the charter (local-LLM resume blurbs, multi-root UI, note-push
policy). The hermes twin-checkout duplicate-hit quirk is fixable with an
`[overrides] ignore` — user's call, not code's.
