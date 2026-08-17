use super::{truncate_output, MAX_OUTPUT_CHARS};
use crate::types::{Tool, ToolContext, ToolDef, ToolOutput};
use serde_json::{json, Value};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

pub struct BashTool {
    def: ToolDef,
}

impl BashTool {
    pub fn new() -> Self {
        Self {
            def: ToolDef {
                name: "bash".into(),
                description: "Run a shell command in the workspace root. Use for builds, tests, git, package managers, and anything without a dedicated tool. Prefer read/grep/glob for file inspection (they are faster and parallel-safe). Output is capped; pipe through head/tail for large output.".into(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "command": {"type": "string", "description": "The shell command to run"},
                        "timeout_ms": {"type": "number", "description": "Max runtime in ms (default 120000, max 600000)"}
                    },
                    "required": ["command"]
                }),
            },
        }
    }
}

impl Tool for BashTool {
    fn def(&self) -> &ToolDef {
        &self.def
    }
    fn read_only(&self) -> bool {
        false
    }

    fn execute(&self, input: &Value, ctx: &ToolContext) -> ToolOutput {
        let Some(command) = input.get("command").and_then(|v| v.as_str()) else {
            return ToolOutput::err("no command provided");
        };
        let timeout_ms = input
            .get("timeout_ms")
            .and_then(|v| v.as_f64())
            .unwrap_or(120_000.0)
            .min(600_000.0) as u64;

        let shell = if cfg!(windows) { "powershell" } else { "bash" };
        let flag = if cfg!(windows) { "-Command" } else { "-c" };
        let child = Command::new(shell)
            .arg(flag)
            .arg(command)
            .current_dir(&ctx.cwd)
            .env("GIT_TERMINAL_PROMPT", "0")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn();

        let mut child = match child {
            Ok(c) => c,
            Err(e) => return ToolOutput::err(format!("spawn failed: {e}")),
        };

        // Drain stdout/stderr on threads so the pipe never backpressures.
        let mut stdout = child.stdout.take().unwrap();
        let mut stderr = child.stderr.take().unwrap();
        let cap = MAX_OUTPUT_CHARS * 2;
        let out_handle = std::thread::spawn(move || read_capped(&mut stdout, cap));
        let err_handle = std::thread::spawn(move || read_capped(&mut stderr, cap));

        let started = Instant::now();
        let deadline = Duration::from_millis(timeout_ms);
        let status = loop {
            if ctx.cancel.is_cancelled() {
                let _ = child.kill();
                let _ = child.wait();
                return ToolOutput::err("cancelled");
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {
                    if started.elapsed() > deadline {
                        let _ = child.kill();
                        let _ = child.wait();
                        let out = out_handle.join().unwrap_or_default();
                        let err = err_handle.join().unwrap_or_default();
                        return ToolOutput::err(format!(
                            "command timed out after {timeout_ms}ms\n{}",
                            truncate_output(&(out + &err), MAX_OUTPUT_CHARS)
                        ));
                    }
                    std::thread::sleep(Duration::from_millis(25));
                }
                Err(e) => return ToolOutput::err(format!("wait failed: {e}")),
            }
        };

        let out = out_handle.join().unwrap_or_default();
        let err = err_handle.join().unwrap_or_default();
        let combined = format!("{out}{err}");
        let body = truncate_output(combined.trim(), MAX_OUTPUT_CHARS);
        let body = if body.is_empty() { "(no output)".to_string() } else { body };
        if status.success() {
            ToolOutput::ok(body)
        } else {
            ToolOutput::err(format!("exit code {}\n{body}", status.code().unwrap_or(-1)))
        }
    }
}

fn read_capped(reader: &mut dyn Read, cap: usize) -> String {
    let mut buf = Vec::new();
    let mut chunk = [0u8; 8192];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => {
                if buf.len() < cap {
                    buf.extend_from_slice(&chunk[..n.min(cap - buf.len())]);
                }
                // keep draining past the cap so the child never blocks
            }
            Err(_) => break,
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}
