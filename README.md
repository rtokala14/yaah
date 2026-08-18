# blurb desktop

A native desktop agentic coding harness. Rust + [GPUI](https://www.gpui.rs)
(Zed's UI framework) + [gpui-component](https://github.com/longbridge/gpui-component).

- **Providers own models**: a provider is a connection (Anthropic, OpenAI,
  or any OpenAI-compatible endpoint — base URL, key/key-env, arbitrary
  extra headers and extra request-body fields); each provider carries any
  number of models (id, label, effort, max tokens, temperature, per-model
  body overrides). **Everything is managed in-app** — add/edit/duplicate/
  delete providers and models from the settings overlay; the TOML file is
  just persistence.
- **Native git**: status, diffstat, commit, a lane-colored commit history
  graph, ahead/behind upstream — and **worktree-isolated sessions**: each
  agent session runs on its own branch in its own checkout, in parallel,
  without touching yours. Session branches merge back in-app
  (fast-forward or merge commit; conflicts abort cleanly and are reported).
- **The harness features from `harness/DESIGN.md`**: cache-disciplined
  prompting, prune-then-compact context management, parallel read-only
  tools, lossless reasoning round-trips, evidence-grounded completion.
- **Planned**: local code index (structure-first, not embeddings-first) —
  see `INDEX-DESIGN.md`; full roadmap in `ROADMAP.md`.

## Layout

| Crate | Purpose | UI dep |
|---|---|---|
| `crates/harness-core` | providers, tools, agent loop, context mgmt, sessions | none |
| `crates/harness-git` | libgit2: status/diff/commit/worktrees/log-graph/merge | none |
| `crates/harness-index` | code index trait (phase 2) | none |
| `crates/blurb-app` | the GPUI application | gpui, gpui-component |

## Building

Verified building with the committed `Cargo.lock` + `rust-toolchain.toml`
(Rust 1.97.1 — gpui uses recently-stabilized std APIs; older toolchains
fail with E0658).

```sh
cargo check -p harness-core -p harness-git   # fast, no UI deps
cargo test  -p harness-core -p harness-git -p blurb-app
cargo run   -p blurb-app                     # first gpui build is slow
```

Linux needs Zed's platform prerequisites (Wayland/X11 dev libraries,
`libxkbcommon-dev` + `libxkbcommon-x11-dev`, Vulkan drivers); macOS needs
Xcode CLT.

### Dependency pinning (important)

`gpui`, `gpui_platform`, and `gpui-component` must resolve to **one** zed
revision or nothing compiles:

- `gpui-component` is pinned by rev in `Cargo.toml`.
- Our gpui deps use the same unversioned zed URL gpui-component uses, so
  the graph shares a single zed source; the exact zed commit is pinned in
  the committed `Cargo.lock` via
  `cargo update -p gpui --precise <rev>` — `<rev>` being what the pinned
  gpui-component's own `Cargo.lock` pins.
- When bumping gpui-component: update its rev, read the new zed rev from
  its lockfile, re-run the precise update, bump `rust-toolchain.toml` to
  at least what zed's own `rust-toolchain.toml` requires, commit both.

## Configuration

Everything is editable in the app (settings overlay → providers list →
Edit). `~/.config/blurb/settings.toml` is created and maintained for you;
legacy flat-format files (provider-level `model`/`effort`) migrate
automatically. The stored shape, should you care:

```toml
[[providers]]
name = "Anthropic"
kind = "anthropic"
api_key_env = "ANTHROPIC_API_KEY"
extra_headers = [["anthropic-beta", "context-management-2025-06-27"]]

[[providers.models]]
id = "claude-opus-5"
effort = "xhigh"
max_tokens = 16000

[[providers.models]]
id = "claude-sonnet-5"
effort = "high"

[[providers]]
name = "Local vLLM"
kind = "openai_compat"
base_url = "http://localhost:8000/v1"

[[providers.models]]
id = "qwen3-coder"
[providers.models.extra_body]
top_k = 20
```
