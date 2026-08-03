# Idea: colony panel — which siblings are installed, and how to reach them

Filed 2026-08-03. Not started; this is the brief for a future session.

Prefrontal is the first place both Andre and an agent land, so it is the
natural front desk for the whole `-RS` colony: show which siblings are
installed on this machine, whether each is live, link straight through to
anything that has a browser UI, and point at install instructions for what is
missing.

## The finding that should shape the design

The obvious framing — *"quick links to all the web interfaces the siblings
serve"* — is smaller than it sounds. A survey of every sibling's source on
2026-08-03 (grepping bind addresses and actual HTML-serving code, **not**
READMEs):

**Serves a browser UI — 3 of 12**

| Project | Default bind | Notes |
|---|---|---|
| Prefrontal-RS | `127.0.0.1:7320` | the dashboard this panel lives in |
| Imaginarium-RS | `127.0.0.1:8791` | embedded browser studio, token auth |
| ApexOS-RS | `~:8787` | `agentd` gateway serves index/desktop/mobile.html |

**HTTP API, no browser UI**

| Project | Default bind | Notes |
|---|---|---|
| CerebroCortex-RS | `127.0.0.1:8765` | 40 JSON routes. **No dashboard** — verified against source; `GET /` 404s |
| Callosum-RS | `0.0.0.0:8788` | mesh face + `/shim/*` on the same router |
| Occipital-RS | REST surface | the HTML in `extract.rs` is reader-mode extraction, not a UI |
| ApexRouter-RS | `127.0.0.1:8888/v1` | OpenAI-compatible proxy |

**No HTTP surface at all:** Sonus-RS (MCP-first; the HTML hits are test
fixtures), Puerperium-RS (CLI; it talks *out* to the router on `:2739`),
Enthea-RS (native wgpu), ApexOS-RV (bare metal), Launchpad-RS (no runtime).

So a link grid would have three tiles. The honest, more useful feature is a
**colony panel**: one row per sibling showing installed / live / how to reach
it, where "reach it" is a URL for the three with UIs, and the CLI binary or
MCP server name for everything else. That covers the whole family instead of
the fraction that happens to be a web app.

## Design notes

- **Do not derive any of this from READMEs.** CerebroCortex's README,
  ARCHITECTURE.md, CLAUDE.md and Cargo.toml description all claim a dashboard
  that does not exist in `crates/cerebro-api/`. Detect from the running system
  or from source, never from docs.
- **Detection, roughly in order of cost:** binary on `PATH` or in a known
  install dir → project directory present under the scan root → port open on
  loopback → an actual liveness probe. Prefrontal already scans `~/Projects`,
  so "installed from source" is nearly free; "running" is the part that needs
  new work.
- **Ports must be configurable, not hardcoded.** Every default above is just a
  default. Reuse the existing `Config`/thresholds pattern rather than inventing
  a second config surface.
- **Liveness probing has to be cheap and non-blocking** — the dashboard is
  live-updating, and a synchronous probe of eight ports on every render is not
  acceptable. Cache with a TTL like the existing 10 s scan cache.
- **Stay loopback-only.** `CLAUDE.md` states nothing binds beyond 127.0.0.1;
  probing must not become a reason to reach across the LAN. (Note that claim
  is currently an invariant by convention, not enforced — `ServerConfig.bind`
  is a free-form String.)
- **ApexRouter-RS is not checked out on this machine** — a good reminder that
  "not installed" is the common case and should look intentional, not broken.

## Open questions for the charter

1. Is this a new dashboard section, or does it fold into the existing project
   cards? A sibling *is* a project Prefrontal already scans.
2. Does it get MCP tools too (`colony_status`), or is it UI-only? An agent
   asking "is Cerebro up?" is arguably the better use case than a human
   clicking a link.
3. Does "not installed" link to the lander on apexaurum.no, to the GitHub
   repo, or to a local install command? The landers now exist for all twelve.

Related: the sibling landers at `apexaurum.no/<Slug>/` each document their own
surfaces accurately as of 2026-08-03.
