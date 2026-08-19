//! Phase 7 working tree — local git reads and allowlisted porcelain writes.
//!
//! Reads: gix for log / refs / tree / blob / repo state. Complete status and
//! unified diffs shell out to `git` (dated exception — see CHARTER 2026-08-19
//! and `docs/ideas/working-tree.md`): gix's index-worktree iterator misses
//! HEAD↔index, and porcelain diffs match the terminal.
//!
//! Writes: system `git -C <project>` with a fixed argv allowlist. Identity
//! and hooks come for free, same hole as `docs::write_doc`. Never `-A`, `.`,
//! `--force`, reset, clean, or free-form arguments.

use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use prefrontal_protocol::{
    CommitSummary, GitChange, GitCommitDetail, GitCommitFile, GitCommitRequest, GitDiff, GitEntry,
    GitFile, GitOpResult, GitRef, GitRefKind, GitStash, GitStashAction, GitStashRequest,
    GitStatus, GitSwitchRequest, GitTreeEntry, GitTreeKind,
};

use crate::scan::SKIP_DIRS;

const PATCH_CAP: usize = 200 * 1024;
const FILE_CAP: usize = 1024 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(15);
const WRITE_TIMEOUT: Duration = Duration::from_secs(30);
const NET_TIMEOUT: Duration = Duration::from_secs(60);

/// Resolve a client-supplied repo-relative path: relative, only `Normal`
/// components, symlink-escape checked. No extension allow-list — this is
/// the working tree, not notes.
pub fn resolve_repo_rel(project_dir: &Path, rel: &str) -> Result<PathBuf> {
    validate_repo_rel(rel)?;
    let full = project_dir.join(rel);
    let canon_root = project_dir.canonicalize().context("project root")?;
    let canon = if full.exists() {
        full.canonicalize()?
    } else {
        let parent = full.parent().context("no parent")?;
        let parent = if parent.exists() {
            parent.canonicalize()?
        } else {
            // untracked file in a new dir — walk up to something that exists
            let mut p = parent.to_path_buf();
            while !p.exists() {
                if !p.pop() {
                    bail!("path escapes the project");
                }
            }
            p.canonicalize()?
        };
        parent.join(full.file_name().context("no file name")?)
    };
    if !canon.starts_with(&canon_root) {
        bail!("path escapes the project");
    }
    Ok(canon)
}

pub fn validate_repo_rel(rel: &str) -> Result<()> {
    if rel.is_empty() {
        bail!("empty path");
    }
    if matches!(rel, "." | ".." | "-A" | "-u" | "--all" | "-a") {
        bail!("refused pathspec: {rel}");
    }
    if rel.starts_with(':') || rel.contains(":(") || rel.contains(":!") {
        bail!("magic pathspec refused");
    }
    if rel.contains('*') || rel.contains('?') || rel.contains('[') {
        bail!("glob pathspec refused");
    }
    if rel.contains('\0') || rel.contains('\n') {
        bail!("path must not contain control characters");
    }
    let rel_path = Path::new(rel);
    if rel_path.is_absolute()
        || rel_path
            .components()
            .any(|c| !matches!(c, Component::Normal(_)))
    {
        bail!("path must be relative and stay inside the project");
    }
    Ok(())
}

pub fn validate_branch_name(name: &str) -> Result<()> {
    if name.is_empty() || name.starts_with('-') {
        bail!("invalid branch name");
    }
    if matches!(name, "HEAD" | "FETCH_HEAD" | "ORIG_HEAD" | "MERGE_HEAD") {
        bail!("invalid branch name");
    }
    if name.starts_with("refs/") {
        bail!("invalid branch name");
    }
    if name.contains("..")
        || name.contains("//")
        || name.contains('\0')
        || name.contains(' ')
        || name.contains('~')
        || name.contains('^')
        || name.contains(':')
        || name.contains('\\')
        || name.contains('*')
        || name.contains('?')
        || name.contains('[')
        || name.contains('@')
        || name.ends_with('.')
        || name.ends_with('/')
        || name.starts_with('/')
        || name.ends_with(".lock")
    {
        bail!("invalid branch name");
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '/' | '-'))
    {
        bail!("invalid branch name");
    }
    Ok(())
}

fn validate_rev(rev: &str) -> Result<()> {
    if rev.is_empty() {
        return Ok(());
    }
    if rev == "WORKTREE" {
        return Ok(());
    }
    if rev.starts_with('-') || rev.contains('\0') || rev.contains('\n') || rev.contains(' ') {
        bail!("invalid revision");
    }
    if rev.contains(':') {
        bail!("invalid revision");
    }
    Ok(())
}

fn git_output(dir: &Path, args: &[&str], timeout: Duration) -> Result<Output> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("spawning git")?;
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait()? {
            Some(st) => break st,
            None if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                bail!("git timed out after {}s", timeout.as_secs());
            }
            None => thread::sleep(Duration::from_millis(20)),
        }
    };
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    if let Some(mut o) = child.stdout.take() {
        o.read_to_end(&mut stdout)?;
    }
    if let Some(mut e) = child.stderr.take() {
        e.read_to_end(&mut stderr)?;
    }
    Ok(Output { status, stdout, stderr })
}

fn git_ok(dir: &Path, args: &[&str], timeout: Duration) -> Result<GitOpResult> {
    let out = git_output(dir, args, timeout)?;
    if out.status.success() {
        Ok(GitOpResult { ok: true, detail: None, commit_id: None })
    } else {
        Ok(GitOpResult {
            ok: false,
            detail: Some(stderr_or_stdout(&out)),
            commit_id: None,
        })
    }
}

fn stderr_or_stdout(out: &Output) -> String {
    let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
    if !err.is_empty() {
        return err;
    }
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn require_git(dir: &Path) -> Result<()> {
    if !dir.join(".git").exists() {
        bail!("not a git repository");
    }
    Ok(())
}

fn xy_char(c: u8) -> GitChange {
    match c {
        b'.' | b' ' => GitChange::None,
        b'M' => GitChange::Modified,
        b'A' => GitChange::Added,
        b'D' => GitChange::Deleted,
        b'R' => GitChange::Renamed,
        b'C' => GitChange::Copied,
        b'T' => GitChange::TypeChanged,
        b'U' => GitChange::Unmerged,
        b'?' => GitChange::Untracked,
        _ => GitChange::Modified,
    }
}

fn name_status_char(c: u8) -> GitChange {
    match c {
        b'M' => GitChange::Modified,
        b'A' => GitChange::Added,
        b'D' => GitChange::Deleted,
        b'R' => GitChange::Renamed,
        b'C' => GitChange::Copied,
        b'T' => GitChange::TypeChanged,
        b'U' => GitChange::Unmerged,
        _ => GitChange::Modified,
    }
}

fn parse_ab(s: &str) -> (Option<u32>, Option<u32>) {
    // "+2 -1"
    let mut ahead = None;
    let mut behind = None;
    for part in s.split_whitespace() {
        if let Some(n) = part.strip_prefix('+') {
            ahead = n.parse().ok();
        } else if let Some(n) = part.strip_prefix('-') {
            behind = n.parse().ok();
        }
    }
    (ahead, behind)
}

fn parse_status_record(rec: &str, entries: &mut Vec<GitEntry>) {
    if rec.is_empty() || rec.starts_with('#') {
        return;
    }
    if let Some(path) = rec.strip_prefix("? ") {
        entries.push(GitEntry {
            path: path.to_string(),
            index: GitChange::None,
            worktree: GitChange::Untracked,
            conflicted: false,
        });
        return;
    }
    if rec.starts_with('!') {
        return;
    }
    if let Some(rest) = rec.strip_prefix("1 ") {
        if rest.len() < 3 {
            return;
        }
        let index = xy_char(rest.as_bytes()[0]);
        let worktree = xy_char(rest.as_bytes()[1]);
        if let Some(path) = rest.splitn(8, ' ').nth(7) {
            entries.push(GitEntry { path: path.to_string(), index, worktree, conflicted: false });
        }
        return;
    }
    if let Some(rest) = rec.strip_prefix("2 ") {
        if rest.len() < 3 {
            return;
        }
        let index = xy_char(rest.as_bytes()[0]);
        let worktree = xy_char(rest.as_bytes()[1]);
        // last field is `path` (orig sits in the following NUL token, ignored)
        if let Some(path) = rest.splitn(10, ' ').nth(9).or_else(|| rest.split(' ').next_back()) {
            let path = path.split('\t').next_back().unwrap_or(path);
            entries.push(GitEntry { path: path.to_string(), index, worktree, conflicted: false });
        }
        return;
    }
    if let Some(rest) = rec.strip_prefix("u ") {
        if rest.len() < 3 {
            return;
        }
        if let Some(path) = rest.splitn(11, ' ').nth(10) {
            entries.push(GitEntry {
                path: path.to_string(),
                index: GitChange::Unmerged,
                worktree: GitChange::Unmerged,
                conflicted: true,
            });
        }
    }
}

fn stash_list(dir: &Path) -> Vec<GitStash> {
    let Ok(out) = git_output(dir, &["stash", "list", "--format=%gd\t%s"], READ_TIMEOUT) else {
        return Vec::new();
    };
    if !out.status.success() {
        return Vec::new();
    }
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .enumerate()
        .map(|(i, line)| {
            let msg = line.split_once('\t').map(|(_, m)| m).unwrap_or(line);
            GitStash { index: i as u32, message: msg.to_string() }
        })
        .collect()
}

fn repo_conflict_flags(dir: &Path) -> (bool, bool) {
    let Ok(repo) = gix::open(dir) else {
        return (false, false);
    };
    match repo.state() {
        None => (false, false),
        Some(gix::state::InProgress::Merge) => (true, false),
        Some(gix::state::InProgress::Rebase)
        | Some(gix::state::InProgress::RebaseInteractive)
        | Some(gix::state::InProgress::ApplyMailbox)
        | Some(gix::state::InProgress::ApplyMailboxRebase) => (false, true),
        Some(_) => (true, false),
    }
}

/// Complete working-tree status, including staged-only changes.
pub fn status(dir: &Path, allow_push: bool) -> Result<GitStatus> {
    require_git(dir)?;
    let out = git_output(
        dir,
        &["status", "--porcelain=v2", "-z", "-b", "--untracked-files=all"],
        READ_TIMEOUT,
    )?;
    if !out.status.success() {
        bail!("{}", stderr_or_stdout(&out));
    }

    let mut branch = None;
    let mut detached = false;
    let mut upstream = None;
    let mut ahead = None;
    let mut behind = None;
    let mut entries = Vec::new();

    for rec in out.stdout.split(|b| *b == 0).filter(|s| !s.is_empty()) {
        let rec = String::from_utf8_lossy(rec);
        if let Some(rest) = rec.strip_prefix("# branch.head ") {
            if rest == "(detached)" {
                detached = true;
                branch = None;
            } else {
                branch = Some(rest.to_string());
            }
            continue;
        }
        if let Some(rest) = rec.strip_prefix("# branch.upstream ") {
            upstream = Some(rest.to_string());
            continue;
        }
        if let Some(rest) = rec.strip_prefix("# branch.ab ") {
            let (a, b) = parse_ab(rest);
            ahead = a;
            behind = b;
            continue;
        }
        parse_status_record(&rec, &mut entries);
    }

    let (merging, rebasing) = repo_conflict_flags(dir);
    Ok(GitStatus {
        branch,
        detached,
        upstream,
        ahead,
        behind,
        merging,
        rebasing,
        allow_push,
        entries,
        stashes: stash_list(dir),
    })
}

fn cap_text(bytes: &[u8], cap: usize) -> (String, bool, bool) {
    if bytes.contains(&0) {
        return (String::new(), true, false);
    }
    let truncated = bytes.len() > cap;
    let slice = if truncated { &bytes[..cap] } else { bytes };
    match std::str::from_utf8(slice) {
        Ok(s) => (s.to_string(), false, truncated),
        Err(_) => {
            // lossy only after we already rejected NULs — treat leftover invalid utf8 as binary
            if truncated {
                (String::from_utf8_lossy(slice).into_owned(), false, true)
            } else {
                (String::new(), true, false)
            }
        }
    }
}

/// Unified diff for one path. `rev` = that commit's patch; `cached` = index vs HEAD.
pub fn diff(dir: &Path, path: &str, cached: bool, rev: Option<&str>) -> Result<GitDiff> {
    require_git(dir)?;
    validate_repo_rel(path)?;
    if let Some(r) = rev {
        validate_rev(r)?;
    }

    let out = if let Some(r) = rev.filter(|r| !r.is_empty() && *r != "WORKTREE") {
        git_output(dir, &["show", "--format=", "-p", "--", r, "--", path], READ_TIMEOUT)?
    } else if cached {
        git_output(dir, &["diff", "--cached", "--", path], READ_TIMEOUT)?
    } else {
        let work = git_output(dir, &["diff", "--", path], READ_TIMEOUT)?;
        if work.status.success() && work.stdout.is_empty() {
            // untracked: synthesize from the file (git diff ignores it)
            let untracked = git_output(
                dir,
                &["diff", "--no-index", "--", "/dev/null", path],
                READ_TIMEOUT,
            );
            match untracked {
                Ok(u) if !u.stdout.is_empty() => u,
                _ => work,
            }
        } else {
            work
        }
    };

    // git diff returns 1 when there are differences — that is success for us
    if !out.status.success() && out.status.code() != Some(1) && out.stdout.is_empty() {
        bail!("{}", stderr_or_stdout(&out));
    }

    let (patch, binary, truncated) = if out.stdout.windows(14).any(|w| w == b"Binary files ")
        || out.stdout.windows(10).any(|w| w == b"GIT binary")
    {
        (String::new(), true, false)
    } else {
        cap_text(&out.stdout, PATCH_CAP)
    };

    Ok(GitDiff {
        path: path.to_string(),
        cached,
        rev: rev.filter(|r| !r.is_empty()).map(|r| r.to_string()),
        patch,
        binary,
        truncated,
    })
}

pub fn log(dir: &Path, limit: u32, skip: u32) -> Result<Vec<CommitSummary>> {
    require_git(dir)?;
    let repo = gix::open(dir).context("opening repository")?;
    let commit = match repo.head_commit() {
        Ok(c) => c,
        Err(_) => return Ok(Vec::new()),
    };
    let walk = commit.id().ancestors().all().context("walking history")?;
    let take = limit.min(200) as usize;
    let skip = skip as usize;
    let mut out = Vec::new();
    for info in walk.filter_map(Result::ok).skip(skip).take(take) {
        let Ok(c) = info.object() else { continue };
        let Ok(t) = c.time().map(|t| t.seconds) else { continue };
        let summary = c
            .message()
            .ok()
            .map(|m| m.summary().to_string())
            .unwrap_or_default();
        let full = info.id.to_string();
        out.push(CommitSummary {
            id: full.chars().take(8).collect(),
            summary,
            time_unix: t,
        });
    }
    Ok(out)
}

pub fn commit_detail(dir: &Path, id: &str) -> Result<GitCommitDetail> {
    require_git(dir)?;
    validate_rev(id)?;
    let repo = gix::open(dir).context("opening repository")?;
    let obj = repo
        .rev_parse_single(id)
        .map_err(|e| anyhow::anyhow!("{e:#}"))?;
    let commit = obj
        .object()
        .context("loading commit")?
        .try_into_commit()
        .map_err(|_| anyhow::anyhow!("not a commit"))?;
    let full = commit.id().to_string();
    let short_id: String = full.chars().take(8).collect();
    let msg = commit.message().context("commit message")?;
    let summary = msg.summary().to_string();
    let body = msg
        .body()
        .map(|b| b.to_string())
        .map(|b| b.trim().to_string())
        .filter(|b| !b.is_empty());
    let (author_name, author_email, time_unix) = match commit.author() {
        Ok(a) => {
            let seconds = a.time().map(|t| t.seconds).unwrap_or(0);
            (a.name.to_string(), a.email.to_string(), seconds)
        }
        Err(_) => (String::new(), String::new(), 0),
    };
    let parents: Vec<String> = commit.parent_ids().map(|p| p.to_string()).collect();

    let name_status = git_output(dir, &["show", "--format=", "--name-status", "--", &full], READ_TIMEOUT)?;
    let mut files = Vec::new();
    for line in String::from_utf8_lossy(&name_status.stdout).lines() {
        let mut parts = line.split('\t');
        let Some(st) = parts.next() else { continue };
        let Some(code) = st.as_bytes().first() else { continue };
        let path = parts.next_back().unwrap_or("").to_string();
        if path.is_empty() {
            continue;
        }
        files.push(GitCommitFile { path, status: name_status_char(*code) });
    }

    Ok(GitCommitDetail {
        id: full,
        short_id,
        summary,
        body,
        author: author_name,
        author_email,
        time_unix,
        parents,
        files,
    })
}

pub fn refs(dir: &Path) -> Result<Vec<GitRef>> {
    require_git(dir)?;
    let repo = gix::open(dir).context("opening repository")?;
    let current = repo
        .head_name()
        .ok()
        .flatten()
        .map(|n| n.shorten().to_string());
    let mut out = Vec::new();
    let platform = repo.references().context("listing refs")?;
    let all = platform.all().context("iterating refs")?;
    for r in all.filter_map(Result::ok) {
        let full = r.name().as_bstr().to_string();
        let (kind, name) = if let Some(n) = full.strip_prefix("refs/heads/") {
            (GitRefKind::Local, n.to_string())
        } else if let Some(n) = full.strip_prefix("refs/remotes/") {
            (GitRefKind::Remote, n.to_string())
        } else if let Some(n) = full.strip_prefix("refs/tags/") {
            (GitRefKind::Tag, n.to_string())
        } else {
            continue;
        };
        let is_current = current.as_deref() == Some(name.as_str()) && kind == GitRefKind::Local;
        out.push(GitRef { name, kind, current: is_current });
    }
    out.sort_by(|a, b| a.kind.cmp(&b.kind).then(a.name.cmp(&b.name)));
    Ok(out)
}

fn skip_tree_name(name: &str) -> bool {
    name.starts_with('.') || SKIP_DIRS.contains(&name)
}

pub fn tree(dir: &Path, rev: Option<&str>, path: Option<&str>) -> Result<Vec<GitTreeEntry>> {
    require_git(dir)?;
    let rev = rev.unwrap_or("HEAD");
    validate_rev(rev)?;
    let path = path.unwrap_or("");
    if !path.is_empty() {
        validate_repo_rel(path)?;
    }
    let repo = gix::open(dir).context("opening repository")?;
    let obj = repo
        .rev_parse_single(rev)
        .map_err(|e| anyhow::anyhow!("{e:#}"))?;
    let mut tree = obj
        .object()
        .context("loading object")?
        .peel_to_tree()
        .context("peeling to tree")?;
    if !path.is_empty() {
        let entry = tree
            .lookup_entry_by_path(path)
            .context("looking up path")?
            .ok_or_else(|| anyhow::anyhow!("path not in tree: {path}"))?;
        tree = repo
            .find_object(entry.oid())
            .context("loading subtree")?
            .try_into_tree()
            .map_err(|_| anyhow::anyhow!("not a directory: {path}"))?;
    }
    let prefix = if path.is_empty() {
        String::new()
    } else {
        format!("{path}/")
    };
    let mut entries = Vec::new();
    for item in tree.iter() {
        let item = item.context("tree entry")?;
        let name = item.filename().to_string();
        if skip_tree_name(&name) {
            continue;
        }
        let kind = if item.mode().is_tree() {
            GitTreeKind::Dir
        } else {
            GitTreeKind::File
        };
        let size = None;
        entries.push(GitTreeEntry { path: format!("{prefix}{name}"), kind, size });
    }
    entries.sort_by(|a, b| match (a.kind, b.kind) {
        (GitTreeKind::Dir, GitTreeKind::File) => std::cmp::Ordering::Less,
        (GitTreeKind::File, GitTreeKind::Dir) => std::cmp::Ordering::Greater,
        _ => a.path.cmp(&b.path),
    });
    Ok(entries)
}

pub fn file_at(dir: &Path, rev: Option<&str>, path: &str) -> Result<GitFile> {
    require_git(dir)?;
    validate_repo_rel(path)?;
    let rev = rev.unwrap_or("HEAD");
    validate_rev(rev)?;

    if rev == "WORKTREE" {
        let full = resolve_repo_rel(dir, path)?;
        let bytes = std::fs::read(&full).with_context(|| format!("reading {path}"))?;
        let (text, binary, truncated) = cap_text(&bytes, FILE_CAP);
        return Ok(GitFile {
            path: path.to_string(),
            rev: "WORKTREE".into(),
            text,
            binary,
            truncated,
        });
    }

    let repo = gix::open(dir).context("opening repository")?;
    let obj = repo
        .rev_parse_single(rev)
        .map_err(|e| anyhow::anyhow!("{e:#}"))?;
    let tree = obj
        .object()
        .context("loading object")?
        .peel_to_tree()
        .context("peeling to tree")?;
    let entry = tree
        .lookup_entry_by_path(path)
        .context("looking up path")?
        .ok_or_else(|| anyhow::anyhow!("path not in tree: {path}"))?;
    let blob = repo
        .find_object(entry.oid())
        .context("loading blob")?
        .try_into_blob()
        .map_err(|_| anyhow::anyhow!("not a file: {path}"))?;
    let (text, binary, truncated) = cap_text(&blob.data, FILE_CAP);
    Ok(GitFile {
        path: path.to_string(),
        rev: rev.to_string(),
        text,
        binary,
        truncated,
    })
}

fn validate_paths(paths: &[String]) -> Result<()> {
    if paths.is_empty() {
        bail!("no paths");
    }
    for p in paths {
        validate_repo_rel(p)?;
    }
    Ok(())
}

pub fn stage(dir: &Path, paths: &[String]) -> Result<GitOpResult> {
    require_git(dir)?;
    validate_paths(paths)?;
    let mut args = vec!["add", "--"];
    args.extend(paths.iter().map(String::as_str));
    git_ok(dir, &args, WRITE_TIMEOUT)
}

pub fn unstage(dir: &Path, paths: &[String]) -> Result<GitOpResult> {
    require_git(dir)?;
    validate_paths(paths)?;
    let mut args = vec!["restore", "--staged", "--"];
    args.extend(paths.iter().map(String::as_str));
    git_ok(dir, &args, WRITE_TIMEOUT)
}

pub fn commit(dir: &Path, req: &GitCommitRequest) -> Result<GitOpResult> {
    require_git(dir)?;
    let msg = req.message.trim();
    if msg.is_empty() {
        return Ok(GitOpResult {
            ok: false,
            detail: Some("commit message is empty".into()),
            commit_id: None,
        });
    }
    let mut owned: Vec<String> = vec!["commit".into(), "-m".into(), msg.to_string()];
    if let Some(paths) = &req.paths {
        validate_paths(paths)?;
        owned.push("--".into());
        owned.extend(paths.iter().cloned());
    }
    let args: Vec<&str> = owned.iter().map(String::as_str).collect();
    let out = git_output(dir, &args, WRITE_TIMEOUT)?;
    if !out.status.success() {
        return Ok(GitOpResult {
            ok: false,
            detail: Some(stderr_or_stdout(&out)),
            commit_id: None,
        });
    }
    let id = git_output(dir, &["rev-parse", "--short", "HEAD"], READ_TIMEOUT)
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string());
    Ok(GitOpResult { ok: true, detail: None, commit_id: id })
}

pub fn switch(dir: &Path, req: &GitSwitchRequest) -> Result<GitOpResult> {
    require_git(dir)?;
    validate_branch_name(&req.name)?;
    if req.create {
        git_ok(dir, &["switch", "-c", &req.name], WRITE_TIMEOUT)
    } else {
        git_ok(dir, &["switch", &req.name], WRITE_TIMEOUT)
    }
}

pub fn stash(dir: &Path, req: &GitStashRequest) -> Result<GitOpResult> {
    require_git(dir)?;
    match req.action {
        GitStashAction::Push => {
            if let Some(msg) = req.message.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
                git_ok(dir, &["stash", "push", "-m", msg], WRITE_TIMEOUT)
            } else {
                git_ok(dir, &["stash", "push"], WRITE_TIMEOUT)
            }
        }
        GitStashAction::Pop => git_ok(dir, &["stash", "pop"], WRITE_TIMEOUT),
    }
}

pub fn push(dir: &Path, allow_push: bool) -> Result<GitOpResult> {
    require_git(dir)?;
    if !allow_push {
        return Ok(GitOpResult {
            ok: false,
            detail: Some("push disabled — set [git] allow_push = true in ~/.config/prefrontal/config.toml".into()),
            commit_id: None,
        });
    }
    git_ok(dir, &["push"], NET_TIMEOUT)
}

pub fn fetch(dir: &Path, allow_push: bool) -> Result<GitOpResult> {
    require_git(dir)?;
    if !allow_push {
        return Ok(GitOpResult {
            ok: false,
            detail: Some("fetch disabled — set [git] allow_push = true in ~/.config/prefrontal/config.toml".into()),
            commit_id: None,
        });
    }
    git_ok(dir, &["fetch"], NET_TIMEOUT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_magic_and_parent_paths() {
        assert!(validate_repo_rel("src/lib.rs").is_ok());
        assert!(validate_repo_rel("../secret").is_err());
        assert!(validate_repo_rel("/etc/passwd").is_err());
        assert!(validate_repo_rel(".").is_err());
        assert!(validate_repo_rel(":!target").is_err());
        assert!(validate_repo_rel("src/*.rs").is_err());
        assert!(validate_repo_rel("-A").is_err());
    }

    #[test]
    fn branch_names() {
        assert!(validate_branch_name("main").is_ok());
        assert!(validate_branch_name("feat/working-tree").is_ok());
        assert!(validate_branch_name("-evil").is_err());
        assert!(validate_branch_name("HEAD").is_err());
        assert!(validate_branch_name("has space").is_err());
        assert!(validate_branch_name("a..b").is_err());
    }

    #[test]
    fn xy_mapping() {
        assert_eq!(xy_char(b'M'), GitChange::Modified);
        assert_eq!(xy_char(b'.'), GitChange::None);
        assert_eq!(xy_char(b'?'), GitChange::Untracked);
    }
}
