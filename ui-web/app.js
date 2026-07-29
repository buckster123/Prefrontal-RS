// Prefrontal-RS ui-web — snapshot over WS with REST fallback, client-side filter.
// Types mirror prefrontal-protocol; frontends never invent their own shapes.

"use strict";

const ACTIVITY_ORDER = ["active", "warm", "cold", "parked", "archived"];
const ACTIVITY_DOT = {
  active: "var(--act-active)",
  warm: "var(--act-warm)",
  cold: "var(--act-cold)",
  parked: "var(--act-parked)",
  archived: "transparent",
};
const LANG_DOT = {
  rust: "var(--lang-rust)",
  node: "var(--lang-node)",
  python: "var(--lang-python)",
  godot: "var(--lang-godot)",
};
// severity classes map to the reserved status palette; icon + label, never color alone
const FLAG_VIEW = {
  no_git: { label: "no git", icon: "✖", cls: "critical" },
  no_remote: { label: "no remote", icon: "☁", cls: "serious" },
  never_committed: { label: "never committed", icon: "∅", cls: "serious" },
  dirty_pile: { label: "dirty", icon: "⚠", cls: "warning" },
};

let projects = [];
// once the user opens/closes the drawer themselves, stop auto-managing it
let healthTouched = false;
let wsRetryMs = 1000;
let wsRetryTimer = null;

function flagText(f) {
  const v = FLAG_VIEW[f.flag] ?? { label: f.flag, icon: "•", cls: "warning" };
  const extra = f.flag === "dirty_pile" ? ` ×${f.count}` : "";
  return { ...v, text: `${v.icon} ${v.label}${extra}` };
}

function ago(unix) {
  const days = Math.max(0, Math.floor(Date.now() / 1000 - unix) / 86400) | 0;
  if (days === 0) return "today";
  if (days === 1) return "1d ago";
  if (days < 60) return `${days}d ago`;
  return `${Math.floor(days / 30)}mo ago`;
}

function el(tag, cls, text) {
  const n = document.createElement(tag);
  if (cls) n.className = cls;
  if (text !== undefined) n.textContent = text;
  return n;
}

function renderStats() {
  const wrap = document.getElementById("stats");
  wrap.replaceChildren();
  const dirtyTotal = projects.reduce((s, p) => s + (p.git?.dirty_files ?? 0), 0);
  const tiles = [
    { k: "projects", v: projects.length },
    { k: "active", v: projects.filter((p) => p.activity === "active").length },
    { k: "warm", v: projects.filter((p) => p.activity === "warm").length },
    { k: "flagged", v: projects.filter((p) => p.health.length).length, alert: true },
    { k: "uncommitted files", v: dirtyTotal, alert: dirtyTotal > 0 },
  ];
  for (const t of tiles) {
    const tile = el("div", "tile" + (t.alert && t.v > 0 ? " alert" : ""));
    tile.append(el("div", "v", String(t.v)), el("div", "k", t.k));
    wrap.append(tile);
  }
}

function renderHealth(list) {
  const panel = document.getElementById("health");
  const flagged = list.filter((p) => p.health.length);
  panel.hidden = flagged.length === 0;

  // collapsed summary: total + per-flag breakdown, so the alert reads at a glance
  const counts = {};
  for (const p of flagged) for (const f of p.health) counts[f.flag] = (counts[f.flag] ?? 0) + 1;
  const parts = Object.entries(counts).map(
    ([k, n]) => `${FLAG_VIEW[k]?.label ?? k} ×${n}`
  );
  document.getElementById("health-summary").textContent =
    `${flagged.length} need${flagged.length === 1 ? "s" : ""} attention — ${parts.join(" · ")}`;

  // a handful auto-opens; a wall of them stays tidy behind the drawer
  if (!healthTouched) panel.open = flagged.length > 0 && flagged.length <= 4;

  const rows = document.getElementById("health-list");
  rows.replaceChildren();
  for (const p of flagged) {
    const row = el("div", "health-row");
    row.append(el("span", "name", p.name));
    for (const f of p.health) {
      const v = flagText(f);
      row.append(el("span", `badge ${v.cls}`, v.text));
    }
    rows.append(row);
  }
}

function dayLabel(unix) {
  const d = new Date(unix * 1000);
  const today = new Date();
  const midnight = new Date(today.getFullYear(), today.getMonth(), today.getDate());
  const daysBack = Math.floor((midnight - new Date(d.getFullYear(), d.getMonth(), d.getDate())) / 86400000);
  if (daysBack <= 0) return "today";
  if (daysBack === 1) return "yesterday";
  return d.toLocaleDateString(undefined, { weekday: "short", day: "numeric", month: "short" });
}

function renderTimeline(list) {
  const panel = document.getElementById("timeline");
  const entries = [];
  for (const p of list)
    for (const c of p.git?.recent_commits ?? [])
      entries.push({ project: p.name, ...c });
  entries.sort((a, b) => b.time_unix - a.time_unix);
  panel.hidden = entries.length === 0;
  if (!entries.length) return;

  const projCount = new Set(entries.map((e) => e.project)).size;
  document.getElementById("timeline-summary").textContent =
    `where was I — ${entries.length} commits across ${projCount} project${projCount === 1 ? "" : "s"}`;

  // day → runs of consecutive same-project commits
  const wrap = document.getElementById("tl-list");
  wrap.replaceChildren();
  let day = null, dayEl = null, block = null, blockProject = null;
  for (const e of entries) {
    const label = dayLabel(e.time_unix);
    if (label !== day) {
      day = label;
      dayEl = el("div", "tl-day");
      dayEl.append(el("h3", "", label));
      wrap.append(dayEl);
      block = null;
      blockProject = null;
    }
    if (e.project !== blockProject) {
      blockProject = e.project;
      block = el("div", "tl-block");
      block.append(el("div", "tl-proj", e.project));
      dayEl.append(block);
    }
    const row = el("div", "tl-commit");
    const time = new Date(e.time_unix * 1000).toLocaleTimeString(undefined, {
      hour: "2-digit",
      minute: "2-digit",
    });
    row.append(el("span", "t", time), el("span", "msg", e.summary));
    row.title = `${e.id} — ${e.summary}`;
    block.append(row);
  }
}

function card(p) {
  const c = el("article", "card" + (p.activity === "archived" ? " archived" : ""));

  const head = el("div", "head");
  head.append(el("span", "name", p.name), el("span", "ago", ago(p.last_touched_unix)));
  c.append(head);

  if (p.tagline) c.append(el("div", "tagline", p.tagline));

  if (p.git) {
    const g = el("div", "git-line");
    if (p.git.branch) g.append(el("span", "branch", `⎇ ${p.git.branch}`));
    if (p.git.commit_count != null) g.append(el("span", "", `${p.git.commit_count} commits`));
    if (p.git.dirty_files) g.append(el("span", "dirty", `${p.git.dirty_files} dirty`));
    c.append(g);
  }

  const meta = el("div", "meta");
  for (const lang of p.languages) {
    const chip = el("span", "chip");
    const dot = el("span", "dot");
    dot.style.background = LANG_DOT[lang] ?? "var(--muted)";
    chip.append(dot, document.createTextNode(lang));
    meta.append(chip);
  }
  for (const tag of p.tags) meta.append(el("span", "chip", `#${tag}`));
  for (const f of p.health) {
    const v = flagText(f);
    meta.append(el("span", `badge ${v.cls}`, v.text));
  }
  if (meta.childElementCount) c.append(meta);
  return c;
}

function render() {
  const q = document.getElementById("filter").value.trim().toLowerCase();
  const list = !q
    ? projects
    : projects.filter((p) =>
        [p.name, p.tagline ?? "", p.languages.join(" "), p.tags.join(" ")]
          .join(" ")
          .toLowerCase()
          .includes(q)
      );

  renderStats();
  renderHealth(list);
  renderTimeline(list);

  const groups = document.getElementById("groups");
  groups.replaceChildren();
  for (const state of ACTIVITY_ORDER) {
    const members = list.filter((p) => p.activity === state);
    if (!members.length) continue;
    const section = el("section", "group");
    const h = el("h2");
    const dot = el("span", "dot");
    dot.style.background = ACTIVITY_DOT[state];
    if (state === "archived") dot.style.border = "1px solid var(--muted)";
    h.append(dot, document.createTextNode(state + " "), el("span", "count", `(${members.length})`));
    section.append(h);
    const grid = el("div", "grid");
    members.forEach((p) => grid.append(card(p)));
    section.append(grid);
    groups.append(section);
  }
  if (!list.length) groups.append(el("div", "empty", "nothing matches"));
}

function setConn(live) {
  const conn = document.getElementById("conn");
  conn.classList.toggle("live", live);
  conn.title = live ? "live (websocket)" : "snapshot (rest)";
}

function handleEvent(ev) {
  if (ev.type === "snapshot") {
    projects = ev.projects;
  } else if (ev.type === "project_changed") {
    const i = projects.findIndex((p) => p.path === ev.project.path);
    if (i >= 0) projects[i] = ev.project;
    else projects.push(ev.project);
    projects.sort((a, b) => b.last_touched_unix - a.last_touched_unix);
  } else if (ev.type === "project_removed") {
    projects = projects.filter((p) => p.path !== ev.path);
  }
  render();
}

function connectWS() {
  wsRetryTimer = null;
  let ws;
  try {
    ws = new WebSocket(`ws://${location.host}/ws`);
  } catch {
    scheduleReconnect();
    return;
  }
  ws.onopen = () => {
    setConn(true);
    wsRetryMs = 1000;
  };
  ws.onmessage = (m) => handleEvent(JSON.parse(m.data));
  ws.onclose = () => {
    setConn(false);
    scheduleReconnect(); // fresh snapshot on reconnect covers anything missed
  };
  ws.onerror = () => ws.close();
}

function scheduleReconnect() {
  if (wsRetryTimer) return;
  wsRetryTimer = setTimeout(connectWS, wsRetryMs);
  wsRetryMs = Math.min(wsRetryMs * 2, 15_000);
}

async function boot() {
  document.getElementById("filter").addEventListener("input", render);
  document.getElementById("health").addEventListener("toggle", (e) => {
    if (e.isTrusted) healthTouched = true;
  });
  setInterval(render, 60_000); // keep "Nd ago" and day labels honest while the tab sits open
  connectWS();
  // REST fallback / first paint even if WS is slow
  try {
    const res = await fetch("/api/projects");
    if (res.ok && !projects.length) {
      projects = await res.json();
      render();
    }
  } catch {
    document.getElementById("groups").replaceChildren(
      el("div", "empty", "daemon unreachable — is prefrontald running?")
    );
  }
}

boot();
