//! The colony panel: which -RS siblings exist on this machine, whether each
//! is live, and how to reach it.
//!
//! Detection is an OR of independent signals — source checkout under a scan
//! root, binary in a known install dir, open loopback port — because dev
//! installs put things anywhere (found on day one: apexrouter live on `:8888`
//! with no checkout at all). The roster and taglines are built in, never
//! derived from READMEs (which have been caught claiming dashboards that
//! don't exist). Probes only ever touch 127.0.0.1: hosts are not
//! configurable, only ports are.

use std::collections::HashSet;
use std::net::{SocketAddr, TcpStream};
use std::path::PathBuf;
use std::time::Duration;

use prefrontal_protocol::{ColonyStatus, Project, Sibling, SiblingSurface};

use crate::config::{expand_tilde, Config};

/// Loopback answers instantly (open or ECONNREFUSED); the timeout only
/// guards against something pathological eating the SYN.
const PROBE_TIMEOUT: Duration = Duration::from_millis(250);

const LANDER_BASE: &str = "https://apexaurum.no";

struct Spec {
    /// Also the project directory name under a scan root.
    name: &'static str,
    tagline: &'static str,
    surface: SiblingSurface,
    /// Default loopback port; `None` = nothing to probe.
    port: Option<u16>,
    /// Binary names worth looking for, most telling first.
    binaries: &'static [&'static str],
    /// MCP server name, for siblings agents reach that way.
    mcp: Option<&'static str>,
}

/// The twelve. Surveyed from source and the running system 2026-08-03
/// (docs/ideas/colony-panel.md holds the receipts); ports are defaults only.
const ROSTER: &[Spec] = &[
    Spec {
        name: "Prefrontal-RS",
        tagline: "this dashboard — projects, notes, recall",
        surface: SiblingSurface::WebUi,
        port: Some(7320),
        binaries: &["prefrontald", "prefrontal"],
        mcp: Some("prefrontal"),
    },
    Spec {
        name: "Imaginarium-RS",
        tagline: "image & video studio",
        surface: SiblingSurface::WebUi,
        port: Some(8791),
        binaries: &["imaginarium", "imaginariumd"],
        mcp: Some("imaginarium"),
    },
    Spec {
        name: "ApexOS-RS",
        tagline: "agent OS — agentd gateway",
        surface: SiblingSurface::WebUi,
        port: Some(8787),
        binaries: &["agentd"],
        mcp: None,
    },
    Spec {
        name: "ApexRouter-RS",
        tagline: "OpenAI-compatible model router",
        surface: SiblingSurface::WebUi, // standalone builds serve a web UI
        port: Some(8888),
        binaries: &["apexrouter"],
        mcp: None,
    },
    Spec {
        name: "CerebroCortex-RS",
        tagline: "agent memory graph",
        surface: SiblingSurface::HttpApi,
        port: Some(8765),
        binaries: &["cerebro-mcp", "cerebro-api"],
        mcp: Some("cerebro-cortex"),
    },
    Spec {
        name: "Callosum-RS",
        tagline: "agent mesh + shim",
        surface: SiblingSurface::HttpApi,
        port: Some(8788),
        binaries: &["callosum", "callosumd"],
        mcp: Some("callosum"),
    },
    Spec {
        name: "Occipital-RS",
        tagline: "polite web reading & extraction",
        surface: SiblingSurface::HttpApi,
        port: None, // REST surface, no settled default port
        binaries: &["occipital"],
        mcp: None,
    },
    Spec {
        name: "Sonus-RS",
        tagline: "audio engine, MCP-first",
        surface: SiblingSurface::Mcp,
        port: None,
        binaries: &["sonus"],
        mcp: Some("sonus"),
    },
    Spec {
        name: "Puerperium-RS",
        tagline: "raises specialist models from memory",
        surface: SiblingSurface::Cli,
        port: None,
        binaries: &["puerperium"],
        mcp: None,
    },
    Spec {
        name: "Enthea-RS",
        tagline: "native wgpu experience",
        surface: SiblingSurface::Native,
        port: None,
        binaries: &["enthea"],
        mcp: None,
    },
    Spec {
        name: "ApexOS-RV",
        tagline: "bare-metal ApexOS (RISC-V)",
        surface: SiblingSurface::NoRuntime,
        port: None,
        binaries: &[],
        mcp: None,
    },
    Spec {
        name: "Launchpad-RS",
        tagline: "project scaffolding, no runtime",
        surface: SiblingSurface::NoRuntime,
        port: None,
        binaries: &["launchpad"],
        mcp: None,
    },
];

/// One full sweep: every roster sibling checked against the scan cache, the
/// install dirs, and (where there's a port) the loopback socket.
pub fn colony_status(cfg: &Config, projects: &[Project]) -> ColonyStatus {
    let install_dirs = install_dirs();
    let siblings = ROSTER
        .iter()
        .map(|spec| {
            let port = cfg.colony.ports.get(spec.name).copied().or(spec.port);
            let checkout = projects
                .iter()
                .find(|p| p.name == spec.name)
                .map(|p| p.path.clone());
            let binary = find_binary(&install_dirs, spec.binaries);
            let live = port.map(probe);
            let url = match (spec.surface, port) {
                (SiblingSurface::WebUi, Some(p)) => Some(format!("http://127.0.0.1:{p}/")),
                _ => None,
            };
            // ApexOS-RV keeps its full name; the -RS siblings drop the suffix.
            let slug = spec.name.strip_suffix("-RS").unwrap_or(spec.name);
            Sibling {
                name: spec.name.to_string(),
                tagline: spec.tagline.to_string(),
                surface: spec.surface,
                port,
                url,
                mcp: spec.mcp.map(str::to_string),
                checkout,
                binary: binary.map(|p| p.display().to_string()),
                live,
                lander: format!("{LANDER_BASE}/{slug}/"),
            }
        })
        .collect();
    let checked_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    ColonyStatus { siblings, checked_unix }
}

/// TCP connect to loopback. Any answer is liveness — an auth-fronted sibling
/// rejecting an HTTP request later still proves something is home.
fn probe(port: u16) -> bool {
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    TcpStream::connect_timeout(&addr, PROBE_TIMEOUT).is_ok()
}

/// `$PATH` plus the dirs dev installs actually land in on this machine —
/// binaries were found split across `/usr/local/bin` and `~/.local/bin`,
/// and a daemon's PATH may carry neither.
fn install_dirs() -> Vec<PathBuf> {
    let mut dirs: Vec<PathBuf> = std::env::var_os("PATH")
        .map(|p| std::env::split_paths(&p).collect())
        .unwrap_or_default();
    for extra in ["~/.local/bin", "~/.cargo/bin"] {
        dirs.push(expand_tilde(extra));
    }
    dirs.push(PathBuf::from("/usr/local/bin"));
    let mut seen = HashSet::new();
    dirs.retain(|d| seen.insert(d.clone()));
    dirs
}

fn find_binary(dirs: &[PathBuf], names: &[&str]) -> Option<PathBuf> {
    for name in names {
        for dir in dirs {
            let candidate = dir.join(name);
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &std::path::Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    path.metadata()
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &std::path::Path) -> bool {
    path.is_file()
}
