use crate::types::{TodoItem, Tool, ToolContext, ToolDef, ToolOutput};
use serde_json::{json, Value};

/// Replaces the session's todo list. Replace-whole-list semantics keep the
/// contract trivial for the model: send the full current plan every time.
pub struct TodoWriteTool {
    def: ToolDef,
}

impl TodoWriteTool {
    pub fn new() -> Self {
        Self {
            def: ToolDef {
                name: "todo_write".into(),
                description: "Replace the session's todo list (shown live to the user). Send the FULL list every time. Use for multi-step work: plan the steps up front, keep exactly one item in_progress, mark items done as you finish them. Statuses: pending, in_progress, done.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "todos": {
                            "type": "array",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "text": {"type": "string"},
                                    "status": {"type": "string", "enum": ["pending", "in_progress", "done"]}
                                },
                                "required": ["text", "status"]
                            }
                        }
                    },
                    "required": ["todos"]
                }),
            },
        }
    }
}

const MAX_TODOS: usize = 50;

impl Tool for TodoWriteTool {
    fn def(&self) -> &ToolDef {
        &self.def
    }
    fn read_only(&self) -> bool {
        true
    }

    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolOutput {
        let Some(raw) = input.get("todos") else {
            return ToolOutput::err("todos is required");
        };
        let todos: Vec<TodoItem> = match serde_json::from_value(raw.clone()) {
            Ok(t) => t,
            Err(e) => return ToolOutput::err(format!("invalid todos: {e}")),
        };
        if todos.len() > MAX_TODOS {
            return ToolOutput::err(format!(
                "{} items is too many (max {MAX_TODOS}) — keep the list at task granularity",
                todos.len()
            ));
        }
        let (done, total) = (
            todos.iter().filter(|t| t.status == crate::types::TodoStatus::Done).count(),
            todos.len(),
        );
        *ctx.todos.lock().unwrap() = todos;
        ToolOutput::ok(format!("todo list updated ({done}/{total} done)"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CancelToken, TodoStatus};

    #[test]
    fn replaces_list_and_validates() {
        let tool = TodoWriteTool::new();
        let ctx = ToolContext::new(std::path::PathBuf::from("."), CancelToken::new());

        let out = tool.execute(
            &json!({"todos": [
                {"text": "read the code", "status": "done"},
                {"text": "write the fix", "status": "in_progress"},
                {"text": "run tests", "status": "pending"}
            ]}),
            &ctx,
        );
        assert!(!out.is_error);
        assert!(out.content.contains("1/3 done"));
        {
            let todos = ctx.todos.lock().unwrap();
            assert_eq!(todos.len(), 3);
            assert_eq!(todos[1].status, TodoStatus::InProgress);
        }

        // Replace semantics: a new call overwrites.
        tool.execute(&json!({"todos": [{"text": "only one", "status": "pending"}]}), &ctx);
        assert_eq!(ctx.todos.lock().unwrap().len(), 1);

        assert!(tool.execute(&json!({}), &ctx).is_error);
        assert!(tool
            .execute(&json!({"todos": [{"text": "bad status", "status": "someday"}]}), &ctx)
            .is_error);
    }
}
