//! Phase-2 index: gitignore-aware scan, per-language regex symbol
//! extraction, an identifier→files inverted map, **file-graph PageRank**
//! ranking, and **mtime-validated incremental refresh** — the index is
//! never trusted-but-wrong: every query revalidates (debounced) against
//! the filesystem, reparsing only what changed. Tree-sitter precision and
//! per-worktree overlays remain future work (INDEX-DESIGN.md).

use crate::{CodeIndex, IndexError, Reference, Symbol, SymbolKind};
use ignore::WalkBuilder;
use regex::Regex;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, RwLock};
use std::time::{Duration, Instant};

/// Per-file size cap and total corpus cap keep build time and memory
/// bounded on large repos; skipped files simply don't contribute.
const MAX_FILE_BYTES: u64 = 512 * 1024;
const MAX_TOTAL_BYTES: u64 = 64 * 1024 * 1024;
/// Watcher-less fallback: lazy queries revalidate at most this often.
const REFRESH_DEBOUNCE: Duration = Duration::from_millis(1000);
/// With a live watcher, revalidate anyway this often (dropped-event net).
const WATCHER_SAFETY_NET: Duration = Duration::from_secs(60);
/// Grep-correctness path: how recent a refresh may be to skip re-walking
/// when the watcher reports clean.
const CANDIDATE_MAX_AGE: Duration = Duration::from_secs(2);
/// Identifiers defined in more than this many files are too generic to
/// contribute reference-graph edges.
const MAX_DEF_FANOUT: usize = 20;
const PAGERANK_ITERS: usize = 20;
const PAGERANK_DAMPING: f64 = 0.85;

struct LangSpec {
    extensions: &'static [&'static str],
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

fn java_kind(keyword: &str, _indented: bool) -> SymbolKind {
    match keyword {
        "interface" => SymbolKind::Interface,
        "enum" => SymbolKind::Enum,
        "record" => SymbolKind::Struct,
        _ => SymbolKind::Class,
    }
}

fn c_kind(keyword: &str, _indented: bool) -> SymbolKind {
    match keyword {
        "struct" | "union" => SymbolKind::Struct,
        "enum" => SymbolKind::Enum,
        "namespace" => SymbolKind::Module,
        _ => SymbolKind::Class,
    }
}

fn rb_kind(keyword: &str, indented: bool) -> SymbolKind {
    match keyword {
        "class" => SymbolKind::Class,
        "module" => SymbolKind::Module,
        _ if indented => SymbolKind::Method,
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
        LangSpec {
            extensions: &["java", "kt", "kts"],
            patterns: vec![(
                Regex::new(
                    r"(?m)^([ \t]*)(?:(?:public|private|protected|static|final|abstract|sealed|data|open)\s+)*(class|interface|enum|record|object)\s+([A-Za-z_][A-Za-z0-9_]*)",
                )
                .unwrap(),
                java_kind,
            )],
        },
        LangSpec {
            extensions: &["c", "h", "cc", "cpp", "hpp", "cxx"],
            patterns: vec![(
                Regex::new(
                    r"(?m)^([ \t]*)(?:typedef\s+)?(struct|class|enum|namespace|union)\s+([A-Za-z_][A-Za-z0-9_]*)",
                )
                .unwrap(),
                c_kind,
            )],
        },
        LangSpec {
            extensions: &["rb"],
            patterns: vec![(
                Regex::new(r"(?m)^([ \t]*)(def|class|module)\s+([A-Za-z_][A-Za-z0-9_]*[?!]?)").unwrap(),
                rb_kind,
            )],
        },
    ]
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

struct FileEntry {
    mtime: u128,
    text: String,
    /// Unique identifiers appearing in the file (sorted).
    idents: Vec<String>,
    /// Symbols defined here (rank filled in at finalize).
    symbols: Vec<Symbol>,
}

#[derive(Default)]
struct Inner {
    files: HashMap<PathBuf, FileEntry>,
    /// Ranked global view: rank desc, then path, then line.
    symbols: Vec<Symbol>,
    ident_files: HashMap<String, Vec<PathBuf>>,
    /// How many files DEFINE a symbol of this name — the data-driven
    /// genericity signal (`fn new` is defined everywhere; `mint_token`
    /// once). The repo map prefers distinctive names.
    def_fanout: HashMap<String, usize>,
}

/// The live index: interior-mutable, revalidating, watcher-invalidated.
pub struct RegexIndex {
    root: PathBuf,
    inner: RwLock<Inner>,
    last_refresh: Mutex<Option<Instant>>,
    /// Set by the fs watcher when something under the root changed; a
    /// query that sees it refreshes immediately.
    dirty: std::sync::Arc<std::sync::atomic::AtomicBool>,
    /// Present while the watcher thread is alive. When the watcher failed
    /// to start (inotify limits, exotic fs), queries fall back to the
    /// 1s-debounced stat walk.
    _watcher: Option<notify::RecommendedWatcher>,
}

fn file_mtime(meta: &std::fs::Metadata) -> u128 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0)
}

fn parse_file(path: &Path, text: &str, by_ext: &HashMap<&str, &LangSpec>) -> Vec<Symbol> {
    // Precision first: a real syntax tree when a grammar exists (strings
    // and comments can't fake definitions; methods know their container).
    if let Some(symbols) = crate::sitter::ts_symbols(path, text) {
        return symbols;
    }
    // Regex fallback for the rest (java/kotlin, c/c++, ruby, …).
    let Some(ext) = path.extension().and_then(|e| e.to_str()) else { return Vec::new() };
    let Some(spec) = by_ext.get(ext) else { return Vec::new() };
    let mut symbols = Vec::new();
    for (regex, kind_fn) in &spec.patterns {
        for caps in regex.captures_iter(text) {
            let indent = caps.get(1).map(|m| !m.as_str().is_empty()).unwrap_or(false);
            let keyword = caps.get(2).map(|m| m.as_str()).unwrap_or("");
            let Some(name) = caps.get(3).map(|m| m.as_str()) else { continue };
            let offset = caps.get(0).map(|m| m.start()).unwrap_or(0);
            let line = text[..offset].matches('\n').count() as u32 + 1;
            let signature: String =
                text[offset..].lines().next().unwrap_or("").trim().chars().take(100).collect();
            symbols.push(Symbol {
                name: name.to_string(),
                kind: kind_fn(keyword, indent),
                file: path.to_path_buf(),
                line,
                signature,
                rank: 0.0,
            });
        }
    }
    symbols
}

/// Extensions the index cares about — watcher events on other files are
/// noise and must not trigger refreshes.
fn indexable_ext(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()),
        Some(
            "rs" | "py" | "ts" | "tsx" | "js" | "jsx" | "mjs" | "go" | "java" | "kt" | "kts"
                | "c" | "h" | "cc" | "cpp" | "hpp" | "cxx" | "rb"
        )
    )
}

impl RegexIndex {
    pub fn build(root: &Path) -> Self {
        use notify::Watcher;
        let dirty = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let watcher_dirty = std::sync::Arc::clone(&dirty);
        let watcher = notify::recommended_watcher(move |event: Result<notify::Event, _>| {
            if let Ok(event) = event {
                // Directory events (renames/creates) matter too; file
                // events only when the file could be indexed.
                let relevant = event
                    .paths
                    .iter()
                    .any(|p| indexable_ext(p) || p.extension().is_none());
                if relevant {
                    watcher_dirty.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
        })
        .ok()
        .and_then(|mut w| {
            w.watch(root, notify::RecursiveMode::Recursive).ok()?;
            Some(w)
        });

        let index = Self {
            root: root.to_path_buf(),
            inner: RwLock::new(Inner::default()),
            last_refresh: Mutex::new(None),
            dirty,
            _watcher: watcher,
        };
        index.refresh_now();
        index
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn file_count(&self) -> usize {
        self.inner.read().unwrap().files.len()
    }

    /// Revalidate against the filesystem immediately: stat-walk, reparse
    /// changed/new files, drop deleted ones, re-rank if anything moved.
    pub fn refresh_now(&self) {
        self.dirty.store(false, std::sync::atomic::Ordering::Relaxed);
        self.refresh_inner();
        *self.last_refresh.lock().unwrap() = Some(Instant::now());
    }

    /// Revalidation used by queries. With a live watcher, the dirty flag
    /// is the trigger (plus a slow safety net in case events were
    /// dropped); without one, fall back to the debounced stat walk.
    fn refresh_if_stale(&self) {
        if self.dirty.load(std::sync::atomic::Ordering::Relaxed) {
            self.refresh_now();
            return;
        }
        let deadline = if self._watcher.is_some() {
            WATCHER_SAFETY_NET
        } else {
            REFRESH_DEBOUNCE
        };
        let stale = match *self.last_refresh.lock().unwrap() {
            Some(t) => t.elapsed() >= deadline,
            None => true,
        };
        if stale {
            self.refresh_now();
        }
    }

    fn refresh_inner(&self) {
        let specs = lang_specs();
        let by_ext: HashMap<&str, &LangSpec> =
            specs.iter().flat_map(|s| s.extensions.iter().map(move |e| (*e, s))).collect();

        // Stat walk: what exists now, with mtimes.
        let mut seen: HashMap<PathBuf, (u128, PathBuf)> = HashMap::new();
        let mut total: u64 = 0;
        for entry in WalkBuilder::new(&self.root).hidden(true).build().flatten() {
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
            total += meta.len();
            let rel = path.strip_prefix(&self.root).unwrap_or(path).to_path_buf();
            seen.insert(rel, (file_mtime(&meta), path.to_path_buf()));
        }

        let mut inner = self.inner.write().unwrap();
        let mut changed = false;

        // Drop deleted files.
        let removed: Vec<PathBuf> =
            inner.files.keys().filter(|p| !seen.contains_key(*p)).cloned().collect();
        for path in removed {
            inner.files.remove(&path);
            changed = true;
        }

        // Add/update new and modified files.
        for (rel, (mtime, abs)) in seen {
            let stale = match inner.files.get(&rel) {
                Some(entry) => entry.mtime != mtime,
                None => true,
            };
            if !stale {
                continue;
            }
            let Ok(text) = std::fs::read_to_string(&abs) else { continue };
            let mut idents: Vec<String> =
                identifiers(&text).into_iter().map(String::from).collect();
            idents.sort();
            let symbols = parse_file(&rel, &text, &by_ext);
            inner.files.insert(rel, FileEntry { mtime, text, idents, symbols });
            changed = true;
        }

        if changed {
            finalize(&mut inner);
        }
    }
}

/// Rebuild the derived views: inverted map, file PageRank, ranked symbols.
fn finalize(inner: &mut Inner) {
    let mut paths: Vec<PathBuf> = inner.files.keys().cloned().collect();
    paths.sort();

    // Inverted identifier map.
    let mut ident_files: HashMap<String, Vec<PathBuf>> = HashMap::new();
    for path in &paths {
        for ident in &inner.files[path].idents {
            ident_files.entry(ident.clone()).or_default().push(path.clone());
        }
    }

    // Definition map: symbol name -> defining file indices.
    let idx_of: HashMap<&PathBuf, usize> =
        paths.iter().enumerate().map(|(i, p)| (p, i)).collect();
    let mut def_map: HashMap<&str, Vec<usize>> = HashMap::new();
    for path in &paths {
        for sym in &inner.files[path].symbols {
            def_map.entry(sym.name.as_str()).or_default().push(idx_of[path]);
        }
    }
    let def_fanout: HashMap<String, usize> = def_map
        .iter()
        .map(|(name, defs)| {
            let mut files: Vec<usize> = defs.clone();
            files.dedup();
            (name.to_string(), files.len())
        })
        .collect();

    // Reference graph: A mentions a symbol defined in B (A != B) => A -> B.
    let n = paths.len().max(1);
    let mut out_edges: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (a_i, a) in paths.iter().enumerate() {
        let mut targets: HashSet<usize> = HashSet::new();
        for ident in &inner.files[a].idents {
            if let Some(defs) = def_map.get(ident.as_str()) {
                if defs.len() > MAX_DEF_FANOUT {
                    continue;
                }
                for &b_i in defs {
                    if b_i != a_i {
                        targets.insert(b_i);
                    }
                }
            }
        }
        let mut t: Vec<usize> = targets.into_iter().collect();
        t.sort_unstable();
        out_edges[a_i] = t;
    }

    // PageRank over files (dangling mass spread evenly).
    let mut pr = vec![1.0f64 / n as f64; n];
    for _ in 0..PAGERANK_ITERS {
        let mut next = vec![(1.0 - PAGERANK_DAMPING) / n as f64; n];
        let mut dangling = 0.0f64;
        for (a, targets) in out_edges.iter().enumerate() {
            if targets.is_empty() {
                dangling += pr[a];
            } else {
                let share = PAGERANK_DAMPING * pr[a] / targets.len() as f64;
                for &b in targets {
                    next[b] += share;
                }
            }
        }
        let dangle_share = PAGERANK_DAMPING * dangling / n as f64;
        for v in next.iter_mut() {
            *v += dangle_share;
        }
        pr = next;
    }
    let max_pr = pr.iter().cloned().fold(f64::MIN, f64::max).max(f64::MIN_POSITIVE);

    // Ranked global symbol view: file rank leads, cross-file mentions
    // break ties within a file.
    let mut symbols: Vec<Symbol> = Vec::new();
    for (i, path) in paths.iter().enumerate() {
        let frank = (pr[i] / max_pr * 100.0) as f32;
        for sym in &inner.files[path].symbols {
            let mentions = ident_files.get(&sym.name).map(|f| f.len()).unwrap_or(1) as f32;
            let mut s = sym.clone();
            s.rank = frank * 1000.0 + (mentions - 1.0);
            symbols.push(s);
        }
    }
    symbols.sort_by(|a, b| {
        b.rank
            .partial_cmp(&a.rank)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.file.cmp(&b.file))
            .then_with(|| a.line.cmp(&b.line))
    });

    inner.ident_files = ident_files;
    inner.symbols = symbols;
    inner.def_fanout = def_fanout;
}

/// Repo-map preference: types anchor understanding, then free functions;
/// methods and locals only when space remains.
fn kind_priority(kind: SymbolKind) -> u8 {
    match kind {
        SymbolKind::Struct
        | SymbolKind::Enum
        | SymbolKind::Trait
        | SymbolKind::Interface
        | SymbolKind::Class => 0,
        SymbolKind::Module | SymbolKind::TypeAlias => 1,
        SymbolKind::Function => 2,
        SymbolKind::Method => 3,
        SymbolKind::Constant | SymbolKind::Variable => 4,
    }
}

/// A name defined in this many files or more is generic boilerplate
/// (`new`, `default`, `tests`, accessor names) — excluded from the map.
const GENERIC_DEF_FANOUT: usize = 3;

impl CodeIndex for RegexIndex {
    fn find_symbols(&self, query: &str, limit: usize) -> Result<Vec<Symbol>, IndexError> {
        self.refresh_if_stale();
        let q = query.to_lowercase();
        let inner = self.inner.read().unwrap();
        Ok(inner
            .symbols
            .iter()
            .filter(|s| s.name.to_lowercase().contains(&q))
            .take(limit)
            .cloned()
            .collect())
    }

    fn find_references(&self, name: &str, limit: usize) -> Result<Vec<Reference>, IndexError> {
        self.refresh_if_stale();
        let inner = self.inner.read().unwrap();
        let mut out = Vec::new();
        let candidates = inner.ident_files.get(name).cloned().unwrap_or_default();
        for path in candidates {
            let Some(entry) = inner.files.get(&path) else { continue };
            for (i, line) in entry.text.lines().enumerate() {
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
        self.refresh_if_stale();
        let inner = self.inner.read().unwrap();
        let mut out: Vec<Symbol> =
            inner.symbols.iter().filter(|s| s.file == file).cloned().collect();
        out.sort_by_key(|s| s.line);
        Ok(out)
    }

    fn repo_map(&self, max_tokens: usize) -> Result<String, IndexError> {
        self.refresh_if_stale();
        let inner = self.inner.read().unwrap();
        let budget_chars = max_tokens.saturating_mul(4);
        let mut file_order: Vec<&PathBuf> = Vec::new();
        let mut seen = HashSet::new();
        for s in &inner.symbols {
            if seen.insert(&s.file) {
                file_order.push(&s.file);
            }
        }
        let mut out = String::new();
        for path in file_order {
            // Distinctive symbols first: skip names defined all over the
            // codebase (fn new / mod tests / accessors), lead with types,
            // keep source order within a priority tier.
            let mut picks: Vec<&Symbol> = inner
                .symbols
                .iter()
                .filter(|s| &s.file == path)
                .filter(|s| {
                    inner.def_fanout.get(&s.name).copied().unwrap_or(1) < GENERIC_DEF_FANOUT
                })
                .collect();
            picks.sort_by_key(|s| (kind_priority(s.kind), s.line));
            let mut section = format!("{}\n", path.display());
            for s in picks.into_iter().take(8) {
                section.push_str(&format!("  {}\n", s.signature));
            }
            if section.lines().count() <= 1 {
                continue; // nothing distinctive to say about this file
            }
            if out.len() + section.len() > budget_chars {
                break;
            }
            out.push_str(&section);
        }
        Ok(out.trim_end().to_string())
    }

    fn candidate_files(&self, literal: &str) -> Result<Vec<PathBuf>, IndexError> {
        // Grep correctness depends on currency. With a clean watcher and a
        // fresh refresh we can trust the index; otherwise re-walk now.
        let fresh_enough = self._watcher.is_some()
            && !self.dirty.load(std::sync::atomic::Ordering::Relaxed)
            && matches!(*self.last_refresh.lock().unwrap(),
                Some(t) if t.elapsed() < CANDIDATE_MAX_AGE);
        if !fresh_enough {
            self.refresh_now();
        }
        let inner = self.inner.read().unwrap();
        let idents: Vec<&str> = identifiers(literal).into_iter().collect();
        if idents.is_empty() {
            return Err(IndexError::Other("no identifier-like tokens in query".into()));
        }
        let mut result: Option<HashSet<PathBuf>> = None;
        for ident in idents {
            let files: HashSet<PathBuf> =
                inner.ident_files.get(ident).cloned().unwrap_or_default().into_iter().collect();
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
        std::fs::write(
            tmp.path().join("Svc.java"),
            "public final class AuthService {}\npublic interface TokenStore {}\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("geo.hpp"),
            "namespace geo {\nstruct Point { int x; };\nenum Axis { X, Y };\n}\n",
        )
        .unwrap();
        std::fs::write(
            tmp.path().join("job.rb"),
            "module Jobs\n  class Mailer\n    def deliver!\n    end\n  end\nend\n",
        )
        .unwrap();
        let index = RegexIndex::build(tmp.path());

        let mint = index.find_symbols("mint_token", 10).unwrap();
        assert_eq!(mint.len(), 1);
        assert_eq!(mint[0].kind, SymbolKind::Function);
        assert_eq!(mint[0].file, PathBuf::from("src/auth.rs"));
        assert_eq!(mint[0].line, 3);
        assert_eq!(index.find_symbols("refresh", 10).unwrap()[0].kind, SymbolKind::Method);
        assert_eq!(index.find_symbols("Server", 10).unwrap()[0].kind, SymbolKind::Class);
        assert_eq!(index.find_symbols("appConfig", 10).unwrap()[0].kind, SymbolKind::Variable);
        // New languages.
        assert_eq!(index.find_symbols("AuthService", 10).unwrap()[0].kind, SymbolKind::Class);
        assert_eq!(index.find_symbols("TokenStore", 10).unwrap()[0].kind, SymbolKind::Interface);
        assert_eq!(index.find_symbols("Point", 10).unwrap()[0].kind, SymbolKind::Struct);
        assert_eq!(index.find_symbols("geo", 10).unwrap()[0].kind, SymbolKind::Module);
        assert_eq!(index.find_symbols("Mailer", 10).unwrap()[0].kind, SymbolKind::Class);
        assert_eq!(index.find_symbols("deliver!", 10).unwrap()[0].kind, SymbolKind::Method);
    }

    #[test]
    fn references_and_candidates() {
        let tmp = fixture();
        let index = RegexIndex::build(tmp.path());
        let refs = index.find_references("mint_token", 10).unwrap();
        let files: HashSet<_> = refs.iter().map(|r| r.file.clone()).collect();
        assert!(files.contains(&PathBuf::from("src/auth.rs")));
        assert!(files.contains(&PathBuf::from("src/main.rs")));

        let candidates = index.candidate_files("mint_token").unwrap();
        assert_eq!(candidates.len(), 2);
        assert!(index.candidate_files("&&&").is_err());
    }

    #[test]
    fn pagerank_ranks_referenced_files_higher() {
        let tmp = tempfile::tempdir().unwrap();
        // core.rs defines things three other files use; leaf.rs is unused.
        std::fs::write(
            tmp.path().join("core.rs"),
            "pub fn central_helper() {}\npub struct CoreThing;\n",
        )
        .unwrap();
        for user in ["a.rs", "b.rs", "c.rs"] {
            std::fs::write(
                tmp.path().join(user),
                "fn go() { central_helper(); let _x: CoreThing; }\n",
            )
            .unwrap();
        }
        std::fs::write(tmp.path().join("leaf.rs"), "pub fn lonely_fn() {}\n").unwrap();
        let index = RegexIndex::build(tmp.path());

        let central = &index.find_symbols("central_helper", 1).unwrap()[0];
        let lonely = &index.find_symbols("lonely_fn", 1).unwrap()[0];
        assert!(
            central.rank > lonely.rank,
            "hub file symbol ({}) must outrank leaf ({})",
            central.rank,
            lonely.rank
        );
        // The repo map leads with the hub file.
        let map = index.repo_map(2000).unwrap();
        let core_pos = map.find("core.rs").unwrap();
        let leaf_pos = map.find("leaf.rs").unwrap_or(usize::MAX);
        assert!(core_pos < leaf_pos);
    }

    #[test]
    fn incremental_refresh_tracks_edits_adds_and_deletes() {
        let tmp = fixture();
        let index = RegexIndex::build(tmp.path());
        assert_eq!(index.find_symbols("brand_new_fn", 10).unwrap().len(), 0);
        let count_before = index.file_count();

        // Add a file, modify one, delete one.
        std::thread::sleep(Duration::from_millis(20)); // distinct mtimes
        std::fs::write(tmp.path().join("newmod.rs"), "pub fn brand_new_fn() {}\n").unwrap();
        std::fs::write(
            tmp.path().join("src/auth.rs"),
            "pub fn renamed_mint(user: &str) -> u64 { 0 }\n",
        )
        .unwrap();
        std::fs::remove_file(tmp.path().join("ui.ts")).unwrap();
        index.refresh_now();

        assert_eq!(index.find_symbols("brand_new_fn", 10).unwrap().len(), 1);
        assert_eq!(index.find_symbols("renamed_mint", 10).unwrap().len(), 1);
        assert_eq!(index.find_symbols("mint_token", 10).unwrap().len(), 0, "old symbol gone");
        assert_eq!(index.find_symbols("renderApp", 10).unwrap().len(), 0, "deleted file gone");
        assert_eq!(index.file_count(), count_before); // +1 new, -1 deleted
        // References reflect the new content, not the cached old text.
        assert!(index
            .find_references("renamed_mint", 10)
            .unwrap()
            .iter()
            .any(|r| r.file == PathBuf::from("src/auth.rs")));
    }

    #[cfg(unix)]
    #[test]
    fn watcher_invalidates_without_manual_refresh() {
        let tmp = fixture();
        let index = RegexIndex::build(tmp.path());
        assert_eq!(index.find_symbols("watched_fn", 5).unwrap().len(), 0);

        std::fs::write(tmp.path().join("watched.rs"), "pub fn watched_fn() {}\n").unwrap();
        // No refresh_now here: the watcher's dirty flag must do it. Event
        // delivery is async — poll briefly.
        let mut found = false;
        for _ in 0..60 {
            std::thread::sleep(Duration::from_millis(50));
            if index.find_symbols("watched_fn", 5).unwrap().len() == 1 {
                found = true;
                break;
            }
        }
        assert!(found, "watcher event should have invalidated the index");
    }

    #[test]
    fn treesitter_precision_replaces_regex_for_covered_languages() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("t.rs"),
            "// fn commented() {}\nconst S: &str = \"fn faked() {}\";\npub fn genuine() {}\n",
        )
        .unwrap();
        let index = RegexIndex::build(tmp.path());
        assert_eq!(index.find_symbols("genuine", 5).unwrap().len(), 1);
        assert_eq!(index.find_symbols("commented", 5).unwrap().len(), 0);
        assert_eq!(index.find_symbols("faked", 5).unwrap().len(), 0);
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
        let small = index.repo_map(20).unwrap();
        assert!(small.len() < map.len());
    }
}
