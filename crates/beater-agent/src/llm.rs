//! Model-provider adapter for the journaled agent loop.
//!
//! The runner keeps one canonical internal wire shape: Anthropic Messages-style
//! requests/responses with `tool_use` and `tool_result` blocks. Provider
//! adapters translate at the boundary so resume, journaling, tools, and traces
//! do not fork by model vendor.

use std::collections::HashMap;
use std::net::IpAddr;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use serde_json::{Value, json};

use crate::anthropic::Anthropic;
use crate::registry::AgentConfig;

const DEFAULT_NVIDIA_BASE_URL: &str = "https://integrate.api.nvidia.com/v1";
const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(600);
const SUPPORTED_PROVIDER_HINT: &str = "anthropic, openai-compatible, nvidia (OpenAI-compatible)";

pub struct LlmSelection {
    pub provider: String,
    pub model: String,
}

impl LlmSelection {
    pub fn from_config(config: &AgentConfig) -> Result<Self> {
        let generic_provider_env = env_non_empty("BEATER_LLM_API_KEY").is_some()
            || env_non_empty("BEATER_LLM_BASE_URL").is_some();
        let env_provider = env_non_empty("BEATER_LLM_PROVIDER");
        let config_provider = config_non_empty(&config.provider);
        let provider = match env_provider.clone() {
            Some(provider) => provider,
            None if generic_provider_env => {
                bail!(
                    "BEATER_LLM_PROVIDER is required when using BEATER_LLM_API_KEY or BEATER_LLM_BASE_URL"
                )
            }
            None => config_provider.clone().context(
                "LLM provider is required; set provider in agent.ts or BEATER_LLM_PROVIDER",
            )?,
        };
        let provider = normalize_provider(&provider);
        let _canonical = match canonical_provider(&provider) {
            Some(canonical) => canonical,
            None => bail!(
                "unsupported LLM provider {provider:?}; supported providers: {SUPPORTED_PROVIDER_HINT}"
            ),
        };
        let config_model = config_non_empty(&config.model);
        let env_model = env_non_empty("BEATER_LLM_MODEL");
        let model = match env_model {
            Some(model) => model,
            None if env_provider.is_some() => {
                bail!("BEATER_LLM_MODEL is required when BEATER_LLM_PROVIDER is set")
            }
            None => config_model
                .context("LLM model is required; set model in agent.ts or BEATER_LLM_MODEL")?,
        };
        Ok(Self { provider, model })
    }
}

pub enum LlmClient {
    Anthropic(Anthropic),
    OpenAiCompatible(OpenAiCompatible),
}

impl LlmClient {
    pub fn from_provider(provider: &str) -> Result<Self> {
        let normalized = normalize_provider(provider);
        let canonical = match canonical_provider(&normalized) {
            Some(provider) => provider,
            None => bail!(
                "unsupported LLM provider {provider:?}; supported providers: {SUPPORTED_PROVIDER_HINT}"
            ),
        };
        match canonical {
            "anthropic" => Ok(Self::Anthropic(Anthropic::from_env()?)),
            "openai-compatible" => Ok(Self::OpenAiCompatible(OpenAiCompatible::from_env(
                &normalized,
            )?)),
            _ => unreachable!("canonical provider set is exhaustive"),
        }
    }

    pub async fn create_message_streaming(
        &self,
        body: &Value,
        on_partial: impl FnMut(&Value) -> Result<()>,
    ) -> Result<Value> {
        match self {
            Self::Anthropic(client) => client.create_message_streaming(body, on_partial).await,
            Self::OpenAiCompatible(client) => {
                client.create_message_streaming(body, on_partial).await
            }
        }
    }
}

fn normalize_provider(provider: &str) -> String {
    provider.trim().to_ascii_lowercase().replace('_', "-")
}

fn canonical_provider(provider: &str) -> Option<&'static str> {
    match normalize_provider(provider).as_str() {
        "anthropic" => Some("anthropic"),
        "openai" | "openai-compatible" | "nvidia" | "nvidia-nim" => Some("openai-compatible"),
        _ => None,
    }
}

fn env_non_empty(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|value| !value.is_empty())
}

fn config_non_empty(value: &Option<String>) -> Option<String> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

pub struct OpenAiCompatible {
    http: reqwest::Client,
    api_key: String,
    chat_completions_url: String,
}

impl OpenAiCompatible {
    fn from_env(provider: &str) -> Result<Self> {
        let is_nvidia = matches!(
            normalize_provider(provider).as_str(),
            "nvidia" | "nvidia-nim"
        );
        let api_key = if is_nvidia {
            let generic = env_non_empty("BEATER_LLM_API_KEY");
            let nvidia_specific =
                env_non_empty("BEATER_NVIDIA_API_KEY").or_else(|| env_non_empty("NVIDIA_API_KEY"));
            if generic.is_some() && nvidia_specific.is_some() {
                bail!(
                    "ambiguous NVIDIA key configuration: set either BEATER_LLM_API_KEY or BEATER_NVIDIA_API_KEY/NVIDIA_API_KEY, not both"
                );
            }
            nvidia_specific.or(generic).context(
                "BEATER_LLM_API_KEY, BEATER_NVIDIA_API_KEY, or NVIDIA_API_KEY is not set for provider nvidia",
            )?
        } else {
            env_non_empty("BEATER_LLM_API_KEY")
                .or_else(|| env_non_empty("BEATER_OPENAI_API_KEY"))
                .or_else(|| env_non_empty("OPENAI_API_KEY"))
                .context(
                    "BEATER_LLM_API_KEY, BEATER_OPENAI_API_KEY, or OPENAI_API_KEY is not set for provider openai-compatible",
                )?
        };
        let base_url = if is_nvidia {
            let generic = env_non_empty("BEATER_LLM_BASE_URL");
            let nvidia_specific = env_non_empty("BEATER_NVIDIA_BASE_URL")
                .or_else(|| env_non_empty("NVIDIA_BASE_URL"));
            if generic.is_some() && nvidia_specific.is_some() {
                bail!(
                    "ambiguous NVIDIA base URL configuration: set either BEATER_LLM_BASE_URL or BEATER_NVIDIA_BASE_URL/NVIDIA_BASE_URL, not both"
                );
            }
            nvidia_specific
                .or(generic)
                .unwrap_or_else(|| DEFAULT_NVIDIA_BASE_URL.to_string())
        } else {
            env_non_empty("BEATER_LLM_BASE_URL")
                .or_else(|| env_non_empty("BEATER_OPENAI_BASE_URL"))
                .or_else(|| env_non_empty("OPENAI_BASE_URL"))
                .unwrap_or_else(|| DEFAULT_OPENAI_BASE_URL.to_string())
        };
        let allow_custom = env_flag("BEATER_OPENAI_ALLOW_CUSTOM_BASE_URL")
            || (is_nvidia && env_flag("BEATER_NVIDIA_ALLOW_CUSTOM_BASE_URL"));
        let allow_insecure_loopback = env_flag("BEATER_OPENAI_ALLOW_INSECURE_LOOPBACK")
            || (is_nvidia && env_flag("BEATER_NVIDIA_ALLOW_INSECURE_LOOPBACK"));
        let trusted_https_hosts = if is_nvidia {
            &["integrate.api.nvidia.com"][..]
        } else {
            &["api.openai.com"][..]
        };
        Self::new(
            api_key,
            &base_url,
            DEFAULT_REQUEST_TIMEOUT,
            allow_custom,
            allow_insecure_loopback,
            trusted_https_hosts,
        )
    }

    fn new(
        api_key: String,
        base_url: &str,
        request_timeout: Duration,
        allow_custom: bool,
        allow_insecure_loopback: bool,
        trusted_https_hosts: &[&str],
    ) -> Result<Self> {
        let chat_completions_url = chat_completions_url(base_url);
        validate_openai_base_url(
            &chat_completions_url,
            allow_custom,
            allow_insecure_loopback,
            trusted_https_hosts,
        )?;
        let http = reqwest::Client::builder()
            .timeout(request_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .context("building openai-compatible http client")?;
        Ok(Self {
            http,
            api_key,
            chat_completions_url,
        })
    }

    async fn create_message_streaming(
        &self,
        body: &Value,
        mut on_partial: impl FnMut(&Value) -> Result<()>,
    ) -> Result<Value> {
        let names = ToolNameMap::from_body(body)?;
        let request = openai_request_body(body, &names)?;
        let resp = self
            .http
            .post(&self.chat_completions_url)
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()
            .await
            .context("sending openai-compatible streaming request")?;
        let status = resp.status();
        if !status.is_success() {
            let body_len = resp.text().await.map(|body| body.len()).unwrap_or_default();
            bail!(
                "openai-compatible api error {status}; response body omitted from journal to avoid leaking provider-returned secrets ({body_len} bytes)"
            );
        }

        let mut decoder = SseDecoder::default();
        let fallback_tool_id_prefix = openai_fallback_tool_id_prefix(&request);
        let mut assembler =
            OpenAiStreamAssembler::new(names.provider_to_original, fallback_tool_id_prefix);
        let mut saw_done = false;
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("reading openai-compatible streaming response body")?;
            for event in decoder.push(&chunk)? {
                handle_openai_event(event, &mut assembler, &mut saw_done, &mut on_partial)?;
            }
        }
        for event in decoder.finish()? {
            handle_openai_event(event, &mut assembler, &mut saw_done, &mut on_partial)?;
        }
        assembler.finish(body["model"].clone(), saw_done)
    }
}

fn chat_completions_url(base_url: &str) -> String {
    let base_url = base_url.trim_end_matches('/');
    if base_url.ends_with("/chat/completions") {
        base_url.to_string()
    } else {
        format!("{base_url}/chat/completions")
    }
}

fn validate_openai_base_url(
    url: &str,
    allow_custom: bool,
    allow_insecure_loopback: bool,
    trusted_https_hosts: &[&str],
) -> Result<()> {
    let url = reqwest::Url::parse(url).context("OpenAI-compatible base URL is invalid")?;
    if !url.username().is_empty() || url.password().is_some() {
        bail!("OpenAI-compatible base URL must not contain credentials");
    }
    let host = url
        .host_str()
        .context("OpenAI-compatible base URL must include a host")?;
    if url.scheme() == "https"
        && trusted_https_hosts
            .iter()
            .any(|trusted| host.eq_ignore_ascii_case(trusted))
    {
        return Ok(());
    }
    if url.scheme() == "https" {
        if allow_custom {
            return Ok(());
        }
        bail!(
            "custom OpenAI-compatible base URL {host:?} requires BEATER_OPENAI_ALLOW_CUSTOM_BASE_URL=1"
        );
    }
    if url.scheme() == "http" && is_loopback_host(host) && allow_insecure_loopback {
        return Ok(());
    }
    bail!(
        "OpenAI-compatible base URL must use https, or http loopback with BEATER_OPENAI_ALLOW_INSECURE_LOOPBACK=1"
    );
}

fn env_flag(name: &str) -> bool {
    std::env::var(name)
        .map(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "YES"))
        .unwrap_or(false)
}

fn is_loopback_host(host: &str) -> bool {
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<IpAddr>()
            .map(|addr| addr.is_loopback())
            .unwrap_or(false)
}

fn openai_request_body(body: &Value, names: &ToolNameMap) -> Result<Value> {
    let mut request = json!({
        "model": body["model"],
        "stream": true,
        "messages": openai_messages(body, names)?,
    });
    if let Some(max_tokens) = body.get("max_tokens") {
        request["max_tokens"] = max_tokens.clone();
    }
    if let Some(tools) = body["tools"].as_array()
        && !tools.is_empty()
    {
        request["tools"] = Value::Array(
            tools
                .iter()
                .map(|tool| {
                    json!({
                        "type": "function",
                        "function": {
                            "name": names.provider_name(tool["name"].as_str().unwrap_or_default()),
                            "description": tool["description"],
                            "parameters": tool["input_schema"],
                        }
                    })
                })
                .collect(),
        );
        request["tool_choice"] = Value::String("auto".to_string());
    }
    Ok(request)
}

fn openai_messages(body: &Value, names: &ToolNameMap) -> Result<Vec<Value>> {
    let mut messages = Vec::new();
    if let Some(system) = body["system"].as_str()
        && !system.is_empty()
    {
        messages.push(json!({"role": "system", "content": system}));
    }
    for message in body["messages"]
        .as_array()
        .context("canonical LLM request missing messages[]")?
    {
        append_openai_message(message, names, &mut messages)?;
    }
    Ok(messages)
}

fn append_openai_message(message: &Value, names: &ToolNameMap, out: &mut Vec<Value>) -> Result<()> {
    let role = message["role"].as_str().unwrap_or("user");
    match message.get("content") {
        Some(Value::String(content)) => out.push(json!({"role": role, "content": content})),
        Some(Value::Array(blocks)) if role == "assistant" => {
            let mut text = String::new();
            let mut tool_calls = Vec::new();
            for block in blocks {
                match block["type"].as_str().unwrap_or_default() {
                    "text" => text.push_str(block["text"].as_str().unwrap_or_default()),
                    "tool_use" => {
                        tool_calls.push(json!({
                            "id": block["id"].as_str().unwrap_or_default(),
                            "type": "function",
                            "function": {
                                "name": names.provider_name(block["name"].as_str().unwrap_or_default()),
                                "arguments": serde_json::to_string(&block["input"])
                                    .context("serializing tool_use input for openai-compatible message")?,
                            }
                        }));
                    }
                    _ => {}
                }
            }
            if tool_calls.is_empty() {
                out.push(json!({"role": "assistant", "content": text}));
            } else {
                out.push(json!({
                    "role": "assistant",
                    "content": if text.is_empty() { Value::Null } else { Value::String(text) },
                    "tool_calls": tool_calls,
                }));
            }
        }
        Some(Value::Array(blocks)) => {
            let mut text = String::new();
            for block in blocks {
                match block["type"].as_str().unwrap_or_default() {
                    "text" => text.push_str(block["text"].as_str().unwrap_or_default()),
                    "tool_result" => out.push(json!({
                        "role": "tool",
                        "tool_call_id": block["tool_use_id"].as_str().unwrap_or_default(),
                        "content": block["content"].as_str().unwrap_or_default(),
                    })),
                    _ => {}
                }
            }
            if !text.is_empty() {
                out.push(json!({"role": role, "content": text}));
            }
        }
        _ => out.push(json!({"role": role, "content": ""})),
    }
    Ok(())
}

#[derive(Default)]
struct SseDecoder {
    pending: Vec<u8>,
    event: Option<String>,
    data: Vec<String>,
}

impl SseDecoder {
    fn push(&mut self, chunk: &[u8]) -> Result<Vec<SseEvent>> {
        self.pending.extend_from_slice(chunk);
        let mut events = Vec::new();
        while let Some(pos) = self.pending.iter().position(|b| *b == b'\n') {
            let mut line = self.pending.drain(..=pos).collect::<Vec<_>>();
            if line.last() == Some(&b'\n') {
                line.pop();
            }
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if let Some(event) = self.push_line(String::from_utf8(line)?)? {
                events.push(event);
            }
        }
        Ok(events)
    }

    fn finish(mut self) -> Result<Vec<SseEvent>> {
        let mut events = Vec::new();
        if !self.pending.is_empty() {
            let line = String::from_utf8(std::mem::take(&mut self.pending))?;
            if let Some(event) = self.push_line(line.trim_end_matches('\r').to_string())? {
                events.push(event);
            }
        }
        if let Some(event) = self.flush() {
            events.push(event);
        }
        Ok(events)
    }

    fn push_line(&mut self, line: String) -> Result<Option<SseEvent>> {
        if line.is_empty() {
            return Ok(self.flush());
        }
        if line.starts_with(':') {
            return Ok(None);
        }
        let (field, value) = line
            .split_once(':')
            .map(|(field, value)| (field, value.strip_prefix(' ').unwrap_or(value)))
            .unwrap_or((line.as_str(), ""));
        match field {
            "event" => self.event = Some(value.to_string()),
            "data" => self.data.push(value.to_string()),
            _ => {}
        }
        Ok(None)
    }

    fn flush(&mut self) -> Option<SseEvent> {
        if self.event.is_none() && self.data.is_empty() {
            return None;
        }
        Some(SseEvent {
            event: self.event.take().unwrap_or_else(|| "message".to_string()),
            data: std::mem::take(&mut self.data).join("\n"),
        })
    }
}

struct SseEvent {
    event: String,
    data: String,
}

impl SseEvent {
    fn json_data(&self, provider: &str) -> Result<Value> {
        serde_json::from_str::<Value>(&self.data)
            .with_context(|| format!("{provider} stream event {:?} was not JSON", self.event))
    }
}

fn handle_openai_event(
    event: SseEvent,
    assembler: &mut OpenAiStreamAssembler,
    saw_done: &mut bool,
    on_partial: &mut impl FnMut(&Value) -> Result<()>,
) -> Result<()> {
    if event.data.trim() == "[DONE]" {
        *saw_done = true;
        return Ok(());
    }
    if *saw_done {
        bail!("openai-compatible stream sent data after [DONE]");
    }
    let chunk = event.json_data("openai-compatible")?;
    if event.event == "error" || chunk.get("error").is_some() {
        bail!(
            "openai-compatible stream error event; payload omitted from journal to avoid leaking provider-returned secrets"
        );
    }
    let partial = json!({"event": "openai_chunk", "data": chunk});
    on_partial(&partial)?;
    assembler.apply(&partial["data"])
}

struct OpenAiStreamAssembler {
    id: Option<String>,
    model: Option<String>,
    text: String,
    tool_calls: Vec<Option<ToolCallState>>,
    finish_reason: Option<String>,
    provider_to_original: HashMap<String, String>,
    fallback_tool_id_prefix: String,
}

#[derive(Default)]
struct ToolCallState {
    id: Option<String>,
    name: String,
    arguments: String,
}

impl OpenAiStreamAssembler {
    fn new(provider_to_original: HashMap<String, String>, fallback_tool_id_prefix: String) -> Self {
        Self {
            id: None,
            model: None,
            text: String::new(),
            tool_calls: Vec::new(),
            finish_reason: None,
            provider_to_original,
            fallback_tool_id_prefix,
        }
    }

    fn apply(&mut self, chunk: &Value) -> Result<()> {
        if self.id.is_none() {
            self.id = chunk["id"].as_str().map(ToString::to_string);
        }
        if self.model.is_none() {
            self.model = chunk["model"].as_str().map(ToString::to_string);
        }
        for choice in chunk["choices"].as_array().into_iter().flatten() {
            let delta = &choice["delta"];
            if let Some(content) = delta["content"].as_str() {
                self.text.push_str(content);
            }
            for call in delta["tool_calls"].as_array().into_iter().flatten() {
                let index = call["index"].as_u64().unwrap_or(0) as usize;
                while self.tool_calls.len() <= index {
                    self.tool_calls.push(None);
                }
                let state = self.tool_calls[index].get_or_insert_with(ToolCallState::default);
                if let Some(id) = call["id"].as_str()
                    && !id.is_empty()
                {
                    state.id = Some(id.to_string());
                }
                if let Some(name) = call["function"]["name"].as_str() {
                    state.name.push_str(name);
                }
                if let Some(arguments) = call["function"]["arguments"].as_str() {
                    state.arguments.push_str(arguments);
                }
            }
            if delta["function_call"].is_object() {
                if self.tool_calls.is_empty() {
                    self.tool_calls.push(None);
                }
                let state = self.tool_calls[0].get_or_insert_with(ToolCallState::default);
                if let Some(name) = delta["function_call"]["name"].as_str() {
                    state.name.push_str(name);
                }
                if let Some(arguments) = delta["function_call"]["arguments"].as_str() {
                    state.arguments.push_str(arguments);
                }
            }
            if let Some(reason) = choice["finish_reason"].as_str()
                && !reason.is_empty()
            {
                self.finish_reason = Some(reason.to_string());
            }
        }
        Ok(())
    }

    fn finish(self, request_model: Value, saw_done: bool) -> Result<Value> {
        if !saw_done {
            bail!("openai-compatible stream ended before [DONE]");
        }
        let mut content = Vec::new();
        if !self.text.is_empty() {
            content.push(json!({"type": "text", "text": self.text}));
        }
        let fallback_tool_id_prefix = &self.fallback_tool_id_prefix;
        for (index, call) in self.tool_calls.into_iter().enumerate() {
            let Some(call) = call else {
                continue;
            };
            let provider_name = call.name;
            let name = self
                .provider_to_original
                .get(&provider_name)
                .cloned()
                .unwrap_or(provider_name);
            let arguments = call.arguments.trim();
            let input = if arguments.is_empty() {
                json!({})
            } else {
                serde_json::from_str(arguments).with_context(|| {
                    format!("openai-compatible tool call {name} arguments did not form JSON")
                })?
            };
            content.push(json!({
                "type": "tool_use",
                "id": call.id.unwrap_or_else(|| format!("{fallback_tool_id_prefix}_{index}")),
                "name": name,
                "input": input,
            }));
        }
        let finish_reason = self
            .finish_reason
            .as_deref()
            .context("openai-compatible stream ended without terminal finish_reason")?;
        let stop_reason = match finish_reason {
            "tool_calls" | "function_call" => "tool_use",
            "stop" => {
                if content.iter().any(|block| block["type"] == "tool_use") {
                    "tool_use"
                } else {
                    "end_turn"
                }
            }
            "length" => "max_tokens",
            "content_filter" => "refusal",
            other => other,
        };
        Ok(json!({
            "id": self.id.unwrap_or_else(|| "chatcmpl_openai_compatible".to_string()),
            "type": "message",
            "role": "assistant",
            "model": self.model.map(Value::String).unwrap_or(request_model),
            "content": content,
            "stop_reason": stop_reason,
        }))
    }
}

#[derive(Debug)]
struct ToolNameMap {
    original_to_provider: HashMap<String, String>,
    provider_to_original: HashMap<String, String>,
}

impl ToolNameMap {
    fn from_body(body: &Value) -> Result<Self> {
        let mut original_to_provider = HashMap::new();
        let mut provider_to_original = HashMap::new();
        for tool in body["tools"].as_array().into_iter().flatten() {
            let Some(original) = tool["name"].as_str() else {
                continue;
            };
            let provider = openai_tool_name(original);
            if let Some(existing) = provider_to_original.get(&provider)
                && existing != original
            {
                bail!(
                    "OpenAI-compatible tool name collision after sanitization: {existing:?} and {original:?} both map to {provider:?}"
                );
            }
            original_to_provider.insert(original.to_string(), provider.clone());
            provider_to_original.insert(provider, original.to_string());
        }
        Ok(Self {
            original_to_provider,
            provider_to_original,
        })
    }

    fn provider_name(&self, original: &str) -> String {
        self.original_to_provider
            .get(original)
            .cloned()
            .unwrap_or_else(|| openai_tool_name(original))
    }
}

fn openai_tool_name(original: &str) -> String {
    let mut sanitized = original
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect::<String>();
    if sanitized.is_empty() {
        sanitized.push_str("tool");
    }
    let changed = sanitized != original || sanitized.len() > 64;
    if !changed {
        return sanitized;
    }
    let hash = fnv1a64(original.as_bytes());
    let suffix = format!("__{hash:08x}", hash = (hash & 0xffff_ffff));
    let keep = 64usize.saturating_sub(suffix.len());
    sanitized.truncate(keep);
    format!("{sanitized}{suffix}")
}

fn openai_fallback_tool_id_prefix(request: &Value) -> String {
    let encoded = serde_json::to_vec(request).unwrap_or_else(|_| request.to_string().into_bytes());
    let hash = fnv1a64(&encoded);
    format!("toolu_openai_{hash:016x}")
}

fn fnv1a64(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

#[cfg(test)]
mod tests {
    use super::{
        LlmSelection, OpenAiCompatible, OpenAiStreamAssembler, SseEvent, ToolNameMap,
        canonical_provider, handle_openai_event, openai_fallback_tool_id_prefix,
        openai_request_body, openai_tool_name, validate_openai_base_url,
    };
    use crate::registry::AgentConfig;
    use std::sync::Mutex;

    static ENV_LOCK: Mutex<()> = Mutex::new(());

    struct EnvGuard {
        name: &'static str,
        previous: Option<String>,
    }

    impl EnvGuard {
        fn set(name: &'static str, value: &str) -> Self {
            let previous = std::env::var(name).ok();
            unsafe {
                std::env::set_var(name, value);
            }
            Self { name, previous }
        }

        fn unset(name: &'static str) -> Self {
            let previous = std::env::var(name).ok();
            unsafe {
                std::env::remove_var(name);
            }
            Self { name, previous }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            unsafe {
                match &self.previous {
                    Some(previous) => std::env::set_var(self.name, previous),
                    None => std::env::remove_var(self.name),
                }
            }
        }
    }

    fn fixture_config() -> AgentConfig {
        AgentConfig {
            name: "support".to_string(),
            provider: Some("anthropic".to_string()),
            model: Some("claude-fixture".to_string()),
            system: String::new(),
            tools: Vec::new(),
        }
    }

    #[test]
    fn generic_llm_env_requires_explicit_provider_override() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _key = EnvGuard::set("BEATER_LLM_API_KEY", "fixture-key");
        let _base = EnvGuard::unset("BEATER_LLM_BASE_URL");
        let _provider = EnvGuard::unset("BEATER_LLM_PROVIDER");

        let err = match LlmSelection::from_config(&fixture_config()) {
            Ok(_) => panic!("generic LLM env should require an explicit provider override"),
            Err(err) => err,
        };

        assert!(
            err.to_string().contains("BEATER_LLM_PROVIDER is required"),
            "{err:#}"
        );
    }

    #[test]
    fn generic_llm_env_accepts_explicit_provider_override() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _key = EnvGuard::set("BEATER_LLM_API_KEY", "fixture-key");
        let _provider = EnvGuard::set("BEATER_LLM_PROVIDER", "openai-compatible");
        let _model = EnvGuard::set("BEATER_LLM_MODEL", "provider-model");

        let selection = LlmSelection::from_config(&fixture_config()).unwrap();

        assert_eq!(selection.provider, "openai-compatible");
        assert_eq!(selection.model, "provider-model");
    }

    #[test]
    fn missing_provider_requires_agent_or_env() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _key = EnvGuard::unset("BEATER_LLM_API_KEY");
        let _base = EnvGuard::unset("BEATER_LLM_BASE_URL");
        let _provider = EnvGuard::unset("BEATER_LLM_PROVIDER");
        let _model = EnvGuard::unset("BEATER_LLM_MODEL");
        let mut config = fixture_config();
        config.provider = None;

        let error = match LlmSelection::from_config(&config) {
            Ok(_) => panic!("missing provider should fail"),
            Err(error) => error,
        };

        assert!(
            error.to_string().contains("LLM provider is required"),
            "{error:#}"
        );
    }

    #[test]
    fn missing_model_requires_agent_or_env() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _key = EnvGuard::unset("BEATER_LLM_API_KEY");
        let _base = EnvGuard::unset("BEATER_LLM_BASE_URL");
        let _provider = EnvGuard::unset("BEATER_LLM_PROVIDER");
        let _model = EnvGuard::unset("BEATER_LLM_MODEL");
        let mut config = fixture_config();
        config.model = None;

        let error = match LlmSelection::from_config(&config) {
            Ok(_) => panic!("missing model should fail"),
            Err(error) => error,
        };

        assert!(
            error.to_string().contains("LLM model is required"),
            "{error:#}"
        );
    }

    #[test]
    fn provider_env_override_requires_matching_model_override() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _key = EnvGuard::unset("BEATER_LLM_API_KEY");
        let _base = EnvGuard::unset("BEATER_LLM_BASE_URL");
        let _provider = EnvGuard::set("BEATER_LLM_PROVIDER", "nvidia");
        let _model = EnvGuard::unset("BEATER_LLM_MODEL");

        let error = match LlmSelection::from_config(&fixture_config()) {
            Ok(_) => panic!("provider override should require model override"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("BEATER_LLM_MODEL"), "{error:#}");
    }

    #[test]
    fn provider_env_override_never_reuses_agent_model() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _key = EnvGuard::unset("BEATER_LLM_API_KEY");
        let _base = EnvGuard::set("BEATER_LLM_BASE_URL", "https://integrate.api.nvidia.com/v1");
        let _provider = EnvGuard::set("BEATER_LLM_PROVIDER", "openai-compatible");
        let _model = EnvGuard::unset("BEATER_LLM_MODEL");
        let mut config = fixture_config();
        config.provider = Some("openai-compatible".to_string());
        config.model = Some("stale-provider-model".to_string());

        let error = match LlmSelection::from_config(&config) {
            Ok(_) => panic!("provider override should require model override"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("BEATER_LLM_MODEL"), "{error:#}");
    }

    #[test]
    fn env_can_fully_select_provider_and_model_without_agent_defaults() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _key = EnvGuard::unset("BEATER_LLM_API_KEY");
        let _base = EnvGuard::unset("BEATER_LLM_BASE_URL");
        let _provider = EnvGuard::set("BEATER_LLM_PROVIDER", "nvidia");
        let _model = EnvGuard::set("BEATER_LLM_MODEL", "provider-model");
        let mut config = fixture_config();
        config.provider = None;
        config.model = None;

        let selection = LlmSelection::from_config(&config).unwrap();

        assert_eq!(selection.provider, "nvidia");
        assert_eq!(selection.model, "provider-model");
    }

    #[test]
    fn provider_aliases_canonicalize_to_protocol_adapters() {
        assert_eq!(canonical_provider("anthropic"), Some("anthropic"));
        assert_eq!(canonical_provider("openai"), Some("openai-compatible"));
        assert_eq!(
            canonical_provider("openai_compatible"),
            Some("openai-compatible")
        );
        assert_eq!(canonical_provider("NVIDIA"), Some("openai-compatible"));
        assert_eq!(canonical_provider("nvidia-nim"), Some("openai-compatible"));
        assert_eq!(canonical_provider("messages"), None);
    }

    #[test]
    fn generic_llm_env_accepts_nvidia_provider_alias() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _key = EnvGuard::set("BEATER_LLM_API_KEY", "fixture-key");
        let _provider = EnvGuard::set("BEATER_LLM_PROVIDER", "nvidia");
        let _model = EnvGuard::set("BEATER_LLM_MODEL", "provider-model");

        let selection = LlmSelection::from_config(&fixture_config()).unwrap();

        assert_eq!(selection.provider, "nvidia");
        assert_eq!(selection.model, "provider-model");
    }

    #[test]
    fn empty_generic_llm_env_is_ignored() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _key = EnvGuard::set("BEATER_LLM_API_KEY", "");
        let _base = EnvGuard::set("BEATER_LLM_BASE_URL", "");
        let _provider = EnvGuard::unset("BEATER_LLM_PROVIDER");
        let _model = EnvGuard::set("BEATER_LLM_MODEL", "");

        let selection = LlmSelection::from_config(&fixture_config()).unwrap();

        assert_eq!(selection.provider, "anthropic");
        assert_eq!(selection.model, "claude-fixture");
    }
    use serde_json::json;
    use std::collections::HashMap;

    #[test]
    fn openai_tool_names_preserve_valid_names_and_sanitize_provider_names() {
        assert_eq!(openai_tool_name("summarize_numbers"), "summarize_numbers");
        let sanitized = openai_tool_name("crm.lookup/customer");
        assert!(
            sanitized.starts_with("crm_lookup_customer__"),
            "{sanitized}"
        );
        assert!(sanitized.len() <= 64, "{sanitized}");
    }

    #[test]
    fn openai_request_translates_canonical_tools_and_tool_results() {
        let body = json!({
            "model": "open-model",
            "max_tokens": 100,
            "system": "be useful",
            "tools": [{
                "name": "crm.lookup",
                "description": "Lookup CRM.",
                "input_schema": {"type": "object", "properties": {}}
            }],
            "messages": [
                {"role": "user", "content": "lookup"},
                {"role": "assistant", "content": [{
                    "type": "tool_use",
                    "id": "call_1",
                    "name": "crm.lookup",
                    "input": {"id": 7}
                }]},
                {"role": "user", "content": [{
                    "type": "tool_result",
                    "tool_use_id": "call_1",
                    "content": "{\"ok\":true}"
                }]}
            ]
        });
        let names = ToolNameMap::from_body(&body).unwrap();
        let request = openai_request_body(&body, &names).unwrap();
        assert_eq!(request["messages"][0]["role"], "system");
        assert_eq!(request["messages"][2]["tool_calls"][0]["id"], "call_1");
        assert_eq!(request["messages"][3]["role"], "tool");
        assert_eq!(
            request["tools"][0]["function"]["name"],
            names.provider_name("crm.lookup")
        );
    }

    #[test]
    fn openai_tool_name_collisions_are_rejected() {
        let generated = openai_tool_name("crm.lookup/customer");
        let body = json!({
            "tools": [
                {"name": "crm.lookup/customer"},
                {"name": generated.clone()}
            ]
        });
        let error = ToolNameMap::from_body(&body).unwrap_err();
        assert!(
            format!("{error:#}").contains("tool name collision"),
            "{error:#}"
        );
    }

    #[test]
    fn openai_fallback_tool_id_prefix_is_request_scoped_and_stable() {
        let first = json!({
            "model": "mock",
            "stream": true,
            "messages": [{"role": "user", "content": "first"}]
        });
        let first_again = json!({
            "model": "mock",
            "stream": true,
            "messages": [{"role": "user", "content": "first"}]
        });
        let second = json!({
            "model": "mock",
            "stream": true,
            "messages": [{"role": "user", "content": "second"}]
        });

        assert_eq!(
            openai_fallback_tool_id_prefix(&first),
            openai_fallback_tool_id_prefix(&first_again)
        );
        assert_ne!(
            openai_fallback_tool_id_prefix(&first),
            openai_fallback_tool_id_prefix(&second)
        );
    }

    #[test]
    fn openai_stream_requires_terminal_finish_reason() {
        let mut assembler = OpenAiStreamAssembler::new(HashMap::new(), "toolu_test".to_string());
        assembler
            .apply(&json!({
                "id": "chatcmpl_1",
                "model": "mock",
                "choices": [{"delta": {"content": "partial"}, "finish_reason": null}]
            }))
            .unwrap();
        let error = assembler.finish(json!("mock"), true).unwrap_err();
        assert!(
            format!("{error:#}").contains("without terminal finish_reason"),
            "{error:#}"
        );
    }

    #[test]
    fn openai_stream_requires_done_frame() {
        let mut assembler = OpenAiStreamAssembler::new(HashMap::new(), "toolu_test".to_string());
        assembler
            .apply(&json!({
                "id": "chatcmpl_1",
                "model": "mock",
                "choices": [{"delta": {"content": "done"}, "finish_reason": "stop"}]
            }))
            .unwrap();
        let error = assembler.finish(json!("mock"), false).unwrap_err();
        assert!(format!("{error:#}").contains("before [DONE]"), "{error:#}");
    }

    #[test]
    fn openai_stream_rejects_data_after_done() {
        let mut assembler = OpenAiStreamAssembler::new(HashMap::new(), "toolu_test".to_string());
        let mut saw_done = false;
        let mut partials = Vec::new();
        let error = {
            let mut on_partial = |partial: &serde_json::Value| {
                partials.push(partial.clone());
                Ok(())
            };
            handle_openai_event(
                SseEvent {
                    event: "message".to_string(),
                    data: "[DONE]".to_string(),
                },
                &mut assembler,
                &mut saw_done,
                &mut on_partial,
            )
            .unwrap();
            handle_openai_event(
                SseEvent {
                    event: "message".to_string(),
                    data: json!({
                        "choices": [{"delta": {"content": "late"}, "finish_reason": "stop"}]
                    })
                    .to_string(),
                },
                &mut assembler,
                &mut saw_done,
                &mut on_partial,
            )
            .unwrap_err()
        };
        assert!(format!("{error:#}").contains("after [DONE]"), "{error:#}");
        assert!(partials.is_empty(), "{partials:?}");
    }

    #[test]
    fn openai_stream_error_events_are_not_journaled() {
        let provider_secret = format!("{}{}", "nv", "api-test-fixture");
        let mut assembler = OpenAiStreamAssembler::new(HashMap::new(), "toolu_test".to_string());
        let mut saw_done = false;
        let mut partials = Vec::new();
        let error = {
            let mut on_partial = |partial: &serde_json::Value| {
                partials.push(partial.clone());
                Ok(())
            };
            handle_openai_event(
                SseEvent {
                    event: "error".to_string(),
                    data: json!({"error": {"message": format!("echoed {provider_secret}")}})
                        .to_string(),
                },
                &mut assembler,
                &mut saw_done,
                &mut on_partial,
            )
            .unwrap_err()
        };
        let message = format!("{error:#}");
        assert!(message.contains("payload omitted"), "{message}");
        assert!(!message.contains(&provider_secret), "{message}");
        assert!(partials.is_empty(), "{partials:?}");
    }

    #[test]
    fn openai_base_url_requires_secure_or_explicit_fixture_origin() {
        validate_openai_base_url(
            "https://api.openai.com/v1/chat/completions",
            false,
            false,
            &["api.openai.com"],
        )
        .unwrap();
        validate_openai_base_url(
            "https://integrate.api.nvidia.com/v1/chat/completions",
            true,
            false,
            &["api.openai.com"],
        )
        .unwrap();
        validate_openai_base_url(
            "https://integrate.api.nvidia.com/v1/chat/completions",
            false,
            false,
            &["integrate.api.nvidia.com"],
        )
        .unwrap();
        assert!(
            validate_openai_base_url(
                "https://integrate.api.nvidia.com/v1/chat/completions",
                false,
                false,
                &["api.openai.com"],
            )
            .is_err()
        );
        assert!(
            validate_openai_base_url(
                "https://api.openai.com/v1/chat/completions",
                false,
                false,
                &["integrate.api.nvidia.com"],
            )
            .is_err()
        );
        validate_openai_base_url(
            "http://127.0.0.1:8080/v1/chat/completions",
            false,
            true,
            &["api.openai.com"],
        )
        .unwrap();
        assert!(
            validate_openai_base_url(
                "http://example.com/v1/chat/completions",
                true,
                true,
                &["api.openai.com"],
            )
            .is_err()
        );
    }

    #[test]
    fn nvidia_provider_uses_nvidia_key_alias_and_pinned_default_origin() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _generic_key = EnvGuard::unset("BEATER_LLM_API_KEY");
        let _openai_key = EnvGuard::unset("BEATER_OPENAI_API_KEY");
        let _openai_key_legacy = EnvGuard::unset("OPENAI_API_KEY");
        let _nvidia_key = EnvGuard::set("BEATER_NVIDIA_API_KEY", "fixture-key");
        let _generic_base = EnvGuard::unset("BEATER_LLM_BASE_URL");
        let _nvidia_base = EnvGuard::unset("BEATER_NVIDIA_BASE_URL");
        let _nvidia_base_legacy = EnvGuard::unset("NVIDIA_BASE_URL");
        let _openai_base = EnvGuard::unset("BEATER_OPENAI_BASE_URL");
        let _openai_base_legacy = EnvGuard::unset("OPENAI_BASE_URL");

        let client = OpenAiCompatible::from_env("nvidia").unwrap();

        assert_eq!(
            client.chat_completions_url,
            "https://integrate.api.nvidia.com/v1/chat/completions"
        );
    }

    #[test]
    fn nvidia_provider_does_not_use_openai_key_aliases() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _generic_key = EnvGuard::unset("BEATER_LLM_API_KEY");
        let _nvidia_key = EnvGuard::unset("BEATER_NVIDIA_API_KEY");
        let _nvidia_key_legacy = EnvGuard::unset("NVIDIA_API_KEY");
        let _openai_key = EnvGuard::set("OPENAI_API_KEY", "fixture-openai-key");

        let error = match OpenAiCompatible::from_env("nvidia") {
            Ok(_) => panic!("nvidia provider should not use OPENAI_API_KEY"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("provider nvidia"), "{error:#}");
    }

    #[test]
    fn nvidia_provider_rejects_mixed_generic_and_specific_key_aliases() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _generic_key = EnvGuard::set("BEATER_LLM_API_KEY", "fixture-generic-key");
        let _nvidia_key = EnvGuard::set("NVIDIA_API_KEY", "fixture-nvidia-key");

        let error = match OpenAiCompatible::from_env("nvidia") {
            Ok(_) => panic!("nvidia provider should reject ambiguous key aliases"),
            Err(error) => error,
        };

        assert!(
            error.to_string().contains("ambiguous NVIDIA key"),
            "{error:#}"
        );
    }

    #[test]
    fn nvidia_provider_rejects_mixed_generic_and_specific_base_urls() {
        let _lock = ENV_LOCK.lock().unwrap();
        let _generic_key = EnvGuard::unset("BEATER_LLM_API_KEY");
        let _openai_key = EnvGuard::unset("BEATER_OPENAI_API_KEY");
        let _openai_key_legacy = EnvGuard::unset("OPENAI_API_KEY");
        let _nvidia_key = EnvGuard::set("NVIDIA_API_KEY", "fixture-nvidia-key");
        let _generic_base = EnvGuard::set("BEATER_LLM_BASE_URL", "https://api.openai.com/v1");
        let _nvidia_base = EnvGuard::set("NVIDIA_BASE_URL", "https://integrate.api.nvidia.com/v1");

        let error = match OpenAiCompatible::from_env("nvidia") {
            Ok(_) => panic!("nvidia provider should reject ambiguous base URL aliases"),
            Err(error) => error,
        };

        assert!(
            error.to_string().contains("ambiguous NVIDIA base URL"),
            "{error:#}"
        );
    }

    #[test]
    fn openai_stream_translates_legacy_function_call_delta() {
        let mut assembler = OpenAiStreamAssembler::new(HashMap::new(), "toolu_test".to_string());
        assembler
            .apply(&json!({
                "id": "chatcmpl_1",
                "model": "mock",
                "choices": [{
                    "delta": {"function_call": {"name": "summarize_numbers", "arguments": "{\"numbers\":[1,2]}"}},
                    "finish_reason": "function_call"
                }]
            }))
            .unwrap();
        let message = assembler.finish(json!("mock"), true).unwrap();
        assert_eq!(message["stop_reason"], "tool_use");
        assert_eq!(message["content"][0]["type"], "tool_use");
        assert_eq!(message["content"][0]["name"], "summarize_numbers");
        assert_eq!(message["content"][0]["input"]["numbers"][1], 2);
    }

    #[test]
    fn openai_fallback_tool_ids_are_unique_across_responses() {
        let first_id = fallback_tool_id_for_omitted_provider_id("toolu_openai_request_a");
        let second_id = fallback_tool_id_for_omitted_provider_id("toolu_openai_request_b");
        assert_ne!(first_id, second_id);
    }

    fn fallback_tool_id_for_omitted_provider_id(prefix: &str) -> String {
        let mut assembler = OpenAiStreamAssembler::new(HashMap::new(), prefix.to_string());
        assembler
            .apply(&json!({
                "model": "mock",
                "choices": [{
                    "delta": {
                        "tool_calls": [{
                            "index": 0,
                            "function": {
                                "name": "summarize_numbers",
                                "arguments": "{\"numbers\":[1]}"
                            }
                        }]
                    },
                    "finish_reason": "tool_calls"
                }]
            }))
            .unwrap();
        assembler.finish(json!("mock"), true).unwrap()["content"][0]["id"]
            .as_str()
            .unwrap()
            .to_string()
    }
}
