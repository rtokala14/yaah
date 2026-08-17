//! Built-in tool suite.
//!
//! Design principles (harness/DESIGN.md):
//!  - Small orthogonal set: bash + read/write/edit + grep/glob. Everything
//!    else goes through bash.
//!  - Read-only tools run in parallel; a mutating call is a barrier.
//!  - edit enforces read-before-write and mtime staleness checks — the
//!    invariants that justify dedicated tools over bash.
//!  - Every output is token-budgeted with an explicit truncation marker
//!    telling the model how to narrow the request; never silent.

mod bash;
mod edit;
mod fsutil;
mod glob_tool;
mod grep;
mod read;
mod write;

pub use bash::BashTool;
pub use edit::EditTool;
pub use glob_tool::GlobTool;
pub use grep::GrepTool;
pub use read::ReadTool;
pub use write::WriteTool;

use crate::types::Tool;
use std::sync::Arc;

pub const MAX_OUTPUT_CHARS: usize = 40_000; // ~10k tokens per tool result

pub fn builtin_tools() -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(BashTool::new()),
        Arc::new(ReadTool::new()),
        Arc::new(WriteTool::new()),
        Arc::new(EditTool::new()),
        Arc::new(GrepTool::new()),
        Arc::new(GlobTool::new()),
    ]
}

/// Middle-out truncation with an explicit marker.
pub fn truncate_output(s: &str, limit: usize) -> String {
    if s.len() <= limit {
        return s.to_string();
    }
    let half = limit / 2;
    let head_end = floor_char_boundary(s, half);
    let tail_start = ceil_char_boundary(s, s.len() - half);
    format!(
        "{}\n\n[... output truncated: {} chars omitted. Narrow the request (offset/limit, tighter pattern, head/tail) to see more ...]\n\n{}",
        &s[..head_end],
        s.len() - limit,
        &s[tail_start..]
    )
}

fn floor_char_boundary(s: &str, mut i: usize) -> usize {
    while i > 0 && !s.is_char_boundary(i) {
        i -= 1;
    }
    i
}

fn ceil_char_boundary(s: &str, mut i: usize) -> usize {
    while i < s.len() && !s.is_char_boundary(i) {
        i += 1;
    }
    i
}
