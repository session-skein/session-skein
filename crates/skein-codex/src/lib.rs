//! Codex app-server discovery and explicit control for Session Skein.

use std::collections::VecDeque;
use std::io::BufRead;
use std::io::BufReader;
use std::io::Write;
use std::process::ChildStdin;
use std::process::Command;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::sync::mpsc::Receiver;
use std::sync::mpsc::RecvTimeoutError;
use std::thread;
use std::time::Duration;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);
const CONTROL_REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const CONTROL_RESUME_TIMEOUT: Duration = Duration::from_secs(5 * 60);
const MAX_DEFERRED_MESSAGES: usize = 1_024;

/// Errors produced by the Codex discovery adapter.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The Codex process could not be started or communicated with.
    #[error("Codex app-server I/O failed: {0}")]
    Io(#[from] std::io::Error),

    /// Codex emitted a response that did not match the negotiated protocol.
    #[error("Codex app-server protocol error: {0}")]
    Protocol(String),

    /// Codex returned a JSON-RPC error response.
    #[error("Codex app-server error {code}: {message}")]
    Server {
        /// JSON-RPC error code.
        code: i64,
        /// Server-provided error message.
        message: String,
    },

    /// A response could not be decoded.
    #[error("Codex app-server JSON could not be decoded: {0}")]
    Json(#[from] serde_json::Error),

    /// Codex did not complete a bounded protocol phase in time.
    #[error("Codex app-server request timed out after {seconds} seconds")]
    Timeout {
        /// Configured watchdog duration.
        seconds: u64,
    },

    /// The installed Codex CLI has no usable ChatGPT login.
    #[error("Codex authentication is required; run `codex login`")]
    AuthenticationRequired,

    /// Codex did not apply the explicitly requested execution policy.
    #[error("Codex effective policy mismatch: {0}")]
    PolicyMismatch(String),

    /// App-server requested an interaction this controller cannot safely answer.
    #[error("Codex requested unsupported interactive input: {0}")]
    InteractiveRequest(String),
}

/// Result type used by this adapter.
pub type Result<T> = std::result::Result<T, Error>;

/// A started or resumed Codex thread with verified effective policy.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlledThread {
    pub thread_id: String,
    pub session_id: String,
    pub cwd: String,
    pub model: String,
    pub model_provider: String,
}

/// One accepted Codex turn.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ControlledTurn {
    pub turn_id: String,
    pub status: String,
}

/// Redaction-aware live event emitted by the control connection.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ControlEvent {
    TurnStarted {
        thread_id: String,
        turn_id: String,
    },
    AgentMessageDelta {
        thread_id: String,
        turn_id: String,
        delta: String,
    },
    ItemStarted {
        thread_id: String,
        turn_id: String,
        item_type: String,
    },
    ItemCompleted {
        thread_id: String,
        turn_id: String,
        item_type: String,
    },
    ThreadStatusChanged {
        thread_id: String,
        status: String,
    },
    RetryingError {
        thread_id: String,
        turn_id: String,
        will_retry: bool,
    },
    TurnCompleted {
        thread_id: String,
        turn_id: String,
        status: String,
    },
    Unknown {
        method: String,
    },
}

/// Long-lived JSONL connection to one locally installed Codex app-server.
pub struct ControlClient {
    child: Arc<Mutex<std::process::Child>>,
    stdin: ChildStdin,
    incoming: Receiver<Result<Value>>,
    queued: VecDeque<Value>,
    next_id: i64,
}

impl ControlClient {
    /// Spawn Codex, initialize the connection, and verify cached authentication.
    pub fn connect() -> Result<Self> {
        let command = std::env::var_os("SKEIN_CODEX_BIN").unwrap_or_else(|| "codex".into());
        let mut child = Command::new(command)
            .args(["app-server", "--listen", "stdio://"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let stdin = child
            .stdin
            .take()
            .ok_or_else(|| Error::Protocol("app-server stdin was unavailable".to_owned()))?;
        let stdout = child
            .stdout
            .take()
            .ok_or_else(|| Error::Protocol("app-server stdout was unavailable".to_owned()))?;
        let (sender, incoming) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = BufReader::new(stdout);
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {
                        if sender
                            .send(serde_json::from_str(&line).map_err(Error::from))
                            .is_err()
                        {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = sender.send(Err(Error::Io(error)));
                        break;
                    }
                }
            }
        });
        let mut client = Self {
            child: Arc::new(Mutex::new(child)),
            stdin,
            incoming,
            queued: VecDeque::new(),
            next_id: 1,
        };
        client.request(
            "initialize",
            json!({
                "clientInfo": {
                    "name": "session_skein",
                    "title": "Session Skein",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }),
        )?;
        write_message(
            &mut client.stdin,
            &json!({"method": "initialized", "params": {}}),
        )?;
        let account = client.request("account/read", json!({"refreshToken": true}))?;
        validate_chatgpt_account(&account)?;
        Ok(client)
    }

    /// Start a persistent thread under an explicit full-access/no-approval policy.
    pub fn start_thread(&mut self, cwd: &std::path::Path) -> Result<ControlledThread> {
        self.open_thread(
            "thread/start",
            json!({
                "cwd": cwd,
                "sandbox": "danger-full-access",
                "approvalPolicy": "never",
                "ephemeral": false,
                "threadSource": "session_skein"
            }),
            cwd,
        )
    }

    /// Resume a stored thread under the same explicit policy and working directory.
    pub fn resume_thread(
        &mut self,
        thread_id: &str,
        cwd: &std::path::Path,
    ) -> Result<ControlledThread> {
        self.open_thread(
            "thread/resume",
            json!({
                "threadId": thread_id,
                "cwd": cwd,
                "sandbox": "danger-full-access",
                "approvalPolicy": "never"
            }),
            cwd,
        )
    }

    /// Start a text turn and reassert the explicit full-access policy.
    pub fn start_turn(
        &mut self,
        thread_id: &str,
        prompt: &str,
        client_message_id: &str,
        cwd: &std::path::Path,
    ) -> Result<ControlledTurn> {
        let result = self.request(
            "turn/start",
            json!({
                "threadId": thread_id,
                "input": [{"type": "text", "text": prompt}],
                "clientUserMessageId": client_message_id,
                "cwd": cwd,
                "sandboxPolicy": {"type": "dangerFullAccess"},
                "approvalPolicy": "never"
            }),
        )?;
        parse_turn(&result)
    }

    /// Read the next live notification, tolerating unknown additive methods.
    pub fn next_event(&mut self) -> Result<ControlEvent> {
        loop {
            let value = self.next_value()?;
            if is_server_request(&value) {
                return Err(Error::InteractiveRequest(reject_server_request(
                    &mut self.stdin,
                    &value,
                )?));
            }
            let Some(method) = value.get("method").and_then(Value::as_str) else {
                continue;
            };
            let params = value.get("params").cloned().unwrap_or(Value::Null);
            return parse_control_event(method, &params);
        }
    }

    fn open_thread(
        &mut self,
        method: &str,
        params: Value,
        expected_cwd: &std::path::Path,
    ) -> Result<ControlledThread> {
        let result = if method == "thread/resume" {
            self.request_with_timeout(method, params, CONTROL_RESUME_TIMEOUT)?
        } else {
            self.request(method, params)?
        };
        parse_controlled_thread(&result, expected_cwd)
    }

    fn request(&mut self, method: &str, params: Value) -> Result<Value> {
        self.request_with_timeout(method, params, CONTROL_REQUEST_TIMEOUT)
    }

    fn request_with_timeout(
        &mut self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        write_message(
            &mut self.stdin,
            &json!({"method": method, "id": id, "params": params}),
        )?;
        let mut deferred = VecDeque::new();
        loop {
            let value = if let Some(value) = self.queued.pop_front() {
                value
            } else {
                self.receive_value_timeout(timeout)?
            };
            if is_server_request(&value) {
                let requested_method = reject_server_request(&mut self.stdin, &value)?;
                self.queued.extend(deferred);
                return Err(Error::InteractiveRequest(requested_method));
            }
            if value.get("id").and_then(Value::as_i64) == Some(id) {
                let response: RpcResponse = serde_json::from_value(value)?;
                self.queued.extend(deferred);
                return response_result(response);
            }
            if deferred.len() >= MAX_DEFERRED_MESSAGES {
                return Err(Error::Protocol(format!(
                    "more than {MAX_DEFERRED_MESSAGES} unrelated messages arrived while awaiting {method}"
                )));
            }
            deferred.push_back(value);
        }
    }

    fn next_value(&mut self) -> Result<Value> {
        if let Some(value) = self.queued.pop_front() {
            return Ok(value);
        }
        self.receive_value()
    }

    fn receive_value(&self) -> Result<Value> {
        self.incoming
            .recv()
            .map_err(|_| Error::Protocol("app-server closed the control connection".to_owned()))?
    }

    fn receive_value_timeout(&self, timeout: Duration) -> Result<Value> {
        match self.incoming.recv_timeout(timeout) {
            Ok(value) => value,
            Err(RecvTimeoutError::Timeout) => Err(Error::Timeout {
                seconds: timeout.as_secs(),
            }),
            Err(RecvTimeoutError::Disconnected) => Err(Error::Protocol(
                "app-server closed the control connection".to_owned(),
            )),
        }
    }
}

fn parse_controlled_thread(
    result: &Value,
    expected_cwd: &std::path::Path,
) -> Result<ControlledThread> {
    let sandbox = result
        .get("sandbox")
        .and_then(|value| value.get("type"))
        .and_then(Value::as_str);
    let approval = result.get("approvalPolicy").and_then(Value::as_str);
    if sandbox != Some("dangerFullAccess") || approval != Some("never") {
        return Err(Error::PolicyMismatch(format!(
            "requested dangerFullAccess/never, received {sandbox:?}/{approval:?}"
        )));
    }
    let cwd = required_string(result, "cwd")?;
    let expected = expected_cwd.to_string_lossy();
    if cwd != expected {
        return Err(Error::PolicyMismatch(format!(
            "requested cwd {expected}, received {cwd}"
        )));
    }
    let thread = result
        .get("thread")
        .ok_or_else(|| Error::Protocol("thread response had no thread".to_owned()))?;
    Ok(ControlledThread {
        thread_id: required_string(thread, "id")?,
        session_id: required_string(thread, "sessionId")?,
        cwd,
        model: required_string(result, "model")?,
        model_provider: required_string(result, "modelProvider")?,
    })
}

fn is_server_request(value: &Value) -> bool {
    value.get("id").is_some() && value.get("method").and_then(Value::as_str).is_some()
}

fn reject_server_request(stdin: &mut ChildStdin, value: &Value) -> Result<String> {
    let method = value
        .get("method")
        .and_then(Value::as_str)
        .unwrap_or("unknown")
        .to_owned();
    write_message(
        stdin,
        &json!({
            "id": value.get("id").cloned().unwrap_or(Value::Null),
            "error": {
                "code": -32601,
                "message": "Session Skein cannot safely answer this interactive request"
            }
        }),
    )?;
    Ok(method)
}

fn validate_chatgpt_account(result: &Value) -> Result<()> {
    if result
        .get("requiresOpenaiAuth")
        .and_then(Value::as_bool)
        .is_none()
    {
        return Err(Error::AuthenticationRequired);
    }
    let account = result.get("account").filter(|value| !value.is_null());
    if account
        .and_then(|value| value.get("type"))
        .and_then(Value::as_str)
        != Some("chatgpt")
    {
        return Err(Error::AuthenticationRequired);
    }
    Ok(())
}

impl Drop for ControlClient {
    fn drop(&mut self) {
        if let Ok(mut child) = self.child.lock() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

/// Options for one bounded `thread/list` request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryOptions {
    /// Maximum threads returned in this page.
    pub limit: u32,
    /// Opaque cursor from a previous preview page.
    pub cursor: Option<String>,
    /// Avoid Codex's JSONL scan-and-repair path when true.
    pub use_state_db_only: bool,
    /// Include user-facing names and first-message previews.
    pub include_text: bool,
}

impl Default for DiscoveryOptions {
    fn default() -> Self {
        Self {
            limit: 50,
            cursor: None,
            use_state_db_only: true,
            include_text: false,
        }
    }
}

/// One redaction-aware thread candidate returned by Codex.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadPreview {
    /// Stable Codex thread identifier.
    pub id: String,
    /// Session-tree identifier shared by related threads.
    pub session_id: String,
    /// Working directory recorded by Codex.
    pub cwd: String,
    /// Unix creation timestamp.
    pub created_at: i64,
    /// Unix update timestamp.
    pub updated_at: i64,
    /// Codex surface or source classification.
    pub source: String,
    /// Current runtime status reported by app-server.
    pub status: String,
    /// Model provider recorded for the thread.
    pub model_provider: String,
    /// Codex CLI version that created the thread.
    pub cli_version: String,
    /// Parent thread for sub-agent relationships.
    pub parent_thread_id: Option<String>,
    /// Source thread when this thread was forked.
    pub forked_from_id: Option<String>,
    /// Whether the source marks the thread as ephemeral.
    pub ephemeral: bool,
    /// User-facing name, present only with `include_text`.
    pub name: Option<String>,
    /// First-message preview, present only with `include_text`.
    pub preview: Option<String>,
    /// True when text fields were deliberately omitted by Session Skein.
    pub text_redacted: bool,
}

/// One page of dry-run Codex discovery results.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryPage {
    /// Candidate threads in Codex's requested order.
    pub threads: Vec<ThreadPreview>,
    /// Cursor for the next page, if available.
    pub next_cursor: Option<String>,
    /// Whether Codex was allowed to scan JSONL rollouts to repair its state index.
    pub repaired_source_index: bool,
}

/// Spawn the installed Codex app-server and make one read-only discovery request.
pub fn discover(options: &DiscoveryOptions) -> Result<DiscoveryPage> {
    let command = std::env::var_os("SKEIN_CODEX_BIN").unwrap_or_else(|| "codex".into());
    let mut child = Command::new(command)
        .args(["app-server", "--listen", "stdio://"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let mut stdin = child
        .stdin
        .take()
        .ok_or_else(|| Error::Protocol("app-server stdin was unavailable".to_owned()))?;
    let stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::Protocol("app-server stdout was unavailable".to_owned()))?;
    let child = Arc::new(Mutex::new(child));
    let cancelled = Arc::new(AtomicBool::new(false));
    let timed_out = Arc::new(AtomicBool::new(false));
    {
        let child = Arc::clone(&child);
        let cancelled = Arc::clone(&cancelled);
        let timed_out = Arc::clone(&timed_out);
        thread::spawn(move || {
            thread::sleep(DISCOVERY_TIMEOUT);
            if !cancelled.load(Ordering::Acquire) {
                timed_out.store(true, Ordering::Release);
                if let Ok(mut child) = child.lock() {
                    let _ = child.kill();
                }
            }
        });
    }

    let result = exchange(BufReader::new(stdout), &mut stdin, options);
    cancelled.store(true, Ordering::Release);
    if let Ok(mut child) = child.lock() {
        let _ = child.kill();
        let _ = child.wait();
    }
    if timed_out.load(Ordering::Acquire) {
        return Err(Error::Timeout {
            seconds: DISCOVERY_TIMEOUT.as_secs(),
        });
    }
    result
}

fn exchange<R: BufRead, W: Write>(
    mut reader: R,
    mut writer: W,
    options: &DiscoveryOptions,
) -> Result<DiscoveryPage> {
    write_message(
        &mut writer,
        &json!({
            "method": "initialize",
            "id": 1,
            "params": {
                "clientInfo": {
                    "name": "session_skein",
                    "title": "Session Skein",
                    "version": env!("CARGO_PKG_VERSION")
                }
            }
        }),
    )?;
    let initialized = read_response(&mut reader, 1)?;
    response_result(initialized)?;

    write_message(&mut writer, &json!({"method": "initialized", "params": {}}))?;
    write_message(
        &mut writer,
        &json!({
            "method": "thread/list",
            "id": 2,
            "params": {
                "limit": options.limit,
                "cursor": options.cursor,
                "sortKey": "updated_at",
                "sortDirection": "desc",
                "useStateDbOnly": options.use_state_db_only
            }
        }),
    )?;
    let response = response_result(read_response(&mut reader, 2)?)?;
    parse_page(response, options)
}

fn write_message(writer: &mut impl Write, value: &Value) -> Result<()> {
    serde_json::to_writer(&mut *writer, value)?;
    writer.write_all(b"\n")?;
    writer.flush()?;
    Ok(())
}

fn read_response(reader: &mut impl BufRead, expected_id: i64) -> Result<RpcResponse> {
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            return Err(Error::Protocol(format!(
                "app-server closed before response id {expected_id}"
            )));
        }
        let value: Value = serde_json::from_str(&line)?;
        if value.get("id").and_then(Value::as_i64) == Some(expected_id) {
            return serde_json::from_value(value).map_err(Error::from);
        }
    }
}

fn response_result(response: RpcResponse) -> Result<Value> {
    if let Some(error) = response.error {
        return Err(Error::Server {
            code: error.code,
            message: error.message,
        });
    }
    response
        .result
        .ok_or_else(|| Error::Protocol(format!("response {} had no result", response.id)))
}

fn parse_page(value: Value, options: &DiscoveryOptions) -> Result<DiscoveryPage> {
    let page: ThreadListResult = serde_json::from_value(value)?;
    let threads = page
        .data
        .into_iter()
        .map(|thread| thread.into_preview(options.include_text))
        .collect();
    Ok(DiscoveryPage {
        threads,
        next_cursor: page.next_cursor,
        repaired_source_index: !options.use_state_db_only,
    })
}

#[derive(Debug, Deserialize)]
struct RpcResponse {
    id: i64,
    result: Option<Value>,
    error: Option<RpcError>,
}

#[derive(Debug, Deserialize)]
struct RpcError {
    code: i64,
    message: String,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct ThreadListResult {
    data: Vec<RawThread>,
    next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RawThread {
    id: String,
    session_id: String,
    cwd: String,
    created_at: i64,
    updated_at: i64,
    source: Value,
    status: Value,
    model_provider: String,
    cli_version: String,
    parent_thread_id: Option<String>,
    forked_from_id: Option<String>,
    ephemeral: bool,
    name: Option<String>,
    preview: String,
}

impl RawThread {
    fn into_preview(self, include_text: bool) -> ThreadPreview {
        ThreadPreview {
            id: self.id,
            session_id: self.session_id,
            cwd: self.cwd,
            created_at: self.created_at,
            updated_at: self.updated_at,
            source: source_label(&self.source),
            status: self
                .status
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
            model_provider: self.model_provider,
            cli_version: self.cli_version,
            parent_thread_id: self.parent_thread_id,
            forked_from_id: self.forked_from_id,
            ephemeral: self.ephemeral,
            name: include_text.then_some(self.name).flatten(),
            preview: include_text.then_some(self.preview),
            text_redacted: !include_text,
        }
    }
}

fn source_label(value: &Value) -> String {
    if let Some(value) = value.as_str() {
        return value.to_owned();
    }
    if value.get("custom").is_some() {
        return "custom".to_owned();
    }
    if let Some(sub_agent) = value.get("subAgent") {
        if let Some(kind) = sub_agent.as_str() {
            return format!("subAgent:{kind}");
        }
        if sub_agent.get("thread_spawn").is_some() {
            return "subAgent:thread_spawn".to_owned();
        }
        if sub_agent.get("other").is_some() {
            return "subAgent:other".to_owned();
        }
        return "subAgent:unknown".to_owned();
    }
    "unknown".to_owned()
}

fn parse_turn(result: &Value) -> Result<ControlledTurn> {
    let turn = result
        .get("turn")
        .ok_or_else(|| Error::Protocol("turn response had no turn".to_owned()))?;
    Ok(ControlledTurn {
        turn_id: required_string(turn, "id")?,
        status: required_string(turn, "status")?,
    })
}

fn required_string(value: &Value, field: &str) -> Result<String> {
    value
        .get(field)
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| Error::Protocol(format!("response field {field} was missing or invalid")))
}

fn parse_control_event(method: &str, params: &Value) -> Result<ControlEvent> {
    let thread_id = || required_string(params, "threadId");
    let turn_id = || required_string(params, "turnId");
    match method {
        "turn/started" => {
            let turn = params
                .get("turn")
                .ok_or_else(|| Error::Protocol("turn/started had no turn".to_owned()))?;
            Ok(ControlEvent::TurnStarted {
                thread_id: thread_id()?,
                turn_id: required_string(turn, "id")?,
            })
        }
        "item/agentMessage/delta" => Ok(ControlEvent::AgentMessageDelta {
            thread_id: thread_id()?,
            turn_id: turn_id()?,
            delta: required_string(params, "delta")?,
        }),
        "item/started" | "item/completed" => {
            let item = params
                .get("item")
                .ok_or_else(|| Error::Protocol(format!("{method} had no item")))?;
            let item_type = item
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned();
            if method == "item/started" {
                Ok(ControlEvent::ItemStarted {
                    thread_id: thread_id()?,
                    turn_id: turn_id()?,
                    item_type,
                })
            } else {
                Ok(ControlEvent::ItemCompleted {
                    thread_id: thread_id()?,
                    turn_id: turn_id()?,
                    item_type,
                })
            }
        }
        "thread/status/changed" => Ok(ControlEvent::ThreadStatusChanged {
            thread_id: thread_id()?,
            status: params
                .get("status")
                .and_then(|status| status.get("type"))
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_owned(),
        }),
        "error" => Ok(ControlEvent::RetryingError {
            thread_id: thread_id()?,
            turn_id: turn_id()?,
            will_retry: params
                .get("willRetry")
                .and_then(Value::as_bool)
                .unwrap_or(false),
        }),
        "turn/completed" => {
            let turn = params
                .get("turn")
                .ok_or_else(|| Error::Protocol("turn/completed had no turn".to_owned()))?;
            Ok(ControlEvent::TurnCompleted {
                thread_id: thread_id()?,
                turn_id: required_string(turn, "id")?,
                status: required_string(turn, "status")?,
            })
        }
        _ => Ok(ControlEvent::Unknown {
            method: method.to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    const INITIALIZED: &str = r#"{"id":1,"result":{"userAgent":"synthetic"}}"#;
    const THREADS: &str = r#"{"id":2,"result":{"data":[{"id":"01900000-0000-7000-8000-000000000001","sessionId":"01900000-0000-7000-8000-000000000000","cwd":"/synthetic/project","createdAt":10,"updatedAt":20,"source":"cli","status":{"type":"notLoaded"},"modelProvider":"openai","cliVersion":"1.2.3","parentThreadId":null,"forkedFromId":null,"ephemeral":false,"name":"Synthetic title","preview":"Synthetic prompt","turns":[]}],"nextCursor":"opaque"}}"#;

    #[test]
    fn exchanges_handshake_and_redacts_text_by_default() -> Result<()> {
        let input = format!("{INITIALIZED}\n{THREADS}\n");
        let mut output = Vec::new();
        let page = exchange(
            Cursor::new(input),
            &mut output,
            &DiscoveryOptions::default(),
        )?;

        assert_eq!(page.threads.len(), 1);
        assert_eq!(page.threads[0].source, "cli");
        assert_eq!(page.threads[0].status, "notLoaded");
        assert_eq!(page.threads[0].name, None);
        assert_eq!(page.threads[0].preview, None);
        assert!(page.threads[0].text_redacted);
        assert_eq!(page.next_cursor.as_deref(), Some("opaque"));

        let messages =
            String::from_utf8(output).map_err(|error| Error::Protocol(error.to_string()))?;
        assert!(messages.contains("\"method\":\"initialize\""));
        assert!(messages.contains("\"method\":\"thread/list\""));
        assert!(messages.contains("\"useStateDbOnly\":true"));
        Ok(())
    }

    #[test]
    fn includes_text_only_when_explicitly_requested() -> Result<()> {
        let input = format!("{INITIALIZED}\n{THREADS}\n");
        let mut output = Vec::new();
        let options = DiscoveryOptions {
            include_text: true,
            ..DiscoveryOptions::default()
        };
        let page = exchange(Cursor::new(input), &mut output, &options)?;
        assert_eq!(page.threads[0].name.as_deref(), Some("Synthetic title"));
        assert_eq!(page.threads[0].preview.as_deref(), Some("Synthetic prompt"));
        assert!(!page.threads[0].text_redacted);
        Ok(())
    }

    #[test]
    fn surfaces_json_rpc_errors() {
        let input = "{\"id\":1,\"error\":{\"code\":-1,\"message\":\"synthetic failure\"}}\n";
        let mut output = Vec::new();
        assert!(matches!(
            exchange(Cursor::new(input), &mut output, &DiscoveryOptions::default()),
            Err(Error::Server { code: -1, message }) if message == "synthetic failure"
        ));
    }

    #[test]
    fn preserves_nonstandard_source_provenance_without_private_agent_fields() {
        assert_eq!(source_label(&json!({"custom": "automation"})), "custom");
        assert_eq!(
            source_label(&json!({"subAgent": "review"})),
            "subAgent:review"
        );
        assert_eq!(
            source_label(&json!({"subAgent": {"other": "private label"}})),
            "subAgent:other"
        );
        assert_eq!(
            source_label(&json!({
                "subAgent": {
                    "thread_spawn": {
                        "agent_nickname": "private nickname",
                        "agent_path": "private/path",
                        "agent_role": "worker",
                        "depth": 1,
                        "parent_thread_id": "parent"
                    }
                }
            })),
            "subAgent:thread_spawn"
        );
    }

    #[test]
    fn accepts_only_an_authenticated_chatgpt_account() {
        for requires_openai_auth in [false, true] {
            assert!(
                validate_chatgpt_account(&json!({
                    "requiresOpenaiAuth": requires_openai_auth,
                    "account": {"type": "chatgpt", "email": null, "planType": "pro"}
                }))
                .is_ok()
            );
        }
        for invalid in [
            json!({"requiresOpenaiAuth": false, "account": null}),
            json!({"requiresOpenaiAuth": false, "account": {"type": "apiKey"}}),
            json!({"account": {"type": "chatgpt"}}),
        ] {
            assert!(matches!(
                validate_chatgpt_account(&invalid),
                Err(Error::AuthenticationRequired)
            ));
        }
    }

    #[test]
    fn controlled_thread_fails_closed_on_policy_or_cwd_mismatch() {
        let valid = json!({
            "thread": {"id": "thread", "sessionId": "session"},
            "model": "synthetic-model",
            "modelProvider": "openai",
            "cwd": "/synthetic/project",
            "approvalPolicy": "never",
            "sandbox": {"type": "dangerFullAccess"}
        });
        assert!(
            parse_controlled_thread(&valid, std::path::Path::new("/synthetic/project")).is_ok()
        );

        let mut wrong_sandbox = valid.clone();
        wrong_sandbox["sandbox"]["type"] = json!("workspaceWrite");
        assert!(matches!(
            parse_controlled_thread(&wrong_sandbox, std::path::Path::new("/synthetic/project")),
            Err(Error::PolicyMismatch(_))
        ));

        let mut wrong_approval = valid.clone();
        wrong_approval["approvalPolicy"] = json!("on-request");
        assert!(matches!(
            parse_controlled_thread(&wrong_approval, std::path::Path::new("/synthetic/project")),
            Err(Error::PolicyMismatch(_))
        ));

        assert!(matches!(
            parse_controlled_thread(&valid, std::path::Path::new("/different/project")),
            Err(Error::PolicyMismatch(_))
        ));
    }
}
