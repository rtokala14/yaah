use crate::types::{
    InteractionError, InteractionReply, InteractionRequest, Tool, ToolContext, ToolDef, ToolOutput,
};
use serde_json::{json, Value};
use std::sync::atomic::Ordering;

pub const APPROVE_OPTION: &str = "Approve plan — start implementing";
pub const KEEP_PLANNING_OPTION: &str = "Keep planning";

/// Presents a plan for approval while in plan mode. Approval flips
/// `plan_mode` off, unlocking the mutating tools for the same run.
pub struct PresentPlanTool {
    def: ToolDef,
}

impl PresentPlanTool {
    pub fn new() -> Self {
        Self {
            def: ToolDef {
                name: "present_plan".into(),
                description: "Present your implementation plan to the user for approval (plan mode). Write the complete plan: what will change, file by file, in order, with verification steps. If approved, plan mode ends and you may implement immediately; otherwise refine the plan with the user's feedback.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "plan": {"type": "string", "description": "The full plan, markdown-friendly."}
                    },
                    "required": ["plan"]
                }),
            },
        }
    }
}

impl Tool for PresentPlanTool {
    fn def(&self) -> &ToolDef {
        &self.def
    }
    // Blocks on the human; must run alone, not in a parallel batch.
    fn read_only(&self) -> bool {
        false
    }

    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolOutput {
        let Some(plan) = input.get("plan").and_then(|v| v.as_str()) else {
            return ToolOutput::err("plan is required");
        };
        if !ctx.plan_mode.load(Ordering::Relaxed) {
            return ToolOutput::err(
                "plan mode is not active — implement directly instead of presenting a plan",
            );
        }
        match ctx.interaction.ask(InteractionRequest::Question {
            question: format!("Proposed plan:\n\n{plan}"),
            options: vec![APPROVE_OPTION.to_string(), KEEP_PLANNING_OPTION.to_string()],
        }) {
            Ok(InteractionReply::Answer(answer)) if answer == APPROVE_OPTION => {
                ctx.plan_mode.store(false, Ordering::Relaxed);
                ToolOutput::ok(
                    "Plan approved. Plan mode is off — implement the plan now, step by step.",
                )
            }
            Ok(InteractionReply::Answer(answer)) => ToolOutput::ok(format!(
                "Not approved yet. The user said: {answer}\nRevise the plan accordingly and present it again."
            )),
            Ok(_) => ToolOutput::err("host returned a mismatched reply"),
            Err(InteractionError::Cancelled) => ToolOutput::err("interrupted before a decision"),
            Err(InteractionError::Unavailable) => ToolOutput::err(
                "no interactive user is attached to approve the plan; stop here and report the plan as your final message",
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{CancelToken, InteractionHandler};
    use std::sync::Arc;

    struct Approver;
    impl InteractionHandler for Approver {
        fn ask(&self, req: InteractionRequest) -> Result<InteractionReply, InteractionError> {
            match req {
                InteractionRequest::Question { question, options } => {
                    assert!(question.contains("step one"));
                    assert_eq!(options.len(), 2);
                    Ok(InteractionReply::Answer(APPROVE_OPTION.to_string()))
                }
                _ => panic!("unexpected request"),
            }
        }
    }

    struct Rejecter;
    impl InteractionHandler for Rejecter {
        fn ask(&self, _req: InteractionRequest) -> Result<InteractionReply, InteractionError> {
            Ok(InteractionReply::Answer("use sqlite instead".to_string()))
        }
    }

    #[test]
    fn approval_exits_plan_mode_rejection_keeps_it() {
        let tool = PresentPlanTool::new();

        let ctx = ToolContext::with_interaction(
            std::path::PathBuf::from("."),
            CancelToken::new(),
            Arc::new(Approver),
        );
        ctx.plan_mode.store(true, Ordering::Relaxed);
        let out = tool.execute(&json!({"plan": "step one: do it"}), &ctx);
        assert!(!out.is_error);
        assert!(!ctx.plan_mode.load(Ordering::Relaxed), "approval must exit plan mode");

        let ctx = ToolContext::with_interaction(
            std::path::PathBuf::from("."),
            CancelToken::new(),
            Arc::new(Rejecter),
        );
        ctx.plan_mode.store(true, Ordering::Relaxed);
        let out = tool.execute(&json!({"plan": "step one: do it"}), &ctx);
        assert!(!out.is_error);
        assert!(out.content.contains("use sqlite instead"));
        assert!(ctx.plan_mode.load(Ordering::Relaxed), "rejection keeps plan mode on");
    }

    #[test]
    fn refuses_outside_plan_mode() {
        let tool = PresentPlanTool::new();
        let ctx = ToolContext::new(std::path::PathBuf::from("."), CancelToken::new());
        assert!(tool.execute(&json!({"plan": "p"}), &ctx).is_error);
    }
}
