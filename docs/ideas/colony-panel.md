# Idea: colony panel — which siblings are installed, and how to reach them

Filed 2026-08-03; implemented the same day (see the CHARTER decisions log).
Kept as the design brief and the survey receipts.

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

**Serves a browser UI — 3 of 12 verified (ApexRouter likely makes it 4, see below)**

| Project | Default bind | Notes |
|---|---|---|
| Prefrontal-RS | `127.0.0.1:7320` | the dashboard this panel lives in |
| Imaginarium-RS | `127.0.0.1:8791` | embedded browser studio, token auth |
| ApexOS-RS | `~:8787` | `agentd` gateway serves index/desktop/mobile.html |

**HTTP API, no browser UI**

| Project | Default bind | Notes |
|---|---|---|
| CerebroCortex-RS | `127.0.0.1:8765` | 40 JSON routes. **No dashboard** — per source survey. But the *live* daemon answers `401` at `/`, not the 404 the source read predicted — auth middleware fronts everything, so status codes can't prove UI absence |
| Callosum-RS | `0.0.0.0:8788` | mesh face + `/shim/*` on the same router |
| Occipital-RS | REST surface | the HTML in `extract.rs` is reader-mode extraction, not a UI |
| ApexRouter-RS | `127.0.0.1:8888/v1` | OpenAI-compatible proxy. Owner confirms the standalone build serves a web UI and runs it on this machine (dev install) — the original survey missed it. A blind probe couldn't find the UI path while no backend was healthy (JSON at `/`, 503 elsewhere); the roster classifies it `web_ui` |

**No HTTP surface at all:** Sonus-RS (MCP-first; the HTML hits are test
fixtures), Puerperium-RS (CLI; it talks *out* to the router on `:2739`),
Enthea-RS (native wgpu), ApexOS-RV (bare metal), Launchpad-RS (no runtime).

So a link grid would have three tiles. The honest, more useful feature is a
**colony panel**: one row per sibling showing installed / live / how to reach
it, where "reach it" is a URL for the three with UIs, and the CLI binary or
MCP server name for everything else. That covers the whole family instead of
the fraction that happens to be a web app.

## Checked against the running machine (2026-08-03, second pass)

A live probe found **all seven surveyed ports open** on loopback. Identities
confirmed where cheap: `:8888` answers `/v1/models` with `owned_by:
apexrouter` — a sibling with *no source checkout* — `:8787` serves HTML,
`:8765` answers 401 at `/`, `:8788` 404s at `/`, `:2739` speaks something
that isn't `/v1/models`. Corrections landed in the tables above and the
detection notes below.

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
- **A port being open identifies nothing.** All seven surveyed ports were open
  when spot-checked — including `:8888` for a sibling with no checkout. TCP
  connect answers "something is there"; knowing *which* something needs one
  cheap identifying request per sibling (e.g. `/v1/models` → `owned_by:
  apexrouter`; an HTML content-type on `:8787`). And treat 401/403 as live —
  auth-fronted siblings (Cerebro) reject the request and prove liveness in
  the same breath.
- **Stay loopback-only.** `CLAUDE.md` states nothing binds beyond 127.0.0.1;
  probing must not become a reason to reach across the LAN. (Note that claim
  is currently an invariant by convention, not enforced — `ServerConfig.bind`
  is a free-form String.)
- **"Not checked out" is not "not installed."** ApexRouter-RS has no source
  checkout here, yet it is installed (`~/.local/bin/apexrouter`) and live on
  `:8888`. The three signals — source dir under the scan root, binary in a
  known dir, live port — are independent ORs; any one alone can be the whole
  story. Binaries split across `/usr/local/bin` (agentd, imaginarium) and
  `~/.local/bin` (apexrouter, cerebro-mcp), so "known install dir" means
  both. And `prefrontal` itself is on neither — it runs from
  `target/release` — so the binary detector alone would miss the very host
  of this panel.
- **"Not installed" should still look intentional, not broken** — it is a
  normal state for any given machine, just rarer here than the original
  survey assumed.

## Open questions — answered 2026-08-03 during implementation

1. ~~New section, or fold into project cards?~~ **Own panel with its own
   built-in roster.** The ApexRouter finding settles it: a colony member can
   be installed with no project directory at all, so the panel can't be a
   decoration on scanned cards. Ports are overridable via `[colony]` in the
   central config (D5); where a roster entry matches a scanned project, both
   views simply coexist.
2. ~~MCP tools too?~~ **Yes — `colony_status`** (the agent asking "is Cerebro
   up?" is the stronger use case), and detection lives in `prefrontal-core`
   so `prefrontal colony` answers daemon-less.
3. ~~Where does "not installed" link?~~ **The lander** — they're documented
   as accurate; a local install command in the UI would drift. One-shot
   installers for the standalone-able siblings are a backlog item (the
   ApexOS installer's flags already pull what they need).
4. ~~NeuralSymphony-RS: thirteenth sibling?~~ **Outside the colony for now** —
   still at the planning/storming stage (a big-model suggestion that surfaced
   during hermes/ApexRouter testing on vast.ai, after the model noticed the
   garden). Revisit if it graduates to code.
   **Revisited 2026-08-04: graduated.** Idea → PRD → M0/M1/M2 in one
   session arc; it has an HTTP surface on :7664 now, so the roster grew to
   thirteen. The planning-stage rule worked exactly as written, in both
   directions.

Related: the sibling landers at `apexaurum.no/<Slug>/` each document their own
surfaces accurately as of 2026-08-03.
