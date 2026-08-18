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
- [ ] **Session persistence.** Transcripts + session/worktree bindings
  survive app restart (journal per session under `.blurb/`). A harness
  that loses your sessions on quit is a toy.

## P1 — what makes it the *best*, not just working

- [ ] **Cost/usage meter.** Per-session cumulative tokens (input/output/
  cache-read/cache-write) surfaced in the sidebar and chat header; per-turn
  usage in the transcript. The data already flows (`Usage`), show it.
- [ ] **Mid-session model switching.** `SessionCommand::SetProfile` swaps
  the provider/model for subsequent turns without losing the transcript
  (context stays; cache prefix resets — say so in the UI).
- [ ] **Diff review UX.** Click a file in the git panel → full patch view
  (colored hunks); review agent changes before committing. Then: partial
  staging, side-by-side mode.
- [ ] **Merge flow polish.** Post-merge cleanup (delete branch + worktree),
  divergence warnings before merge, "merge & close session" one-click.
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
