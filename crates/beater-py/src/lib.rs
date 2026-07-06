//! Embedded CPython (tier 2): interpreter init, runtime venv attach,
//! spawn_blocking tool bridge.
//!
//! pyo3's `auto-initialize` initializes the interpreter with
//! `Py_InitializeEx(0)` — no Python signal handlers — so tokio owns SIGINT.
//! Build-time linking is controlled by `PYO3_PYTHON` (.cargo/config.toml);
//! runtime packages are attached via `site.addsitedir(<venv>/site-packages)`
//! (ARCHITECTURE.md §4). Tools are plain .py files: module-level `TOOL`
//! metadata dict + a `run(input) -> dict` entrypoint, executed fresh per call
//! via runpy so edits are picked up without restarting.

use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use pyo3::prelude::*;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Cap concurrent Python executions: every call holds the GIL on a blocking
/// thread, so unbounded fan-out would only pile up blocked threads.
static PY_PERMITS: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(4)));

#[derive(Debug, Clone)]
pub struct PythonRuntime {
    pub version: String,
    pub executable: String,
    pub major: u32,
    pub minor: u32,
}

/// Interpreter version + executable, for `beater doctor`.
pub fn python_info() -> Result<String> {
    let runtime = python_runtime()?;
    Ok(format!(
        "{} ({})",
        runtime
            .version
            .split_whitespace()
            .next()
            .unwrap_or(&runtime.version),
        runtime.executable
    ))
}

pub fn python_runtime() -> Result<PythonRuntime> {
    Python::attach(|py| {
        let sys = py.import("sys")?;
        let version: String = sys.getattr("version")?.extract()?;
        let executable: String = sys.getattr("executable")?.extract()?;
        let version_info = sys.getattr("version_info")?;
        let major: u32 = version_info.getattr("major")?.extract()?;
        let minor: u32 = version_info.getattr("minor")?.extract()?;
        Ok(PythonRuntime {
            version,
            executable,
            major,
            minor,
        })
    })
}

pub fn expected_venv_site_packages(venv: &Path) -> Result<PathBuf> {
    let runtime = python_runtime()?;
    Ok(venv
        .join("lib")
        .join(format!("python{}.{}", runtime.major, runtime.minor))
        .join("site-packages"))
}

pub fn check_venv(venv: &Path) -> Result<PathBuf> {
    let runtime = python_runtime()?;
    let site_packages = expected_venv_site_packages(venv)?;
    if !venv.is_dir() {
        bail!(
            "missing venv at {}; create it with `python{}.{} -m venv {}`",
            venv.display(),
            runtime.major,
            runtime.minor,
            venv.display()
        );
    }
    if !site_packages.is_dir() {
        bail!(
            "venv at {} has no {} — the embedded interpreter is python{}.{}; \
             recreate the venv with a matching version (e.g. `python{}.{} -m venv {}`)",
            venv.display(),
            site_packages.display(),
            runtime.major,
            runtime.minor,
            runtime.major,
            runtime.minor,
            venv.display(),
        );
    }
    Ok(site_packages)
}

/// Attach a venv's site-packages to the embedded interpreter.
///
/// This is the *runtime* half of Python setup — the linked libpython is fixed
/// at build time, so the venv must match its minor version. Callers that want
/// stdlib-only tools should skip this when no venv exists.
pub fn attach_venv(venv: &Path) -> Result<()> {
    let site_packages = check_venv(venv)?;
    Python::attach(|py| {
        py.import("site")?
            .call_method1("addsitedir", (site_packages.to_string_lossy().as_ref(),))?;
        tracing::info!("attached venv site-packages: {}", site_packages.display());
        Ok(())
    })
}

/// Read a tool file's `TOOL` metadata: (description, input_schema).
pub fn load_tool_spec(path: &Path) -> Result<(String, serde_json::Value)> {
    Python::attach(|py| {
        let module = run_path(py, path)?;
        let tool = module
            .get_item("TOOL")
            .with_context(|| format!("{} does not define a TOOL dict", path.display()))?;
        let json = py.import("json")?;
        let spec_json: String = json.call_method1("dumps", (tool,))?.extract()?;
        let spec: serde_json::Value = serde_json::from_str(&spec_json)?;
        let description = spec
            .get("description")
            .and_then(|d| d.as_str())
            .unwrap_or_default()
            .to_string();
        let input_schema = spec
            .get("input_schema")
            .cloned()
            .unwrap_or_else(|| serde_json::json!({"type": "object"}));
        Ok((description, input_schema))
    })
}

/// Execute a tool's `run(input)` with a JSON input, returning JSON output.
/// Runs on the blocking pool behind a semaphore — the GIL never blocks the
/// async runtime.
pub async fn call_tool(path: PathBuf, input_json: String) -> Result<String> {
    call_tool_with_timeout(path, input_json, Duration::from_secs(10)).await
}

pub async fn call_tool_with_timeout(
    path: PathBuf,
    input_json: String,
    timeout: Duration,
) -> Result<String> {
    let display_path = path.display().to_string();
    let timeout_ms = timeout.as_millis();
    let task = async move {
        let permit = PY_PERMITS
            .clone()
            .acquire_owned()
            .await
            .expect("semaphore never closed");
        tokio::task::spawn_blocking(move || call_tool_blocking(permit, &path, &input_json))
            .await
            .context("python tool task panicked")?
    };

    match tokio::time::timeout(timeout, task).await {
        Ok(result) => result,
        Err(_) => {
            tracing::warn!(
                path = %display_path,
                timeout_ms,
                "python tool timed out; blocking execution continues until Python returns"
            );
            bail!("python tool {display_path} timed out after {timeout_ms}ms");
        }
    }
}

fn call_tool_blocking(
    _permit: OwnedSemaphorePermit,
    path: &Path,
    input_json: &str,
) -> Result<String> {
    Python::attach(|py| {
        let module = run_path(py, path)?;
        let run = module
            .get_item("run")
            .with_context(|| format!("{} does not define run(input)", path.display()))?;
        let json = py.import("json")?;
        let input = json.call_method1("loads", (input_json,))?;
        let result = run
            .call1((input,))
            .with_context(|| format!("python tool {} raised", path.display()))?;
        let out: String = json.call_method1("dumps", (result,))?.extract()?;
        Ok(out)
    })
}

/// Execute a .py file into a fresh namespace dict (runpy.run_path).
fn run_path<'py>(py: Python<'py>, path: &Path) -> Result<Bound<'py, PyAny>> {
    let runpy = py.import("runpy")?;
    let module = runpy
        .call_method1("run_path", (path.to_string_lossy().as_ref(),))
        .with_context(|| format!("failed to load python tool {}", path.display()))?;
    Ok(module)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use serde_json::json;

    use super::{PY_PERMITS, call_tool_with_timeout};

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn timed_out_calls_hold_permits_until_python_returns() {
        let path = write_sleep_tool();
        let handles: Vec<_> = (0..4)
            .map(|_| {
                let path = path.clone();
                tokio::spawn(async move {
                    call_tool_with_timeout(
                        path,
                        json!({"sleepMs": 1000}).to_string(),
                        Duration::from_millis(50),
                    )
                    .await
                })
            })
            .collect();

        wait_for_available_permits(0, Duration::from_secs(2)).await;
        for handle in handles {
            let error = handle
                .await
                .expect("python timeout task should not panic")
                .expect_err("sleeping python call should time out");
            assert!(format!("{error:#}").contains("timed out"), "{error:#}");
        }
        assert_eq!(PY_PERMITS.available_permits(), 0);

        let error = call_tool_with_timeout(
            path.clone(),
            json!({"sleepMs": 0}).to_string(),
            Duration::from_millis(50),
        )
        .await
        .expect_err("fast call should wait behind timed-out executions");
        assert!(format!("{error:#}").contains("timed out"), "{error:#}");

        wait_for_available_permits(4, Duration::from_secs(3)).await;
        let output = call_tool_with_timeout(
            path,
            json!({"sleepMs": 0}).to_string(),
            Duration::from_secs(1),
        )
        .await
        .expect("python permits should recover after sleepers return");
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&output).expect("json output"),
            json!({"ok": true})
        );
    }

    async fn wait_for_available_permits(expected: usize, timeout: Duration) {
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if PY_PERMITS.available_permits() == expected {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!(
            "expected {expected} python permits, got {}",
            PY_PERMITS.available_permits()
        );
    }

    fn write_sleep_tool() -> PathBuf {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock should be after epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("beater-py-timeout-{unique}"));
        fs::create_dir_all(&dir).expect("create temp python tool dir");
        let path = dir.join("sleep_tool.py");
        fs::write(
            &path,
            r#"
TOOL = {"description": "Sleep briefly.", "input_schema": {"type": "object"}}

def run(input):
    import time
    time.sleep(input.get("sleepMs", 0) / 1000)
    return {"ok": True}
"#,
        )
        .expect("write temp python tool");
        path
    }
}
