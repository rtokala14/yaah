//! Phase-1 index: a single gitignore-aware walk, per-language regex symbol
//! extraction, and an identifier→files inverted map. No tree-sitter, no
//! embeddings — structure first, cheap enough to rebuild per session (see
//! INDEX-DESIGN.md; watcher-driven incremental updates are phase 2).

use crate::{CodeIndex, IndexError, Reference, Symbol, SymbolKind};
use ignore::WalkBuilder;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

/// Per-file size cap and total corpus cap keep build time and memory
/// bounded on large repos; skipped files simply don't contribute.
const MAX_FILE_BYTES: u64 = 512 * 1024;
const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;

struct LangSpec {
    extensions: &'static [&'static str],
    /// (regex with two captures: keyword, name)
    patterns: Vec<(Regex, fn(&str, bool) -> SymbolKind)>,
}

fn rust_kind(keyword: &str, indented: bool) -> SymbolKind {
    match keyword {
        "fn" if indented => SymbolKind::Method,
        "fn" => SymbolKind::Function,
        "struct" => SymbolKind::Struct,
        "enum" => SymbolKind::Enum,
        "trait" => SymbolKind::Trait,
        "mod" => SymbolKind::Module,
        "const" | "static" => SymbolKind::Constant,
        "type" => SymbolKind::TypeAlias,
        _ => SymbolKind::Function,
    }
}

fn py_kind(keyword: &str, indented: bool) -> SymbolKind {
    match keyword {
        "class" => SymbolKind::Class,
        _ if indented => SymbolKind::Method,
        _ => SymbolKind::Function,
    }
}

fn ts_kind(keyword: &str, _indented: bool) -> SymbolKind {
    match keyword {
        "class" => SymbolKind::Class,
        "interface" => SymbolKind::Interface,
        "type" => SymbolKind::TypeAlias,
        "enum" => SymbolKind::Enum,
        "const" | "let" | "var" => SymbolKind::Variable,
        _ => SymbolKind::Function,
    }
}

fn go_kind(keyword: &str, _indented: bool) -> SymbolKind {
    match keyword {
        "type" => SymbolKind::Struct,
        _ => SymbolKind::Function,
    }
}

fn lang_specs() -> Vec<LangSpec> {
    vec![
        LangSpec {
            extensions: &["rs"],
            patterns: vec![(
                Regex::new(
                    r"(?m)^([ \t]*)(?:pub(?:\([^)]*\))?\s+)?(fn|struct|enum|trait|mod|const|static|type)\s+([A-Za-z_][A-Za-z0-9_]*)",
                )
                .unwrap(),
                rust_kind,
            )],
        },
        LangSpec {
            extensions: &["py"],
            patterns: vec![(
                Regex::new(r"(?m)^([ \t]*)(?:async\s+)?(def|class)\s+([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
                py_kind,
            )],
        },
        LangSpec {
            extensions: &["ts", "tsx", "js", "jsx", "mjs"],
            patterns: vec![
                (
                    Regex::new(
                        r"(?m)^([ \t]*)(?:export\s+)?(?:default\s+)?(?:async\s+)?(function|class|interface|enum)\s+([A-Za-z_$][A-Za-z0-9_$]*)",
                    )
                    .unwrap(),
                    ts_kind,
                ),
                (
                    Regex::new(
                        r"(?m)^([ \t]*)(?:export\s+)?(const|let|var|type)\s+([A-Za-z_$][A-Za-z0-9_$]*)\s*=",
                    )
                    .unwrap(),
                    ts_kind,
                ),
            ],
        },
        LangSpec {
            extensions: &["go"],
            patterns: vec![(
                Regex::new(r"(?m)^([ \t]*)(func|type)\s+(?:\([^)]*\)\s*)?([A-Za-z_][A-Za-z0-9_]*)").unwrap(),
                go_kind,
            )],
        },
    ]
}

/// The phase-1 in-memory index.
pub struct RegexIndex {
    root: PathBuf,
    symbols: Vec<Symbol>,
    /// Relative path → file text (for reference scans and outlines).
    files: HashMap<PathBuf, String>,
    /// identifier → set of files whose text contains it.
    ident_files: HashMap<String, Vec<PathBuf>>,
}

fn identifiers(text: &str) -> HashSet<&str> {
    let mut out = HashSet::new();
    let mut start = None;
    for (i, c) in text.char_indices() {
        if c.is_ascii_alphanumeric() || c == '_' {
            if start.is_none() {
                start = Some(i);
            }
        } else if let Some(s) = start.take() {
            let word = &text[s..i];
            if word.len() >= 3 && !word.chars().next().unwrap().is_ascii_digit() {
                out.insert(word);
            }
        }
    }
    if let Some(s) = start {
        let word = &text[s..];
        if word.len() >= 3 && !word.chars().next().unwrap().is_ascii_digit() {
            out.insert(word);
        }
    }
    out
}

impl RegexIndex {
    pub fn build(root: &Path) -> Self {
        let specs = lang_specs();
        let by_ext: HashMap<&str, &LangSpec> = specs
            .iter()
            .flat_map(|s| s.extensions.iter().map(move |e| (*e, s)))
            .collect();

        let mut files = HashMap::new();
        let mut total: u64 = 0;
        for entry in WalkBuilder::new(root).hidden(true).build().flatten() {
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let path = entry.path();
            let Some(ext) = path.extension().and_then(|e| e.to_str()) else { continue };
            if !by_ext.contains_key(ext) {
                continue;
            }
            let Ok(meta) = entry.metadata() else { continue };
            if meta.len() > MAX_FILE_BYTES || total + meta.len() > MAX_TOTAL_BYTES {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(path) else { continue };
            total += meta.len();
            let rel = path.strip_prefix(root).unwrap_or(path).to_path_buf();
            files.insert(rel, text);
        }

        // Inverted identifier map (for candidate filtering and ranking).
        let mut ident_files: HashMap<String, Vec<PathBuf>> = HashMap::new();
        for (path, text) in &files {
            for ident in identifiers(text) {
                ident_files.entry(ident.to_string()).or_default().push(path.clone());
            }
        }
        for list in ident_files.values_mut() {
            list.sort();
        }

        // Symbols; rank = number of OTHER files mentioning the name (a
        // cheap stand-in for reference-graph centrality).
        let mut symbols = Vec::new();
        for (path, text) in &files {
            let Some(ext) = path.extension().and_then(|e| e.to_str()) else { continue };
            let Some(spec) = by_ext.get(ext) else { continue };
            for (regex, kind_fn) in &spec.patterns {
                for caps in regex.captures_iter(text) {
                    let indent = caps.get(1).map(|m| !m.as_str().is_empty()).unwrap_or(false);
                    let keyword = caps.get(2).map(|m| m.as_str()).unwrap_or("");
                    let Some(name) = caps.get(3).map(|m| m.as_str()) else { continue };
                    let offset = caps.get(0).map(|m| m.start()).unwrap_or(0);
                    let line = text[..offset].matches('\n').count() as u32 + 1;
                    let signature: String = text[offset..]
                        .lines()
                        .next()
                        .unwrap_or("")
                        .trim()
                        .chars()
                        .take(100)
                        .collect();
                    let mentions = ident_files.get(name).map(|f| f.len()).unwrap_or(1);
                    symbols.push(Symbol {
                        name: name.to_string(),
                        kind: kind_fn(keyword, indent),
                        file: path.clone(),
                        line,
                        signature,
                        rank: (mentions.saturating_sub(1)) as f32,
                    });
                }
            }
        }
        // Deterministic order: rank desc, then path, then line.
        symbols.sort_by(|a, b| {
            b.rank
                .partial_cmp(&a.rank)
                .unwrap_or(std::cmp::Ordering::Equal)
                .then_with(|| a.file.cmp(&b.file))
                .then_with(|| a.line.cmp(&b.line))
        });

        Self { root: root.to_path_buf(), symbols, files, ident_files }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn file_count(&self) -> usize {
        self.files.len()
    }
}

impl CodeIndex for RegexIndex {
    fn find_symbols(&self, query: &str, limit: usize) -> Result<Vec<Symbol>, IndexError> {
        let q = query.to_lowercase();
        Ok(self
            .symbols
            .iter()
            .filter(|s| s.name.to_lowercase().contains(&q))
            .take(limit)
            .cloned()
            .collect())
    }

    fn find_references(&self, name: &str, limit: usize) -> Result<Vec<Reference>, IndexError> {
        let mut out = Vec::new();
        let candidates = self.ident_files.get(name).cloned().unwrap_or_default();
        for path in candidates {
            let Some(text) = self.files.get(&path) else { continue };
            for (i, line) in text.lines().enumerate() {
                if line.contains(name) {
                    out.push(Reference {
                        file: path.clone(),
                        line: i as u32 + 1,
                        context: line.trim().chars().take(120).collect(),
                    });
                    if out.len() >= limit {
                        return Ok(out);
                    }
                }
            }
        }
        Ok(out)
    }

    fn file_outline(&self, file: &Path) -> Result<Vec<Symbol>, IndexError> {
        let mut out: Vec<Symbol> =
            self.symbols.iter().filter(|s| s.file == file).cloned().collect();
        out.sort_by_key(|s| s.line);
        Ok(out)
    }

    fn repo_map(&self, max_tokens: usize) -> Result<String, IndexError> {
        let budget_chars = max_tokens.saturating_mul(4);
        // Rank files by their best symbols, then list each file's top
        // symbols. Deterministic by construction (symbols are pre-sorted).
        let mut file_order: Vec<&PathBuf> = Vec::new();
        let mut seen = HashSet::new();
        for s in &self.symbols {
            if seen.insert(&s.file) {
                file_order.push(&s.file);
            }
        }
        let mut out = String::new();
        for path in file_order {
            let mut section = format!("{}\n", path.display());
            let mut listed = 0;
            for s in self.symbols.iter().filter(|s| &s.file == path) {
                if listed >= 8 {
                    break;
                }
                section.push_str(&format!("  {}\n", s.signature));
                listed += 1;
            }
            if out.len() + section.len() > budget_chars {
                break;
            }
            out.push_str(&section);
        }
        Ok(out.trim_end().to_string())
    }

    fn candidate_files(&self, literal: &str) -> Result<Vec<PathBuf>, IndexError> {
        let idents: Vec<&str> = identifiers(literal).into_iter().collect();
        if idents.is_empty() {
            return Err(IndexError::Other("no identifier-like tokens in query".into()));
        }
        // Intersection across all tokens.
        let mut result: Option<HashSet<PathBuf>> = None;
        for ident in idents {
            let files: HashSet<PathBuf> =
                self.ident_files.get(ident).cloned().unwrap_or_default().into_iter().collect();
            result = Some(match result {
                None => files,
                Some(acc) => acc.intersection(&files).cloned().collect(),
            });
        }
        let mut out: Vec<PathBuf> = result.unwrap_or_default().into_iter().collect();
        out.sort();
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> tempfile::TempDir {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("src")).unwrap();
        std::fs::write(
            tmp.path().join("src/auth.rs"),
            "pub struct AuthToken { id: u64 }\n\npub fn mint_token(user: &str) -> AuthToken {\n    AuthToken { id: 1 }\n}\n\nimpl AuthToken {\n    fn refresh(&self) {}\n}\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("src/main.rs"),
            "mod auth;\n\nfn main() {\n    let t = auth::mint_token(\"u\");\n}\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("app.py"),
            "class Server:\n    def handle(self):\n        pass\n\ndef run_server():\n    pass\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("ui.ts"),
            "export function renderApp() {}\nexport const appConfig = { x: 1 };\nclass Panel {}\n",
        )
        .unwrap();
        tmp
    }

    #[test]
    fn extracts_symbols_across_languages() {
        let tmp = fixture();
        let index = RegexIndex::build(tmp.path());

        let mint = index.find_symbols("mint_token", 10).unwrap();
        assert_eq!(mint.len(), 1);
        assert_eq!(mint[0].kind, SymbolKind::Function);
        assert_eq!(mint[0].file, PathBuf::from("src/auth.rs"));
        assert_eq!(mint[0].line, 3);

        // Indented rust fn = method.
        let refresh = index.find_symbols("refresh", 10).unwrap();
        assert_eq!(refresh[0].kind, SymbolKind::Method);

        assert_eq!(index.find_symbols("Server", 10).unwrap()[0].kind, SymbolKind::Class);
        assert_eq!(index.find_symbols("renderApp", 10).unwrap()[0].kind, SymbolKind::Function);
        assert_eq!(index.find_symbols("appConfig", 10).unwrap()[0].kind, SymbolKind::Variable);
    }

    #[test]
    fn references_rank_and_candidates() {
        let tmp = fixture();
        let index = RegexIndex::build(tmp.path());

        // mint_token is mentioned in two files → refs from both.
        let refs = index.find_references("mint_token", 10).unwrap();
        let files: HashSet<_> = refs.iter().map(|r| r.file.clone()).collect();
        assert!(files.contains(&PathBuf::from("src/auth.rs")));
        assert!(files.contains(&PathBuf::from("src/main.rs")));

        // Cross-file mention boosts rank above single-file symbols.
        let mint = &index.find_symbols("mint_token", 1).unwrap()[0];
        let panel = &index.find_symbols("Panel", 1).unwrap()[0];
        assert!(mint.rank > panel.rank);

        let candidates = index.candidate_files("mint_token").unwrap();
        assert_eq!(candidates.len(), 2);
        assert!(index.candidate_files("&&&").is_err());
    }

    #[test]
    fn outline_and_budgeted_repo_map() {
        let tmp = fixture();
        let index = RegexIndex::build(tmp.path());

        let outline = index.file_outline(Path::new("src/auth.rs")).unwrap();
        let names: Vec<&str> = outline.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec!["AuthToken", "mint_token", "refresh"]);

        let map = index.repo_map(2000).unwrap();
        assert!(map.contains("src/auth.rs"));
        assert!(map.contains("pub fn mint_token"));

        // Budget is respected (tiny budget → tiny map).
        let small = index.repo_map(20).unwrap();
        assert!(small.len() <= 20 * 4 + 80);
        assert!(small.len() < map.len());
    }
}
