//! Symbol extraction — pure-Rust, line-based regex per language (charter D7:
//! tree-sitter's C runtime is the linking pattern we avoid; regex covers
//! "find every fn/class/struct by name" and can be upgraded if precision
//! ever demands real parsing).

use std::sync::LazyLock;

use regex::Regex;

pub struct Symbol {
    /// "fn" | "struct" | "enum" | "trait" | "macro" | "class" | "def" | "func" | "function"
    pub kind: &'static str,
    pub name: String,
    /// 1-based.
    pub line: u32,
    /// The declaration line, trimmed.
    pub signature: String,
}

const MAX_SYMBOLS_PER_FILE: usize = 500;
const MAX_SIGNATURE_CHARS: usize = 120;

struct LangRules {
    extensions: &'static [&'static str],
    rules: Vec<(&'static str, Regex)>,
}

static LANGS: LazyLock<Vec<LangRules>> = LazyLock::new(|| {
    let re = |p: &str| Regex::new(p).expect("static symbol regex");
    vec![
        LangRules {
            extensions: &["rs"],
            rules: vec![
                (
                    "fn",
                    re(r#"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:default\s+|const\s+|async\s+|unsafe\s+|extern\s+"[^"]*"\s+)*fn\s+([A-Za-z_]\w*)"#),
                ),
                ("struct", re(r"^\s*(?:pub(?:\([^)]*\))?\s+)?struct\s+([A-Za-z_]\w*)")),
                ("enum", re(r"^\s*(?:pub(?:\([^)]*\))?\s+)?enum\s+([A-Za-z_]\w*)")),
                ("trait", re(r"^\s*(?:pub(?:\([^)]*\))?\s+)?(?:unsafe\s+)?trait\s+([A-Za-z_]\w*)")),
                ("macro", re(r"^\s*macro_rules!\s*([A-Za-z_]\w*)")),
            ],
        },
        LangRules {
            extensions: &["py"],
            rules: vec![
                ("def", re(r"^\s*(?:async\s+)?def\s+([A-Za-z_]\w*)")),
                ("class", re(r"^\s*class\s+([A-Za-z_]\w*)")),
            ],
        },
        LangRules {
            extensions: &["js", "jsx", "ts", "tsx"],
            rules: vec![
                (
                    "function",
                    re(r"^\s*(?:export\s+)?(?:default\s+)?(?:async\s+)?function\s*\*?\s*([A-Za-z_$][\w$]*)"),
                ),
                ("class", re(r"^\s*(?:export\s+)?(?:abstract\s+)?class\s+([A-Za-z_$][\w$]*)")),
                (
                    "function",
                    re(r"^\s*(?:export\s+)?(?:const|let)\s+([A-Za-z_$][\w$]*)\s*=\s*(?:async\s*)?(?:\([^)]*\)|[A-Za-z_$][\w$]*)\s*=>"),
                ),
            ],
        },
        LangRules {
            extensions: &["gd"],
            rules: vec![
                ("func", re(r"^\s*(?:static\s+)?func\s+([A-Za-z_]\w*)")),
                ("class", re(r"^class_name\s+([A-Za-z_]\w*)")),
                ("class", re(r"^class\s+([A-Za-z_]\w*)")),
            ],
        },
        LangRules {
            extensions: &["go"],
            rules: vec![("func", re(r"^func\s+(?:\([^)]*\)\s*)?([A-Za-z_]\w*)"))],
        },
        LangRules {
            extensions: &["sh", "bash"],
            rules: vec![("function", re(r"^\s*(?:function\s+)?([A-Za-z_]\w*)\s*\(\)\s*\{"))],
        },
    ]
});

/// Extract declarations from one file. Unknown extensions yield nothing.
pub fn extract(extension: &str, content: &str) -> Vec<Symbol> {
    let Some(lang) = LANGS.iter().find(|l| l.extensions.contains(&extension)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for (i, line) in content.lines().enumerate() {
        if out.len() >= MAX_SYMBOLS_PER_FILE {
            break;
        }
        for (kind, rule) in &lang.rules {
            if let Some(caps) = rule.captures(line) {
                if let Some(name) = caps.get(1) {
                    let mut signature: String = line.trim().to_string();
                    if signature.chars().count() > MAX_SIGNATURE_CHARS {
                        signature = signature.chars().take(MAX_SIGNATURE_CHARS - 1).collect();
                        signature.push('…');
                    }
                    out.push(Symbol {
                        kind,
                        name: name.as_str().to_string(),
                        line: i as u32 + 1,
                        signature,
                    });
                    break; // one symbol per line is plenty
                }
            }
        }
    }
    out
}
