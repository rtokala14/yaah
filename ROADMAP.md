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
- [ ] **Permission model.** Tool allowlists per project (bash command
  approval, path fences) — trust is a product feature.
- [ ] **Model catalog fetch.** "Fetch models" button per provider: query
  `/v1/models` (OpenAI-compat) / Anthropic models endpoint, one-click add.
- [ ] **Session titles that mean something.** Auto-title from first prompt
  (cheap model call), rename inline.

## P2 — compounding advantages

- [ ] **Code index, phase 1** (see INDEX-DESIGN.md): tree-sitter symbol
  layer + trigram-accelerated grep + PageRank repo map injected in the
  cached prefix. Structure first, embeddings never (until proven needed).
- [ ] **Sub-agents.** Spawn read-only explorer agents from the main loop
  (parallel worktree-free sessions), results folded into context.
- [ ] **MCP client.** Connect stdio MCP servers per project; tools join the
  registry with the same permission model.
- [ ] **Hooks.** Pre/post tool-call hooks (lint-on-edit, test-on-done),
  configured per project in-app.
- [ ] **Plan mode.** Read-only exploration turn producing an approvable
  plan before edits are allowed.
- [ ] **Prompt-cache observability.** Show cache hit/miss per turn; warn
  when an action (prune/compact/profile switch) will invalidate the prefix.

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
