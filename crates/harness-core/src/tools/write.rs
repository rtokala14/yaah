use super::fsutil::{mtime_nanos, resolve_safe, str_input};
use crate::types::{Tool, ToolContext, ToolDef, ToolOutput};
use serde_json::{json, Value};

pub struct WriteTool {
    def: ToolDef,
}

impl WriteTool {
    pub fn new() -> Self {
        Self {
            def: ToolDef {
                name: "write".into(),
                description: "Create or overwrite a file with the given content. For partial changes to an existing file, use edit instead. Overwriting an existing file requires reading it first.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "path": {"type": "string"},
                        "content": {"type": "string"}
                    },
                    "required": ["path", "content"]
                }),
            },
        }
    }
}

impl Tool for WriteTool {
    fn def(&self) -> &ToolDef {
        &self.def
    }
    fn read_only(&self) -> bool {
        false
    }

    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolOutput {
        let (Some(path), Some(content)) = (str_input(input, "path"), str_input(input, "content"))
        else {
            return ToolOutput::err("path and content are required");
        };
        let full = match resolve_safe(&ctx.cwd, path) {
            Ok(p) => p,
            Err(e) => return ToolOutput::err(e),
        };
        let exists = full.exists();
        if exists && !ctx.read_files.lock().unwrap().contains_key(&full) {
            return ToolOutput::err(format!("refusing to overwrite {path}: read it first"));
        }
        if let Some(parent) = full.parent() {
            if let Err(e) = std::fs::create_dir_all(parent) {
                return ToolOutput::err(format!("mkdir {}: {e}", parent.display()));
            }
        }
        if let Err(e) = std::fs::write(&full, content) {
            return ToolOutput::err(format!("{path}: {e}"));
        }
        if let Ok(mtime) = mtime_nanos(&full) {
            ctx.read_files.lock().unwrap().insert(full, mtime);
        }
        ToolOutput::ok(format!("wrote {} chars to {path}", content.len()))
    }
}
