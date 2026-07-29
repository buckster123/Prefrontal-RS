//! Notes/docs: list, read, write — and the write-side auto-commit (charter D9:
//! a captured idea always lands as a local commit; push is never our business).

use std::path::{Component, Path, PathBuf};
use std::process::Command;
use std::time::UNIX_EPOCH;

use anyhow::{bail, Context, Result};
use prefrontal_protocol::{DocEntry, DocWriteResult};

use crate::scan::SKIP_DIRS;

const DOC_EXTENSIONS: &[&str] = &["md", "markdown", "txt"];
/// What `/raw` may serve — images referenced by docs, nothing executable.
const ASSET_EXTENSIONS: &[&str] =
    &["png", "jpg", "jpeg", "gif", "svg", "webp", "ico", "bmp", "avif"];
const MAX_DEPTH: u32 = 8;
const MAX_DOCS: usize = 500;

fn has_ext(path: &Path, allowed: &[&str]) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| allowed.contains(&e.to_lowercase().as_str()))
}

fn is_doc(path: &Path) -> bool {
    has_ext(path, DOC_EXTENSIONS)
}

/// All doc files under a project, README first, then by path.
pub fn list_docs(project_dir: &Path) -> Vec<DocEntry> {
    let mut docs = Vec::new();
    walk(project_dir, project_dir, 0, &mut docs);
    docs.sort_by_key(|d| (d.path != "README.md", d.path.clone()));
    docs.truncate(MAX_DOCS);
    docs
}

fn walk(root: &Path, dir: &Path, depth: u32, out: &mut Vec<DocEntry>) {
    if depth > MAX_DEPTH || out.len() >= MAX_DOCS {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.filter_map(|e| e.ok()) {
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().to_string();
        if ft.is_dir() {
            if !name.starts_with('.') && !SKIP_DIRS.contains(&name.as_str()) {
                walk(root, &path, depth + 1, out);
            }
        } else if is_doc(&path) && !name.starts_with('.') {
            let Ok(meta) = entry.metadata() else { continue };
            let modified_unix = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_secs() as i64)
                .unwrap_or(0);
            let Ok(rel) = path.strip_prefix(root) else { continue };
            out.push(DocEntry {
                path: rel.to_string_lossy().to_string(),
                size: meta.len(),
                modified_unix,
            });
        }
    }
}

/// Resolve a client-supplied relative path to an absolute one, refusing
/// absolute paths, `..`, non-doc extensions, and symlink escapes.
pub fn resolve_doc_path(project_dir: &Path, rel: &str, for_write: bool) -> Result<PathBuf> {
    resolve_rel_path(project_dir, rel, DOC_EXTENSIONS, for_write)
}

/// Same guards as docs, image extensions, read-only — backs `/raw`.
pub fn resolve_asset_path(project_dir: &Path, rel: &str) -> Result<PathBuf> {
    resolve_rel_path(project_dir, rel, ASSET_EXTENSIONS, false)
}

fn resolve_rel_path(
    project_dir: &Path,
    rel: &str,
    allowed_exts: &[&str],
    for_write: bool,
) -> Result<PathBuf> {
    let rel_path = Path::new(rel);
    if rel_path.is_absolute()
        || rel_path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        bail!("path must be relative and stay inside the project");
    }
    if !has_ext(rel_path, allowed_exts) {
        bail!("only {allowed_exts:?} files");
    }
    let full = project_dir.join(rel_path);
    if for_write {
        if let Some(parent) = full.parent() {
            std::fs::create_dir_all(parent).context("creating parent directories")?;
        }
    }
    // canonicalize the existing part so a symlinked dir can't escape the root
    let canon_root = project_dir.canonicalize().context("project root")?;
    let canon = if full.exists() {
        full.canonicalize()?
    } else {
        let parent = full.parent().context("no parent")?.canonicalize()?;
        parent.join(full.file_name().context("no file name")?)
    };
    if !canon.starts_with(&canon_root) {
        bail!("path escapes the project");
    }
    Ok(canon)
}

pub fn read_doc(project_dir: &Path, rel: &str) -> Result<(String, i64)> {
    let path = resolve_doc_path(project_dir, rel, false)?;
    let raw = std::fs::read_to_string(&path).with_context(|| format!("reading {rel}"))?;
    let modified_unix = std::fs::metadata(&path)
        .and_then(|m| m.modified())
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0);
    Ok((raw, modified_unix))
}

/// Write the doc, then commit *only that file* locally.
///
/// The commit shells out to the system `git`: it applies the user's identity,
/// hooks, and config for free, and the pathspec form (`git commit -- <file>`)
/// can never sweep up other staged work. Swap for gix porcelain when it grows one.
pub fn write_doc(project_dir: &Path, rel: &str, content: &str) -> Result<DocWriteResult> {
    let path = resolve_doc_path(project_dir, rel, true)?;
    std::fs::write(&path, content).with_context(|| format!("writing {rel}"))?;

    if !project_dir.join(".git").exists() {
        return Ok(DocWriteResult {
            saved: true,
            committed: false,
            commit_id: None,
            detail: Some("not a git repository".into()),
        });
    }

    let git = |args: &[&str]| {
        Command::new("git")
            .arg("-C")
            .arg(project_dir)
            .args(args)
            .output()
    };

    let add = git(&["add", "--", rel])?;
    if !add.status.success() {
        return Ok(DocWriteResult {
            saved: true,
            committed: false,
            commit_id: None,
            detail: Some(String::from_utf8_lossy(&add.stderr).trim().to_string()),
        });
    }

    let msg = format!("[prefrontal] note: {rel}");
    let commit = git(&["commit", "-m", &msg, "--", rel])?;
    if !commit.status.success() {
        let noise = String::from_utf8_lossy(&commit.stdout);
        let detail = if noise.contains("nothing to commit") || noise.contains("no changes") {
            "no changes to commit".to_string()
        } else {
            let err = String::from_utf8_lossy(&commit.stderr).trim().to_string();
            if err.is_empty() { noise.trim().to_string() } else { err }
        };
        return Ok(DocWriteResult {
            saved: true,
            committed: false,
            commit_id: None,
            detail: Some(detail),
        });
    }

    let id = git(&["rev-parse", "--short", "HEAD"])
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    Ok(DocWriteResult { saved: true, committed: true, commit_id: id, detail: None })
}
