use super::{truncate_output, MAX_OUTPUT_CHARS};
use crate::types::{Tool, ToolContext, ToolDef, ToolOutput};
use globset::Glob;
use ignore::WalkBuilder;
use serde_json::{json, Value};

pub struct GlobTool {
    def: ToolDef,
}

impl GlobTool {
    pub fn new() -> Self {
        Self {
            def: ToolDef {
                name: "glob".into(),
                description: "List files matching a glob pattern (e.g. 'src/**/*.rs'), sorted by most recently modified. Gitignore-aware.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "pattern": {"type": "string"}
                    },
                    "required": ["pattern"]
                }),
            },
        }
    }
}

impl Tool for GlobTool {
    fn def(&self) -> &ToolDef {
        &self.def
    }
    fn read_only(&self) -> bool {
        true
    }

    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolOutput {
        let Some(pattern) = input.get("pattern").and_then(|v| v.as_str()) else {
            return ToolOutput::err("pattern is required");
        };
        let matcher = match Glob::new(pattern) {
            Ok(g) => g.compile_matcher(),
            Err(e) => return ToolOutput::err(format!("invalid glob: {e}")),
        };

        let mut matches: Vec<(String, u128)> = Vec::new();
        for entry in WalkBuilder::new(&ctx.cwd).hidden(true).build().flatten() {
            if ctx.cancel.is_cancelled() || matches.len() >= 2000 {
                break;
            }
            if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                continue;
            }
            let rel = entry
                .path()
                .strip_prefix(&ctx.cwd)
                .unwrap_or(entry.path())
                .to_string_lossy()
                .replace('\\', "/");
            if !matcher.is_match(&rel) {
                continue;
            }
            let mtime = entry
                .metadata()
                .ok()
                .and_then(|m| m.modified().ok())
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            matches.push((rel, mtime));
        }
        matches.sort_by(|a, b| b.1.cmp(&a.1));

        if matches.is_empty() {
            ToolOutput::ok(format!("no files match {pattern}"))
        } else {
            let list: Vec<String> = matches.into_iter().map(|(p, _)| p).collect();
            ToolOutput::ok(truncate_output(&list.join("\n"), MAX_OUTPUT_CHARS))
        }
    }
}
