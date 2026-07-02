//! The beater.js host runtime: axum HTTP server, file-based router,
//! deno_core (V8) worker thread, TS/TSX transpiling module loader, hot reload.

mod agent_config;
mod config;
mod crawl;
mod loader;
mod mcp;
mod router;
mod server;
mod worker;

pub use agent_config::load_agent_config;
pub use config::AppConfig;
pub use mcp::AccessConfig as McpAccessConfig;
pub use router::{Route, RouteKind, RouteTable};

use std::path::Path;

use anyhow::{Context, Result};

/// Start the dev server for the app at `app_dir`. Blocks until ctrl-c.
pub fn dev(
    app_dir: &Path,
    port_override: Option<u16>,
    host_override: Option<std::net::IpAddr>,
) -> Result<()> {
    let config = AppConfig::load(app_dir)?;
    let port = port_override.unwrap_or(config.port);
    let host = host_override.unwrap_or(config.host);

    // Agent surfaces are built before the runtime starts: config extraction
    // spins one-shot isolates (their own mini-runtimes), and the venv attach
    // must precede any Python tool loading.
    if let Some(venv) = &config.python_venv
        && venv.is_dir()
    {
        beater_py::attach_venv(venv)?;
    }
    let (registry, agents) = build_registry(&config.app_dir)?;

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(server::serve(config, host, port, registry, agents))
}

/// Merge every agent's tools into the registry served over /mcp.
fn build_registry(app_dir: &Path) -> Result<(beater_agent::ToolRegistry, Vec<String>)> {
    let mut registry = beater_agent::ToolRegistry::empty();
    let mut agents = Vec::new();
    let agents_dir = app_dir.join("agents");
    if agents_dir.is_dir() {
        for entry in std::fs::read_dir(&agents_dir)? {
            let dir = entry?.path();
            if !dir.join("agent.ts").is_file() {
                continue;
            }
            let name = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .context("agent dir name")?;
            let value = load_agent_config(app_dir, &name)?;
            let config: beater_agent::AgentConfig = serde_json::from_value(value)
                .with_context(|| format!("agents/{name}/agent.ts config shape"))?;
            registry.extend(beater_agent::ToolRegistry::build(&dir, &config.tools)?);
            agents.push(name);
        }
    }
    agents.sort();
    Ok((registry, agents))
}

/// The embedded V8 version, for `beater doctor`.
pub fn v8_version() -> &'static str {
    deno_core::v8::VERSION_STRING
}
