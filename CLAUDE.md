# Blurb — repo memory

Native desktop agentic coding harness. Rust + GPUI (Zed's UI framework) +
longbridge `gpui-component` widgets. Read `ROADMAP.md` first — it is the
prioritized product plan and gets updated every working session. `DESIGN.md`
covers architecture, `INDEX-DESIGN.md` the future code index.

## Crate map

| Crate | Role | Constraints |
|---|---|---|
| `harness-core` | providers, tools, agent loop, context mgmt, session threads | **no UI deps, no tokio** — synchronous by design, tests run headless |
| `harness-git` | libgit2: status/diff/commit/worktrees/log-graph/merge | no UI deps, tests headless |
| `harness-index` | code-index trait + phase-1 `RegexIndex` (symbols/refs/outline/repo-map) | no UI deps |
| `blurb-app` | the GPUI app | only crate allowed to touch gpui |

## Provider model (v2 — multi-model)

- `config::ProviderConfig` is a **connection**: name, kind
  (anthropic / openai / openai_compat), base_url, api_key / api_key_env,
  extra HTTP headers, extra body fields, quirk switches — and a list of
  `ModelConfig`s (id, label, effort, temperature, max_tokens, per-model
  extra body). `active_model` indexes into `models`.
- `config::RunProfile` is the **resolved flat profile** (one provider + one
  model, extra_body merged with model winning) that `providers::build`,
  the adapters, and `SessionHandle::spawn` consume. UI/settings never hand
  a raw ProviderConfig to the session layer — always `resolve()`.
- Legacy flat settings files (provider-level `model`/`effort`/…) migrate in
  `ProviderConfig::normalize()`, called on load. Keep that path working.
- Everything is editable in-app (settings overlay). The TOML settings file
  (`~/.config/blurb/settings.toml`, `%APPDATA%\blurb\` on Windows) is
  persistence, not an interface — never build a feature that requires
  hand-editing it.
- **Diagnosis belongs in core.** `providers::validate_profile` (static, no
  network) + `providers::test_connection` (real adapter, real auth) +
  `providers::explain_error` return a `ProbeReport`; the UI only renders it.
  Two distinctions the code must keep making, because both cost real debugging
  time when collapsed:
  - `base_url` is a **prefix** — the adapters append `/chat/completions` or
    `/v1/messages`. A pasted full endpoint URL silently doubles the path.
  - `api_key_env` holds a **variable name**, not a secret. A pasted key there
    leaves the effective key empty, which surfaces as a 401 that looks like a
    bad key.
  - A **403 can mean the model, not the key** (per-model region/entitlement
    gating). Never report those as auth failures — the key may be fine for
    every other model on the same provider.

## Context model (long sessions are a feature)

- Sessions are unbounded; one run defaults to 250 turns
  (`Settings::max_turns_per_run`, editable in the settings overlay).
- `context.rs` escalation ladder, cheapest first: prune stale tool
  results → strip stale thinking → force-prune (keep only current turn)
  → compact. Compaction pins the original user task verbatim (unwrapped,
  never re-wrapped, across repeated compactions), keeps a boundary-safe
  verbatim tail (`compact_keep_tail`), and middle-truncates its own
  summarization request against the budget.
- Budget = 80% of the model's `context_window` (per-model setting),
  default 160k when unset. `AgentEvent::TurnStart` carries
  context/budget estimates; the chat header renders `ctx N%`.
- Tail cuts must never orphan tool results (`safe_tail_start`) — keep it
  that way or Anthropic requests 400.
- Durable memory: `types::SessionMemory` lives in `ToolContext` (single
  source of truth) — `remember` writes notes, `recall` searches notes +
  archived summaries, the agent persists it in the journal and injects a
  bounded digest at every compaction, archiving each summary. The
  replacement shape is [pinned task][memory digest][summary][verbatim
  tail].

## Interaction model (agent ⇄ human, mid-run)

- `types::InteractionHandler` is how the agent reaches the human: the
  session thread blocks in `ask()` (cancel-aware 100ms poll) while the UI
  answers via `SessionHandle::respond(id, reply)` on a dedicated channel —
  never through the command queue (it's not drained during a run).
- Permission gate lives in `agent::permission_gate` (write/edit/bash only;
  read-only tools never gate). `PermissionPolicy` in config.rs: master
  `ask` switch, `allow_edits`, `allowed_bash` (bare word = program, spaced
  entry = prefix). AllowAlways = session-scoped in the agent + persisted
  into settings by the UI. Headless (`NoInteraction`) denies with an
  actionable message.
- `ask_user` (blocks for an answer) and `todo_write` (replace-whole-list,
  live PLAN panel) ride the same plumbing. Typed input answers a pending
  question instead of starting a new run (`Transcript::pending_question`).

## Extensibility surfaces

- **Skills**: markdown packs in `<repo>/.blurb/skills/` (project) and
  `~/.config/blurb/skills/` (global; project shadows). Names+descriptions
  live in the cached prompt; the `skill` tool loads bodies on demand.
- **MCP**: stdio servers in settings (`mcp.rs` — newline JSON-RPC,
  reader thread, serial id-matched requests). Tools appear as
  `mcp_<server>_<tool>` and are permission-gated; AllowAlways persists
  the exact tool name into `PermissionPolicy::allowed_tools`.
- **Sub-agents**: `subagent` tool → `NestedRunner` in agent.rs builds a
  fresh Agent with `nested_toolset` (read-only, minus subagent/todo/
  remember). read_only=true ⇒ parallel fan-out for free. Nested contexts
  get no runner, so recursion fails closed.
- **Index**: sessions get `registry::shared_index(root)` — one live
  index per root, weak-freed. Extraction: tree-sitter (rs/py/ts/js/go)
  with regex fallback (java/kt, c/c++, rb). Invalidation: notify watcher
  dirty flag (60s safety net; debounced stat-walk without a watcher);
  `candidate_files` re-walks unless watcher-clean and <2s fresh.
  Ranking = file-graph PageRank; repo map excludes names defined in 3+
  files (data-driven genericity) and leads with types. Grep is
  candidate-filtered when `grep::extract_required_literals` deems the
  pattern sound. Dev tool: `cargo run -p harness-index --release
  --example repomap -- <path> [query]`.
- **Hooks**: `hooks.rs` — pre (blocks on failure) / post (appends
  feedback) shell hooks run inside `run_tool`; settings hooks merge with
  `.blurb/hooks.json`. Subagents run hook-free.
- **Plan mode**: `ToolContext.plan_mode` AtomicBool. The gate refuses all
  gated + mcp tools while on (outranks every allowance); `present_plan`
  flips it off on user approval; transitions announced via
  `AgentEvent::PlanMode` (watermarked after tool batches).

## Persistence model

- Session **message history** is journaled by the session thread itself
  (`SessionJournal`, write-then-rename after every run) to a path the host
  passes into `SessionHandle::spawn`. Cumulative usage rides along.
- App-side **metadata** (titles, worktree bindings, provider labels) lives
  in `blurb-app/src/persist.rs` (`ProjectStore`), under the platform data
  dir keyed by project-root hash. Nothing is written into the user's repo.
- On open, `Workspace::restore_sessions` re-spawns each session seeded
  from its journal and rebuilds the UI transcript from the same messages
  (`Transcript::from_messages`). One format, two readers, one writer.
- `RunFinished.usage` is **cumulative** for the session, not per-run.

## Threading model (don't fight it)

UI thread (GPUI) ⇄ one OS thread per session over crossbeam channels
(`SessionCommand` in, `SessionEvent` out). Blocking `ureq` SSE in the
session thread; scoped threads for parallel read-only tools. git (libgit2)
runs via `cx.background_spawn`; the git panel renders immutable
`RepoSnapshot` values only. Interrupt = shared atomic `CancelToken`
(re-armed after each run).

## Build (heavy — plan around it)

- Fast inner loop: `cargo check -p harness-core -p harness-git` and
  `cargo test -p harness-core -p harness-git` — seconds-to-minutes, no UI.
- `cargo check -p blurb-app` pulls gpui from the zed repo — first build is
  very expensive (tens of minutes). gpui + gpui_platform are pinned to the
  zed rev that gpui-component's Cargo.lock pins; bump all three in
  lockstep or nothing compiles (see README "first build checklist").
- GPUI API drifts; expect renames when bumping revs, not redesigns.

## Conventions

- Keep view files thin: `workspace.rs` / `transcript.rs` are GPUI-free
  models with unit tests; `views/*` only render and dispatch.
- New git capabilities go in `harness-git` with headless tests first, then
  get a panel affordance.
- Provider quirks are settings entries (headers/body extras), never code
  forks per vendor.
- **Streaming tool calls are hostile input.** `openai.rs`'s
  `ToolCallAccumulator` groups deltas by `index`, falling back to `id`, then to
  a name change plus "args already parse as complete JSON". Never default a
  missing `index` to 0 (it merges every call in the turn) and never blindly
  append `function.name` (servers resend the full name each delta). Both bugs
  together produced real tool names like `readreadglobread`. Any change here
  must keep the `providers::openai::tests` cases green — they encode observed
  server behaviour, not hypotheticals.
- Session worktrees live in a sibling dir `.<repo>-blurb-worktrees/`,
  branches under `blurb/<slug>-<hash>`; branch is kept when a worktree is
  removed.
- Anything the **model** reads must be platform-neutral: use
  `harness_index::display_path`, never `Path::display()`, or Windows leaks
  `src\auth.rs` into the prompt.
- Tests that touch git must set `core.autocrlf=false` + `core.eol=lf`
  locally; Git for Windows turns autocrlf on in the *system* config, so
  content assertions are not hermetic without it.

## Platform gotchas (Windows / corporate networks)

- **libgit2 ownership.** `harness-git::init()` disables libgit2's owner
  validation once per process. A repo cloned from an elevated shell is owned
  by `BUILTIN\Administrators`, so every `Repository::discover` fails with
  `GIT_EOWNER`. `safe.directory` was rejected deliberately: it is
  hand-maintained out-of-band config, and it would need a fresh entry for
  every session worktree. The option is process-global and sessions open
  repos from several threads, so it must stay one-shot — never a
  disable/re-enable pair.
- **TLS.** `ureq` is built with `native-certs`, which *replaces*
  `webpki-roots` (they are mutually exclusive cfgs). Without it a
  TLS-inspecting proxy's root CA sits in the OS store and rustls still can't
  see it. Do not "add" webpki-roots back alongside it.
- **GPUI modals need `.occlude()`**, not an empty `on_click`. Click handlers
  fire on bubble gated only on hover, so a backdrop's dismiss listener still
  runs for clicks on the card above it. This bit both the settings and diff
  overlays.
- A `cargo run` exit of **101 can just be a locked output binary** — a stale
  `blurb.exe` still running makes cargo fail to overwrite `target/debug`.
  Check for live processes before diagnosing it as a panic.
- **Main-thread stack.** MSVC reserves 1 MiB; GPUI recurses per element and
  debug builds don't inline, so deep views (settings overlay with several
  model rows) overflow it while handling plain text input. Symptom is
  `STATUS_STACK_OVERFLOW` (0xc00000fd) with no panic and no backtrace.
  `crates/blurb-app/build.rs` links with `/stack:33554432` (Zed does the same
  at 8 MiB). Reserve is address space; commit stays at 4 KiB, so it is nearly
  free. Verify with the PE optional header, not by eyeballing RSS.

## Session ritual

1. Read ROADMAP.md; pick from the highest unfinished tier.
2. Core-first: land testable logic in harness-core/-git, then UI.
3. Run headless tests; `cargo check -p blurb-app` when UI changed.
4. Update ROADMAP.md checkboxes + this file if architecture moved.
5. Commit with clear messages; push to the designated branch.
