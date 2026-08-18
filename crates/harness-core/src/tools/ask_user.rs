use crate::types::{
    InteractionError, InteractionReply, InteractionRequest, Tool, ToolContext, ToolDef, ToolOutput,
};
use serde_json::{json, Value};

/// Asks the human a question and blocks until they answer. The transport is
/// the session's `InteractionHandler`; headless hosts return an error the
/// model can act on ("proceed with your best judgement").
pub struct AskUserTool {
    def: ToolDef,
}

impl AskUserTool {
    pub fn new() -> Self {
        Self {
            def: ToolDef {
                name: "ask_user".into(),
                description: "Ask the user a question and wait for their answer. Use only when genuinely blocked on a decision the user must make: ambiguous requirements, mutually exclusive approaches with real trade-offs, or confirmation of something destructive. Offer options when discrete choices exist. Do NOT ask when a sensible default exists — pick it and note the assumption.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "question": {"type": "string"},
                        "options": {
                            "type": "array",
                            "items": {"type": "string"},
                            "description": "Optional discrete choices shown as buttons."
                        }
                    },
                    "required": ["question"]
                }),
            },
        }
    }
}

impl Tool for AskUserTool {
    fn def(&self) -> &ToolDef {
        &self.def
    }
    // Not read-only: must never run inside a parallel batch (it blocks on
    // the human) and acts as a barrier like other sequential tools.
    fn read_only(&self) -> bool {
        false
    }

    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolOutput {
        let Some(question) = input.get("question").and_then(|v| v.as_str()) else {
            return ToolOutput::err("question is required");
        };
        let options: Vec<String> = input
            .get("options")
            .and_then(|v| v.as_array())
            .map(|a| a.iter().filter_map(|v| v.as_str().map(String::from)).collect())
            .unwrap_or_default();

        match ctx.interaction.ask(InteractionRequest::Question {
            question: question.to_string(),
            options,
        }) {
            Ok(InteractionReply::Answer(text)) => {
                ToolOutput::ok(format!("The user answered: {text}"))
            }
            Ok(_) => ToolOutput::err("host returned a mismatched reply"),
            Err(InteractionError::Cancelled) => ToolOutput::err("interrupted before an answer"),
            Err(InteractionError::Unavailable) => ToolOutput::err(
                "no interactive user is attached — proceed with your best judgement and state the assumption",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CancelToken, InteractionHandler};
    use std::sync::Arc;

    struct CannedAnswer(&'static str);
    impl InteractionHandler for CannedAnswer {
        fn ask(&self, req: InteractionRequest) -> Result<InteractionReply, InteractionError> {
            match req {
                InteractionRequest::Question { question, options } => {
                    assert_eq!(question, "Which db?");
                    assert_eq!(options, vec!["sqlite".to_string(), "postgres".to_string()]);
                    Ok(InteractionReply::Answer(self.0.to_string()))
                }
                _ => panic!("unexpected request kind"),
            }
        }
    }

    #[test]
    fn relays_question_and_returns_answer() {
        let tool = AskUserTool::new();
        let ctx = ToolContext::with_interaction(
            std::path::PathBuf::from("."),
            CancelToken::new(),
            Arc::new(CannedAnswer("sqlite")),
        );
        let out = tool.execute(
            &json!({"question": "Which db?", "options": ["sqlite", "postgres"]}),
            &ctx,
        );
        assert!(!out.is_error);
        assert!(out.content.contains("sqlite"));
    }

    #[test]
    fn headless_host_yields_actionable_error() {
        let tool = AskUserTool::new();
        let ctx = ToolContext::new(std::path::PathBuf::from("."), CancelToken::new());
        let out = tool.execute(&json!({"question": "Which db?"}), &ctx);
        assert!(out.is_error);
        assert!(out.content.contains("best judgement"));
    }
}
