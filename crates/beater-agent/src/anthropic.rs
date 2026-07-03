//! Minimal Messages API client over raw reqwest (there is no official Rust
//! SDK). Non-streaming: each request is exactly one journaled step.

use anyhow::{Context, Result, anyhow};
use serde_json::Value;
use std::time::Duration;

const DEFAULT_BASE_URL: &str = "https://api.anthropic.com";
const DEFAULT_REQUEST_TIMEOUT: Duration = Duration::from_secs(600);
const DEFAULT_RETRY_DELAY: Duration = Duration::from_secs(2);
const MAX_ATTEMPTS: u32 = 3;

pub struct Anthropic {
    http: reqwest::Client,
    api_key: String,
    messages_url: String,
    retry_initial_delay: Duration,
}

impl Anthropic {
    pub fn from_env() -> Result<Self> {
        let api_key = std::env::var("ANTHROPIC_API_KEY")
            .context("ANTHROPIC_API_KEY is not set — required for `beater agent run`")?;
        let base_url =
            std::env::var("ANTHROPIC_BASE_URL").unwrap_or_else(|_| DEFAULT_BASE_URL.to_string());
        Self::new(
            api_key,
            &base_url,
            DEFAULT_REQUEST_TIMEOUT,
            DEFAULT_RETRY_DELAY,
        )
    }

    fn new(
        api_key: String,
        base_url: &str,
        request_timeout: Duration,
        retry_initial_delay: Duration,
    ) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(request_timeout)
            .build()
            .context("building anthropic http client")?;
        Ok(Self {
            http,
            api_key,
            messages_url: messages_url(base_url),
            retry_initial_delay,
        })
    }

    pub async fn create_message(&self, body: &Value) -> Result<Value> {
        let mut delay = self.retry_initial_delay;
        for attempt in 1..=MAX_ATTEMPTS {
            match self.create_message_once(body).await {
                Ok(value) => return Ok(value),
                Err(AttemptError::Retryable(error)) if attempt < MAX_ATTEMPTS => {
                    tracing::warn!("anthropic request failed ({error:#}), retrying in {delay:?}");
                    tokio::time::sleep(delay).await;
                    delay *= 4;
                }
                Err(error) => return Err(error.into_error()),
            }
        }
        unreachable!("retry loop returns or bails")
    }

    async fn create_message_once(&self, body: &Value) -> std::result::Result<Value, AttemptError> {
        let resp = self
            .http
            .post(&self.messages_url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .json(body)
            .send()
            .await
            .map_err(|error| {
                AttemptError::Retryable(anyhow!("sending anthropic request: {error}"))
            })?;
        let status = resp.status();
        let text = match resp.text().await {
            Ok(text) => text,
            Err(error) if is_retryable_status(status) || status.is_success() => {
                return Err(AttemptError::Retryable(anyhow!(
                    "reading anthropic response body: {error}"
                )));
            }
            Err(error) => {
                return Err(AttemptError::Fatal(anyhow!(
                    "anthropic api error {status}: failed to read response body: {error}"
                )));
            }
        };
        // 429 / 500 / 529 are retryable per the API error reference.
        if is_retryable_status(status) {
            return Err(AttemptError::Retryable(anyhow!(
                "anthropic api error {status}: {text}"
            )));
        }
        if !status.is_success() {
            return Err(AttemptError::Fatal(anyhow!(
                "anthropic api error {status}: {text}"
            )));
        }
        serde_json::from_str(&text)
            .context("anthropic response was not JSON")
            .map_err(AttemptError::Fatal)
    }
}

fn is_retryable_status(status: reqwest::StatusCode) -> bool {
    status.as_u16() == 429 || status.as_u16() >= 500
}

enum AttemptError {
    Retryable(anyhow::Error),
    Fatal(anyhow::Error),
}

impl AttemptError {
    fn into_error(self) -> anyhow::Error {
        match self {
            Self::Retryable(error) | Self::Fatal(error) => error,
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

#[cfg(test)]
mod tests {
    use super::{Anthropic, messages_url};
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

    #[tokio::test(flavor = "current_thread")]
    async fn create_message_retries_after_request_timeout() {
        let server = MockAnthropic::new(vec![
            MockResponse::Stall(Duration::from_millis(100)),
            MockResponse::Json(json!({"id": "msg_ok", "content": []})),
        ]);
        let client = Anthropic::new(
            "test-key".to_string(),
            &server.base_url,
            Duration::from_millis(25),
            Duration::from_millis(1),
        )
        .expect("test client should build");

        let response = client
            .create_message(&json!({"model": "mock", "messages": []}))
            .await
            .expect("timeout should retry and recover");

        assert_eq!(response["id"], "msg_ok");
        assert_eq!(server.join(), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn create_message_retries_truncated_response_body() {
        let server = MockAnthropic::new(vec![
            MockResponse::TruncatedBody(200),
            MockResponse::Json(json!({"id": "msg_after_body_error", "content": []})),
        ]);
        let client = Anthropic::new(
            "test-key".to_string(),
            &server.base_url,
            Duration::from_secs(5),
            Duration::from_millis(1),
        )
        .expect("test client should build");

        let response = client
            .create_message(&json!({"model": "mock", "messages": []}))
            .await
            .expect("body error should retry and recover");

        assert_eq!(response["id"], "msg_after_body_error");
        assert_eq!(server.join(), 2);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn create_message_does_not_retry_truncated_non_retryable_error() {
        let server = MockAnthropic::new(vec![MockResponse::TruncatedBody(401)]);
        let client = Anthropic::new(
            "test-key".to_string(),
            &server.base_url,
            Duration::from_secs(5),
            Duration::from_millis(1),
        )
        .expect("test client should build");

        let error = client
            .create_message(&json!({"model": "mock", "messages": []}))
            .await
            .expect_err("non-retryable API statuses should not retry on body errors");

        assert!(
            format!("{error:#}").contains("anthropic api error 401 Unauthorized"),
            "{error:#}"
        );
        assert_eq!(server.join(), 1);
    }

    enum MockResponse {
        Json(Value),
        Stall(Duration),
        TruncatedBody(u16),
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
                let mut stall_handles = Vec::new();
                for response in responses {
                    let (mut stream, _) = listener.accept().unwrap();
                    server_requests.fetch_add(1, Ordering::SeqCst);
                    read_http_headers(&mut stream);
                    match response {
                        MockResponse::Json(value) => write_json_response(&mut stream, &value),
                        MockResponse::Stall(duration) => {
                            stall_handles.push(thread::spawn(move || {
                                thread::sleep(duration);
                                drop(stream);
                            }));
                        }
                        MockResponse::TruncatedBody(status) => {
                            write_truncated_response(&mut stream, status)
                        }
                    }
                }
                for handle in stall_handles {
                    handle.join().unwrap();
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

    fn write_json_response(stream: &mut TcpStream, value: &Value) {
        let body = value.to_string();
        let reply = format!(
            "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
            body.len(),
            body
        );
        stream.write_all(reply.as_bytes()).unwrap();
    }

    fn write_truncated_response(stream: &mut TcpStream, status: u16) {
        let reason = match status {
            200 => "OK",
            401 => "Unauthorized",
            429 => "Too Many Requests",
            500 => "Internal Server Error",
            _ => "Error",
        };
        let reply = format!(
            "HTTP/1.1 {status} {reason}\r\ncontent-type: application/json\r\ncontent-length: 64\r\nconnection: close\r\n\r\n{{\"id\""
        );
        stream.write_all(reply.as_bytes()).unwrap();
    }
}
