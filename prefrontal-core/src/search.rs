//! Recall: tantivy full-text over code, docs, and commit messages.
//!
//! One index for everything, one document per file or commit. Projects are
//! the unit of (re)indexing — the watcher's per-project rescan maps 1:1 to
//! `delete_term(project) + re-add`. Snippets and line numbers come from the
//! file on disk at query time, so file content is indexed but never stored.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result};
use prefrontal_protocol::SearchHit;
use tantivy::collector::TopDocs;
use tantivy::query::QueryParser;
use tantivy::schema::{Field, Schema, Value, STORED, STRING, TEXT};
use tantivy::{doc, Index, IndexWriter, TantivyDocument, Term};

use crate::scan::SKIP_DIRS;
use crate::symbols;

const CODE_EXTENSIONS: &[&str] = &[
    "rs", "py", "js", "ts", "jsx", "tsx", "gd", "c", "h", "cpp", "hpp", "cc", "go", "java",
    "rb", "sh", "bash", "toml", "yaml", "yml", "css", "html", "slint", "sql", "proto", "json",
];
const DOC_EXTENSIONS: &[&str] = &["md", "markdown", "txt"];
const MAX_FILE_BYTES: u64 = 256 * 1024;
const MAX_COMMITS: usize = 1000;
const MAX_DEPTH: u32 = 8;
const SNIPPET_CHARS: usize = 160;

#[derive(Clone, Copy)]
pub struct Fields {
    pub project: Field,
    pub path: Field,
    pub kind: Field,
    pub content: Field,
    pub stored_text: Field,
    /// 1-based declaration line — set on symbol documents only.
    pub line: Field,
}

pub struct SearchIndex {
    pub index: Index,
    pub writer: Mutex<IndexWriter>,
    pub fields: Fields,
}

/// Default index home: `~/.local/share/prefrontal/index` — never inside projects.
pub fn default_index_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("prefrontal").join("index"))
}

fn schema() -> (Schema, Fields) {
    let mut b = Schema::builder();
    let fields = Fields {
        project: b.add_text_field("project", STRING | STORED),
        path: b.add_text_field("path", STRING | STORED),
        kind: b.add_text_field("kind", STRING | STORED),
        content: b.add_text_field("content", TEXT),
        stored_text: b.add_text_field("stored_text", STORED),
        line: b.add_u64_field("line", STORED),
    };
    (b.build(), fields)
}

/// Open (or create) the index for writing. A schema mismatch from an older
/// build wipes and recreates — the index is a cache, never the source of truth.
pub fn open(dir: &Path) -> Result<SearchIndex> {
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    let (sch, fields) = schema();
    let mmap = || tantivy::directory::MmapDirectory::open(dir);
    let index = match Index::open_or_create(mmap()?, sch.clone()) {
        Ok(i) => i,
        Err(_) => {
            std::fs::remove_dir_all(dir).ok();
            std::fs::create_dir_all(dir)?;
            Index::open_or_create(mmap()?, sch)?
        }
    };
    let writer = index.writer(50_000_000)?;
    Ok(SearchIndex { index, writer: Mutex::new(writer), fields })
}

/// Read-only open for the CLI; fails politely if the daemon never built one.
pub fn open_readonly(dir: &Path) -> Result<(Index, Fields)> {
    let index = Index::open_in_dir(dir)
        .context("no search index — run prefrontald once to build it")?;
    let (_, fields) = schema();
    Ok((index, fields))
}

impl SearchIndex {
    /// Drop and re-add everything for one project. Returns documents indexed.
    pub fn reindex_project(&self, name: &str, project_dir: &Path) -> Result<usize> {
        let mut files = Vec::new();
        walk_files(project_dir, project_dir, 0, &mut files);
        let commits = commit_log(project_dir);

        let writer = self.writer.lock().expect("index writer poisoned");
        writer.delete_term(Term::from_field_text(self.fields.project, name));
        let mut added = 0usize;
        for (rel, kind) in &files {
            let Some(content) = read_indexable(&project_dir.join(rel)) else { continue };
            if kind == "code" {
                let ext = rel.rsplit('.').next().unwrap_or_default().to_lowercase();
                for sym in symbols::extract(&ext, &content) {
                    // tiny doc per declaration: a name query ranks it far above
                    // the file it lives in, and the signature ships in-index
                    writer.add_document(doc!(
                        self.fields.project => name,
                        self.fields.path => rel.as_str(),
                        self.fields.kind => "symbol",
                        self.fields.content => format!("{} {}", sym.kind, sym.name),
                        self.fields.stored_text => sym.signature,
                        self.fields.line => sym.line as u64,
                    ))?;
                    added += 1;
                }
            }
            writer.add_document(doc!(
                self.fields.project => name,
                self.fields.path => rel.as_str(),
                self.fields.kind => kind.as_str(),
                self.fields.content => content,
            ))?;
            added += 1;
        }
        for (id, summary) in &commits {
            writer.add_document(doc!(
                self.fields.project => name,
                self.fields.path => id.as_str(),
                self.fields.kind => "commit",
                self.fields.content => summary.as_str(),
                self.fields.stored_text => summary.as_str(),
            ))?;
            added += 1;
        }
        drop(writer);
        self.commit()?;
        Ok(added)
    }

    pub fn remove_project(&self, name: &str) -> Result<()> {
        let writer = self.writer.lock().expect("index writer poisoned");
        writer.delete_term(Term::from_field_text(self.fields.project, name));
        drop(writer);
        self.commit()
    }

    fn commit(&self) -> Result<()> {
        self.writer.lock().expect("index writer poisoned").commit()?;
        Ok(())
    }
}

fn walk_files(root: &Path, dir: &Path, depth: u32, out: &mut Vec<(String, String)>) {
    if depth > MAX_DEPTH {
        return;
    }
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for entry in entries.filter_map(|e| e.ok()) {
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let path = entry.path();
        if ft.is_dir() {
            if !SKIP_DIRS.contains(&name.as_str()) {
                walk_files(root, &path, depth + 1, out);
            }
            continue;
        }
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_default();
        let kind = if DOC_EXTENSIONS.contains(&ext.as_str()) {
            "doc"
        } else if CODE_EXTENSIONS.contains(&ext.as_str()) {
            "code"
        } else {
            continue;
        };
        if let Ok(rel) = path.strip_prefix(root) {
            out.push((rel.to_string_lossy().to_string(), kind.to_string()));
        }
    }
}

/// Size-capped, binary-sniffed read.
fn read_indexable(path: &Path) -> Option<String> {
    let meta = std::fs::metadata(path).ok()?;
    if meta.len() > MAX_FILE_BYTES {
        return None;
    }
    let bytes = std::fs::read(path).ok()?;
    if bytes.iter().take(1024).any(|&b| b == 0) {
        return None;
    }
    Some(String::from_utf8_lossy(&bytes).into_owned())
}

/// Newest-first commit summaries, capped — recall reaches further back than
/// the 14-day timeline window.
fn commit_log(project_dir: &Path) -> Vec<(String, String)> {
    let Ok(repo) = gix::open(project_dir) else { return Vec::new() };
    let Ok(head) = repo.head_commit() else { return Vec::new() };
    let Ok(walk) = head.id().ancestors().all() else { return Vec::new() };
    walk.filter_map(Result::ok)
        .take(MAX_COMMITS)
        .filter_map(|info| {
            let commit = info.object().ok()?;
            let summary = commit.message().ok()?.summary().to_string();
            let id: String = info.id.to_string().chars().take(8).collect();
            Some((id, summary))
        })
        .collect()
}

/// Query the index; enrich hits with snippets and line numbers from disk.
/// `project_dirs` maps project name → absolute path (from the scan).
pub fn search(
    index: &Index,
    fields: Fields,
    query: &str,
    limit: usize,
    project_dirs: &HashMap<String, PathBuf>,
) -> Result<Vec<SearchHit>> {
    let reader = index.reader()?;
    let searcher = reader.searcher();
    let parser = QueryParser::for_index(index, vec![fields.content]);
    let (parsed, _errors) = parser.parse_query_lenient(query);
    let top = searcher.search(&parsed, &TopDocs::with_limit(limit).order_by_score())?;

    let terms: Vec<String> = query
        .split_whitespace()
        .map(|t| t.trim_matches('"').to_lowercase())
        .filter(|t| !t.is_empty())
        .collect();

    let mut hits = Vec::new();
    for (score, addr) in top {
        let doc: TantivyDocument = searcher.doc(addr)?;
        let get = |f: Field| {
            doc.get_first(f)
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string()
        };
        let project = get(fields.project);
        let path = get(fields.path);
        let kind = get(fields.kind);
        let (line, snippet) = match kind.as_str() {
            "commit" => (None, get(fields.stored_text)),
            "symbol" => {
                let line = doc
                    .get_first(fields.line)
                    .and_then(|v| v.as_u64())
                    .map(|l| l as u32);
                (line, get(fields.stored_text))
            }
            _ => match project_dirs.get(&project) {
                Some(dir) => locate_snippet(&dir.join(&path), &terms),
                None => (None, String::new()),
            },
        };
        hits.push(SearchHit { project, path, kind, line, snippet, score });
    }
    Ok(hits)
}

/// First line containing any query term (1-based), else the first non-empty line.
fn locate_snippet(path: &Path, terms: &[String]) -> (Option<u32>, String) {
    let Some(content) = read_indexable(path) else { return (None, String::new()) };
    let mut first_nonempty: Option<(u32, &str)> = None;
    for (i, raw) in content.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() {
            continue;
        }
        if first_nonempty.is_none() {
            first_nonempty = Some((i as u32 + 1, line));
        }
        let lower = line.to_lowercase();
        if terms.iter().any(|t| lower.contains(t)) {
            return (Some(i as u32 + 1), truncate(line));
        }
    }
    match first_nonempty {
        Some((n, line)) => (Some(n), truncate(line)),
        None => (None, String::new()),
    }
}

fn truncate(s: &str) -> String {
    if s.chars().count() <= SNIPPET_CHARS {
        s.to_string()
    } else {
        let cut: String = s.chars().take(SNIPPET_CHARS - 1).collect();
        format!("{cut}…")
    }
}
