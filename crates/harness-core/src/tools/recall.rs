use super::{truncate_output, MAX_OUTPUT_CHARS};
use crate::types::{Tool, ToolContext, ToolDef, ToolOutput};
use serde_json::{json, Value};

/// Searches durable session memory: the notes recorded via `remember` and
/// every archived compaction summary. This is how knowledge from earlier
/// context epochs — summarized away from the live transcript — is brought
/// back on demand.
pub struct RecallTool {
    def: ToolDef,
}

impl RecallTool {
    pub fn new() -> Self {
        Self {
            def: ToolDef {
                name: "recall".into(),
                description: "Search durable session memory: notes recorded with the remember tool and archived summaries of earlier compacted context. Use when you need something from earlier in a long session that is no longer in the visible conversation (a decision, a path, an error already fixed). Returns matching notes verbatim and matching summary lines with their epoch.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "Keywords to search for (matched case-insensitively; more matching terms rank higher)."}
                    },
                    "required": ["query"]
                }),
            },
        }
    }
}

fn score(text: &str, terms: &[String]) -> usize {
    let lower = text.to_lowercase();
    terms.iter().filter(|t| lower.contains(t.as_str())).count()
}

impl Tool for RecallTool {
    fn def(&self) -> &ToolDef {
        &self.def
    }
    fn read_only(&self) -> bool {
        true
    }

    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolOutput {
        let Some(query) = input.get("query").and_then(|v| v.as_str()) else {
            return ToolOutput::err("query is required");
        };
        let terms: Vec<String> = query
            .to_lowercase()
            .split_whitespace()
            .filter(|t| t.len() >= 2)
            .map(String::from)
            .collect();
        if terms.is_empty() {
            return ToolOutput::err("query needs at least one term of 2+ characters");
        }

        let memory = ctx.memory.lock().unwrap().clone();
        let epochs = memory.summaries.len();

        // Notes: whole-note matches, best score first (stable within score).
        let mut notes: Vec<(usize, &String)> = memory
            .notes
            .iter()
            .map(|n| (score(n, &terms), n))
            .filter(|(s, _)| *s > 0)
            .collect();
        notes.sort_by(|a, b| b.0.cmp(&a.0));
        notes.truncate(20);

        // Summaries: line-level matches, labeled with their epoch so the
        // model knows how old the information is.
        let mut lines: Vec<(usize, String)> = Vec::new();
        for (i, summary) in memory.summaries.iter().enumerate() {
            for line in summary.lines() {
                let s = score(line, &terms);
                if s > 0 && !line.trim().is_empty() {
                    lines.push((s, format!("[summary {}/{epochs}] {}", i + 1, line.trim())));
                }
            }
        }
        lines.sort_by(|a, b| b.0.cmp(&a.0));
        lines.truncate(30);

        if notes.is_empty() && lines.is_empty() {
            return ToolOutput::ok(format!(
                "no matches for \"{query}\" in session memory ({} notes, {epochs} archived summaries)",
                memory.notes.len()
            ));
        }

        let mut out = String::new();
        if !notes.is_empty() {
            out.push_str("Matching notes:\n");
            for (_, n) in &notes {
                out.push_str(&format!("- {n}\n"));
            }
        }
        if !lines.is_empty() {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str("Matching lines from archived summaries:\n");
            for (_, l) in &lines {
                out.push_str(&format!("{l}\n"));
            }
        }
        ToolOutput::ok(truncate_output(out.trim_end(), MAX_OUTPUT_CHARS))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CancelToken, SessionMemory};

    fn ctx_with_memory() -> ToolContext {
        let ctx = ToolContext::new(std::path::PathBuf::from("."), CancelToken::new());
        *ctx.memory.lock().unwrap() = SessionMemory {
            notes: vec![
                "auth tokens are minted in src/auth/mint.rs".into(),
                "CI needs libxkbcommon-dev installed".into(),
                "the user prefers small commits".into(),
            ],
            summaries: vec![
                "Goal: fix login.\nState: rewrote src/auth/mint.rs, tests green.\nPitfalls: retry loop in login_handler was a dead end.".into(),
                "Goal: fix login.\nState: shipped auth fix; now refactoring session store.".into(),
            ],
        };
        ctx
    }

    #[test]
    fn finds_notes_and_summary_lines_with_epochs() {
        let tool = RecallTool::new();
        let ctx = ctx_with_memory();
        let out = tool.execute(&json!({"query": "auth mint"}), &ctx);
        assert!(!out.is_error);
        assert!(out.content.contains("src/auth/mint.rs"));
        assert!(out.content.contains("[summary 1/2]"));
        // Multi-term matches rank above single-term ones.
        let notes_pos = out.content.find("auth tokens are minted").unwrap();
        assert!(notes_pos < out.content.find("Matching lines").unwrap());
    }

    #[test]
    fn no_match_reports_inventory() {
        let tool = RecallTool::new();
        let ctx = ctx_with_memory();
        let out = tool.execute(&json!({"query": "kubernetes"}), &ctx);
        assert!(!out.is_error);
        assert!(out.content.contains("no matches"));
        assert!(out.content.contains("3 notes"));
        assert!(out.content.contains("2 archived summaries"));
    }

    #[test]
    fn rejects_empty_queries() {
        let tool = RecallTool::new();
        let ctx = ctx_with_memory();
        assert!(tool.execute(&json!({}), &ctx).is_error);
        assert!(tool.execute(&json!({"query": "a"}), &ctx).is_error);
    }
}
