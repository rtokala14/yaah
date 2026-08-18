//! Code-index tools: `symbols`, `refs`, `outline` — millisecond answers
//! from the session's in-memory index instead of grep walks.

use crate::types::{Tool, ToolContext, ToolDef, ToolOutput};
use harness_index::CodeIndex;
use serde_json::{json, Value};
use std::sync::Arc;

fn render_symbols(symbols: &[harness_index::Symbol]) -> String {
    symbols
        .iter()
        .map(|s| format!("{}:{}  {:?}  {}", s.file.display(), s.line, s.kind, s.signature))
        .collect::<Vec<_>>()
        .join("\n")
}

pub struct SymbolsTool {
    def: ToolDef,
    index: Arc<dyn CodeIndex>,
}

impl SymbolsTool {
    pub fn new(index: Arc<dyn CodeIndex>) -> Self {
        Self {
            def: ToolDef {
                name: "symbols".into(),
                description: "Find where a symbol (function, type, class, …) is DEFINED, by name substring. Much cheaper than grep for \"where is X\" questions. Returns file:line, kind, and the definition line.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {"query": {"type": "string"}},
                    "required": ["query"]
                }),
            },
            index,
        }
    }
}

impl Tool for SymbolsTool {
    fn def(&self) -> &ToolDef {
        &self.def
    }
    fn read_only(&self) -> bool {
        true
    }
    fn execute(&self, input: &Value, _ctx: &ToolContext) -> ToolOutput {
        let Some(query) = input.get("query").and_then(|v| v.as_str()) else {
            return ToolOutput::err("query is required");
        };
        match self.index.find_symbols(query, 30) {
            Ok(symbols) if symbols.is_empty() => {
                ToolOutput::ok(format!("no definitions matching \"{query}\" in the index"))
            }
            Ok(symbols) => ToolOutput::ok(render_symbols(&symbols)),
            Err(e) => ToolOutput::err(format!("index unavailable: {e} — use grep instead")),
        }
    }
}

pub struct RefsTool {
    def: ToolDef,
    index: Arc<dyn CodeIndex>,
}

impl RefsTool {
    pub fn new(index: Arc<dyn CodeIndex>) -> Self {
        Self {
            def: ToolDef {
                name: "refs".into(),
                description: "Find every line referencing an EXACT symbol name across the workspace (\"who uses X\"). Returns file:line with the referencing line.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {"name": {"type": "string"}},
                    "required": ["name"]
                }),
            },
            index,
        }
    }
}

impl Tool for RefsTool {
    fn def(&self) -> &ToolDef {
        &self.def
    }
    fn read_only(&self) -> bool {
        true
    }
    fn execute(&self, input: &Value, _ctx: &ToolContext) -> ToolOutput {
        let Some(name) = input.get("name").and_then(|v| v.as_str()) else {
            return ToolOutput::err("name is required");
        };
        match self.index.find_references(name, 100) {
            Ok(refs) if refs.is_empty() => {
                ToolOutput::ok(format!("no references to \"{name}\" in the index"))
            }
            Ok(refs) => ToolOutput::ok(
                refs.iter()
                    .map(|r| format!("{}:{}  {}", r.file.display(), r.line, r.context))
                    .collect::<Vec<_>>()
                    .join("\n"),
            ),
            Err(e) => ToolOutput::err(format!("index unavailable: {e} — use grep instead")),
        }
    }
}

pub struct OutlineTool {
    def: ToolDef,
    index: Arc<dyn CodeIndex>,
}

impl OutlineTool {
    pub fn new(index: Arc<dyn CodeIndex>) -> Self {
        Self {
            def: ToolDef {
                name: "outline".into(),
                description: "List the definitions in one file (functions, types, methods with line numbers) without reading its body — cheap orientation before a targeted read.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {"path": {"type": "string", "description": "Workspace-relative path."}},
                    "required": ["path"]
                }),
            },
            index,
        }
    }
}

impl Tool for OutlineTool {
    fn def(&self) -> &ToolDef {
        &self.def
    }
    fn read_only(&self) -> bool {
        true
    }
    fn execute(&self, input: &Value, _ctx: &ToolContext) -> ToolOutput {
        let Some(path) = input.get("path").and_then(|v| v.as_str()) else {
            return ToolOutput::err("path is required");
        };
        match self.index.file_outline(std::path::Path::new(path)) {
            Ok(symbols) if symbols.is_empty() => ToolOutput::ok(format!(
                "no indexed definitions in {path} (unknown file type, or file not indexed)"
            )),
            Ok(symbols) => ToolOutput::ok(render_symbols(&symbols)),
            Err(e) => ToolOutput::err(format!("index unavailable: {e}")),
        }
    }
}
