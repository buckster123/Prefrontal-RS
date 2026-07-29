<div align="center">

# Prefrontal-RS

### Executive function as a service — a fully local, live dashboard over all your projects.

*Where was I? What's rotting? Didn't I already write this?*
*One daemon, one browser tab, zero cloud.*

[![rust](https://img.shields.io/badge/100%25-Rust-orange?style=for-the-badge&logo=rust)](https://www.rust-lang.org/)
[![local](https://img.shields.io/badge/127.0.0.1-only-22c55e?style=for-the-badge)]()
[![status](https://img.shields.io/badge/status-phase_1:_pulse-f59e0b?style=for-the-badge)]()

</div>

---

Prefrontal-RS watches your projects root, understands git, and answers the three questions
an AuDHD brain asks after being AFK:

- **Where was I?** — cross-project activity timeline, resume cards, dirty-state at a glance.
- **What's rotting?** — health flags for uncommitted piles, remoteless repos, un-git'ed folders.
- **Didn't I already write this?** — full-text + symbol search across every project, live or parked.

Plus: read, edit, and create markdown notes in-app — the dash commits them locally so ideas
don't evaporate. Agents get the same brain via MCP and CLI.

## Quick start

```sh
cargo run -p prefrontald        # scans ~/Projects, serves http://127.0.0.1:7320
cargo run -p prefrontal-cli --  status   # same scan, in your terminal
```

The `prefrontal` binary also carries `health` (rot check), `timeline` (where was I),
`find <terms>` (full-text over code/docs/commits), and `mcp` — an MCP stdio server
so agents can ask the same questions:

```sh
claude mcp add prefrontal -- /path/to/prefrontal mcp
# or drop an .mcp.json next to your projects root:
# { "mcpServers": { "prefrontal": { "command": "/path/to/prefrontal", "args": ["mcp"] } } }
```

Tools: `list_projects` · `project_status` · `where_was_i` · `search` ·
`list_docs` · `read_doc` · `write_doc` (auto-commits, never pushes).

Zero config needed. To customize roots, thresholds, or per-project overrides:
copy `config.example.toml` to `~/.config/prefrontal/config.toml`.

## Layout

| Crate / dir | What |
|---|---|
| `prefrontald` | The daemon — scanner, git, axum REST + WS, serves `ui-web` |
| `prefrontal-core` | Scanner + config as a library (daemon and CLI share it) |
| `prefrontal-protocol` | Wire types — frontends deserialize the same enum the daemon serializes |
| `prefrontal-cli` | `prefrontal` binary — human subcommands now, MCP server in phase 4 |
| `ui-web` | Static web frontend, no build step, no CDN — the daily driver |
| `docs/CHARTER.md` | **The plan.** Locked decisions, phases, non-goals |

Part of the [ApexOS](https://github.com/buckster123/ApexOS-RS) ecosystem.
Sibling brain-region: [CerebroCortex-RS](https://github.com/buckster123/CerebroCortex-RS)
(optional semantic-memory backend, feature-flagged, off by default).
