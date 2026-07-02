//! Embedded CPython (tier 2): interpreter init, runtime venv attach,
//! spawn_blocking tool bridge.
//!
//! pyo3's `auto-initialize` initializes the interpreter with
//! `Py_InitializeEx(0)` — no Python signal handlers — so tokio owns SIGINT.
//! Build-time linking is controlled by `PYO3_PYTHON`; runtime packages are
//! attached via `site.addsitedir(<venv>/site-packages)` (ARCHITECTURE.md §4).

use anyhow::Result;
use pyo3::prelude::*;

/// Interpreter version + executable, for `beater doctor`.
pub fn python_info() -> Result<String> {
    Python::attach(|py| {
        let sys = py.import("sys")?;
        let version: String = sys.getattr("version")?.extract()?;
        let executable: String = sys.getattr("executable")?.extract()?;
        Ok(format!(
            "{} ({executable})",
            version.split_whitespace().next().unwrap_or(&version)
        ))
    })
}
