# Idea: working tree — a local-git GUI for session prep

Filed 2026-08-19; implemented the same day (see the CHARTER decisions log).
Kept as the design brief.

Prefrontal already answered "which garden beds are dirty?" It did not answer
"what is dirty, what is the patch, and can I put this repo in a state I trust
before I open the editor?" Notes could not fill that hole — they are the only
write path, and they only touch markdown.

This is not a GitHub clone (no issues, PRs, Actions) and not an IDE (no code
editing). It is the GitHub *repo page* plus Magit-lite porcelain, aimed at
the local object store, with network as an explicit opt-in.

## Locked rules

- Notes remain the only content editor. The file tree is read-only.
- Writes are UI-only in v1. CLI/MCP get reads (`status`, `diff`, `log`,
  `show`, `tree`, `file`). Agents already have `write_doc`.
- Notes still never push (D9). Human Push is a different verb, gated by
  `[git] allow_push` (default **false**). Never `--force`.
- Pathspecs: relative, no `..`, no magic `:(` / `:!` / globs. Never `git add -A`
  or `git add .`.
- Diffs and commit bodies are rendered as text, never `innerHTML`.
- No merge / rebase / reset / cherry-pick / discard / worktrees / `git init`.

## Surfaces

| Surface | Role |
|---|---|
| `GET /api/git/{project}/status` | paths, staged/unstaged/untracked, ahead/behind, stash list |
| `GET /api/git/{project}/diff` | unified patch (`path`, `cached`, `rev`) |
| `GET /api/git/{project}/log` | paginated `CommitSummary` |
| `GET /api/git/{project}/commit/{id}` | author, body, files |
| `GET /api/git/{project}/refs` | local + remote-tracking + tags |
| `GET /api/git/{project}/tree` | directory at `rev` (default HEAD) |
| `GET /api/git/{project}/file` | blob at `rev`, or `WORKTREE` |
| `POST .../stage|unstage|commit|switch|stash` | allowlisted porcelain |
| `POST .../push|fetch` | only if `allow_push` |

ui-web overlay: **Notes | Repo**. Dirty projects land on Repo. Timeline,
health rows, and search commit/code hits click through.

## Dated exceptions (next to the write allowlist)

- **Status** uses `git status --porcelain=v2` because gix's
  `into_index_worktree_iter` is what the scanner `.count()`s and it misses
  HEAD↔index (staged-only) changes. The pane must not lie relative to
  `git status`.
- **Unified diffs** use `git diff` / `git diff --cached` / `git show`. Gix
  can produce them; porcelain matches what the terminal already showed.

Reads that stay on gix: log, commit metadata, refs, tree walk, blob-at-rev,
`repo.state()` for merge/rebase, `SKIP_DIRS` on the tree.
