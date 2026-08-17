# Blurb Desktop — Architecture

The desktop incarnation of the harness (see `harness/DESIGN.md` for the
research foundation: landscape survey, context engineering evidence, and the
provider-abstraction design). Rust + GPUI, with the longbridge
`gpui-component` library for widgets.

## Crate layout

```
desktop/
  crates/
    harness-core/    the agent: providers, tools, loop, context mgmt   (no UI deps)
    harness-git/     libgit2 integration: status, diff, commit, worktrees
    harness-index/   code-index trait + NullIndex (phase 2: INDEX-DESIGN.md)
    blurb-app/       GPUI application
```

`harness-core` is a faithful Rust port of the TypeScript prototype in
`harness/` — same internal model (Anthropic-shaped content blocks with
lossless reasoning round-trip), same loop (flat, parallel read-only tools),
same context strategy (batched restorable pruning → structured compaction),
same evidence-grounded completion gate. Differences from the prototype:

- **Provider profiles are fully user-configurable** (`ProviderConfig`):
  kind (anthropic / openai / openai-compatible), base URL, key or key-env,
  model, effort, plus **arbitrary extra HTTP headers and extra top-level
  body fields** merged into every request. Any gateway or compat-server
  quirk is a settings entry, not a code change.
- Tools use ripgrep's `ignore` walker (gitignore-aware) and `globset`.

## Threading model — why there is no async runtime

GPUI has its own executors; tokio does not belong in the same process
without friction. So the core is **synchronous by design**:

```
UI thread (GPUI)                     session threads (1 per session)
┌───────────────────┐  SessionCommand  ┌──────────────────────────────┐
│ Workspace (model) │ ───────────────▶ │ Agent loop (blocking):       │
│  Transcript per   │                  │  ureq SSE stream → tools     │
│  session          │ ◀─────────────── │  (std::thread parallelism)   │
└───────────────────┘  SessionEvent    └──────────────────────────────┘
        ▲ pump_events() drains crossbeam channels; cx.notify() re-renders
```

- Each session is one OS thread running the blocking agent loop
  (`SessionHandle::spawn`). HTTP/SSE is blocking `ureq`; parallel read-only
  tool batches use scoped threads.
- Events cross to the UI over `crossbeam-channel`; the root view drains
  them (`Workspace::pump_events`) from a lightweight GPUI background task
  and calls `cx.notify()` only when something changed.
- Interrupts are an atomic `CancelToken` shared with the session thread —
  cancellation takes effect at the next loop/stream/tool checkpoint, and
  the token re-arms so the session survives interruption.
- git operations (libgit2) also run off the UI thread; the git panel
  renders immutable `RepoSnapshot` values.

This keeps `harness-core` embeddable anywhere (CLI later, tests trivially)
and the UI thread never blocks on network or git.

## The session-per-worktree model

The unit of work is a **session bound to a git worktree**:

- Starting a session (default) creates `blurb/<slug>-<id>` branched from
  HEAD and a worktree checkout under a sibling directory
  (`.<repo>-blurb-worktrees/`), so parallel sessions never collide with
  each other or with the user's own checkout.
- The git panel shows each worktree with its owning session, its
  uncommitted diffstat, and actions: commit all, remove worktree (keeps
  the branch), open in terminal.
- Merging back is ordinary git (the branch is visible in any git tool);
  a built-in "merge to main" flow is on the roadmap.
- Worktree isolation is a setting; sessions can also run directly in the
  main checkout.

This is the desktop-native answer to what CLI harnesses approximate with
`--worktree` flags: isolation is the default, visible, and reversible.

## UI structure (blurb-app)

- `workspace.rs` — GPUI-free model: settings, sessions, git snapshot,
  event pumping. Unit-testable.
- `transcript.rs` — GPUI-free fold of `SessionEvent`s into render-ready
  blocks (user / assistant streaming text / thinking / tool cards /
  notices / errors). Unit-tested.
- `views/` — GPUI views over those models:
  - root: resizable three-pane layout (sidebar | chat | git panel),
  - sidebar: session list (running indicator, provider label) + new-session,
  - chat: virtualized transcript + input, interrupt button while running,
  - git panel: branch/HEAD, status list, per-worktree diffstat, commit box,
  - settings: provider profile editor including extra headers (key/value
    rows) and extra body fields (key/JSON-value rows).
- Theming, buttons, inputs, lists, split panes come from `gpui-component`;
  the design language is its default dark theme with dense spacing —
  sleek/modern comes free, we add restraint.

## Cost & cache discipline (carried over from the prototype)

- Byte-stable system prompt per session; volatile facts travel as
  `SystemNote` messages appended after the cached prefix.
- Anthropic adapter sets two cache breakpoints (system+tools, last message
  block) so each turn extends the cached prefix.
- Pruning is batched at a budget threshold to amortize the cache
  invalidation it causes; compaction (full rebuild) is last resort.

## Phase 2: local code index

See `INDEX-DESIGN.md`. Summary: index *structure, not meaning* — a
tree-sitter symbol layer (defs/refs, fst name automaton), a trigram layer
that accelerates the existing grep tool by candidate-file filtering, and a
PageRank-ranked, token-budgeted repo map injected once per session inside
the cached prefix. Watcher-driven incremental updates with mtime-validated
results so the index is never trusted-but-wrong; per-worktree overlays so N
sessions don't cost N full indexes. Embeddings are explicitly deferred —
they're the last layer, not the first.

## Build

Not built yet by agreement — `gpui` + `gpui-component` are git
dependencies with substantial first-build cost, and their revisions must be
pinned in lockstep (gpui-component tracks specific zed revisions; see
README). First build session: pin both revs, `cargo check`, fix drift
(GPUI's API moves fast), then `cargo test -p harness-core -p harness-git`
— those crates have no UI dependency and their tests run headless.
