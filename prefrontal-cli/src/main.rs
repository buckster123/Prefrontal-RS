use anyhow::Result;
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
    }
    Ok(())
}

fn print_table(projects: &[Project]) {
    println!(
        "{:<28} {:<8} {:<12} {:<24} {:>5} {:>8}  {}",
        "PROJECT", "STATE", "LANGS", "BRANCH", "DIRTY", "TOUCHED", "FLAGS"
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
