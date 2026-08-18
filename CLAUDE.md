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
  (`~/.config/blurb/settings.toml`) is persistence, not an interface —
  never build a feature that requires hand-editing it.

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
- **Index**: `RegexIndex::build` runs once per session thread, then stays
  live via mtime-validated refresh (debounced on queries;
  `candidate_files` forces it because grep correctness depends on it).
  Ranking = file-graph PageRank; `symbols`/`refs`/`outline` tools always
  join; grep is candidate-filtered when
  `grep::extract_required_literals` deems the pattern sound; the
  2k-token repo map is appended to the system prompt when
  `inject_repo_map` is on.
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
- Session worktrees live in a sibling dir `.<repo>-blurb-worktrees/`,
  branches under `blurb/<slug>-<hash>`; branch is kept when a worktree is
  removed.

## Session ritual

1. Read ROADMAP.md; pick from the highest unfinished tier.
2. Core-first: land testable logic in harness-core/-git, then UI.
3. Run headless tests; `cargo check -p blurb-app` when UI changed.
4. Update ROADMAP.md checkboxes + this file if architecture moved.
5. Commit with clear messages; push to the designated branch.
