# Local Code Index — Design (phase 2)

Goal: agents should *know* the codebase instead of rediscovering it every
session — without repeating the mistakes that made Claude Code, Amp, Cline,
Windsurf, and Devin all abandon embedding-RAG for code (stale indexes, chunk
noise, retrieval that competes with the model's own judgment).

## Why not plain embeddings

The published experience (see harness/DESIGN.md §2–3) is consistent: agentic
grep beat vector retrieval everywhere except Cursor, which made it work only
with a custom-trained code embedding model plus continuous re-indexing.
The failure modes were staleness (index lags edits the agent itself just
made), semantic chunks that cut through syntax, and precision (the model
trusts retrieved-but-wrong context). Meanwhile the *cost* of no index is
real too: every session re-runs the same walk — "where is X defined, who
calls it, what's the layout" — burning tokens and turns on rediscovery.

The conclusion: **index structure, not meaning.** Code has an exact,
cheaply-computable skeleton (definitions, references, imports, file tree)
that answers most discovery questions deterministically. Embeddings, if ever
added, come last and only for "find similar concept" queries.

## Architecture: three layers, all local, all incremental

```
             ┌───────────────────────────────────────────────┐
   watcher ─▶│ L1 symbols   tree-sitter defs/refs per file   │─▶ symbols/refs/outline tools
  (notify)   │ L2 trigrams  posting lists per file           │─▶ candidate_files → fast grep
             │ L3 repo map  PageRank over the reference graph │─▶ session primer (token-budgeted)
             └───────────────────────────────────────────────┘
                      stored in .blurb/index/ (mmap'd, per-worktree overlayable)
```

**L1 — Symbol layer (tree-sitter).** Per-file parse extracting definitions
(name, kind, signature line, span) and reference sites. Stored as:
- an `fst` automaton over symbol names → fuzzy/prefix lookup in microseconds,
- a compact postings table `symbol → [(file, line)]` for definitions and
  references.
Per-language grammars start with ts/tsx, rust, python, go — pluggable.
This is Aider's repo-map insight plus LSP-grade lookup, without needing a
live language server per language.

**L2 — Trigram layer.** Classic code-search (Google Code Search / Zoekt
lineage): trigram → file postings, mmap'd. Queries don't return matches —
they return *candidate files* so the existing grep tool runs on 1–5% of the
tree. This accelerates the tool agents already use rather than replacing it;
zero behavior change, pure latency/token win on large repos.

**L3 — Repo map.** The reference graph (who references whom) ranked with
PageRank, rendered as a deterministic, token-budgeted skeleton:

```
src/agent.rs
  pub struct Agent            (rank .92)
  pub fn run(&mut self, ...)  (rank .88)
src/providers/anthropic.rs
  impl Provider for AnthropicProvider
```

Injected once per session as a primer block (after the system prompt, before
the first user message — inside the cached prefix, so it's ~free after the
first turn). Determinism matters: same repo state → same bytes → cache holds.

## Freshness: the make-or-break property

The index must never be *trusted-but-wrong*:

1. **Watcher-driven incremental updates** (`notify`): re-parse only changed
   files; tree-sitter makes single-file re-parse sub-millisecond.
2. **Mtime-validated reads**: every query result carries the indexed mtime;
   the tool layer compares against the file's current mtime and marks stale
   hits (`[index stale for this file — verify with read]`) instead of
   serving them silently.
3. **Agent-edit fast path**: the harness's own write/edit tools notify the
   indexer synchronously, so the agent's next query sees its own edit.
4. **Per-worktree overlay**: sessions run in worktrees (see harness-git).
   The base index is built once for the main checkout; each worktree gets a
   thin overlay of its divergent files. No N-worktrees × full-index cost.

## Exposure to agents

Three new read-only tools (parallel-safe, answering in ms):
- `symbols(query)` — where is it defined (fuzzy),
- `refs(name)` — who uses it,
- `outline(file)` — what's in this file without reading the body.

Plus the invisible wins: `repo_map` primer at session start, and
`candidate_files` silently accelerating `grep`. The tool descriptions steer:
*"prefer symbols/refs over grep when you know a name; grep when you know
text."* If the index is unavailable (`NullIndex`), tools are simply not
registered and grep/glob behavior is unchanged — graceful degradation is a
hard requirement.

## Sizing / expectations

- Zoekt-lineage trigram indexes run ~2–3× corpus size on disk; symbols are
  far smaller. A 1M-LOC repo ⇒ low hundreds of MB, built in seconds to low
  minutes once, updated incrementally forever after.
- Query targets: symbol lookup < 1ms, candidate_files < 10ms, repo_map
  render < 50ms.

## Later, maybe: semantic layer

Only after L1–L3 prove out, and only as an *additive* signal: local
embeddings (small code model via candle/ort) over symbol-anchored chunks,
used for "find code that does something like X" when name/text search
returned nothing. Ranked below exact hits, always mtime-validated, never a
prerequisite for correctness.
