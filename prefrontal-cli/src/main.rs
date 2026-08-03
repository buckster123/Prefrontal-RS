mod mcp;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use prefrontal_core::{scan_all, Config};
use prefrontal_protocol::{Activity, HealthFlag, Project};

#[derive(Parser)]
#[command(name = "prefrontal", about = "Executive function as a service", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Every project, newest-touched first
    Status {
        /// Emit the raw protocol JSON instead of a table
        #[arg(long)]
        json: bool,
    },
    /// Only projects with health flags (rot check)
    Health,
    /// Where was I — recent commits across all projects, grouped by day
    Timeline,
    /// Full-text search across code, docs, and commit messages
    Find {
        /// Search terms
        query: Vec<String>,
        #[arg(long, default_value_t = 20)]
        limit: usize,
    },
    /// Which -RS siblings are installed and live on this machine
    Colony {
        /// Emit the raw protocol JSON instead of a table
        #[arg(long)]
        json: bool,
    },
    /// Serve the MCP stdio server (for agents: claude mcp add prefrontal -- prefrontal mcp)
    Mcp,
    /// Semantic recall via the CerebroCortex layer (needs features.cerebro)
    Recall {
        query: Vec<String>,
    },
    /// Push/update every project's summary into the cortex
    CortexSync,
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = Config::load()?;
    if matches!(cli.command, Command::Mcp) {
        return mcp::run(cfg); // scans lazily per tool call, not up front
    }
    let projects = scan_all(&cfg);

    match cli.command {
        Command::Status { json } => {
            if json {
                println!("{}", serde_json::to_string_pretty(&projects)?);
            } else {
                print_table(&projects);
            }
        }
        Command::Health => {
            let flagged: Vec<&Project> =
                projects.iter().filter(|p| !p.health.is_empty()).collect();
            if flagged.is_empty() {
                println!("all clear — nothing rotting");
            } else {
                for p in flagged {
                    println!("{:<28} {}", p.name, flags_str(&p.health));
                }
            }
        }
        Command::Timeline => print_timeline(&projects),
        Command::Find { query, limit } => find(&projects, &query.join(" "), limit)?,
        Command::Colony { json } => {
            if !cfg.colony.enabled {
                anyhow::bail!("colony panel disabled in the config (colony.enabled = false)");
            }
            let colony = prefrontal_core::colony_status(&cfg, &projects);
            if json {
                println!("{}", serde_json::to_string_pretty(&colony)?);
            } else {
                print_colony(&colony);
            }
        }
        Command::Mcp => unreachable!("handled before the scan"),
        Command::Recall { query } => {
            let mut client = cortex_client(&cfg)?;
            for h in client.recall(&query.join(" "), cfg.cortex.top_k)? {
                let score = h.score.map(|s| format!(" ({s:.2})")).unwrap_or_default();
                println!("[{}]{score} {}", h.agent_id, h.content.replace('\n', "\n  "));
                println!();
            }
        }
        Command::CortexSync => {
            let mut client = cortex_client(&cfg)?;
            let (mut created, mut updated) = (0, 0);
            for p in &projects {
                match client.sync_project(p)? {
                    true => created += 1,
                    false => updated += 1,
                }
                println!("  synced {}", p.name);
            }
            println!("cortex sync done — {created} created, {updated} updated");
        }
    }
    Ok(())
}

fn cortex_client(cfg: &Config) -> Result<prefrontal_core::cortex::CortexClient> {
    if !cfg.features.cerebro {
        anyhow::bail!("cortex layer disabled — set features.cerebro = true and [cortex] command in the config");
    }
    prefrontal_core::cortex::CortexClient::spawn(&cfg.cortex)
}

fn find(projects: &[Project], query: &str, limit: usize) -> Result<()> {
    use prefrontal_core::search as ps;
    if query.trim().is_empty() {
        println!("give me something to look for");
        return Ok(());
    }
    let dir = ps::default_index_dir().context("no data dir")?;
    let (index, fields) = ps::open_readonly(&dir)?;
    let dirs = projects
        .iter()
        .map(|p| (p.name.clone(), std::path::PathBuf::from(&p.path)))
        .collect();
    let hits = ps::search(&index, fields, query, limit, &dirs)?;
    if hits.is_empty() {
        println!("nothing found for \"{query}\"");
        return Ok(());
    }
    for h in hits {
        let loc = match (h.kind.as_str(), h.line) {
            ("commit", _) => format!("commit {}", h.path),
            (_, Some(line)) => format!("{}:{}", h.path, line),
            _ => h.path.clone(),
        };
        println!("{:<24} {:<6} {}", h.project, h.kind, loc);
        println!("{:24} {:6} └ {}", "", "", h.snippet);
    }
    Ok(())
}

fn print_timeline(projects: &[Project]) {
    use chrono::{DateTime, Datelike, Local};

    let mut entries: Vec<(&str, &prefrontal_protocol::CommitSummary)> = projects
        .iter()
        .flat_map(|p| {
            p.git
                .iter()
                .flat_map(|g| g.recent_commits.iter())
                .map(move |c| (p.name.as_str(), c))
        })
        .collect();
    entries.sort_by_key(|(_, c)| std::cmp::Reverse(c.time_unix));
    if entries.is_empty() {
        println!("no commits in the timeline window");
        return;
    }

    let today = Local::now().date_naive();
    let mut day = None;
    for (project, c) in entries {
        let Some(dt) = DateTime::from_timestamp(c.time_unix, 0) else { continue };
        let local: DateTime<Local> = dt.into();
        let date = local.date_naive();
        if day != Some(date) {
            day = Some(date);
            let label = match (today - date).num_days() {
                0 => "today".to_string(),
                1 => "yesterday".to_string(),
                _ => format!("{} {} {}", local.weekday(), date.day(), local.format("%b")),
            };
            println!("\n{label}");
        }
        println!("  {}  {:<24} {}", local.format("%H:%M"), project, c.summary);
    }
}

fn print_colony(colony: &prefrontal_protocol::ColonyStatus) {
    use prefrontal_protocol::{Sibling, SiblingSurface};

    let installed =
        |s: &Sibling| s.checkout.is_some() || s.binary.is_some() || s.live == Some(true);
    let surface = |s: SiblingSurface| match s {
        SiblingSurface::WebUi => "web ui",
        SiblingSurface::HttpApi => "api",
        SiblingSurface::Mcp => "mcp",
        SiblingSurface::Cli => "cli",
        SiblingSurface::Native => "native",
        SiblingSurface::NoRuntime => "—",
    };

    println!("{:<20} {:<8} {:<14} {:<6} REACH", "SIBLING", "SURFACE", "INSTALLED", "LIVE");
    for s in &colony.siblings {
        let how = match (&s.checkout, &s.binary) {
            (Some(_), Some(_)) => "checkout+bin".to_string(),
            (Some(_), None) => "checkout".to_string(),
            (None, Some(_)) => "binary".to_string(),
            (None, None) if s.live == Some(true) => "port only".to_string(),
            (None, None) => "—".to_string(),
        };
        let live = match s.live {
            Some(true) => "up",
            Some(false) => "down",
            None => "·",
        };
        let mut parts: Vec<String> = Vec::new();
        if installed(s) {
            match (&s.url, s.port) {
                (Some(url), _) => parts.push(url.clone()),
                (None, Some(p)) => parts.push(format!("http://127.0.0.1:{p} (api)")),
                _ => {}
            }
            if let Some(mcp) = &s.mcp {
                parts.push(format!("mcp: {mcp}"));
            }
            if parts.is_empty() {
                if let Some(bin) = &s.binary {
                    parts.push(bin.clone());
                }
            }
        } else {
            parts.push(format!("not here → {}", s.lander));
        }
        let reach = if parts.is_empty() { "—".to_string() } else { parts.join(" · ") };
        println!("{:<20} {:<8} {:<14} {:<6} {}", s.name, surface(s.surface), how, live, reach);
    }
}

fn print_table(projects: &[Project]) {
    println!(
        "{:<28} {:<8} {:<12} {:<24} {:>5} {:>8}  FLAGS",
        "PROJECT", "STATE", "LANGS", "BRANCH", "DIRTY", "TOUCHED"
    );
    for p in projects {
        let branch = p.git.as_ref().and_then(|g| g.branch.clone()).unwrap_or_default();
        let dirty = p
            .git
            .as_ref()
            .and_then(|g| g.dirty_files)
            .map(|d| d.to_string())
            .unwrap_or_else(|| "-".into());
        println!(
            "{:<28} {:<8} {:<12} {:<24} {:>5} {:>8}  {}",
            p.name,
            activity_str(p.activity),
            p.languages.join(","),
            truncate(&branch, 24),
            dirty,
            ago(p.last_touched_unix),
            flags_str(&p.health),
        );
    }
}

fn activity_str(a: Activity) -> &'static str {
    match a {
        Activity::Active => "active",
        Activity::Warm => "warm",
        Activity::Cold => "cold",
        Activity::Parked => "parked",
        Activity::Archived => "archived",
    }
}

fn flags_str(flags: &[HealthFlag]) -> String {
    flags
        .iter()
        .map(|f| match f {
            HealthFlag::NoGit => "no-git".to_string(),
            HealthFlag::NoRemote => "no-remote".to_string(),
            HealthFlag::NeverCommitted => "never-committed".to_string(),
            HealthFlag::DirtyPile { count } => format!("dirty-pile({count})"),
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn ago(unix: i64) -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    let days = (now - unix).max(0) / 86_400;
    match days {
        0 => "today".into(),
        1 => "1d ago".into(),
        d if d < 60 => format!("{d}d ago"),
        d => format!("{}mo ago", d / 30),
    }
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(max - 1).collect::<String>())
    }
}
