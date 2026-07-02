//! Durable agent runtime: tool registry, Anthropic tool loop,
//! step-lifecycle journal (SQLite), MCP server module.
//!
//! The loop lives in Rust — not in the JS isolate — so it survives hot
//! reloads and every step is journaled before it executes (ARCHITECTURE.md §5).

use std::path::Path;

use anyhow::Result;

/// Start a new run of the named agent. Blocks until the run finishes.
pub fn run(_app_dir: &Path, name: &str, _prompt: &str) -> Result<()> {
    anyhow::bail!("`beater agent run` lands in M2 (agent: {name})")
}

/// Resume an interrupted run from its journal.
pub fn resume(_app_dir: &Path, run_id: &str) -> Result<()> {
    anyhow::bail!("`beater agent resume` lands in M2 (run: {run_id})")
}

/// List recorded runs.
pub fn list_runs(_app_dir: &Path) -> Result<()> {
    anyhow::bail!("`beater agent runs` lands in M2")
}
