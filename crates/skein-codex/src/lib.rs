//! Read-only Codex app-server discovery for Session Skein.

use std::io::BufRead;
use std::io::BufReader;
use std::io::Write;
use std::process::Command;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;
use std::thread;
use std::time::Duration;

use serde::Deserialize;
use serde::Serialize;
use serde_json::Value;
use serde_json::json;

const DISCOVERY_TIMEOUT: Duration = Duration::from_secs(15);

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

    /// Codex did not complete the bounded preview in time.
    #[error("Codex app-server preview timed out after {seconds} seconds")]
    Timeout {
        /// Configured watchdog duration.
        seconds: u64,
    },
}

/// Result type used by this adapter.
pub type Result<T> = std::result::Result<T, Error>;

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
    if value.get("subAgent").is_some() {
        return "subAgent".to_owned();
    }
    if value.get("custom").is_some() {
        return "custom".to_owned();
    }
    "unknown".to_owned()
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
}
