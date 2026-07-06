use std::collections::BTreeMap;
use std::hash::{Hash, Hasher};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use chrono::{TimeZone, Utc};
use reqwest::header::{HeaderName, HeaderValue};
use serde_json::{Value, json};

use crate::journal::{Journal, RunRow, StepRow};

const EXPORT_URL_ENV: &str = "BEATER_TRACE_EXPORT_URL";
const API_KEY_ENV: &str = "BEATER_API_KEY";
const TENANT_ENV: &str = "BEATER_TENANT_ID";
const PROJECT_ENV: &str = "BEATER_PROJECT_ID";
const ENVIRONMENT_ENV: &str = "BEATER_ENVIRONMENT_ID";
const OTLP_EXPORT_URL_ENV: &str = "BEATER_OTLP_EXPORT_URL";
const OTEL_OTLP_ENDPOINT_ENV: &str = "OTEL_EXPORTER_OTLP_ENDPOINT";
const OTEL_OTLP_TRACES_ENDPOINT_ENV: &str = "OTEL_EXPORTER_OTLP_TRACES_ENDPOINT";
const OTEL_OTLP_HEADERS_ENV: &str = "OTEL_EXPORTER_OTLP_HEADERS";
const OTEL_OTLP_TRACES_HEADERS_ENV: &str = "OTEL_EXPORTER_OTLP_TRACES_HEADERS";
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

#[derive(Debug, Clone, PartialEq, Eq)]
struct OtlpExportConfig {
    traces_url: String,
    headers: BTreeMap<String, String>,
    tenant_id: String,
    project_id: String,
    environment_id: String,
}

impl OtlpExportConfig {
    fn from_env() -> Option<Self> {
        let traces_url = otlp_traces_url_from_env()?;
        let mut headers = parse_otlp_headers();
        if let Some(api_key) = non_empty_env(API_KEY_ENV) {
            headers
                .entry("x-beater-api-key".to_string())
                .or_insert(api_key);
        }
        Some(Self {
            traces_url,
            headers,
            tenant_id: non_empty_env(TENANT_ENV).unwrap_or_else(|| DEFAULT_TENANT.to_string()),
            project_id: non_empty_env(PROJECT_ENV).unwrap_or_else(|| DEFAULT_PROJECT.to_string()),
            environment_id: non_empty_env(ENVIRONMENT_ENV)
                .unwrap_or_else(|| DEFAULT_ENVIRONMENT.to_string()),
        })
    }
}

pub fn export_run_if_configured(app_dir: &Path, run_id: &str) -> Result<()> {
    let native_config = TraceExportConfig::from_env();
    let otlp_config = OtlpExportConfig::from_env();
    if native_config.is_none() && otlp_config.is_none() {
        return Ok(());
    }
    let journal = Journal::open(app_dir)?;
    let run = journal.run(run_id)?;
    let steps = journal.steps(run_id)?;
    let client = reqwest::blocking::Client::builder()
        .timeout(EXPORT_TIMEOUT)
        .build()?;

    if let Some(config) = native_config {
        let spans = native_spans(&config, &run, &steps);
        export_native_spans(&client, &config, spans)?;
    }
    if let Some(config) = otlp_config {
        export_otlp_trace(&client, &config, &run, &steps)?;
    }
    Ok(())
}

fn export_native_spans(
    client: &reqwest::blocking::Client,
    config: &TraceExportConfig,
    spans: Vec<Value>,
) -> Result<()> {
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

fn export_otlp_trace(
    client: &reqwest::blocking::Client,
    config: &OtlpExportConfig,
    run: &RunRow,
    steps: &[StepRow],
) -> Result<()> {
    let payload = otlp_payload(config, run, steps);
    let mut request = client.post(&config.traces_url).json(&payload);
    for (name, value) in &config.headers {
        request = request.header(
            HeaderName::from_bytes(name.as_bytes())
                .with_context(|| format!("invalid OTLP header name {name:?}"))?,
            HeaderValue::from_str(value)
                .with_context(|| format!("invalid OTLP header value for {name:?}"))?,
        );
    }
    let response = request.send().context("export run trace to OTLP")?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().unwrap_or_default();
        anyhow::bail!("OTLP trace export failed with {status}: {body}");
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

fn otlp_payload(config: &OtlpExportConfig, run: &RunRow, steps: &[StepRow]) -> Value {
    json!({
        "resourceSpans": [{
            "resource": {
                "attributes": otlp_attrs(BTreeMap::from([
                    ("service.name".to_string(), json!("beater.js")),
                    ("service.namespace".to_string(), json!("beater")),
                    ("service.version".to_string(), json!(env!("CARGO_PKG_VERSION"))),
                    ("deployment.environment.name".to_string(), json!(config.environment_id)),
                    ("beater.tenant_id".to_string(), json!(config.tenant_id)),
                    ("beater.project_id".to_string(), json!(config.project_id)),
                ])),
            },
            "scopeSpans": [{
                "scope": {
                    "name": "beater-agent",
                    "version": env!("CARGO_PKG_VERSION"),
                },
                "spans": otlp_spans(run, steps),
            }],
        }],
    })
}

fn otlp_spans(run: &RunRow, steps: &[StepRow]) -> Vec<Value> {
    let trace_id = otlp_trace_id(&run.id);
    let run_span_id = otlp_span_id(&run.id, "run");
    let mut spans = Vec::with_capacity(steps.len() + 1);
    spans.push(otlp_span(
        &trace_id,
        &run_span_id,
        None,
        "beater.js agent run",
        "agent.run",
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
    ));
    for step in steps {
        let step_span_id = otlp_span_id(&run.id, &format!("step:{}", step.seq));
        spans.push(otlp_span(
            &trace_id,
            &step_span_id,
            Some(&run_span_id),
            &step_name(step),
            step_kind(&step.kind),
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
        ));
    }
    spans
}

#[allow(clippy::too_many_arguments)]
fn otlp_span(
    trace_id: &str,
    span_id: &str,
    parent_span_id: Option<&str>,
    name: &str,
    kind: &str,
    status: &str,
    start_time: i64,
    end_time: Option<i64>,
    input: Option<Value>,
    output: Option<Value>,
    attributes: BTreeMap<String, Value>,
) -> Value {
    let mut span = json!({
        "traceId": trace_id,
        "spanId": span_id,
        "name": name,
        "kind": "SPAN_KIND_INTERNAL",
        "startTimeUnixNano": timestamp_nanos(start_time),
        "endTimeUnixNano": timestamp_nanos(end_time.unwrap_or(start_time)),
        "attributes": otlp_attrs(attributes),
        "status": {
            "code": otlp_status_code(status),
        },
    });
    if let Some(parent_span_id) = parent_span_id {
        span["parentSpanId"] = json!(parent_span_id);
    }
    let mut events = Vec::new();
    if let Some(input) = input {
        events.push(otlp_payload_event("beater.input", start_time, input));
    }
    if let Some(output) = output {
        events.push(otlp_payload_event(
            "beater.output",
            end_time.unwrap_or(start_time),
            output,
        ));
    }
    if !events.is_empty() {
        span["events"] = json!(events);
    }
    span["attributes"]
        .as_array_mut()
        .expect("OTLP attributes are an array")
        .push(otlp_attr("beater.span_kind", json!(kind)));
    span
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

fn otlp_payload_event(name: &str, timestamp: i64, payload: Value) -> Value {
    json!({
        "timeUnixNano": timestamp_nanos(timestamp),
        "name": name,
        "attributes": [otlp_attr("beater.payload_json", payload)],
    })
}

fn otlp_attrs(attributes: BTreeMap<String, Value>) -> Vec<Value> {
    attributes
        .into_iter()
        .filter(|(_, value)| !value.is_null())
        .map(|(key, value)| otlp_attr(&key, value))
        .collect()
}

fn otlp_attr(key: &str, value: Value) -> Value {
    json!({
        "key": key,
        "value": otlp_any_value(value),
    })
}

fn otlp_any_value(value: Value) -> Value {
    match value {
        Value::Bool(value) => json!({"boolValue": value}),
        Value::Number(value) => {
            if let Some(value) = value.as_i64() {
                json!({"intValue": value.to_string()})
            } else if let Some(value) = value.as_u64() {
                json!({"intValue": value.to_string()})
            } else if let Some(value) = value.as_f64() {
                json!({"doubleValue": value})
            } else {
                json!({"stringValue": value.to_string()})
            }
        }
        Value::String(value) => json!({"stringValue": value}),
        other => json!({"stringValue": other.to_string()}),
    }
}

fn otlp_status_code(status: &str) -> &'static str {
    match status {
        "ok" => "STATUS_CODE_OK",
        "error" => "STATUS_CODE_ERROR",
        _ => "STATUS_CODE_UNSET",
    }
}

fn non_empty_env(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn otlp_traces_url_from_env() -> Option<String> {
    if let Some(url) = non_empty_env(OTLP_EXPORT_URL_ENV) {
        return Some(with_otlp_traces_path(&url));
    }
    if let Some(url) = non_empty_env(OTEL_OTLP_TRACES_ENDPOINT_ENV) {
        return Some(url.trim_end_matches('/').to_string());
    }
    non_empty_env(OTEL_OTLP_ENDPOINT_ENV).map(|url| with_otlp_traces_path(&url))
}

fn with_otlp_traces_path(url: &str) -> String {
    let url = url.trim_end_matches('/');
    if url.ends_with("/v1/traces") {
        url.to_string()
    } else {
        format!("{url}/v1/traces")
    }
}

fn parse_otlp_headers() -> BTreeMap<String, String> {
    let mut headers = BTreeMap::new();
    for name in [OTEL_OTLP_HEADERS_ENV, OTEL_OTLP_TRACES_HEADERS_ENV] {
        if let Some(value) = non_empty_env(name) {
            for pair in value.split(',') {
                let Some((header, value)) = pair.split_once('=') else {
                    continue;
                };
                let header = header.trim();
                let value = value.trim();
                if !header.is_empty() && !value.is_empty() {
                    headers.insert(header.to_ascii_lowercase(), value.to_string());
                }
            }
        }
    }
    headers
}

fn timestamp(seconds: i64) -> String {
    Utc.timestamp_opt(seconds, 0)
        .single()
        .unwrap_or_else(Utc::now)
        .to_rfc3339()
}

fn timestamp_nanos(seconds: i64) -> String {
    seconds.saturating_mul(1_000_000_000).to_string()
}

fn trace_id(run_id: &str) -> String {
    format!("beater-js-{run_id}")
}

fn otlp_trace_id(run_id: &str) -> String {
    format!(
        "{:016x}{:016x}",
        stable_hash("trace-a", run_id),
        stable_hash("trace-b", run_id)
    )
}

fn otlp_span_id(run_id: &str, span_key: &str) -> String {
    format!("{:016x}", stable_hash(run_id, span_key))
}

fn stable_hash(left: &str, right: &str) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    left.hash(&mut hasher);
    right.hash(&mut hasher);
    hasher.finish()
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

    #[test]
    fn otlp_payload_projects_journal_run_and_steps() {
        let app = TempDir::new("otlp-spans");
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
        let payload = otlp_payload(
            &OtlpExportConfig {
                traces_url: "http://127.0.0.1:4318/v1/traces".to_string(),
                headers: BTreeMap::new(),
                tenant_id: "tenant".to_string(),
                project_id: "project".to_string(),
                environment_id: "prod".to_string(),
            },
            &run,
            &steps,
        );

        let resource_spans = payload["resourceSpans"].as_array().unwrap();
        assert_eq!(resource_spans.len(), 1);
        let resource_attrs = resource_spans[0]["resource"]["attributes"]
            .as_array()
            .unwrap();
        assert!(resource_attrs.iter().any(|attr| {
            attr["key"] == "service.name" && attr["value"]["stringValue"] == "beater.js"
        }));
        assert!(resource_attrs.iter().any(|attr| {
            attr["key"] == "beater.project_id" && attr["value"]["stringValue"] == "project"
        }));

        let spans = resource_spans[0]["scopeSpans"][0]["spans"]
            .as_array()
            .unwrap();
        assert_eq!(spans.len(), 3);
        assert_eq!(
            spans[0]["traceId"].as_str().unwrap().len(),
            32,
            "OTLP trace id must be hex encoded 16 bytes"
        );
        assert_eq!(
            spans[0]["spanId"].as_str().unwrap().len(),
            16,
            "OTLP span id must be hex encoded 8 bytes"
        );
        assert_eq!(spans[0]["name"], "beater.js agent run");
        assert_eq!(spans[0]["status"]["code"], "STATUS_CODE_ERROR");
        assert_eq!(spans[1]["parentSpanId"], spans[0]["spanId"]);
        assert_eq!(spans[2]["name"], "beater.js tool get_time");
        assert!(
            spans[2]["attributes"]
                .as_array()
                .unwrap()
                .iter()
                .any(|attr| {
                    attr["key"] == "beater.tool_use_id" && attr["value"]["stringValue"] == "toolu_1"
                })
        );
        assert!(spans[2]["events"].as_array().unwrap().iter().any(|event| {
            event["name"] == "beater.output"
                && event["attributes"][0]["value"]["stringValue"]
                    .as_str()
                    .unwrap()
                    .contains("network error")
        }));
    }

    #[test]
    fn otlp_export_config_accepts_beater_and_standard_endpoints() {
        assert_eq!(
            with_otlp_traces_path("http://127.0.0.1:4318"),
            "http://127.0.0.1:4318/v1/traces"
        );
        assert_eq!(
            with_otlp_traces_path("http://127.0.0.1:4318/v1/traces"),
            "http://127.0.0.1:4318/v1/traces"
        );
    }
}
