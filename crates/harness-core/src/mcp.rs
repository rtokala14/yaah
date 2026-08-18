//! MCP client (stdio transport): connect user-configured Model Context
//! Protocol servers and expose their tools in the agent's registry.
//!
//! Transport: newline-delimited JSON-RPC 2.0 over the child's stdin/stdout.
//! A reader thread forwards every incoming message to a channel; requests
//! are serial (one session thread), so correlation is a simple id match.
//! MCP tools are named `mcp_<server>_<tool>` and are gated by the
//! permission model like other side-effecting tools.

use crate::types::{Tool, ToolContext, ToolDef, ToolOutput};
use crossbeam_channel::{unbounded, Receiver, Sender};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// One configured server: a display name and the command line to spawn.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct McpServerConfig {
    pub name: String,
    /// Full command line, split on whitespace ("npx -y my-server --flag").
    pub command: String,
}

const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CALL_TIMEOUT: Duration = Duration::from_secs(120);

pub struct McpClient {
    pub name: String,
    child: Mutex<Child>,
    stdin: Mutex<ChildStdin>,
    incoming: Receiver<Value>,
    next_id: AtomicU64,
}

impl McpClient {
    /// Spawn the server, run the initialize handshake, and list its tools.
    pub fn start(config: &McpServerConfig, cwd: &std::path::Path) -> Result<(Arc<Self>, Vec<ToolDef>), String> {
        let mut parts = config.command.split_whitespace();
        let program = parts.next().ok_or("empty command")?;
        let mut child = Command::new(program)
            .args(parts)
            .current_dir(cwd)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .map_err(|e| format!("spawn {program}: {e}"))?;
        let stdin = child.stdin.take().ok_or("no stdin")?;
        let stdout = child.stdout.take().ok_or("no stdout")?;

        let (tx, rx): (Sender<Value>, Receiver<Value>) = unbounded();
        std::thread::Builder::new()
            .name(format!("mcp-{}", config.name))
            .spawn(move || {
                let reader = BufReader::new(stdout);
                for line in reader.lines() {
                    let Ok(line) = line else { break };
                    if let Ok(v) = serde_json::from_str::<Value>(&line) {
                        if tx.send(v).is_err() {
                            break;
                        }
                    }
                }
            })
            .map_err(|e| e.to_string())?;

        let client = Arc::new(Self {
            name: config.name.clone(),
            child: Mutex::new(child),
            stdin: Mutex::new(stdin),
            incoming: rx,
            next_id: AtomicU64::new(0),
        });

        client.request(
            "initialize",
            json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "blurb", "version": env!("CARGO_PKG_VERSION")}
            }),
            REQUEST_TIMEOUT,
        )?;
        client.notify("notifications/initialized")?;
        let tools_result =
            client.request("tools/list", json!({}), REQUEST_TIMEOUT)?;
        let defs = parse_tool_defs(&config.name, &tools_result);
        Ok((client, defs))
    }

    fn send(&self, message: &Value) -> Result<(), String> {
        let mut stdin = self.stdin.lock().unwrap();
        let line = serde_json::to_string(message).map_err(|e| e.to_string())?;
        stdin
            .write_all(line.as_bytes())
            .and_then(|_| stdin.write_all(b"\n"))
            .and_then(|_| stdin.flush())
            .map_err(|e| format!("mcp {} write: {e}", self.name))
    }

    fn notify(&self, method: &str) -> Result<(), String> {
        self.send(&json!({"jsonrpc": "2.0", "method": method}))
    }

    /// Serial request/response with timeout; notifications and stale
    /// messages are skipped.
    fn request(&self, method: &str, params: Value, timeout: Duration) -> Result<Value, String> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed) + 1;
        self.send(&json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params}))?;
        let deadline = std::time::Instant::now() + timeout;
        loop {
            let remaining = deadline
                .checked_duration_since(std::time::Instant::now())
                .ok_or_else(|| format!("mcp {}: {method} timed out", self.name))?;
            let msg = self
                .incoming
                .recv_timeout(remaining)
                .map_err(|_| format!("mcp {}: {method} — no response", self.name))?;
            if msg.get("id").and_then(|v| v.as_u64()) != Some(id) {
                continue; // notification or stale
            }
            if let Some(err) = msg.get("error") {
                return Err(format!(
                    "mcp {}: {}",
                    self.name,
                    err.get("message").and_then(|m| m.as_str()).unwrap_or("server error")
                ));
            }
            return Ok(msg.get("result").cloned().unwrap_or(Value::Null));
        }
    }

    pub fn call_tool(&self, remote_name: &str, arguments: &Value) -> Result<(String, bool), String> {
        let result = self.request(
            "tools/call",
            json!({"name": remote_name, "arguments": arguments}),
            CALL_TIMEOUT,
        )?;
        Ok(render_call_result(&result))
    }
}

impl Drop for McpClient {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// tools/list result → sanitized ToolDefs named `mcp_<server>_<tool>`.
pub fn parse_tool_defs(server: &str, result: &Value) -> Vec<ToolDef> {
    let server_slug = slug(server);
    result
        .get("tools")
        .and_then(|t| t.as_array())
        .map(|tools| {
            tools
                .iter()
                .filter_map(|t| {
                    let name = t.get("name")?.as_str()?;
                    Some(ToolDef {
                        name: format!("mcp_{server_slug}_{}", slug(name)),
                        description: format!(
                            "[MCP: {server}] {}",
                            t.get("description").and_then(|d| d.as_str()).unwrap_or("")
                        ),
                        input_schema: t
                            .get("inputSchema")
                            .cloned()
                            .unwrap_or_else(|| json!({"type": "object"})),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// tools/call result → (text, is_error). Text content items concatenate;
/// non-text items are noted by type.
pub fn render_call_result(result: &Value) -> (String, bool) {
    let is_error = result.get("isError").and_then(|v| v.as_bool()).unwrap_or(false);
    let text = result
        .get("content")
        .and_then(|c| c.as_array())
        .map(|items| {
            items
                .iter()
                .map(|item| match item.get("type").and_then(|t| t.as_str()) {
                    Some("text") => {
                        item.get("text").and_then(|t| t.as_str()).unwrap_or("").to_string()
                    }
                    Some(other) => format!("[{other} content omitted]"),
                    None => String::new(),
                })
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default();
    (text, is_error)
}

fn slug(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c.to_ascii_lowercase() } else { '_' })
        .collect()
}

/// A remote MCP tool exposed through the standard Tool trait.
pub struct McpTool {
    def: ToolDef,
    remote_name: String,
    client: Arc<McpClient>,
}

impl McpTool {
    pub fn new(def: ToolDef, remote_name: String, client: Arc<McpClient>) -> Self {
        Self { def, remote_name, client }
    }
}

impl Tool for McpTool {
    fn def(&self) -> &ToolDef {
        &self.def
    }
    // Unknown side effects: run sequentially and go through the permission
    // gate (the agent gates every `mcp_*` tool).
    fn read_only(&self) -> bool {
        false
    }

    fn execute(&self, input: &Value, _ctx: &ToolContext) -> ToolOutput {
        match self.client.call_tool(&self.remote_name, input) {
            Ok((text, false)) => {
                ToolOutput::ok(crate::tools::truncate_output(&text, crate::tools::MAX_OUTPUT_CHARS))
            }
            Ok((text, true)) => ToolOutput::err(crate::tools::truncate_output(
                &text,
                crate::tools::MAX_OUTPUT_CHARS,
            )),
            Err(e) => ToolOutput::err(e),
        }
    }
}

/// Connect every configured server; returns the clients (keep them alive —
/// dropping kills the child) plus their wrapped tools, and per-server
/// errors for surfacing.
pub fn connect_all(
    configs: &[McpServerConfig],
    cwd: &std::path::Path,
) -> (Vec<Arc<McpClient>>, Vec<Arc<dyn Tool>>, Vec<String>) {
    let mut clients = Vec::new();
    let mut tools: Vec<Arc<dyn Tool>> = Vec::new();
    let mut errors = Vec::new();
    for config in configs {
        match McpClient::start(config, cwd) {
            Ok((client, defs)) => {
                for def in defs {
                    // Remote name = last segment after our prefix is not
                    // reliable; recover it from the def we built instead.
                    let remote = def
                        .name
                        .strip_prefix(&format!("mcp_{}_", slug(&config.name)))
                        .unwrap_or(&def.name)
                        .to_string();
                    tools.push(Arc::new(McpTool::new(def, remote, Arc::clone(&client))));
                }
                clients.push(client);
            }
            Err(e) => errors.push(format!("{}: {e}", config.name)),
        }
    }
    (clients, tools, errors)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tool_defs_and_call_results() {
        let listing = json!({"tools": [
            {"name": "search-docs", "description": "Searches docs", "inputSchema": {"type": "object", "properties": {"q": {"type": "string"}}}},
            {"name": "no_schema"}
        ]});
        let defs = parse_tool_defs("My Server", &listing);
        assert_eq!(defs.len(), 2);
        assert_eq!(defs[0].name, "mcp_my_server_search_docs");
        assert!(defs[0].description.contains("[MCP: My Server]"));
        assert_eq!(defs[1].input_schema, json!({"type": "object"}));

        let ok = json!({"content": [{"type": "text", "text": "hello"}, {"type": "image", "data": "…"}]});
        assert_eq!(render_call_result(&ok), ("hello\n[image content omitted]".into(), false));
        let err = json!({"isError": true, "content": [{"type": "text", "text": "boom"}]});
        assert_eq!(render_call_result(&err), ("boom".into(), true));
    }

    #[cfg(unix)]
    #[test]
    fn full_handshake_against_scripted_server() {
        // A shell script speaking just enough MCP: initialize (id 1),
        // tools/list (id 2), tools/call (id 3).
        let script = r#"
while read -r line; do
  case "$line" in
    *'"initialize"'*) echo '{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18","capabilities":{},"serverInfo":{"name":"fake"}}}' ;;
    *'"tools/list"'*) echo '{"jsonrpc":"2.0","id":2,"result":{"tools":[{"name":"echo","description":"Echoes input","inputSchema":{"type":"object"}}]}}' ;;
    *'"tools/call"'*) echo '{"jsonrpc":"2.0","id":3,"result":{"content":[{"type":"text","text":"echoed!"}]}}' ;;
  esac
done
"#;
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("fake-mcp.sh");
        std::fs::write(&path, script).unwrap();

        let config = McpServerConfig {
            name: "fake".into(),
            command: format!("sh {}", path.display()),
        };
        let (clients, tools, errors) = connect_all(&[config], tmp.path());
        assert!(errors.is_empty(), "errors: {errors:?}");
        assert_eq!(clients.len(), 1);
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].def().name, "mcp_fake_echo");

        let ctx = ToolContext::new(tmp.path().to_path_buf(), crate::types::CancelToken::new());
        let out = tools[0].execute(&json!({"q": "hi"}), &ctx);
        assert!(!out.is_error);
        assert_eq!(out.content, "echoed!");
    }

    #[test]
    fn connect_failures_are_reported_not_fatal() {
        let config = McpServerConfig {
            name: "ghost".into(),
            command: "/definitely/not/a/real/binary-xyz".into(),
        };
        let (clients, tools, errors) = connect_all(&[config], std::path::Path::new("."));
        assert!(clients.is_empty() && tools.is_empty());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].starts_with("ghost:"));
    }
}
