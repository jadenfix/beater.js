//! Durable agent runtime: tool registry, Anthropic tool loop,
//! step-lifecycle journal (SQLite).
//!
//! The loop lives in Rust — not in the JS isolate — so it survives hot
//! reloads and every step is journaled before it executes (ARCHITECTURE.md §5).

mod anthropic;
mod journal;
mod registry;
mod runner;

pub use registry::{AgentConfig, BeatboxConfig, DEFAULT_BEATBOX_URL, ToolDecl, ToolRegistry};
pub use runner::{list_runs, resume, run};
