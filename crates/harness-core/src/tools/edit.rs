use super::fsutil::{mtime_nanos, resolve_safe, str_input};
use crate::types::{Tool, ToolContext, ToolDef, ToolOutput};
use serde_json::{json, Value};

pub struct EditTool {
    def: ToolDef,
}

impl EditTool {
    pub fn new() -> Self {
        Self {
            def: ToolDef {
                name: "edit".into(),
                description: "Exact string replacement in a file. old_string must match exactly once (include enough surrounding context to be unique), or pass replace_all=true. The file must have been read this session and not modified since.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"},
                        "old_string": {"type": "string"},
                        "new_string": {"type": "string"},
                        "replace_all": {"type": "boolean"}
                    },
                    "required": ["path", "old_string", "new_string"]
                }),
            },
        }
    }
}

impl Tool for EditTool {
    fn def(&self) -> &ToolDef {
        &self.def
    }
    fn read_only(&self) -> bool {
        false
    }

    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolOutput {
        let (Some(path), Some(old_str), Some(new_str)) = (
            str_input(input, "path"),
            str_input(input, "old_string"),
            str_input(input, "new_string"),
        ) else {
            return ToolOutput::err("path, old_string, new_string are required");
        };
        let replace_all = input.get("replace_all").and_then(|v| v.as_bool()).unwrap_or(false);
        if old_str == new_str {
            return ToolOutput::err("old_string and new_string are identical");
        }

        let full = match resolve_safe(&ctx.cwd, path) {
            Ok(p) => p,
            Err(e) => return ToolOutput::err(e),
        };
        let recorded = ctx.read_files.lock().unwrap().get(&full).copied();
        let Some(read_mtime) = recorded else {
            return ToolOutput::err(format!("read {path} before editing it"));
        };
        match mtime_nanos(&full) {
            Ok(current) if current != read_mtime => {
                return ToolOutput::err(format!(
                    "{path} changed on disk since you read it; re-read it first"
                ));
            }
            Err(e) => return ToolOutput::err(format!("{path}: {e}")),
            _ => {}
        }

        let text = match std::fs::read_to_string(&full) {
            Ok(t) => t,
            Err(e) => return ToolOutput::err(format!("{path}: {e}")),
        };
        let count = text.matches(old_str).count();
        if count == 0 {
            return ToolOutput::err(format!("old_string not found in {path}"));
        }
        if count > 1 && !replace_all {
            return ToolOutput::err(format!(
                "old_string occurs {count} times; add surrounding context to make it unique or set replace_all=true"
            ));
        }
        let next = if replace_all {
            text.replace(old_str, new_str)
        } else {
            text.replacen(old_str, new_str, 1)
        };
        if let Err(e) = std::fs::write(&full, next) {
            return ToolOutput::err(format!("{path}: {e}"));
        }
        if let Ok(mtime) = mtime_nanos(&full) {
            ctx.read_files.lock().unwrap().insert(full, mtime);
        }
        ToolOutput::ok(format!(
            "replaced {} occurrence(s) in {path}",
            if replace_all { count } else { 1 }
        ))
    }
}
