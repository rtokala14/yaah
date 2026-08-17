# blurb desktop

A native desktop agentic coding harness. Rust + [GPUI](https://www.gpui.rs)
(Zed's UI framework) + [gpui-component](https://github.com/longbridge/gpui-component).

- **Providers**: Anthropic, OpenAI, and any OpenAI-compatible endpoint —
  each profile fully customizable: base URL, key/key-env, model, effort,
  **arbitrary extra headers and extra request-body fields**.
- **Native git**: status, diffstat, commit — and **worktree-isolated
  sessions**: each agent session runs on its own branch in its own checkout,
  in parallel, without touching yours.
- **The harness features from `harness/DESIGN.md`**: cache-disciplined
  prompting, prune-then-compact context management, parallel read-only
  tools, lossless reasoning round-trips, evidence-grounded completion.
- **Planned**: local code index (structure-first, not embeddings-first) —
  see `INDEX-DESIGN.md`.

## Layout

| Crate | Purpose | UI dep |
|---|---|---|
| `crates/harness-core` | providers, tools, agent loop, context mgmt, sessions | none |
| `crates/harness-git` | libgit2: status/diff/commit/worktrees | none |
| `crates/harness-index` | code index trait (phase 2) | none |
| `crates/blurb-app` | the GPUI application | gpui, gpui-component |

## Building (first build checklist)

Deliberately not built yet. When we do:

1. **Revisions are pinned in lockstep** in `desktop/Cargo.toml`: `gpui` and
   `gpui_platform` are pinned to zed rev `cc053a4a` — the rev
   gpui-component's own Cargo.lock pins (as of gpui-component v0.5.2).
   Before first build, verify gpui-component main still pins that rev (check
   its Cargo.lock) and bump all three together if not. Mismatched revs =
   type errors between the crates. Note: crates.io hosts an older pairing
   (gpui 0.2.2 + gpui-component 0.5.1) with a different bootstrap
   (`Application::new()`); this code targets the git pairing
   (`gpui_platform::application()`), do not mix them.
2. Platform prerequisites are Zed's: recent stable Rust; on Linux, Wayland
   or X11 dev libraries, Vulkan drivers; on macOS, Xcode CLT.
3. `cargo check -p harness-core -p harness-git` first — these have no UI
   deps and validate the core quickly. Their unit tests run headless:
   `cargo test -p harness-core -p harness-git`.
4. `cargo check -p blurb-app`, then chase GPUI API drift (the framework
   moves fast; expect renames, not redesigns).
5. `cargo run -p blurb-app`.

## Configuration

`~/.config/blurb/settings.toml` (created on first run) — provider profiles:

```toml
[[providers]]
name = "Anthropic"
kind = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"
model = "claude-opus-5"
effort = "xhigh"
extra_headers = [["anthropic-beta", "context-management-2025-06-27"]]

[[providers]]
name = "Local vLLM"
kind = "openai_compat"
base_url = "http://localhost:8000/v1"
model = "qwen3-coder"
[providers.extra_body]
top_k = 20
```
