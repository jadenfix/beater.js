//! Minimal Messages API client over raw reqwest (there is no official Rust
//! SDK). Each request is exactly one journaled step; streaming requests can
//! additionally emit durable partial records before the final message lands.

use anyhow::{Context, Result, bail};
use futures_util::StreamExt;
use serde_json::Value;
use std::net::IpAddr;
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

pub struct Anthropic {
    http: reqwest::Client,
    api_key: String,
    messages_url: String,
}

impl Anthropic {
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .context("ANTHROPIC_API_KEY is not set — required for `beater agent run`")?;
        let base_url =
            std::env::var("ANTHROPIC_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        validate_anthropic_base_url(
            &messages_url(&base_url),
            env_flag("BEATER_ANTHROPIC_ALLOW_CUSTOM_BASE_URL"),
            env_flag("BEATER_ANTHROPIC_ALLOW_INSECURE_LOOPBACK"),
        )?;
        Self::new(api_key, &base_url, DEFAULT_REQUEST_TIMEOUT)
    }

    fn new(api_key: String, base_url: &str, request_timeout: Duration) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(request_timeout)
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .build()
            .context("building anthropic http client")?;
        Ok(Self {
            http,
            api_key,
            messages_url: messages_url(base_url),
        })
    }

    pub async fn create_message_streaming(
        &self,
        body: &Value,
        mut on_partial: impl FnMut(&Value) -> Result<()>,
    ) -> Result<Value> {
        let mut body = body.clone();
        body.as_object_mut()
            .context("anthropic request body must be a JSON object")?
            .insert("stream".to_string(), Value::Bool(true));
        let resp = self
            .http
            .post(&self.messages_url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(&body)
            .send()
            .await
            .context("sending anthropic streaming request")?;
        let status = resp.status();
        if !status.is_success() {
            let body_len = resp.text().await.map(|body| body.len()).unwrap_or_default();
            bail!(
                "anthropic api error {status}; response body omitted from journal to avoid leaking provider-returned secrets ({body_len} bytes)"
            );
        }

        let mut decoder = SseDecoder::default();
        let mut assembler = MessageStreamAssembler::default();
        let mut stream = resp.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.context("reading anthropic streaming response body")?;
            for event in decoder.push(&chunk)? {
                handle_anthropic_event(event, &mut assembler, &mut on_partial)?;
            }
        }
        for event in decoder.finish()? {
            handle_anthropic_event(event, &mut assembler, &mut on_partial)?;
        }
        assembler.finish()
    }
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
    fn as_partial(&self) -> Result<Value> {
        Ok(serde_json::json!({
            "event": self.event,
            "data": serde_json::from_str::<Value>(&self.data)
                .with_context(|| format!("anthropic stream event {:?} was not JSON", self.event))?,
        }))
    }
}

fn handle_anthropic_event(
    event: SseEvent,
    assembler: &mut MessageStreamAssembler,
    on_partial: &mut impl FnMut(&Value) -> Result<()>,
) -> Result<()> {
    if event.data.trim() == "[DONE]" {
        return Ok(());
    }
    let partial = event.as_partial()?;
    if partial["data"]["type"].as_str() == Some("error") {
        return assembler.apply(&partial["data"]);
    }
    on_partial(&partial)?;
    assembler.apply(&partial["data"])
}

#[derive(Default)]
struct MessageStreamAssembler {
    message: Option<Value>,
    content: Vec<Option<ContentBlockState>>,
    saw_message_stop: bool,
}

#[derive(Default)]
struct ContentBlockState {
    value: Value,
    partial_json: String,
}

impl MessageStreamAssembler {
    fn apply(&mut self, event: &Value) -> Result<()> {
        match event["type"].as_str().unwrap_or_default() {
            "message_start" => {
                self.message = Some(event["message"].clone());
            }
            "content_block_start" => {
                let index = event["index"]
                    .as_u64()
                    .context("content_block_start missing index")?
                    as usize;
                self.ensure_content_index(index);
                self.content[index] = Some(ContentBlockState {
                    value: event["content_block"].clone(),
                    partial_json: String::new(),
                });
            }
            "content_block_delta" => {
                let index = event["index"]
                    .as_u64()
                    .context("content_block_delta missing index")?
                    as usize;
                self.ensure_content_index(index);
                let block = self.content[index]
                    .as_mut()
                    .context("content_block_delta arrived before content_block_start")?;
                let delta = &event["delta"];
                match delta["type"].as_str().unwrap_or_default() {
                    "text_delta" => {
                        let next = delta["text"].as_str().unwrap_or_default();
                        let text = block.value["text"].as_str().unwrap_or_default().to_string();
                        block.value["text"] = Value::String(format!("{text}{next}"));
                    }
                    "input_json_delta" => {
                        block
                            .partial_json
                            .push_str(delta["partial_json"].as_str().unwrap_or_default());
                    }
                    "thinking_delta" => {
                        let next = delta["thinking"].as_str().unwrap_or_default();
                        let thinking = block.value["thinking"]
                            .as_str()
                            .unwrap_or_default()
                            .to_string();
                        block.value["thinking"] = Value::String(format!("{thinking}{next}"));
                    }
                    "signature_delta" => {
                        if let Some(signature) = delta["signature"].as_str() {
                            block.value["signature"] = Value::String(signature.to_string());
                        }
                    }
                    _ => {}
                }
            }
            "content_block_stop" => {
                let index = event["index"]
                    .as_u64()
                    .context("content_block_stop missing index")?
                    as usize;
                if let Some(Some(block)) = self.content.get_mut(index)
                    && block.value["type"] == "tool_use"
                    && !block.partial_json.is_empty()
                {
                    block.value["input"] = serde_json::from_str(&block.partial_json)
                        .context("tool_use input_json_delta did not form JSON")?;
                }
            }
            "message_delta" => {
                let message = self.message.get_or_insert_with(
                    || serde_json::json!({"type": "message", "role": "assistant", "content": []}),
                );
                if event["delta"].get("stop_reason").is_some() {
                    message["stop_reason"] = event["delta"]["stop_reason"].clone();
                }
                if event["delta"].get("stop_sequence").is_some() {
                    message["stop_sequence"] = event["delta"]["stop_sequence"].clone();
                }
                if event.get("usage").is_some() {
                    merge_object_field(message, "usage", &event["usage"]);
                }
            }
            "message_stop" => {
                self.saw_message_stop = true;
            }
            "error" => bail!(
                "anthropic stream error event; payload omitted from journal to avoid leaking provider-returned secrets"
            ),
            _ => {}
        }
        Ok(())
    }

    fn finish(mut self) -> Result<Value> {
        let mut message = self
            .message
            .take()
            .context("anthropic stream ended before message_start")?;
        if !self.saw_message_stop {
            bail!("anthropic stream ended before message_stop");
        }
        message["content"] = Value::Array(
            self.content
                .into_iter()
                .flatten()
                .map(|block| block.value)
                .collect(),
        );
        Ok(message)
    }

    fn ensure_content_index(&mut self, index: usize) {
        while self.content.len() <= index {
            self.content.push(None);
        }
    }
}

fn merge_object_field(target: &mut Value, field: &str, update: &Value) {
    if !target[field].is_object() {
        target[field] = serde_json::json!({});
    }
    if let (Some(existing), Some(update)) = (target[field].as_object_mut(), update.as_object()) {
        for (key, value) in update {
            existing.insert(key.clone(), value.clone());
        }
    }
}

fn messages_url(base_url: &str) -> String {
    let base_url = base_url.trim_end_matches('/');
    if base_url.ends_with("/v1/messages") {
        base_url.to_string()
    } else {
        format!("{base_url}/v1/messages")
    }
}

fn validate_anthropic_base_url(
    url: &str,
    allow_custom: bool,
    allow_insecure_loopback: bool,
) -> Result<()> {
    let url = reqwest::Url::parse(url).context("Anthropic base URL is invalid")?;
    if !url.username().is_empty() || url.password().is_some() {
        bail!("Anthropic base URL must not contain credentials");
    }
    let host = url
        .host_str()
        .context("Anthropic base URL must include a host")?;
    if url.scheme() == "https" && host.eq_ignore_ascii_case("api.anthropic.com") {
        return Ok(());
    }
    if url.scheme() == "https" {
        if allow_custom {
            return Ok(());
        }
        bail!(
            "custom Anthropic base URL {host:?} requires BEATER_ANTHROPIC_ALLOW_CUSTOM_BASE_URL=1"
        );
    }
    if url.scheme() == "http" && is_loopback_host(host) && allow_insecure_loopback {
        return Ok(());
    }
    bail!(
        "Anthropic base URL must use https, or http loopback with BEATER_ANTHROPIC_ALLOW_INSECURE_LOOPBACK=1"
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

#[cfg(test)]
mod tests {
    use super::{Anthropic, messages_url, validate_anthropic_base_url};
    use serde_json::{Value, json};
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };
    use std::thread;
    use std::time::Duration;

    #[test]
    fn base_url_override_accepts_root_or_messages_endpoint() {
        assert_eq!(
            messages_url("http://127.0.0.1:8123"),
            "http://127.0.0.1:8123/v1/messages"
        );
        assert_eq!(
            messages_url("http://127.0.0.1:8123/v1/messages"),
            "http://127.0.0.1:8123/v1/messages"
        );
        assert_eq!(
            messages_url("http://127.0.0.1:8123/"),
            "http://127.0.0.1:8123/v1/messages"
        );
    }

    #[test]
    fn base_url_requires_secure_or_explicit_fixture_origin() {
        validate_anthropic_base_url("https://api.anthropic.com/v1/messages", false, false).unwrap();
        validate_anthropic_base_url("https://anthropic.internal.test/v1/messages", true, false)
            .unwrap();
        assert!(
            validate_anthropic_base_url(
                "https://anthropic.internal.test/v1/messages",
                false,
                false
            )
            .is_err()
        );
        validate_anthropic_base_url("http://127.0.0.1:8080/v1/messages", false, true).unwrap();
        assert!(validate_anthropic_base_url("http://example.com/v1/messages", true, true).is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn create_message_streaming_records_partials_and_rebuilds_text_message() {
        let server = MockAnthropic::new(vec![MockResponse::Sse(
            [
                sse(
                    "message_start",
                    json!({
                        "type": "message_start",
                        "message": {
                            "id": "msg_1",
                            "type": "message",
                            "role": "assistant",
                            "model": "mock",
                            "content": [],
                            "stop_reason": null,
                            "stop_sequence": null,
                            "usage": {"input_tokens": 3, "output_tokens": 0}
                        }
                    }),
                ),
                sse(
                    "content_block_start",
                    json!({
                        "type": "content_block_start",
                        "index": 0,
                        "content_block": {"type": "text", "text": ""}
                    }),
                ),
                sse(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": {"type": "text_delta", "text": "hel"}
                    }),
                ),
                sse(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": {"type": "text_delta", "text": "lo"}
                    }),
                ),
                sse(
                    "content_block_stop",
                    json!({"type": "content_block_stop", "index": 0}),
                ),
                sse(
                    "message_delta",
                    json!({
                        "type": "message_delta",
                        "delta": {"stop_reason": "end_turn", "stop_sequence": null},
                        "usage": {"output_tokens": 2}
                    }),
                ),
                sse("message_stop", json!({"type": "message_stop"})),
            ]
            .join(""),
        )]);
        let client = Anthropic::new(
            "test-key".to_string(),
            &server.base_url,
            Duration::from_secs(5),
        )
        .expect("test client should build");
        let mut partials = Vec::new();

        let response = client
            .create_message_streaming(&json!({"model": "mock", "messages": []}), |partial| {
                partials.push(partial.clone());
                Ok(())
            })
            .await
            .expect("stream should assemble");

        assert_eq!(response["id"], "msg_1");
        assert_eq!(response["content"][0]["text"], "hello");
        assert_eq!(response["stop_reason"], "end_turn");
        assert_eq!(response["usage"]["input_tokens"], 3);
        assert_eq!(response["usage"]["output_tokens"], 2);
        assert_eq!(
            partials
                .iter()
                .filter(|partial| partial["event"] == "content_block_delta")
                .count(),
            2
        );
        assert_eq!(partials[2]["data"]["delta"]["text"], "hel");
        assert_eq!(server.join(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn create_message_streaming_rebuilds_tool_use_input_json_delta() {
        let server = MockAnthropic::new(vec![MockResponse::Sse(
            [
                sse(
                    "message_start",
                    json!({
                        "type": "message_start",
                        "message": {
                            "id": "msg_tools",
                            "type": "message",
                            "role": "assistant",
                            "model": "mock",
                            "content": [],
                            "stop_reason": null,
                            "stop_sequence": null,
                            "usage": {"input_tokens": 1, "output_tokens": 0}
                        }
                    }),
                ),
                sse(
                    "content_block_start",
                    json!({
                        "type": "content_block_start",
                        "index": 0,
                        "content_block": {
                            "type": "tool_use",
                            "id": "toolu_1",
                            "name": "echo",
                            "input": {}
                        }
                    }),
                ),
                sse(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": {"type": "input_json_delta", "partial_json": "{\"value\":"}
                    }),
                ),
                sse(
                    "content_block_delta",
                    json!({
                        "type": "content_block_delta",
                        "index": 0,
                        "delta": {"type": "input_json_delta", "partial_json": "\"ok\"}"}
                    }),
                ),
                sse(
                    "content_block_stop",
                    json!({"type": "content_block_stop", "index": 0}),
                ),
                sse(
                    "message_delta",
                    json!({
                        "type": "message_delta",
                        "delta": {"stop_reason": "tool_use", "stop_sequence": null},
                        "usage": {"output_tokens": 4}
                    }),
                ),
                sse("message_stop", json!({"type": "message_stop"})),
            ]
            .join(""),
        )]);
        let client = Anthropic::new(
            "test-key".to_string(),
            &server.base_url,
            Duration::from_secs(5),
        )
        .expect("test client should build");

        let response = client
            .create_message_streaming(&json!({"model": "mock", "messages": []}), |_| Ok(()))
            .await
            .expect("stream should assemble");

        assert_eq!(response["content"][0]["type"], "tool_use");
        assert_eq!(response["content"][0]["id"], "toolu_1");
        assert_eq!(response["content"][0]["name"], "echo");
        assert_eq!(response["content"][0]["input"]["value"], "ok");
        assert_eq!(response["stop_reason"], "tool_use");
        assert_eq!(server.join(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn create_message_streaming_rejects_truncated_stream_without_message_stop() {
        let server = MockAnthropic::new(vec![MockResponse::Sse(
            [
                sse(
                    "message_start",
                    json!({
                        "type": "message_start",
                        "message": {
                            "id": "msg_1",
                            "type": "message",
                            "role": "assistant",
                            "model": "mock",
                            "content": [],
                            "stop_reason": null,
                            "stop_sequence": null,
                            "usage": {"input_tokens": 1, "output_tokens": 0}
                        }
                    }),
                ),
                sse(
                    "message_delta",
                    json!({
                        "type": "message_delta",
                        "delta": {"stop_reason": "end_turn", "stop_sequence": null},
                        "usage": {"output_tokens": 1}
                    }),
                ),
            ]
            .join(""),
        )]);
        let client = Anthropic::new(
            "test-key".to_string(),
            &server.base_url,
            Duration::from_secs(5),
        )
        .expect("test client should build");

        let error = client
            .create_message_streaming(&json!({"model": "mock", "messages": []}), |_| Ok(()))
            .await
            .unwrap_err();

        assert!(format!("{error:#}").contains("before message_stop"));
        assert_eq!(server.join(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn create_message_streaming_omits_stream_error_payload() {
        let echoed_secret = format!("{}{}", "sk-ant-", "api03-test-fixture");
        let server = MockAnthropic::new(vec![MockResponse::Sse(sse(
            "error",
            json!({
                "type": "error",
                "error": {
                    "type": "invalid_request_error",
                    "message": format!("echoed secret {echoed_secret}")
                }
            }),
        ))]);
        let client = Anthropic::new(
            "test-key".to_string(),
            &server.base_url,
            Duration::from_secs(5),
        )
        .expect("test client should build");

        let mut partials = Vec::new();
        let error = client
            .create_message_streaming(&json!({"model": "mock", "messages": []}), |partial| {
                partials.push(partial.clone());
                Ok(())
            })
            .await
            .unwrap_err();
        let message = format!("{error:#}");

        assert!(message.contains("payload omitted"), "{message}");
        assert!(!message.contains(&echoed_secret), "{message}");
        assert!(partials.is_empty(), "{partials:?}");
        assert_eq!(server.join(), 1);
    }

    enum MockResponse {
        Sse(String),
    }

    struct MockAnthropic {
        base_url: String,
        requests: Arc<AtomicUsize>,
        handle: Option<thread::JoinHandle<()>>,
    }

    impl MockAnthropic {
        fn new(responses: Vec<MockResponse>) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            let addr = listener.local_addr().unwrap();
            let requests = Arc::new(AtomicUsize::new(0));
            let server_requests = Arc::clone(&requests);
            let handle = thread::spawn(move || {
                for response in responses {
                    let (mut stream, _) = listener.accept().unwrap();
                    server_requests.fetch_add(1, Ordering::SeqCst);
                    read_http_headers(&mut stream);
                    match response {
                        MockResponse::Sse(body) => write_sse_response(&mut stream, &body),
                    }
                }
            });
            Self {
                base_url: format!("http://{addr}"),
                requests,
                handle: Some(handle),
            }
        }

        fn join(mut self) -> usize {
            if let Some(handle) = self.handle.take() {
                handle.join().unwrap();
            }
            self.requests.load(Ordering::SeqCst)
        }
    }

    fn read_http_headers(stream: &mut TcpStream) {
        stream
            .set_read_timeout(Some(Duration::from_secs(2)))
            .unwrap();
        let mut data = Vec::new();
        let mut buf = [0; 1024];
        loop {
            match stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    data.extend_from_slice(&buf[..n]);
                    if data.windows(4).any(|window| window == b"\r\n\r\n") {
                        break;
                    }
                }
                Err(error)
                    if error.kind() == std::io::ErrorKind::WouldBlock
                        || error.kind() == std::io::ErrorKind::TimedOut =>
                {
                    break;
                }
                Err(error) => panic!("reading request headers failed: {error}"),
            }
        }
    }

    fn sse(event: &str, data: Value) -> String {
        format!("event: {event}\ndata: {data}\n\n")
    }

    fn write_sse_response(stream: &mut TcpStream, body: &str) {
        let reply = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\nconnection: close\r\n\r\n{body}"
        );
        stream.write_all(reply.as_bytes()).unwrap();
    }
}
