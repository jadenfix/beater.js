use std::collections::BTreeMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{TimeZone, Utc};
use serde_json::{Value, json};

use crate::journal::{Journal, RunRow, StepRow};

const EXPORT_URL_ENV: &str = "BEATER_TRACE_EXPORT_URL";
const API_KEY_ENV: &str = "BEATER_API_KEY";
const TENANT_ENV: &str = "BEATER_TENANT_ID";
const PROJECT_ENV: &str = "BEATER_PROJECT_ID";
const ENVIRONMENT_ENV: &str = "BEATER_ENVIRONMENT_ID";
const DEFAULT_TENANT: &str = "demo";
const DEFAULT_PROJECT: &str = "beater-js";
const DEFAULT_ENVIRONMENT: &str = "local";
const EXPORT_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
struct TraceExportConfig {
    base_url: String,
    api_key: Option<String>,
    tenant_id: String,
    project_id: String,
    environment_id: String,
}

impl TraceExportConfig {
    fn from_env() -> Option<Self> {
        let base_url = std::env::var(EXPORT_URL_ENV).ok()?.trim().to_string();
        if base_url.is_empty() {
            return None;
        }
        Some(Self {
            base_url: base_url.trim_end_matches('/').to_string(),
            api_key: non_empty_env(API_KEY_ENV),
            tenant_id: non_empty_env(TENANT_ENV).unwrap_or_else(|| DEFAULT_TENANT.to_string()),
            project_id: non_empty_env(PROJECT_ENV).unwrap_or_else(|| DEFAULT_PROJECT.to_string()),
            environment_id: non_empty_env(ENVIRONMENT_ENV)
                .unwrap_or_else(|| DEFAULT_ENVIRONMENT.to_string()),
        })
    }

    fn ingest_url(&self) -> String {
        format!("{}/v1/traces/native", self.base_url)
    }
}

pub fn export_run_if_configured(app_dir: &Path, run_id: &str) -> Result<()> {
    let Some(config) = TraceExportConfig::from_env() else {
        return Ok(());
    };
    let journal = Journal::open(app_dir)?;
    let run = journal.run(run_id)?;
    let steps = journal.steps(run_id)?;
    let spans = native_spans(&config, &run, &steps);
    let client = reqwest::blocking::Client::builder()
        .timeout(EXPORT_TIMEOUT)
        .build()?;
    for span in spans {
        let mut request = client.post(config.ingest_url()).json(&span);
        if let Some(api_key) = &config.api_key {
            request = request.header("x-beater-api-key", api_key);
        }
        let response = request.send().context("export run trace to Beater")?;
        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().unwrap_or_default();
            anyhow::bail!("Beater trace export failed with {status}: {body}");
        }
    }
    Ok(())
}

fn native_spans(config: &TraceExportConfig, run: &RunRow, steps: &[StepRow]) -> Vec<Value> {
    let trace_id = trace_id(&run.id);
    let mut spans = Vec::with_capacity(steps.len() + 1);
    spans.push(native_span(
        config,
        &trace_id,
        "run",
        None,
        1,
        "agent.run",
        &format!("beater.js agent run {}", run.agent),
        status(&run.status),
        run.created_at,
        Some(run.updated_at),
        Some(json!(run.input)),
        None,
        BTreeMap::from([
            ("beater.run_id".to_string(), json!(run.id)),
            ("beater.agent".to_string(), json!(run.agent)),
            ("beater.run_status".to_string(), json!(run.status)),
        ]),
        Some(format!("beater-js:{}:run", run.id)),
    ));
    for step in steps {
        spans.push(native_span(
            config,
            &trace_id,
            &format!("step-{}", step.seq),
            Some("run"),
            (step.seq as u64).saturating_add(1),
            step_kind(&step.kind),
            &step_name(step),
            status(&step.status),
            run.created_at,
            Some(run.updated_at),
            Some(step.request.clone()),
            step.result.clone(),
            BTreeMap::from([
                ("beater.run_id".to_string(), json!(run.id)),
                ("beater.step_seq".to_string(), json!(step.seq)),
                ("beater.step_kind".to_string(), json!(step.kind)),
                ("beater.step_status".to_string(), json!(step.status)),
                ("beater.step_attempt".to_string(), json!(step.attempt)),
                ("beater.tool_name".to_string(), json!(step.tool_name)),
                ("beater.tool_use_id".to_string(), json!(step.tool_use_id)),
            ]),
            Some(format!("beater-js:{}:step:{}", run.id, step.seq)),
        ));
    }
    spans
}

#[allow(clippy::too_many_arguments)]
fn native_span(
    config: &TraceExportConfig,
    trace_id: &str,
    span_id: &str,
    parent_span_id: Option<&str>,
    seq: u64,
    kind: &str,
    name: &str,
    status: &str,
    start_time: i64,
    end_time: Option<i64>,
    input: Option<Value>,
    output: Option<Value>,
    attributes: BTreeMap<String, Value>,
    idempotency_key: Option<String>,
) -> Value {
    json!({
        "scope": {
            "tenant_id": config.tenant_id,
            "project_id": config.project_id,
            "environment_id": config.environment_id,
        },
        "trace_id": trace_id,
        "span_id": span_id,
        "parent_span_id": parent_span_id,
        "seq": seq,
        "kind": kind,
        "name": name,
        "status": status,
        "start_time": timestamp(start_time),
        "end_time": end_time.map(timestamp),
        "model": serde_json::Value::Null,
        "cost": serde_json::Value::Null,
        "tokens": serde_json::Value::Null,
        "input": input,
        "output": output,
        "attributes": attributes,
        "redaction_class": "internal",
        "idempotency_key": idempotency_key,
        "auth_context": serde_json::Value::Null,
    })
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn timestamp(seconds: i64) -> String {
    Utc.timestamp_opt(seconds, 0)
        .single()
        .unwrap_or_else(Utc::now)
        .to_rfc3339()
}

fn trace_id(run_id: &str) -> String {
    format!("beater-js-{run_id}")
}

fn step_kind(kind: &str) -> &'static str {
    match kind {
        "llm_call" => "llm.call",
        "tool_call" => "tool.call",
        _ => "agent.step",
    }
}

fn step_name(step: &StepRow) -> String {
    match step.kind.as_str() {
        "llm_call" => "beater.js llm call".to_string(),
        "tool_call" => step
            .tool_name
            .as_ref()
            .map(|name| format!("beater.js tool {name}"))
            .unwrap_or_else(|| "beater.js tool call".to_string()),
        _ => format!("beater.js step {}", step.kind),
    }
}

fn status(status: &str) -> &'static str {
    match status {
        "completed" => "ok",
        "failed" | "needs_review" => "error",
        _ => "unset",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::Journal;
    use std::fs;
    use std::path::{Path, PathBuf};

    struct TempDir {
        path: PathBuf,
    }

    impl TempDir {
        fn new(name: &str) -> Self {
            let path = std::env::temp_dir().join(format!(
                "beater-trace-export-{name}-{}-{}",
                std::process::id(),
                chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
            ));
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn path(&self) -> &Path {
            &self.path
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    #[test]
    fn native_spans_project_journal_run_and_steps() {
        let app = TempDir::new("spans");
        let journal = Journal::open(app.path()).unwrap();
        journal.create_run("run-1", "support", "hello").unwrap();
        let llm = journal
            .start_step("run-1", "llm_call", &json!({"messages": []}), None, None, 1)
            .unwrap();
        journal
            .complete_step("run-1", llm, &json!({"stop_reason": "tool_use"}))
            .unwrap();
        let tool = journal
            .start_step(
                "run-1",
                "tool_call",
                &json!({"name": "get_time"}),
                Some("get_time"),
                Some("toolu_1"),
                1,
            )
            .unwrap();
        journal.fail_step("run-1", tool, "network error").unwrap();
        journal.set_run_status("run-1", "failed").unwrap();

        let run = journal.run("run-1").unwrap();
        let steps = journal.steps("run-1").unwrap();
        let spans = native_spans(
            &TraceExportConfig {
                base_url: "http://127.0.0.1:8080".to_string(),
                api_key: None,
                tenant_id: "tenant".to_string(),
                project_id: "project".to_string(),
                environment_id: "prod".to_string(),
            },
            &run,
            &steps,
        );

        assert_eq!(spans.len(), 3);
        assert_eq!(spans[0]["kind"], "agent.run");
        assert_eq!(spans[0]["status"], "error");
        assert_eq!(spans[1]["kind"], "llm.call");
        assert_eq!(spans[1]["parent_span_id"], "run");
        assert_eq!(spans[2]["kind"], "tool.call");
        assert_eq!(spans[2]["name"], "beater.js tool get_time");
        assert_eq!(spans[2]["attributes"]["beater.tool_use_id"], "toolu_1");
        assert_eq!(spans[2]["idempotency_key"], "beater-js:run-1:step:2");
    }
}
