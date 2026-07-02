//! Agent config extraction: evaluate agents/<name>/agent.ts in a one-shot
//! isolate and pull out the default export as plain JSON. The agent *loop*
//! never runs in JS — this is config, not execution.

use std::path::Path;
use std::rc::Rc;

use anyhow::{Context, Result};
use deno_core::{JsRuntime, PollEventLoopOptions, RuntimeOptions, v8};

use crate::loader::BeaterModuleLoader;
use crate::worker::{beater_ext, format_js_error};

pub fn load_agent_config(app_dir: &Path, name: &str) -> Result<serde_json::Value> {
    let app_dir = app_dir
        .canonicalize()
        .with_context(|| format!("app dir not found: {}", app_dir.display()))?;
    let agent_file = app_dir.join("agents").join(name).join("agent.ts");
    anyhow::ensure!(
        agent_file.is_file(),
        "no agent named {name:?}: expected {}",
        agent_file.display()
    );

    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;
    let local = tokio::task::LocalSet::new();
    local.block_on(&rt, async move {
        let mut runtime = JsRuntime::new(RuntimeOptions {
            module_loader: Some(Rc::new(BeaterModuleLoader)),
            extensions: vec![beater_ext::init()],
            ..Default::default()
        });
        runtime
            .execute_script("beater:bootstrap", include_str!("bootstrap.js"))
            .map_err(|e| anyhow::anyhow!(format_js_error(&e)))?;

        let specifier = deno_core::ModuleSpecifier::from_file_path(&agent_file)
            .map_err(|_| anyhow::anyhow!("bad agent path {}", agent_file.display()))?;
        let mod_id = runtime
            .load_main_es_module(&specifier)
            .await
            .with_context(|| format!("loading {}", agent_file.display()))?;
        let eval = runtime.mod_evaluate(mod_id);
        runtime
            .run_event_loop(PollEventLoopOptions::default())
            .await?;
        eval.await?;

        let namespace = runtime.get_module_namespace(mod_id)?;
        deno_core::scope!(scope, runtime);
        let namespace = v8::Local::new(scope, namespace);
        let key = v8::String::new(scope, "default").expect("static str");
        let default = namespace
            .get(scope, key.into())
            .filter(|v| !v.is_undefined())
            .with_context(|| {
                format!(
                    "{} has no default export (defineAgent)",
                    agent_file.display()
                )
            })?;
        let config: serde_json::Value = deno_core::serde_v8::from_v8(scope, default)
            .context("agent config is not plain JSON (defineAgent output)")?;
        Ok(config)
    })
}
