//! The beater.js host runtime: axum HTTP server, file-based router,
//! deno_core (V8) worker thread, TS/TSX transpiling module loader, hot reload.

mod config;

pub use config::AppConfig;

use std::path::Path;

use anyhow::Result;

/// Start the dev server for the app at `app_dir`. Blocks until shutdown.
pub fn dev(app_dir: &Path, port_override: Option<u16>) -> Result<()> {
    let config = AppConfig::load(app_dir)?;
    let port = port_override.unwrap_or(config.port);
    anyhow::bail!(
        "`beater dev` lands in M1 (app: {}, port {port})",
        config.name
    )
}

/// The embedded V8 version, for `beater doctor`.
pub fn v8_version() -> &'static str {
    deno_core::v8::VERSION_STRING
}
