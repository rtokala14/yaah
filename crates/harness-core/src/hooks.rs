//! Tool-call hooks: user-configured shell commands that run before or after
//! matching tool calls.
//!
//! - **pre**: runs before the tool; a non-zero exit BLOCKS the call and the
//!   hook's output becomes the (error) tool result the model sees.
//! - **post**: runs after the tool; a non-zero exit appends the hook's
//!   output to the tool result as feedback (the lint-on-edit pattern —
//!   the edit stands, the model sees the lint errors immediately).
//!
//! Hooks come from two places, merged: the app's settings (global,
//! in-app editable) and the project's `.blurb/hooks.json` (checked into
//! the repo, shared with the team). The hook command receives context in
//! env vars: BLURB_HOOK, BLURB_TOOL, BLURB_INPUT (JSON), BLURB_OUTPUT
//! (post only), and runs in the session's working directory.

use serde::{Deserialize, Serialize};
use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    Pre,
    Post,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct HookConfig {
    pub name: String,
    pub event: HookEvent,
    /// Comma-separated tool names, or "*" for every tool
    /// (e.g. "edit,write" for the lint-on-edit pattern).
    pub tools: String,
    /// Shell command (run via `sh -c`).
    pub command: String,
}

impl HookConfig {
    pub fn matches(&self, event: HookEvent, tool: &str) -> bool {
        self.event == event
            && self
                .tools
                .split(',')
                .map(str::trim)
                .any(|t| t == "*" || t == tool)
    }
}

#[derive(Debug)]
pub struct HookOutcome {
    pub success: bool,
    /// Combined stdout + stderr, truncated.
    pub output: String,
}

const HOOK_TIMEOUT: Duration = Duration::from_secs(120);
const MAX_HOOK_OUTPUT: usize = 8_000;
const MAX_ENV_JSON: usize = 16_000;

/// Run one hook to completion (bounded by a timeout; a timed-out hook is
/// killed and reported as failed).
pub fn run_hook(
    hook: &HookConfig,
    tool: &str,
    input: &serde_json::Value,
    output: Option<&str>,
    cwd: &Path,
) -> HookOutcome {
    let input_json: String =
        serde_json::to_string(input).unwrap_or_default().chars().take(MAX_ENV_JSON).collect();
    let mut cmd = Command::new("sh");
    cmd.arg("-c")
        .arg(&hook.command)
        .current_dir(cwd)
        .env("BLURB_HOOK", &hook.name)
        .env("BLURB_TOOL", tool)
        .env("BLURB_INPUT", input_json)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(out) = output {
        let out: String = out.chars().take(MAX_ENV_JSON).collect();
        cmd.env("BLURB_OUTPUT", out);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => {
            return HookOutcome { success: false, output: format!("hook failed to start: {e}") }
        }
    };

    let deadline = Instant::now() + HOOK_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            Err(_) => break None,
        }
    };

    let mut text = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        let _ = stdout.read_to_string(&mut text);
    }
    if let Some(mut stderr) = child.stderr.take() {
        let mut err = String::new();
        let _ = stderr.read_to_string(&mut err);
        if !err.trim().is_empty() {
            if !text.trim().is_empty() {
                text.push('\n');
            }
            text.push_str(&err);
        }
    }
    let output: String = text.trim().chars().take(MAX_HOOK_OUTPUT).collect();

    match status {
        Some(s) => HookOutcome { success: s.success(), output },
        None => HookOutcome {
            success: false,
            output: format!("hook timed out after {}s and was killed", HOOK_TIMEOUT.as_secs()),
        },
    }
}

/// Project-level hooks: `<workspace>/.blurb/hooks.json` — a JSON array of
/// HookConfig. Missing or invalid files yield no hooks.
pub fn load_project_hooks(workspace: &Path) -> Vec<HookConfig> {
    std::fs::read_to_string(workspace.join(".blurb").join("hooks.json"))
        .ok()
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn hook(event: HookEvent, tools: &str, command: &str) -> HookConfig {
        HookConfig { name: "t".into(), event, tools: tools.into(), command: command.into() }
    }

    #[test]
    fn matching_by_event_and_tool_list() {
        let h = hook(HookEvent::Post, "edit, write", "true");
        assert!(h.matches(HookEvent::Post, "edit"));
        assert!(h.matches(HookEvent::Post, "write"));
        assert!(!h.matches(HookEvent::Post, "bash"));
        assert!(!h.matches(HookEvent::Pre, "edit"));
        assert!(hook(HookEvent::Pre, "*", "true").matches(HookEvent::Pre, "anything"));
    }

    #[cfg(unix)]
    #[test]
    fn runs_with_env_and_reports_exit() {
        let tmp = tempfile::tempdir().unwrap();
        let ok = run_hook(
            &hook(HookEvent::Pre, "*", "echo \"tool=$BLURB_TOOL\"; test -n \"$BLURB_INPUT\""),
            "edit",
            &json!({"path": "a.rs"}),
            None,
            tmp.path(),
        );
        assert!(ok.success);
        assert_eq!(ok.output, "tool=edit");

        let fail = run_hook(
            &hook(HookEvent::Post, "*", "echo lint broke >&2; exit 1"),
            "edit",
            &json!({}),
            Some("tool output"),
            tmp.path(),
        );
        assert!(!fail.success);
        assert!(fail.output.contains("lint broke"));

        // BLURB_OUTPUT reaches post hooks.
        let sees_output = run_hook(
            &hook(HookEvent::Post, "*", "test \"$BLURB_OUTPUT\" = \"tool output\""),
            "edit",
            &json!({}),
            Some("tool output"),
            tmp.path(),
        );
        assert!(sees_output.success);
    }

    #[test]
    fn project_hooks_load_and_tolerate_garbage() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(load_project_hooks(tmp.path()).is_empty());
        let dir = tmp.path().join(".blurb");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("hooks.json"),
            r#"[{"name": "fmt", "event": "post", "tools": "edit,write", "command": "cargo fmt --check"}]"#,
        )
        .unwrap();
        let hooks = load_project_hooks(tmp.path());
        assert_eq!(hooks.len(), 1);
        assert_eq!(hooks[0].event, HookEvent::Post);

        std::fs::write(dir.join("hooks.json"), "{not json").unwrap();
        assert!(load_project_hooks(tmp.path()).is_empty());
    }
}
