use crate::types::{Tool, ToolContext, ToolDef, ToolOutput};
use serde_json::{json, Value};

/// Spawns a read-only explorer subagent with a fresh context. Marked
/// read-only so several calls in one turn fan out in parallel threads —
/// the mechanism behind dynamic multi-agent workflows.
pub struct SubagentTool {
    def: ToolDef,
}

impl SubagentTool {
    pub fn new() -> Self {
        Self {
            def: ToolDef {
                name: "subagent".into(),
                description: "Spawn a read-only explorer subagent with its own fresh context to investigate something in the workspace and report back. It can read/grep/glob/recall but cannot modify anything, and its intermediate steps never touch your context — only its final report returns. Use for broad or independent investigations, and issue SEVERAL subagent calls in one turn to run them in parallel. Give each one a specific question and say what the report must contain.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "task": {"type": "string", "description": "The investigation to run and what to report back."}
                    },
                    "required": ["task"]
                }),
            },
        }
    }
}

impl Tool for SubagentTool {
    fn def(&self) -> &ToolDef {
        &self.def
    }
    fn read_only(&self) -> bool {
        true
    }

    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolOutput {
        let Some(task) = input.get("task").and_then(|v| v.as_str()) else {
            return ToolOutput::err("task is required");
        };
        let runner = ctx.subagent.lock().unwrap().clone();
        let Some(runner) = runner else {
            return ToolOutput::err(
                "subagents are not available in this context (no nested spawning)",
            );
        };
        match runner.run(task, &ctx.cancel) {
            Ok(report) if report.trim().is_empty() => {
                ToolOutput::err("subagent finished without a report")
            }
            Ok(report) => ToolOutput::ok(crate::tools::truncate_output(
                &report,
                crate::tools::MAX_OUTPUT_CHARS,
            )),
            Err(e) => ToolOutput::err(format!("subagent failed: {e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CancelToken, SubagentRunner};
    use std::sync::Arc;

    struct FakeRunner;
    impl SubagentRunner for FakeRunner {
        fn run(&self, task: &str, _cancel: &CancelToken) -> Result<String, String> {
            Ok(format!("report for: {task}"))
        }
    }

    #[test]
    fn runs_via_installed_runner_and_fails_closed_without_one() {
        let tool = SubagentTool::new();
        let ctx = ToolContext::new(std::path::PathBuf::from("."), CancelToken::new());

        // Nested contexts have no runner: recursion fails closed.
        let out = tool.execute(&json!({"task": "find the auth code"}), &ctx);
        assert!(out.is_error);

        *ctx.subagent.lock().unwrap() = Some(Arc::new(FakeRunner));
        let out = tool.execute(&json!({"task": "find the auth code"}), &ctx);
        assert!(!out.is_error);
        assert_eq!(out.content, "report for: find the auth code");
    }
}
