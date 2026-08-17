//! harness-core — the agentic coding harness, UI-independent.
//!
//! Architecture mirrors the TypeScript prototype in `harness/` (see
//! harness/DESIGN.md for the research behind the decisions):
//!
//! - The internal message model is Anthropic-shaped (ordered typed content
//!   blocks) because it is the superset of the wire formats we target;
//!   provider adapters project onto their dialect losslessly.
//! - One flat agent loop; consecutive read-only tool calls run in parallel.
//! - Context management is prune-first (restorable stubs for stale tool
//!   results, batched to amortize prompt-cache invalidation), compaction
//!   (model-written structured handoff) only as a last resort.
//! - Evidence-grounded completion: a deterministic gate refuses "done" when
//!   files were mutated but nothing was executed afterwards.
//!
//! Threading model: everything here is synchronous/blocking and runs on a
//! dedicated session thread (`session::SessionHandle`); the UI communicates
//! over crossbeam channels. No async runtime — this keeps the core trivially
//! embeddable under GPUI (or any other host) without executor conflicts.

pub mod agent;
pub mod config;
pub mod context;
pub mod http;
pub mod prompt;
pub mod providers;
pub mod session;
pub mod sse;
pub mod tools;
pub mod types;

pub use agent::{Agent, AgentOptions, AgentResult};
pub use config::{ProviderConfig, ProviderKind};
pub use session::{SessionCommand, SessionEvent, SessionHandle};
pub use types::*;
