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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use serde_json::json;

    use super::load_agent_config;

    struct TempApp {
        path: PathBuf,
    }

    impl TempApp {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "beater-agent-config-{name}-{}-{}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ));
            fs::create_dir_all(path.join("agents/operator")).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempApp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn remote_mcp_tool_serializes_network_contract() {
        let app = TempApp::new("remote-mcp");
        fs::write(
            app.path().join("agents/operator/agent.ts"),
            r#"
import { defineAgent, remoteMcpTool } from "beater:agent";

export default defineAgent({
  name: "operator",
  tools: [
    remoteMcpTool("crm.lookup", {
      endpoint: "http://127.0.0.1:9000/mcp",
      tool: "lookup_contact",
      description: "Look up a CRM contact.",
      inputSchema: {
        type: "object",
        properties: {email: {type: "string"}},
        required: ["email"],
      },
      auth: {type: "bearer", env: "CRM_MCP_TOKEN"},
      timeoutMs: 5000,
      retry: {attempts: 2, backoffMs: 25, idempotencyKey: "tool_use_id"},
      session: {scope: "run", cleanup: "always"},
      egress: ["127.0.0.1:9000"],
      idempotent: false,
    }),
  ],
});
"#,
        )
        .unwrap();

        let config = load_agent_config(app.path(), "operator").unwrap();
        assert_eq!(
            config["tools"][0],
            json!({
                "kind": "remote_mcp",
                "name": "crm.lookup",
                "idempotent": false,
                "description": "Look up a CRM contact.",
                "inputSchema": {
                    "type": "object",
                    "properties": {"email": {"type": "string"}},
                    "required": ["email"]
                },
                "endpoint": "http://127.0.0.1:9000/mcp",
                "tool": "lookup_contact",
                "auth": {"type": "bearer", "env": "CRM_MCP_TOKEN"},
                "timeoutMs": 5000,
                "retry": {"attempts": 2, "backoffMs": 25, "idempotencyKey": "tool_use_id"},
                "session": {"scope": "run", "cleanup": "always"},
                "egress": ["127.0.0.1:9000"]
            })
        );
    }

    #[test]
    fn browser_tool_serializes_session_contract() {
        let app = TempApp::new("browser-tool");
        fs::write(
            app.path().join("agents/operator/agent.ts"),
            r#"
import { browserTool, defineAgent } from "beater:agent";

export default defineAgent({
  name: "operator",
  tools: [
    browserTool("browser.checkout", {
      provider: "mock_cdp",
      description: "Verify checkout in a browser.",
      inputSchema: {
        type: "object",
        properties: {
          url: {type: "string"},
          task: {type: "string"},
        },
        required: ["url", "task"],
      },
      session: {scope: "run", cleanup: "always"},
      allowedOrigins: ["https://shop.example"],
      timeoutMs: 5000,
      idempotent: false,
    }),
  ],
});
"#,
        )
        .unwrap();

        let config = load_agent_config(app.path(), "operator").unwrap();
        assert_eq!(
            config["tools"][0],
            json!({
                "kind": "browser",
                "name": "browser.checkout",
                "idempotent": false,
                "provider": "mock_cdp",
                "description": "Verify checkout in a browser.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "url": {"type": "string"},
                        "task": {"type": "string"}
                    },
                    "required": ["url", "task"]
                },
                "session": {"scope": "run", "cleanup": "always"},
                "allowedOrigins": ["https://shop.example"],
                "timeoutMs": 5000,
                "secrets": {}
            })
        );
    }

    #[test]
    fn wasmtime_tool_serializes_hermetic_contract() {
        let app = TempApp::new("wasmtime-tool");
        fs::write(
            app.path().join("agents/operator/agent.ts"),
            r#"
import { defineAgent, wasmtimeTool } from "beater:agent";

export default defineAgent({
  name: "operator",
  tools: [
    wasmtimeTool("double_wasm", {
      source: {
        kind: "wasm_wat",
        text: "(module (func (export \"run\") (param i64) (result i64) local.get 0))",
      },
      description: "Double an integer in a hermetic wasm sandbox.",
      inputSchema: {
        type: "object",
        properties: {n: {type: "integer"}},
        required: ["n"],
      },
      policy: {
        limits: {wall_ms: 1000, memory_bytes: 1048576, fuel: 100000},
      },
      idempotent: true,
    }),
  ],
});
"#,
        )
        .unwrap();

        let config = load_agent_config(app.path(), "operator").unwrap();
        assert_eq!(
            config["tools"][0],
            json!({
                "kind": "wasmtime",
                "name": "double_wasm",
                "idempotent": true,
                "source": {
                    "kind": "wasm_wat",
                    "text": "(module (func (export \"run\") (param i64) (result i64) local.get 0))"
                },
                "description": "Double an integer in a hermetic wasm sandbox.",
                "inputSchema": {
                    "type": "object",
                    "properties": {"n": {"type": "integer"}},
                    "required": ["n"]
                },
                "policy": {
                    "limits": {"wall_ms": 1000, "memory_bytes": 1048576, "fuel": 100000}
                }
            })
        );
    }
}
