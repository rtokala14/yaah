use super::{truncate_output, MAX_OUTPUT_CHARS};
use crate::types::{Tool, ToolContext, ToolDef, ToolOutput};
use globset::{Glob, GlobMatcher};
use ignore::WalkBuilder;
use regex::Regex;
use serde_json::{json, Value};

pub struct GrepTool {
    def: ToolDef,
}

impl GrepTool {
    pub fn new() -> Self {
        Self {
            def: ToolDef {
                name: "grep".into(),
                description: "Regex search across files in the workspace (gitignore-aware). Returns matching lines as path:line:text. Filter with the glob parameter (e.g. '**/*.rs').".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "pattern": {"type": "string", "description": "Rust-flavored regular expression"},
                        "glob": {"type": "string", "description": "Only search files matching this glob"},
                        "max_results": {"type": "number", "description": "default 200"}
                    },
                    "required": ["pattern"]
                }),
            },
        }
    }
}

impl Tool for GrepTool {
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
        let re = match Regex::new(pattern) {
            Ok(r) => r,
            Err(e) => return ToolOutput::err(format!("invalid regex: {e}")),
        };
        let glob: Option<GlobMatcher> = match input.get("glob").and_then(|v| v.as_str()) {
            Some(g) => match Glob::new(g) {
                Ok(g) => Some(g.compile_matcher()),
                Err(e) => return ToolOutput::err(format!("invalid glob: {e}")),
            },
            None => None,
        };
        let max_results = input
            .get("max_results")
            .and_then(|v| v.as_f64())
            .unwrap_or(200.0)
            .min(1000.0) as usize;

        let mut results: Vec<String> = Vec::new();
        let mut searched = 0usize;
        let walker = WalkBuilder::new(&ctx.cwd).hidden(true).max_filesize(Some(2_000_000)).build();
        'outer: for entry in walker.flatten() {
            if ctx.cancel.is_cancelled() {
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
            if let Some(g) = &glob {
                if !g.is_match(&rel) {
                    continue;
                }
            }
            let Ok(text) = std::fs::read_to_string(entry.path()) else { continue };
            if text.contains('\0') {
                continue; // binary
            }
            searched += 1;
            for (i, line) in text.lines().enumerate() {
                if re.is_match(line) {
                    let shown: String = line.chars().take(300).collect();
                    results.push(format!("{rel}:{}:{shown}", i + 1));
                    if results.len() >= max_results {
                        results.push(format!(
                            "[... hit max_results={max_results}; tighten the pattern or glob]"
                        ));
                        break 'outer;
                    }
                }
            }
        }

        if results.is_empty() {
            ToolOutput::ok(format!("no matches for /{pattern}/ in {searched} files"))
        } else {
            ToolOutput::ok(truncate_output(&results.join("\n"), MAX_OUTPUT_CHARS))
        }
    }
}
