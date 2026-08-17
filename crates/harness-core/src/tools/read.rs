use super::fsutil::{mtime_nanos, num_input, resolve_safe, str_input};
use super::{truncate_output, MAX_OUTPUT_CHARS};
use crate::types::{Tool, ToolContext, ToolDef, ToolOutput};
use serde_json::{json, Value};

pub struct ReadTool {
    def: ToolDef,
}

impl ReadTool {
    pub fn new() -> Self {
        Self {
            def: ToolDef {
                name: "read".into(),
                description: "Read a text file. Returns numbered lines. Use offset/limit for large files — reading only the region you need keeps context small.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"},
                        "offset": {"type": "number", "description": "1-based first line (default 1)"},
                        "limit": {"type": "number", "description": "max lines (default 1500)"}
                    },
                    "required": ["path"]
                }),
            },
        }
    }
}

impl Tool for ReadTool {
    fn def(&self) -> &ToolDef {
        &self.def
    }
    fn read_only(&self) -> bool {
        true
    }

    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolOutput {
        let Some(path) = str_input(input, "path") else {
            return ToolOutput::err("path is required");
        };
        let full = match resolve_safe(&ctx.cwd, path) {
            Ok(p) => p,
            Err(e) => return ToolOutput::err(e),
        };
        let meta = match std::fs::metadata(&full) {
            Ok(m) => m,
            Err(e) => return ToolOutput::err(format!("{path}: {e}")),
        };
        if meta.len() > 5_000_000 {
            return ToolOutput::err(format!(
                "file too large ({} bytes); use bash with head/grep",
                meta.len()
            ));
        }
        let text = match std::fs::read_to_string(&full) {
            Ok(t) => t,
            Err(e) => return ToolOutput::err(format!("{path}: {e}")),
        };
        if let Ok(mtime) = mtime_nanos(&full) {
            ctx.read_files.lock().unwrap().insert(full.clone(), mtime);
        }

        let lines: Vec<&str> = text.split('\n').collect();
        let offset = num_input(input, "offset").unwrap_or(1.0).max(1.0) as usize;
        let limit = num_input(input, "limit").unwrap_or(1500.0).min(5000.0) as usize;
        let slice: Vec<String> = lines
            .iter()
            .skip(offset - 1)
            .take(limit)
            .enumerate()
            .map(|(i, l)| format!("{:>5}\t{}", offset + i, l))
            .collect();
        let shown = slice.len();
        let remaining = lines.len().saturating_sub(offset - 1 + shown);
        let mut body = truncate_output(&slice.join("\n"), MAX_OUTPUT_CHARS);
        if remaining > 0 {
            body.push_str(&format!(
                "\n[... {remaining} more lines; re-read with offset={}]",
                offset + shown
            ));
        }
        ToolOutput::ok(body)
    }
}
