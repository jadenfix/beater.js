//! Durable agent runtime: tool registry, Anthropic tool loop,
//! step-lifecycle journal (SQLite).
//!
//! The loop lives in Rust — not in the JS isolate — so it survives hot
//! reloads and every step is journaled before it executes (ARCHITECTURE.md §5).

mod anthropic;
mod cpp_bridge;
mod journal;
mod registry;
mod runner;
mod trace_export;

pub use journal::{Journal, RunRow, StepRow};
pub use registry::{
    AgentConfig, BeatboxConfig, DEFAULT_BEATBOX_URL, ToolCallContext, ToolDecl, ToolNeedsReview,
    ToolRegistry,
};
pub use runner::{
    JournaledToolCall, complete_journaled_tool_call, fail_journaled_tool_call, list_runs, resume,
    run, start_journaled_tool_call,
};
