use crate::types::{Tool, ToolContext, ToolDef, ToolOutput};
use serde_json::{json, Value};

/// Records a durable note in session memory. Notes survive context
/// compaction: they are re-injected verbatim into every compacted
/// transcript, unlike ordinary conversation content which may be
/// summarized away.
pub struct RememberTool {
    def: ToolDef,
}

impl RememberTool {
    pub fn new() -> Self {
        Self {
            def: ToolDef {
                name: "remember".into(),
                description: "Record a durable note in session memory. Memory notes survive context compaction verbatim, unlike ordinary conversation content. Use for decisions made, constraints discovered, user preferences, and facts that were expensive to learn. One dense sentence per note; include exact paths and identifiers.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "note": {"type": "string", "description": "The note to remember, one dense sentence."}
                    },
                    "required": ["note"]
                }),
            },
        }
    }
}

const MAX_NOTE_CHARS: usize = 500;

impl Tool for RememberTool {
    fn def(&self) -> &ToolDef {
        &self.def
    }
    fn read_only(&self) -> bool {
        true
    }

    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolOutput {
        let Some(note) = input.get("note").and_then(|v| v.as_str()) else {
            return ToolOutput::err("note is required");
        };
        let note = note.trim();
        if note.is_empty() {
            return ToolOutput::err("note is empty");
        }
        if note.len() > MAX_NOTE_CHARS {
            return ToolOutput::err(format!(
                "note too long ({} chars, max {MAX_NOTE_CHARS}) — memory is for dense facts, not transcripts",
                note.len()
            ));
        }
        ctx.memory.lock().unwrap().notes.push(note.to_string());
        ToolOutput::ok("noted — will survive context compaction")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::CancelToken;

    #[test]
    fn records_and_validates_notes() {
        let tool = RememberTool::new();
        let ctx = ToolContext::new(std::path::PathBuf::from("."), CancelToken::new());

        let out = tool.execute(&json!({"note": "  tests live in crates/x  "}), &ctx);
        assert!(!out.is_error);
        assert_eq!(ctx.memory.lock().unwrap().notes.as_slice(), ["tests live in crates/x"]);

        assert!(tool.execute(&json!({}), &ctx).is_error);
        assert!(tool.execute(&json!({"note": "   "}), &ctx).is_error);
        assert!(tool.execute(&json!({"note": "x".repeat(600)}), &ctx).is_error);
        assert_eq!(ctx.memory.lock().unwrap().notes.len(), 1);
    }
}
