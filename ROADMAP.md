# Blurb — Product Roadmap

Owner-maintained. This is repo memory: read it at the start of every working
session, update it at the end. Ordering within a tier is priority order.

**North star:** the best coding harness there is — native and fast, works
with *any* model provider, treats git as a first-class surface (parallel
worktree sessions you can see, review, and merge), and manages
context/cost with discipline instead of vibes.

**Product pillars** (every feature must serve one):

1. **Bring-your-own-model** — providers and models are user data, not code.
2. **Git-native parallelism** — sessions are branches; review and merge are
   in-app, visible, reversible.
3. **Context discipline** — cache-stable prompts, restorable pruning,
   evidence-grounded completion; the user can always see cost.
4. **Native speed** — GPUI, no web stack, no async-runtime sprawl.

---

## P0 — must exist for the product to be real

- [x] **Provider registry v2: providers own multiple models.**
  A provider is a connection (kind, base URL, auth, headers, body quirks);
  it contains N models (id, label, effort, max tokens, temperature,
  per-model body overrides). Active selection = provider + model.
  Legacy flat settings migrate automatically. *(done — this iteration)*
- [x] **Everything editable in-app.** Full CRUD for providers and models in
  the settings overlay: add/duplicate/delete provider, add/delete model,
  every field editable, no settings-file round-trip ever required.
  *(done — this iteration)*
- [x] **Git history graph.** Lane-assigned commit graph (log + branch tips
  + ahead/behind upstream) in the git panel. *(done — this iteration)*
- [x] **Merge session branches in-app.** Fast-forward or merge-commit,
  conflict-safe (abort + report conflicted paths), from the git panel.
  *(done — this iteration)*
- [x] **First real build + drift fixes.** `blurb-app` compiles clean (no
  warnings) against gpui-component `1c4e681` / zed `e0931d5a` on Rust
  1.97.1; Cargo.lock + rust-toolchain.toml committed; pinning procedure
  documented in README. *(done — this iteration)*
- [x] **Session persistence.** Transcripts + session/worktree bindings
  survive app restart: session metadata in the platform data dir
  (`~/.local/share/blurb/projects/<key>/sessions.json`), full message
  history in a per-session journal the session thread rewrites after every
  run (write-then-rename). On open, agents are re-seeded from their
  journals and transcripts rebuilt from the same messages; a vanished
  worktree falls back to the project root. *(done — this iteration)*

- [x] **Long-session endurance.** Sessions run as long as needed (runs
  default to 250 turns, editable in-app). Context pressure is handled by
  an escalation ladder — prune stale tool results → strip stale thinking
  → force-prune everything re-readable → only then compact. Compaction
  pins the user's original task verbatim (survives repeated compactions),
  keeps a boundary-safe recent tail verbatim, and caps its own
  summarization request so it can't overflow. Per-model `context_window`
  drives the budget; the chat header shows live context pressure
  (`ctx N%`). *(done — this iteration)*
- [x] **Durable cross-compaction memory.** The agent records dense notes
  via a `remember` tool (decisions, constraints, learned facts); notes
  live in `SessionMemory`, persist in the journal, and are re-injected
  verbatim into every compaction replacement (bounded digest — oldest and
  newest notes win when over budget). Every compaction summary is
  archived in memory for the record. The system prompt teaches the model
  to use it in long sessions. *(done — this iteration)*
- [x] **Recall over archived memory.** A `recall` tool searches notes and
  archived compaction summaries (scored keyword match; summary hits are
  labeled with their epoch) so knowledge summarized out of the live
  transcript is retrievable on demand instead of re-derived. Memory lives
  in `ToolContext` as the single source of truth (`remember` writes,
  `recall` reads, the agent persists/injects). *(done — this iteration)*
  Still open: a memory panel in the UI to browse/edit notes.
- [x] **UI polish, round 1.** Session rows close in-place (worktree kept),
  SESSIONS/CONNECTION section labels, memory indicator (`◆ N`) in the
  chat header, welcome screen with active-model chip, user messages get
  an accent bar, tool cards get status dots, primary commit button.
  *(done — this iteration)* A deeper visual pass (typography scale,
  spacing rhythm, light theme) stays open under P3.

## P1 — what makes it the *best*, not just working

- [x] **Cost/usage meter.** Sessions report cumulative usage (persisted in
  the journal); chat header shows turns / prompt tokens / cache-hit % /
  output tokens; sidebar rows show compact totals. *(done — this iteration;
  per-turn usage rows + $ estimates remain open)*
- [x] **Mid-session model switching.** `SessionCommand::SetProfile` swaps
  provider/model for subsequent turns, transcript kept; a `→ <profile>`
  button appears in the chat header when the settings default differs from
  the session's profile; the switch renders as a transcript notice (cache
  prefix reset called out). *(done — this iteration)*
- [x] **Diff review UX, v1.** Click a file in the git panel → colored
  unified-diff overlay (per-file patch, untracked files included).
  *(done — this iteration)* Still open: partial staging, side-by-side
  mode, diffs for session-worktree checkouts.
- [x] **Merge flow polish.** After a successful merge the git panel offers
  one-click cleanup: close the owning session, remove the worktree, delete
  the merged branch (dismissable). *(done — this iteration)* Still open:
  divergence warnings before merge.
- [x] **Permission model.** Gated tools (write/edit/bash) ask the user
  before running: Allow / Always allow / Deny cards in the chat.
  AllowAlways persists (bash programs into an allowlist with prefix
  matching, edits as a flag); the allowlist is managed as removable chips
  in settings, next to the master ask switch. The session thread blocks on
  a cancel-aware interaction channel; headless hosts auto-deny with an
  actionable message. *(done — this iteration)* Still open: path fences,
  per-project (vs global) policies.
- [x] **Ask-the-user.** An `ask_user` tool lets the agent pose a question
  mid-run (with optional option buttons); typed input answers a pending
  question instead of starting a new task. *(done — this iteration)*
- [x] **Todo lists.** A `todo_write` tool (replace-whole-list semantics,
  pending/in_progress/done) renders as a live PLAN panel in the chat pane,
  persists in the journal, and restores with the session.
  *(done — this iteration)*
- [x] **Model catalog fetch.** "Fetch models" in the provider editor
  queries the endpoint's catalog (Anthropic + OpenAI-compatible) with the
  current form values; returned ids render as click-to-add chips.
  *(done — this iteration)*
- [x] **Session titles that mean something.** Sessions auto-title from
  their first prompt with one cheap low-effort model call (sanitized,
  background, failure keeps the placeholder). *(done — this iteration)*
  Still open: inline rename.

## P2 — compounding advantages

- [x] **Code index, phase 1.** `harness-index::RegexIndex`: one
  gitignore-aware walk, per-language regex symbol extraction (rust, py,
  ts/js, go), identifier→files inverted map. Powers `symbols` / `refs` /
  `outline` tools and a token-budgeted repo map (ranked by cross-file
  mentions) injected into the cached system prompt (toggleable in
  settings). *(done)*
- [x] **Code index, phase 2.** The index is now *live and honest*:
  mtime-validated incremental refresh (stat-walk, reparse only changed
  files, drop deleted; debounced on queries, forced where correctness
  depends on it) — never trusted-but-wrong. Ranking is file-graph
  PageRank (A mentions a symbol defined in B ⇒ edge A→B, generic names
  capped), driving both symbol ordering and repo-map file order. Grep is
  index-accelerated: patterns whose structure makes literal extraction
  sound (no alternation/optionality/classes) search only candidate files
  containing every required token, with a forced refresh so late-created
  files are never missed. Languages: rust, python, ts/js, go + new
  java/kotlin, c/c++, ruby. *(done)*
- [x] **Code index, phase 3.** (a) **Tree-sitter precision** for rust,
  python, ts/tsx/js, go: real syntax trees, so strings/comments can't
  fake definitions and methods know their enclosing type (incl. trait
  method signatures, `export const` arrow functions); regex remains the
  fallback for java/kotlin, c/c++, ruby. (b) **fs-watcher invalidation**
  (notify): a dirty flag replaces stat-walk polling — queries refresh
  only when something actually changed (60s dropped-event safety net;
  watcher-less filesystems fall back to debounced walks). (c) **Shared
  per-root registry**: sessions on the same checkout converge on one
  live index (weak entries free on last close); distinct worktrees keep
  honest separate indexes. (d) **Repo-map quality** (dogfooded on this
  repo): data-driven genericity — names defined in 3+ files (`new`,
  `tests`, accessors) are excluded, types lead, so the map reads as the
  domain model, not boilerplate. Release-profile timings on this repo:
  ~230ms build, ~2.6ms clean refresh, ~3.5ms map. Dev tool:
  `cargo run -p harness-index --release --example repomap -- <path>
  [query]`. *(done — this iteration)* Deferred until profiling demands:
  content-level cross-worktree dedup, fst symbol automaton, trigram
  postings.
- [x] **Sub-agents.** The `subagent` tool spawns read-only explorer agents
  with fresh contexts; several calls in one turn fan out in parallel via
  the existing tool threads; only final reports return. Recursion fails
  closed. *(done — this iteration)*
- [x] **MCP client.** Stdio JSON-RPC servers configured in settings connect
  per session; their tools join the registry as `mcp_<server>_<tool>`,
  permission-gated with per-tool AllowAlways persistence.
  *(done — this iteration)* Still open: HTTP/SSE transports, resources
  and prompts (tools only today).
- [x] **Skills.** Global (`~/.config/blurb/skills/`) + project
  (`.blurb/skills/`) markdown packs, project shadowing global; listed in
  the cache-stable prompt, full body loaded on demand via the `skill`
  tool. *(done — this iteration)*
- [x] **Hooks.** Pre/post tool-call shell hooks: pre blocks the call on
  non-zero exit (its output becomes the error result), post appends its
  output as feedback (lint-on-edit). Env-passed context, session cwd,
  120s timeout. Global hooks in settings (HOOKS section) merge with the
  project's `.blurb/hooks.json`. *(done — this iteration)*
- [x] **Plan mode.** A per-session toggle (chat-header Plan button, or
  the model's own flow): mutating tools are refused with guidance while
  planning; the `present_plan` tool submits the plan through the
  interaction channel with Approve / Keep-planning options — approval
  unlocks implementation mid-run. Transitions render as notices; system
  notes steer the model on toggle. *(done — this iteration)*
- [x] **Prompt-cache observability.** Per-turn cache-hit % in the header;
  unexpected-miss notices with the reprocessed token count; prune/compact
  notices state their cache cost. *(done — this iteration)*

## P3 — breadth

- [ ] Packaging: macOS bundle + Linux AppImage/deb; auto-update channel.
- [ ] Light theme parity; theme picker in settings.
- [ ] Windows support (gpui windows backend is maturing).
- [ ] Multi-repo workspaces; per-project provider defaults.
- [ ] Optional local telemetry dashboard (tokens/cost over time), local-only.

---

## Engineering invariants (do not trade these away)

- `harness-core` and `harness-git` stay UI-free and fully testable headless.
- No tokio; the core is synchronous, GPUI owns scheduling (see DESIGN.md).
- gpui + gpui_platform + gpui-component revs move in lockstep (README).
- Settings files remain human-readable TOML, but the app must never
  *require* hand-editing them.
- Every provider quirk is config (headers/body extras), never a code fork.
