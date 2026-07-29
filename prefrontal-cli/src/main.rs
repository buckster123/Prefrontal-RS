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
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let cfg = Config::load()?;
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
    }
    Ok(())
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
