//! harness-index — local codebase index (interface + design; implementation
//! is phase 2, see INDEX-DESIGN.md at the workspace root).
//!
//! Purpose: give agents *standing knowledge* of a codebase instead of
//! rediscovering it with grep walks every session, without the failure modes
//! that made most harnesses drop embedding-RAG (staleness, chunk noise,
//! index infra). The design is code-native — symbols, references, and
//! structure first; token-level search second; embeddings optional and last.
//!
//! The index is exposed to agents as *tools* (`symbols`, `refs`, `outline`)
//! and to the harness as a *context primer* (a compact repo map injected
//! once per session, aider-style). Both consume this trait.

use serde::Serialize;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum SymbolKind {
    Function,
    Method,
    Struct,
    Enum,
    Trait,
    Interface,
    Class,
    Module,
    Constant,
    TypeAlias,
    Variable,
}

#[derive(Debug, Clone, Serialize)]
pub struct Symbol {
    pub name: String,
    pub kind: SymbolKind,
    pub file: PathBuf,
    /// 1-based line of the definition.
    pub line: u32,
    /// One-line signature as written in source (for the repo map).
    pub signature: String,
    /// Graph centrality score (PageRank over the reference graph) — used to
    /// rank the repo map under a token budget.
    pub rank: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct Reference {
    pub file: PathBuf,
    pub line: u32,
    pub context: String,
}

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("index not built")]
    NotBuilt,
    #[error("{0}")]
    Other(String),
}

/// The query surface both the agent tools and the harness primer consume.
/// Implementations must answer from the index in low single-digit
/// milliseconds — the whole point is being cheaper than a grep walk.
pub trait CodeIndex: Send + Sync {
    /// Fuzzy/prefix symbol lookup ("where is X defined").
    fn find_symbols(&self, query: &str, limit: usize) -> Result<Vec<Symbol>, IndexError>;

    /// All reference sites of an exact symbol name ("who calls X").
    fn find_references(&self, name: &str, limit: usize) -> Result<Vec<Reference>, IndexError>;

    /// Definition outline of one file (for cheap file orientation without
    /// reading the body).
    fn file_outline(&self, file: &std::path::Path) -> Result<Vec<Symbol>, IndexError>;

    /// The top-ranked symbols across the repo, under a token budget — the
    /// session primer ("repo map"). Deterministic for cache stability.
    fn repo_map(&self, max_tokens: usize) -> Result<String, IndexError>;

    /// Trigram-accelerated literal/regex candidate filtering: returns the
    /// files that *could* match, letting grep run on 1% of the tree.
    fn candidate_files(&self, literal: &str) -> Result<Vec<PathBuf>, IndexError>;
}

/// Placeholder used until phase 2 lands: every query reports NotBuilt, and
/// callers (tools, primer) degrade gracefully to grep/glob.
pub struct NullIndex;

impl CodeIndex for NullIndex {
    fn find_symbols(&self, _: &str, _: usize) -> Result<Vec<Symbol>, IndexError> {
        Err(IndexError::NotBuilt)
    }
    fn find_references(&self, _: &str, _: usize) -> Result<Vec<Reference>, IndexError> {
        Err(IndexError::NotBuilt)
    }
    fn file_outline(&self, _: &std::path::Path) -> Result<Vec<Symbol>, IndexError> {
        Err(IndexError::NotBuilt)
    }
    fn repo_map(&self, _: usize) -> Result<String, IndexError> {
        Err(IndexError::NotBuilt)
    }
    fn candidate_files(&self, _: &str) -> Result<Vec<PathBuf>, IndexError> {
        Err(IndexError::NotBuilt)
    }
}
