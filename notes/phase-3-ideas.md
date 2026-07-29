# Phase 3 (Recall) — idea parking lot

Captured via the phase-2 notes flow itself, as its own first test.

- tantivy index lives in `~/.local/share/prefrontal/index/` — never inside projects
- index commit messages too: half of "where did I do X" is findable in git log alone
- tree-sitter symbol cards: start with rust/python/js/gdscript grammars, that covers the garden
- search result = project + path + line + surrounding snippet; click-through opens the doc panel
- ranking: recency-boost by project activity state (active projects first, parked last)
