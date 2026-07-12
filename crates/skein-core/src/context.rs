use std::collections::VecDeque;
use std::fs;
use std::fs::File;
use std::io::Read;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::time::SystemTime;

use rusqlite::Transaction;
use rusqlite::TransactionBehavior;
use rusqlite::params;
use serde::Serialize;

use crate::Error;
use crate::Project;
use crate::Registry;
use crate::Result;
use crate::registry::unix_timestamp;

pub(crate) const CREATE_CONTEXT_DOCUMENT_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS recall_settings (
    id INTEGER PRIMARY KEY CHECK(id = 1),
    include_codex_memories INTEGER NOT NULL CHECK(include_codex_memories IN (0, 1)),
    include_codex_sessions INTEGER NOT NULL CHECK(include_codex_sessions IN (0, 1)),
    updated_at INTEGER NOT NULL
);
INSERT OR IGNORE INTO recall_settings (
    id, include_codex_memories, include_codex_sessions, updated_at
) VALUES (1, 0, 0, unixepoch());
CREATE TABLE IF NOT EXISTS context_documents (
    id INTEGER PRIMARY KEY,
    source_kind TEXT NOT NULL CHECK(source_kind IN ('codex_memory', 'codex_session')),
    source_path TEXT NOT NULL,
    project_id INTEGER REFERENCES projects(id) ON DELETE SET NULL,
    context_path TEXT,
    fingerprint TEXT NOT NULL,
    refreshed_at INTEGER NOT NULL,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    imported_bytes INTEGER NOT NULL CHECK(imported_bytes >= 0 AND imported_bytes <= 524288),
    UNIQUE(source_kind, source_path)
);
CREATE INDEX IF NOT EXISTS context_documents_project ON context_documents(project_id, source_kind);
CREATE VIRTUAL TABLE IF NOT EXISTS context_documents_fts USING fts5(title, body);";

/// Hard upper bound for one deep-recall refresh.
pub const MAX_CONTEXT_FILES: usize = 10_000;
const DEFAULT_CONTEXT_FILES: usize = 1_000;
const MAX_SOURCE_FILE_BYTES: u64 = 1024 * 1024;
const MAX_DOCUMENT_TEXT_BYTES: usize = 512 * 1024;
const MAX_CONTEXT_SNIPPET_BYTES: usize = 2 * 1024;
const MAX_CONTEXT_TITLE_BYTES: usize = 256;
const MAX_CONTEXT_SEARCH_RESULTS: usize = 100;

/// Durable, public-safe gates for local deep-recall sources.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RecallSettings {
    /// Import generated summaries beneath `CODEX_HOME/memories`.
    pub include_codex_memories: bool,
    /// Import user/assistant text from approved-root sessions beneath `CODEX_HOME/sessions`.
    pub include_codex_sessions: bool,
}

/// Bounded options for one explicit deep-recall refresh.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextDocumentRefreshOptions {
    /// Maximum source files considered independently for each enabled source.
    pub max_files: usize,
}

impl Default for ContextDocumentRefreshOptions {
    fn default() -> Self {
        Self {
            max_files: DEFAULT_CONTEXT_FILES,
        }
    }
}

/// Stable deep-recall source identity.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextSourceKind {
    /// Generated Codex memory summary.
    CodexMemory,
    /// Opted-in raw Codex session message projection.
    CodexSession,
}

impl ContextSourceKind {
    const fn as_str(self) -> &'static str {
        match self {
            Self::CodexMemory => "codex_memory",
            Self::CodexSession => "codex_session",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "codex_memory" => Some(Self::CodexMemory),
            "codex_session" => Some(Self::CodexSession),
            _ => None,
        }
    }
}

/// Whether a source-specific atomic rebuild changed durable context rows.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextSourceRefreshStatus {
    /// Durable rows already matched the bounded source fingerprints.
    Unchanged,
    /// Source rows and FTS entries were atomically replaced or removed.
    Updated,
    /// The enabled source could not be observed authoritatively, so prior rows were retained.
    DeferredUnavailable,
    /// The file cap prevented an authoritative rebuild, so prior rows were retained.
    DeferredTruncated,
}

/// Privacy and bounds accounting for one deep-recall source.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSourceRefreshReport {
    /// Source represented by this report.
    pub source_kind: ContextSourceKind,
    /// Durable opt-in state used by the refresh.
    pub enabled: bool,
    /// Atomic rebuild decision.
    pub status: ContextSourceRefreshStatus,
    /// Candidate regular files admitted by this source's file budget.
    pub files_considered: usize,
    /// Documents prepared from this observation; deferred statuses retain prior rows.
    pub documents: usize,
    /// Prepared text bytes after message-only extraction and bounds.
    pub imported_bytes: usize,
    /// Files skipped because their source size exceeded 1 MiB.
    pub skipped_oversized: usize,
    /// Files with no valid document structure or admitted text.
    pub skipped_malformed_or_empty: usize,
    /// Session files rejected because their cwd was outside every approved scan root.
    pub skipped_outside_roots: usize,
    /// Malformed JSONL records ignored inside otherwise usable session files.
    pub malformed_lines: usize,
    /// Whether this source's configurable file budget stopped discovery.
    pub truncated: bool,
}

/// Result of rebuilding opted-in local context sources.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextDocumentRefreshReport {
    /// Settings snapshot revalidated inside the write transaction.
    pub settings: RecallSettings,
    /// Configured per-source file budget.
    pub max_files: usize,
    /// Generated-memory refresh accounting.
    pub memories: ContextSourceRefreshReport,
    /// Raw-session refresh accounting.
    pub sessions: ContextSourceRefreshReport,
}

/// Bounded public projection of a private deep-recall search hit.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextDocumentSearchResult {
    /// Stable local database identity.
    pub document_id: i64,
    /// Provenance source kind.
    pub source_kind: ContextSourceKind,
    /// Source path relative to the explicit Codex home.
    pub source_path: PathBuf,
    /// Conservative registered-project relationship, when one exists.
    pub project_id: Option<i64>,
    /// Registered project path, when one exists.
    pub project_path: Option<PathBuf>,
    /// Approved session cwd; absent for generated memories.
    pub context_path: Option<PathBuf>,
    /// Derived title.
    pub title: String,
    /// FTS-generated context bounded to 2 KiB.
    pub snippet: String,
    /// Stable content/provenance fingerprint.
    pub fingerprint: String,
    /// SQLite FTS rank; lower values are stronger matches.
    pub rank: f64,
}

#[derive(Debug)]
struct ContextDocument {
    source_kind: ContextSourceKind,
    source_path: PathBuf,
    project_id: Option<i64>,
    context_path: Option<PathBuf>,
    fingerprint: String,
    title: String,
    body: String,
    imported_bytes: usize,
}

#[derive(Default)]
struct SourceAccounting {
    files_considered: usize,
    skipped_oversized: usize,
    skipped_malformed_or_empty: usize,
    skipped_outside_roots: usize,
    malformed_lines: usize,
    truncated: bool,
    unavailable: bool,
}

struct CollectedFiles {
    paths: Vec<PathBuf>,
    truncated: bool,
    unavailable: bool,
}

struct SourceFile {
    path: PathBuf,
    modified: SystemTime,
}

enum BoundedFile {
    Bytes(Vec<u8>),
    Oversized,
}

impl Registry {
    /// Read durable deep-recall gates. A new database returns both disabled.
    pub fn get_recall_settings(&self) -> Result<RecallSettings> {
        recall_settings_on(&self.connection)
    }

    /// Persist explicit deep-recall gates without reading or deleting source content.
    pub fn set_recall_settings(&self, settings: RecallSettings) -> Result<RecallSettings> {
        self.connection.execute(
            "INSERT INTO recall_settings (
                id, include_codex_memories, include_codex_sessions, updated_at
             ) VALUES (1, ?1, ?2, ?3)
             ON CONFLICT(id) DO UPDATE SET
                include_codex_memories = excluded.include_codex_memories,
                include_codex_sessions = excluded.include_codex_sessions,
                updated_at = excluded.updated_at",
            params![
                i64::from(settings.include_codex_memories),
                i64::from(settings.include_codex_sessions),
                unix_timestamp()
            ],
        )?;
        self.get_recall_settings()
    }

    /// Rebuild enabled Codex memory/session sources using one atomic private-state update.
    pub fn refresh_context_documents(
        &mut self,
        codex_home: &Path,
        options: ContextDocumentRefreshOptions,
    ) -> Result<ContextDocumentRefreshReport> {
        if options.max_files == 0 || options.max_files > MAX_CONTEXT_FILES {
            return Err(Error::InvalidRecallFileLimit {
                found: options.max_files,
                maximum: MAX_CONTEXT_FILES,
            });
        }
        let settings = self.get_recall_settings()?;
        let projects = self.list_projects()?;
        let approved_roots = self
            .list_scan_roots()?
            .into_iter()
            .map(|root| root.path)
            .collect::<Vec<_>>();
        let (memory_documents, memory_accounting) = if settings.include_codex_memories {
            let mut remaining = options.max_files;
            collect_memory_documents(codex_home, &projects, &mut remaining)?
        } else {
            (Vec::new(), SourceAccounting::default())
        };
        let (session_documents, mut session_accounting) = if settings.include_codex_sessions {
            let mut remaining = options.max_files;
            collect_session_documents(codex_home, &approved_roots, &projects, &mut remaining)?
        } else {
            (Vec::new(), SourceAccounting::default())
        };
        if settings.include_codex_sessions
            && existing_sessions_use_unavailable_roots(&self.connection, &approved_roots)?
        {
            session_accounting.unavailable = true;
        }

        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let current_settings = recall_settings_on(&transaction)?;
        if current_settings != settings {
            return Err(Error::ControlStateConflict(
                "recall settings changed during source refresh; retry with the current settings"
                    .to_owned(),
            ));
        }
        if settings.include_codex_sessions && scan_root_paths_on(&transaction)? != approved_roots {
            return Err(Error::ControlStateConflict(
                "approved scan roots changed during session refresh; retry with current roots"
                    .to_owned(),
            ));
        }
        let memory_status = if settings.include_codex_memories && memory_accounting.unavailable {
            ContextSourceRefreshStatus::DeferredUnavailable
        } else if settings.include_codex_memories && memory_accounting.truncated {
            ContextSourceRefreshStatus::DeferredTruncated
        } else {
            replace_context_source(
                &transaction,
                ContextSourceKind::CodexMemory,
                &memory_documents,
            )?
        };
        let session_status = if settings.include_codex_sessions && session_accounting.unavailable {
            ContextSourceRefreshStatus::DeferredUnavailable
        } else if settings.include_codex_sessions && session_accounting.truncated {
            ContextSourceRefreshStatus::DeferredTruncated
        } else {
            replace_context_source(
                &transaction,
                ContextSourceKind::CodexSession,
                &session_documents,
            )?
        };
        transaction.commit()?;

        Ok(ContextDocumentRefreshReport {
            settings,
            max_files: options.max_files,
            memories: source_report(
                ContextSourceKind::CodexMemory,
                settings.include_codex_memories,
                memory_status,
                memory_accounting,
                &memory_documents,
            ),
            sessions: source_report(
                ContextSourceKind::CodexSession,
                settings.include_codex_sessions,
                session_status,
                session_accounting,
                &session_documents,
            ),
        })
    }

    /// Search opted-in private context without exposing complete stored bodies.
    pub fn search_context_documents(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<ContextDocumentSearchResult>> {
        let Some(query) = context_fts_query(query) else {
            return Ok(Vec::new());
        };
        let limit = limit.clamp(1, MAX_CONTEXT_SEARCH_RESULTS);
        let mut statement = self.connection.prepare(
            "SELECT d.id, d.source_kind, d.source_path, d.project_id, p.path,
                    d.context_path, d.title,
                    snippet(context_documents_fts, 1, '[', ']', '...', 24),
                    d.fingerprint, bm25(context_documents_fts)
               FROM context_documents_fts
               JOIN context_documents d ON d.id = context_documents_fts.rowid
               LEFT JOIN projects p ON p.id = d.project_id
              WHERE context_documents_fts MATCH ?1
              ORDER BY bm25(context_documents_fts), d.source_kind, d.source_path
              LIMIT ?2",
        )?;
        statement
            .query_map(
                params![query, i64::try_from(limit).unwrap_or(i64::MAX)],
                |row| {
                    let source: String = row.get(1)?;
                    let source_kind = ContextSourceKind::parse(&source).ok_or_else(|| {
                        rusqlite::Error::InvalidColumnType(
                            1,
                            "source_kind".to_owned(),
                            rusqlite::types::Type::Text,
                        )
                    })?;
                    let mut snippet: String = row.get(7)?;
                    truncate_context_utf8(&mut snippet, MAX_CONTEXT_SNIPPET_BYTES);
                    Ok(ContextDocumentSearchResult {
                        document_id: row.get(0)?,
                        source_kind,
                        source_path: PathBuf::from(row.get::<_, String>(2)?),
                        project_id: row.get(3)?,
                        project_path: row.get::<_, Option<String>>(4)?.map(PathBuf::from),
                        context_path: row.get::<_, Option<String>>(5)?.map(PathBuf::from),
                        title: row.get(6)?,
                        snippet,
                        fingerprint: row.get(8)?,
                        rank: row.get(9)?,
                    })
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Error::from)
    }
}

fn recall_settings_on(connection: &rusqlite::Connection) -> Result<RecallSettings> {
    connection
        .query_row(
            "SELECT include_codex_memories, include_codex_sessions
               FROM recall_settings WHERE id = 1",
            [],
            |row| {
                Ok(RecallSettings {
                    include_codex_memories: row.get::<_, i64>(0)? != 0,
                    include_codex_sessions: row.get::<_, i64>(1)? != 0,
                })
            },
        )
        .map_err(Error::from)
}

fn scan_root_paths_on(connection: &rusqlite::Connection) -> Result<Vec<PathBuf>> {
    let mut statement = connection.prepare("SELECT path FROM scan_roots ORDER BY path")?;
    statement
        .query_map([], |row| row.get::<_, String>(0).map(PathBuf::from))?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Error::from)
}

fn source_report(
    source_kind: ContextSourceKind,
    enabled: bool,
    status: ContextSourceRefreshStatus,
    accounting: SourceAccounting,
    documents: &[ContextDocument],
) -> ContextSourceRefreshReport {
    ContextSourceRefreshReport {
        source_kind,
        enabled,
        status,
        files_considered: accounting.files_considered,
        documents: documents.len(),
        imported_bytes: documents
            .iter()
            .map(|document| document.imported_bytes)
            .sum(),
        skipped_oversized: accounting.skipped_oversized,
        skipped_malformed_or_empty: accounting.skipped_malformed_or_empty,
        skipped_outside_roots: accounting.skipped_outside_roots,
        malformed_lines: accounting.malformed_lines,
        truncated: accounting.truncated,
    }
}

fn existing_sessions_use_unavailable_roots(
    connection: &rusqlite::Connection,
    approved_roots: &[PathBuf],
) -> Result<bool> {
    let unavailable = approved_roots
        .iter()
        .filter(|root| !root.is_dir())
        .collect::<Vec<_>>();
    if unavailable.is_empty() {
        return Ok(false);
    }
    let mut statement = connection.prepare(
        "SELECT context_path FROM context_documents
          WHERE source_kind = 'codex_session' AND context_path IS NOT NULL",
    )?;
    let paths = statement
        .query_map([], |row| row.get::<_, String>(0).map(PathBuf::from))?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(paths
        .iter()
        .any(|path| unavailable.iter().any(|root| path.starts_with(root))))
}

fn replace_context_source(
    transaction: &Transaction<'_>,
    source_kind: ContextSourceKind,
    documents: &[ContextDocument],
) -> Result<ContextSourceRefreshStatus> {
    let mut statement = transaction.prepare(
        "SELECT source_path, fingerprint, project_id, context_path
           FROM context_documents WHERE source_kind = ?1 ORDER BY source_path",
    )?;
    let existing = statement
        .query_map([source_kind.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    drop(statement);
    let requested = documents
        .iter()
        .map(|document| {
            (
                document.source_path.to_string_lossy().into_owned(),
                document.fingerprint.clone(),
                document.project_id,
                document
                    .context_path
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned()),
            )
        })
        .collect::<Vec<_>>();
    if existing == requested {
        return Ok(ContextSourceRefreshStatus::Unchanged);
    }

    transaction.execute(
        "DELETE FROM context_documents_fts
          WHERE rowid IN (SELECT id FROM context_documents WHERE source_kind = ?1)",
        [source_kind.as_str()],
    )?;
    transaction.execute(
        "DELETE FROM context_documents WHERE source_kind = ?1",
        [source_kind.as_str()],
    )?;
    for document in documents {
        transaction.execute(
            "INSERT INTO context_documents (
                source_kind, source_path, project_id, context_path, fingerprint,
                refreshed_at, title, body, imported_bytes
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                document.source_kind.as_str(),
                document.source_path.to_string_lossy(),
                document.project_id,
                document
                    .context_path
                    .as_ref()
                    .map(|path| path.to_string_lossy()),
                document.fingerprint,
                unix_timestamp(),
                document.title,
                document.body,
                i64::try_from(document.imported_bytes).unwrap_or(i64::MAX),
            ],
        )?;
        let id = transaction.last_insert_rowid();
        transaction.execute(
            "INSERT INTO context_documents_fts(rowid, title, body) VALUES (?1, ?2, ?3)",
            params![id, document.title, document.body],
        )?;
    }
    Ok(ContextSourceRefreshStatus::Updated)
}

fn collect_memory_documents(
    codex_home: &Path,
    projects: &[Project],
    remaining: &mut usize,
) -> Result<(Vec<ContextDocument>, SourceAccounting)> {
    let collected =
        collect_source_files(codex_home, &codex_home.join("memories"), "md", remaining)?;
    let mut accounting = SourceAccounting {
        files_considered: collected.paths.len(),
        truncated: collected.truncated,
        unavailable: collected.unavailable,
        ..SourceAccounting::default()
    };
    let mut documents = Vec::new();
    for path in collected.paths {
        let bytes = match read_bounded_source(&path)? {
            BoundedFile::Bytes(bytes) => bytes,
            BoundedFile::Oversized => {
                accounting.skipped_oversized += 1;
                continue;
            }
        };
        if bytes.is_empty() {
            accounting.skipped_malformed_or_empty += 1;
            continue;
        }
        let source_path = relative_source_path(codex_home, &path)?;
        let mut body = String::from_utf8_lossy(&bytes).into_owned();
        truncate_context_utf8(&mut body, MAX_DOCUMENT_TEXT_BYTES);
        if body.trim().is_empty() {
            accounting.skipped_malformed_or_empty += 1;
            continue;
        }
        let imported_bytes = body.len();
        let title = first_heading(&body).unwrap_or_else(|| source_title(&source_path));
        let (project_id, context_path) = memory_project_context(&body, projects);
        documents.push(ContextDocument {
            source_kind: ContextSourceKind::CodexMemory,
            source_path,
            project_id,
            context_path,
            fingerprint: content_fingerprint(ContextSourceKind::CodexMemory, &path, &bytes),
            title,
            body,
            imported_bytes,
        });
    }
    documents.sort_by(|left, right| left.source_path.cmp(&right.source_path));
    Ok((documents, accounting))
}

fn collect_session_documents(
    codex_home: &Path,
    approved_roots: &[PathBuf],
    projects: &[Project],
    remaining: &mut usize,
) -> Result<(Vec<ContextDocument>, SourceAccounting)> {
    let collected =
        collect_source_files(codex_home, &codex_home.join("sessions"), "jsonl", remaining)?;
    let mut accounting = SourceAccounting {
        files_considered: collected.paths.len(),
        truncated: collected.truncated,
        unavailable: collected.unavailable,
        ..SourceAccounting::default()
    };
    let mut documents = Vec::new();
    for path in collected.paths {
        let bytes = match read_bounded_source(&path)? {
            BoundedFile::Bytes(bytes) => bytes,
            BoundedFile::Oversized => {
                accounting.skipped_oversized += 1;
                continue;
            }
        };
        let parsed = parse_session_document(&bytes);
        accounting.malformed_lines += parsed.malformed_lines;
        let Some(recorded_cwd) = parsed.cwd else {
            accounting.skipped_malformed_or_empty += 1;
            continue;
        };
        let lexical_roots = approved_roots
            .iter()
            .filter(|root| recorded_cwd.starts_with(root))
            .collect::<Vec<_>>();
        if lexical_roots.is_empty() {
            accounting.skipped_outside_roots += 1;
            continue;
        }
        let Some(cwd) = canonical_existing_directory(&recorded_cwd) else {
            accounting.unavailable = true;
            continue;
        };
        let authorized = approved_roots
            .iter()
            .filter_map(|root| canonical_existing_directory(root))
            .any(|root| cwd.starts_with(root));
        if !authorized {
            if lexical_roots.iter().any(|root| !root.is_dir()) {
                accounting.unavailable = true;
            } else {
                accounting.skipped_outside_roots += 1;
            }
            continue;
        }
        if parsed.body.trim().is_empty() {
            accounting.skipped_malformed_or_empty += 1;
            continue;
        }
        let source_path = relative_source_path(codex_home, &path)?;
        let project_id = projects
            .iter()
            .filter_map(|project| {
                canonical_existing_directory(&project.path)
                    .filter(|path| cwd.starts_with(path))
                    .map(|path| (project.id, path.components().count()))
            })
            .max_by_key(|(_, depth)| *depth)
            .map(|(id, _)| id);
        let title = parsed
            .first_user_text
            .map(|value| format!("Codex session: {}", compact_title(&value, 120)))
            .unwrap_or_else(|| format!("Codex session in {}", cwd.display()));
        let imported_bytes = parsed.body.len();
        documents.push(ContextDocument {
            source_kind: ContextSourceKind::CodexSession,
            source_path,
            project_id,
            context_path: Some(cwd),
            fingerprint: content_fingerprint(ContextSourceKind::CodexSession, &path, &bytes),
            title,
            body: parsed.body,
            imported_bytes,
        });
    }
    documents.sort_by(|left, right| left.source_path.cmp(&right.source_path));
    Ok((documents, accounting))
}

struct ParsedSession {
    cwd: Option<PathBuf>,
    body: String,
    first_user_text: Option<String>,
    malformed_lines: usize,
}

fn parse_session_document(bytes: &[u8]) -> ParsedSession {
    let mut cwd = None;
    let mut body = String::new();
    let mut first_user_text = None;
    let mut malformed_lines = 0;
    for line in bytes.split(|byte| *byte == b'\n') {
        if line.iter().all(u8::is_ascii_whitespace) {
            continue;
        }
        let value = match serde_json::from_slice::<serde_json::Value>(line) {
            Ok(value) => value,
            Err(_) => {
                malformed_lines += 1;
                continue;
            }
        };
        let Some(payload) = value.get("payload").and_then(serde_json::Value::as_object) else {
            continue;
        };
        if value.get("type").and_then(serde_json::Value::as_str) == Some("session_meta") {
            if let Some(path) = payload
                .get("cwd")
                .and_then(serde_json::Value::as_str)
                .and_then(safe_absolute_path)
            {
                cwd = Some(path);
            }
            continue;
        }
        if value.get("type").and_then(serde_json::Value::as_str) != Some("response_item")
            || payload.get("type").and_then(serde_json::Value::as_str) != Some("message")
        {
            continue;
        }
        let Some(role) = payload.get("role").and_then(serde_json::Value::as_str) else {
            continue;
        };
        if !matches!(role, "user" | "assistant") {
            continue;
        }
        let Some(content) = payload.get("content").and_then(serde_json::Value::as_array) else {
            continue;
        };
        for item in content {
            let Some(kind) = item.get("type").and_then(serde_json::Value::as_str) else {
                continue;
            };
            if !matches!(kind, "input_text" | "output_text" | "text") {
                continue;
            }
            let Some(text) = item.get("text").and_then(serde_json::Value::as_str) else {
                continue;
            };
            let text = text.trim();
            if text.is_empty() {
                continue;
            }
            if role == "user" && first_user_text.is_none() {
                first_user_text = Some(text.to_owned());
            }
            append_context_message(&mut body, role, text);
            if body.len() >= MAX_DOCUMENT_TEXT_BYTES {
                break;
            }
        }
        if body.len() >= MAX_DOCUMENT_TEXT_BYTES {
            break;
        }
    }
    ParsedSession {
        cwd,
        body,
        first_user_text,
        malformed_lines,
    }
}

fn append_context_message(body: &mut String, role: &str, text: &str) {
    if body.len() >= MAX_DOCUMENT_TEXT_BYTES {
        return;
    }
    let separator = if body.is_empty() { "" } else { "\n\n" };
    let prefix = format!("{separator}{role}: ");
    let remaining = MAX_DOCUMENT_TEXT_BYTES.saturating_sub(body.len());
    if prefix.len() >= remaining {
        return;
    }
    body.push_str(&prefix);
    let remaining = MAX_DOCUMENT_TEXT_BYTES.saturating_sub(body.len());
    let mut admitted = text.to_owned();
    truncate_context_utf8(&mut admitted, remaining);
    body.push_str(&admitted);
}

fn collect_source_files(
    codex_home: &Path,
    root: &Path,
    extension: &str,
    remaining: &mut usize,
) -> Result<CollectedFiles> {
    if !is_real_directory(root) {
        return Ok(CollectedFiles {
            paths: Vec::new(),
            truncated: false,
            unavailable: true,
        });
    }
    let mut queue = VecDeque::from([root.to_path_buf()]);
    let mut candidates = Vec::new();
    while let Some(directory) = queue.pop_front() {
        let entries = fs::read_dir(&directory).map_err(|source| Error::Io {
            path: directory.clone(),
            source,
        })?;
        let mut entries = entries
            .collect::<std::io::Result<Vec<_>>>()
            .map_err(|source| Error::Io {
                path: directory.clone(),
                source,
            })?;
        entries.sort_by_key(fs::DirEntry::file_name);
        for entry in entries {
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).map_err(|source| Error::Io {
                path: path.clone(),
                source,
            })?;
            let file_type = metadata.file_type();
            if file_type.is_symlink() {
                continue;
            }
            if file_type.is_dir() {
                queue.push_back(path);
                continue;
            }
            if !file_type.is_file()
                || path.extension().and_then(|value| value.to_str()) != Some(extension)
            {
                continue;
            }
            if path.strip_prefix(codex_home).is_err() {
                continue;
            }
            candidates.push(SourceFile {
                path,
                modified: metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            });
        }
    }
    candidates.sort_by(|left, right| {
        right
            .modified
            .cmp(&left.modified)
            .then_with(|| left.path.cmp(&right.path))
    });
    let admitted = candidates.len().min(*remaining);
    let truncated = candidates.len() > admitted;
    let mut paths = candidates
        .into_iter()
        .take(admitted)
        .map(|candidate| candidate.path)
        .collect::<Vec<_>>();
    *remaining -= admitted;
    paths.sort();
    Ok(CollectedFiles {
        paths,
        truncated,
        unavailable: false,
    })
}

fn is_real_directory(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_dir())
}

fn read_bounded_source(path: &Path) -> Result<BoundedFile> {
    let metadata = fs::symlink_metadata(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if !metadata.file_type().is_file() || metadata.len() > MAX_SOURCE_FILE_BYTES {
        return Ok(BoundedFile::Oversized);
    }
    let mut bytes = Vec::with_capacity(usize::try_from(metadata.len()).unwrap_or(0));
    File::open(path)
        .map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?
        .take(MAX_SOURCE_FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
    if bytes.len() > usize::try_from(MAX_SOURCE_FILE_BYTES).unwrap_or(usize::MAX) {
        return Ok(BoundedFile::Oversized);
    }
    Ok(BoundedFile::Bytes(bytes))
}

fn relative_source_path(codex_home: &Path, path: &Path) -> Result<PathBuf> {
    path.strip_prefix(codex_home)
        .map(Path::to_path_buf)
        .map_err(|_| {
            Error::InvalidControlRequest(
                "context source escaped the explicit Codex home".to_owned(),
            )
        })
}

fn safe_absolute_path(value: &str) -> Option<PathBuf> {
    let path = Path::new(value);
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
    {
        return None;
    }
    Some(path.components().collect())
}

fn canonical_existing_directory(path: &Path) -> Option<PathBuf> {
    path.is_dir().then(|| fs::canonicalize(path).ok()).flatten()
}

fn memory_project_context(body: &str, projects: &[Project]) -> (Option<i64>, Option<PathBuf>) {
    let matches = body
        .lines()
        .filter_map(memory_metadata_path)
        .filter_map(|context_path| {
            projects
                .iter()
                .filter(|project| context_path.starts_with(&project.path))
                .map(|project| (project.id, project.path.components().count()))
                .max_by_key(|(_, depth)| *depth)
                .map(|(project_id, depth)| (project_id, context_path, depth))
        })
        .collect::<Vec<_>>();
    let project_id = matches.first().map(|(project_id, _, _)| *project_id);
    if project_id.is_none()
        || matches
            .iter()
            .any(|(candidate, _, _)| Some(*candidate) != project_id)
    {
        return (None, None);
    }
    matches
        .into_iter()
        .max_by_key(|(_, _, depth)| *depth)
        .map_or((None, None), |(project_id, context_path, _)| {
            (Some(project_id), Some(context_path))
        })
}

fn memory_metadata_path(line: &str) -> Option<PathBuf> {
    let trimmed = line.trim();
    if let Some(value) = trimmed
        .strip_prefix("<cwd>")
        .and_then(|value| value.strip_suffix("</cwd>"))
    {
        return clean_metadata_path(value);
    }
    let markdown = trimmed.trim_start_matches(['-', '*', '>']).trim_start();
    if let Some((key, value)) = markdown
        .split_once(':')
        .or_else(|| markdown.split_once('='))
    {
        let key = key.trim().trim_matches(['*', '`']).to_ascii_lowercase();
        if matches!(
            key.as_str(),
            "cwd" | "project_path" | "project path" | "working_directory" | "working directory"
        ) {
            return clean_metadata_path(value);
        }
    }
    if let Some(heading) = markdown.strip_prefix("#### ") {
        return clean_metadata_path(heading);
    }
    serde_json::from_str::<serde_json::Value>(trimmed)
        .ok()
        .and_then(|value| {
            ["cwd", "project_path"]
                .into_iter()
                .find_map(|key| value.get(key).and_then(serde_json::Value::as_str))
                .or_else(|| {
                    value.get("payload").and_then(|payload| {
                        ["cwd", "project_path"]
                            .into_iter()
                            .find_map(|key| payload.get(key).and_then(serde_json::Value::as_str))
                    })
                })
                .and_then(clean_metadata_path)
        })
}

fn clean_metadata_path(value: &str) -> Option<PathBuf> {
    let mut value = value.trim().trim_end_matches([',', ';']);
    for _ in 0..2 {
        value = value.trim().trim_matches(['*', '`', '"']);
    }
    safe_absolute_path(value)
}

fn first_heading(text: &str) -> Option<String> {
    text.lines().find_map(|line| {
        line.trim()
            .strip_prefix("# ")
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(|value| compact_title(value, MAX_CONTEXT_TITLE_BYTES))
    })
}

fn source_title(path: &Path) -> String {
    let value = path
        .file_stem()
        .and_then(|value| value.to_str())
        .filter(|value| !value.is_empty())
        .map_or("Codex memory", |value| value);
    compact_title(value, MAX_CONTEXT_TITLE_BYTES)
}

fn compact_title(value: &str, maximum: usize) -> String {
    let mut compact = value.split_whitespace().collect::<Vec<_>>().join(" ");
    truncate_context_utf8(&mut compact, maximum);
    compact
}

fn content_fingerprint(kind: ContextSourceKind, path: &Path, bytes: &[u8]) -> String {
    let mut fingerprint = ContextFingerprint::new();
    fingerprint.write(kind.as_str().as_bytes());
    fingerprint.write(path.file_name().unwrap_or_default().as_encoded_bytes());
    fingerprint.write(bytes);
    fingerprint.finish()
}

fn context_fts_query(query: &str) -> Option<String> {
    let tokens = query
        .split_whitespace()
        .map(|value| {
            value
                .chars()
                .filter(|character| {
                    character.is_alphanumeric() || *character == '_' || *character == '-'
                })
                .collect::<String>()
        })
        .filter(|value| !value.is_empty())
        .take(32)
        .map(|value| format!("\"{}\"", value.replace('"', "\"\"")))
        .collect::<Vec<_>>();
    (!tokens.is_empty()).then(|| tokens.join(" OR "))
}

fn truncate_context_utf8(value: &mut String, maximum: usize) {
    if value.len() <= maximum {
        return;
    }
    let mut boundary = maximum;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
}

struct ContextFingerprint(u64);

impl ContextFingerprint {
    const fn new() -> Self {
        Self(0xcbf2_9ce4_8422_2325)
    }

    fn write(&mut self, bytes: &[u8]) {
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x0000_0100_0000_01b3);
        }
    }

    fn finish(&self) -> String {
        format!("fnv1a64:{:016x}", self.0)
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;
    use crate::ScanRootOptions;
    use crate::SkeinPaths;

    fn registry() -> Result<(TempDir, Registry, PathBuf)> {
        let temp = tempfile::tempdir().map_err(|source| Error::Io {
            path: PathBuf::from("synthetic context test directory"),
            source,
        })?;
        let paths = SkeinPaths::new(temp.path().join("config"), temp.path().join("data"));
        let codex_home = temp.path().join("codex-home");
        fs::create_dir(&codex_home).map_err(|source| Error::Io {
            path: codex_home.clone(),
            source,
        })?;
        Ok((temp, Registry::open(&paths)?, codex_home))
    }

    fn write(path: &Path, value: &[u8]) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|source| Error::Io {
                path: parent.to_path_buf(),
                source,
            })?;
        }
        fs::write(path, value).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    #[test]
    fn settings_default_off_and_disabling_then_refresh_removes_documents() -> Result<()> {
        let (_temp, mut registry, codex_home) = registry()?;
        write(
            &codex_home.join("memories/nested/summary.md"),
            b"# Synthetic memory\nmemory-private-marker",
        )?;
        assert_eq!(registry.get_recall_settings()?, RecallSettings::default());

        let disabled = registry
            .refresh_context_documents(&codex_home, ContextDocumentRefreshOptions::default())?;
        assert!(!disabled.memories.enabled);
        assert_eq!(disabled.memories.files_considered, 0);
        assert!(
            registry
                .search_context_documents("memory-private-marker", 10)?
                .is_empty()
        );

        registry.set_recall_settings(RecallSettings {
            include_codex_memories: true,
            include_codex_sessions: false,
        })?;
        let enabled = registry
            .refresh_context_documents(&codex_home, ContextDocumentRefreshOptions::default())?;
        assert_eq!(enabled.memories.status, ContextSourceRefreshStatus::Updated);
        assert_eq!(enabled.memories.documents, 1);
        let hits = registry.search_context_documents("memory-private-marker", 10)?;
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].source_kind, ContextSourceKind::CodexMemory);
        assert_eq!(
            hits[0].source_path,
            PathBuf::from("memories/nested/summary.md")
        );
        assert!(hits[0].snippet.len() <= MAX_CONTEXT_SNIPPET_BYTES);

        let unchanged = registry
            .refresh_context_documents(&codex_home, ContextDocumentRefreshOptions::default())?;
        assert_eq!(
            unchanged.memories.status,
            ContextSourceRefreshStatus::Unchanged
        );
        registry.set_recall_settings(RecallSettings::default())?;
        let removed = registry
            .refresh_context_documents(&codex_home, ContextDocumentRefreshOptions::default())?;
        assert_eq!(removed.memories.status, ContextSourceRefreshStatus::Updated);
        assert_eq!(removed.memories.documents, 0);
        assert!(
            registry
                .search_context_documents("memory-private-marker", 10)?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn sessions_require_an_approved_root_and_extract_only_message_text() -> Result<()> {
        let (temp, mut registry, codex_home) = registry()?;
        let approved = temp.path().join("approved");
        let project = approved.join("project");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&project).map_err(|source| Error::Io {
            path: project.clone(),
            source,
        })?;
        fs::create_dir(&outside).map_err(|source| Error::Io {
            path: outside.clone(),
            source,
        })?;
        let registered = registry.add_project(&project, Some("Approved project"))?;
        registry.add_scan_root(
            &approved,
            ScanRootOptions {
                recursive: true,
                max_depth: Some(4),
            },
        )?;
        let inside = format!(
            "not-json\n{{\"type\":\"session_meta\",\"payload\":{{\"cwd\":{cwd:?}}}}}\n\
             {{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"developer\",\"content\":[{{\"type\":\"input_text\",\"text\":\"developer-secret-marker\"}}]}}}}\n\
             {{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"user-session-marker\"}}]}}}}\n\
             {{\"type\":\"response_item\",\"payload\":{{\"type\":\"function_call_output\",\"output\":\"tool-secret-marker\"}}}}\n\
             {{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{{\"type\":\"output_text\",\"text\":\"assistant-session-marker\"}}]}}}}\n",
            cwd = project.to_string_lossy()
        );
        let outside_session = format!(
            "{{\"type\":\"session_meta\",\"payload\":{{\"cwd\":{cwd:?}}}}}\n\
             {{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"outside-session-marker\"}}]}}}}\n",
            cwd = outside.to_string_lossy()
        );
        write(
            &codex_home.join("sessions/2026/inside.jsonl"),
            inside.as_bytes(),
        )?;
        write(
            &codex_home.join("sessions/2026/outside.jsonl"),
            outside_session.as_bytes(),
        )?;
        registry.set_recall_settings(RecallSettings {
            include_codex_memories: false,
            include_codex_sessions: true,
        })?;

        let report = registry
            .refresh_context_documents(&codex_home, ContextDocumentRefreshOptions::default())?;
        assert_eq!(report.sessions.documents, 1);
        assert_eq!(report.sessions.skipped_outside_roots, 1);
        assert_eq!(report.sessions.malformed_lines, 1);
        for marker in ["user-session-marker", "assistant-session-marker"] {
            let hits = registry.search_context_documents(marker, 10)?;
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].project_id, Some(registered.id));
            assert_eq!(hits[0].context_path.as_deref(), Some(project.as_path()));
        }
        for marker in [
            "developer-secret-marker",
            "tool-secret-marker",
            "outside-session-marker",
        ] {
            assert!(registry.search_context_documents(marker, 10)?.is_empty());
        }
        Ok(())
    }

    #[test]
    fn enforces_source_text_and_file_count_bounds() -> Result<()> {
        let (_temp, mut registry, codex_home) = registry()?;
        write(
            &codex_home.join("memories/00-large.md"),
            &vec![b'a'; MAX_DOCUMENT_TEXT_BYTES + 128 * 1024],
        )?;
        write(
            &codex_home.join("memories/01-oversized.md"),
            &vec![b'b'; usize::try_from(MAX_SOURCE_FILE_BYTES + 1).unwrap_or(usize::MAX)],
        )?;
        write(
            &codex_home.join("memories/02-extra.md"),
            b"extra-budget-marker",
        )?;
        registry.set_recall_settings(RecallSettings {
            include_codex_memories: true,
            include_codex_sessions: false,
        })?;
        assert!(matches!(
            registry.refresh_context_documents(
                &codex_home,
                ContextDocumentRefreshOptions { max_files: 0 }
            ),
            Err(Error::InvalidRecallFileLimit { found: 0, .. })
        ));
        assert!(matches!(
            registry.refresh_context_documents(
                &codex_home,
                ContextDocumentRefreshOptions {
                    max_files: MAX_CONTEXT_FILES + 1
                }
            ),
            Err(Error::InvalidRecallFileLimit { .. })
        ));

        let report = registry.refresh_context_documents(
            &codex_home,
            ContextDocumentRefreshOptions { max_files: 3 },
        )?;
        assert_eq!(report.memories.files_considered, 3);
        assert!(!report.memories.truncated);
        assert_eq!(report.memories.documents, 2);
        assert_eq!(report.memories.skipped_oversized, 1);
        assert_eq!(
            report.memories.imported_bytes,
            MAX_DOCUMENT_TEXT_BYTES + b"extra-budget-marker".len()
        );
        let stored_bytes: i64 = registry.connection.query_row(
            "SELECT MAX(length(CAST(body AS BLOB))) FROM context_documents",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(
            stored_bytes,
            i64::try_from(MAX_DOCUMENT_TEXT_BYTES).unwrap_or(i64::MAX)
        );
        assert_eq!(
            registry
                .search_context_documents("extra-budget-marker", 10)?
                .len(),
            1
        );
        Ok(())
    }

    #[test]
    fn source_budget_admits_newest_files_before_stable_path_ties() -> Result<()> {
        let (_temp, _registry, codex_home) = registry()?;
        let root = codex_home.join("memories");
        write(&root.join("z-old.md"), b"old")?;
        std::thread::sleep(std::time::Duration::from_millis(10));
        write(&root.join("a-new.md"), b"new")?;

        let mut remaining = 1;
        let collected = collect_source_files(&codex_home, &root, "md", &mut remaining)?;
        assert!(collected.truncated);
        assert_eq!(remaining, 0);
        assert_eq!(collected.paths, vec![root.join("a-new.md")]);
        Ok(())
    }

    #[test]
    fn truncated_or_unavailable_memory_source_retains_prior_rows() -> Result<()> {
        let (temp, mut registry, codex_home) = registry()?;
        let memories = codex_home.join("memories");
        write(&memories.join("old.md"), b"retained-memory-marker")?;
        registry.set_recall_settings(RecallSettings {
            include_codex_memories: true,
            include_codex_sessions: false,
        })?;
        registry.refresh_context_documents(
            &codex_home,
            ContextDocumentRefreshOptions { max_files: 1 },
        )?;

        std::thread::sleep(std::time::Duration::from_millis(10));
        write(&memories.join("new.md"), b"new-memory-marker")?;
        let truncated = registry.refresh_context_documents(
            &codex_home,
            ContextDocumentRefreshOptions { max_files: 1 },
        )?;
        assert_eq!(
            truncated.memories.status,
            ContextSourceRefreshStatus::DeferredTruncated
        );
        assert_eq!(
            registry
                .search_context_documents("retained-memory-marker", 10)?
                .len(),
            1
        );
        assert!(
            registry
                .search_context_documents("new-memory-marker", 10)?
                .is_empty()
        );

        fs::rename(&memories, temp.path().join("memories-offline")).map_err(|source| {
            Error::Io {
                path: memories.clone(),
                source,
            }
        })?;
        let unavailable = registry.refresh_context_documents(
            &codex_home,
            ContextDocumentRefreshOptions { max_files: 1 },
        )?;
        assert_eq!(
            unavailable.memories.status,
            ContextSourceRefreshStatus::DeferredUnavailable
        );
        assert_eq!(
            registry
                .search_context_documents("retained-memory-marker", 10)?
                .len(),
            1
        );
        Ok(())
    }

    #[test]
    fn enabled_sources_have_independent_file_budgets() -> Result<()> {
        let (temp, mut registry, codex_home) = registry()?;
        let approved = temp.path().join("approved");
        let project = approved.join("project");
        fs::create_dir_all(&project).map_err(|source| Error::Io {
            path: project.clone(),
            source,
        })?;
        registry.add_project(&project, None)?;
        registry.add_scan_root(
            &approved,
            ScanRootOptions {
                recursive: true,
                max_depth: Some(4),
            },
        )?;
        write(
            &codex_home.join("memories/one.md"),
            b"independent-memory-budget-marker",
        )?;
        write(
            &codex_home.join("sessions/one.jsonl"),
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"cwd\":{:?}}}}}\n\
                 {{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"independent-session-budget-marker\"}}]}}}}\n",
                project.to_string_lossy()
            )
            .as_bytes(),
        )?;
        registry.set_recall_settings(RecallSettings {
            include_codex_memories: true,
            include_codex_sessions: true,
        })?;

        let report = registry.refresh_context_documents(
            &codex_home,
            ContextDocumentRefreshOptions { max_files: 1 },
        )?;
        assert_eq!(report.memories.documents, 1);
        assert_eq!(report.sessions.documents, 1);
        assert!(!report.memories.truncated);
        assert!(!report.sessions.truncated);
        Ok(())
    }

    #[test]
    fn offline_approved_root_preserves_session_rows_and_memory_routing() -> Result<()> {
        let (temp, mut registry, codex_home) = registry()?;
        let approved = temp.path().join("network-root");
        let project = approved.join("project");
        fs::create_dir_all(&project).map_err(|source| Error::Io {
            path: project.clone(),
            source,
        })?;
        let registered = registry.add_project(&project, None)?;
        registry.add_scan_root(
            &approved,
            ScanRootOptions {
                recursive: true,
                max_depth: Some(4),
            },
        )?;
        write(
            &codex_home.join("memories/routing.md"),
            format!(
                "# Routed memory\nproject_path: {}\noffline-memory-route-marker",
                project.display()
            )
            .as_bytes(),
        )?;
        write(
            &codex_home.join("sessions/routing.jsonl"),
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"cwd\":{:?}}}}}\n\
                 {{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"offline-session-route-marker\"}}]}}}}\n",
                project.to_string_lossy()
            )
            .as_bytes(),
        )?;
        registry.set_recall_settings(RecallSettings {
            include_codex_memories: true,
            include_codex_sessions: true,
        })?;
        registry
            .refresh_context_documents(&codex_home, ContextDocumentRefreshOptions::default())?;

        fs::rename(&approved, temp.path().join("network-root-offline")).map_err(|source| {
            Error::Io {
                path: approved.clone(),
                source,
            }
        })?;
        let report = registry
            .refresh_context_documents(&codex_home, ContextDocumentRefreshOptions::default())?;
        assert_eq!(
            report.sessions.status,
            ContextSourceRefreshStatus::DeferredUnavailable
        );
        for marker in [
            "offline-memory-route-marker",
            "offline-session-route-marker",
        ] {
            let hits = registry.search_context_documents(marker, 10)?;
            assert_eq!(hits.len(), 1);
            assert_eq!(hits[0].project_id, Some(registered.id));
            assert_eq!(hits[0].context_path.as_deref(), Some(project.as_path()));
        }
        Ok(())
    }

    #[test]
    fn generated_memories_map_conservative_paths_to_the_longest_project() -> Result<()> {
        let (temp, mut registry, codex_home) = registry()?;
        let parent = temp.path().join("workspace");
        let nested = parent.join("nested-project");
        let work = nested.join("worktree");
        fs::create_dir_all(&work).map_err(|source| Error::Io {
            path: work.clone(),
            source,
        })?;
        let parent_project = registry.add_project(&parent, Some("Workspace"))?;
        let nested_project = registry.add_project(&nested, Some("Nested"))?;
        write(
            &codex_home.join("memories/metadata.md"),
            format!(
                "# Metadata memory\n- **project_path:** `{}`\nmetadata-route-marker",
                work.display()
            )
            .as_bytes(),
        )?;
        write(
            &codex_home.join("memories/rollout.md"),
            format!(
                "# Rollout summary\n#### {}\nrollout-route-marker",
                parent.display()
            )
            .as_bytes(),
        )?;
        registry.set_recall_settings(RecallSettings {
            include_codex_memories: true,
            include_codex_sessions: false,
        })?;

        registry
            .refresh_context_documents(&codex_home, ContextDocumentRefreshOptions::default())?;
        let metadata = registry.search_context_documents("metadata-route-marker", 10)?;
        assert_eq!(metadata.len(), 1);
        assert_eq!(metadata[0].project_id, Some(nested_project.id));
        assert_eq!(metadata[0].context_path.as_deref(), Some(work.as_path()));
        let rollout = registry.search_context_documents("rollout-route-marker", 10)?;
        assert_eq!(rollout.len(), 1);
        assert_eq!(rollout[0].project_id, Some(parent_project.id));
        assert_eq!(rollout[0].context_path.as_deref(), Some(parent.as_path()));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn raw_session_cwd_symlink_cannot_escape_an_approved_root() -> Result<()> {
        use std::os::unix::fs::symlink;

        let (temp, mut registry, codex_home) = registry()?;
        let approved = temp.path().join("approved");
        let outside = temp.path().join("outside");
        fs::create_dir(&approved).map_err(|source| Error::Io {
            path: approved.clone(),
            source,
        })?;
        fs::create_dir(&outside).map_err(|source| Error::Io {
            path: outside.clone(),
            source,
        })?;
        let escaped = approved.join("escaped");
        symlink(&outside, &escaped).map_err(|source| Error::Io {
            path: escaped.clone(),
            source,
        })?;
        registry.add_scan_root(
            &approved,
            ScanRootOptions {
                recursive: true,
                max_depth: Some(4),
            },
        )?;
        write(
            &codex_home.join("sessions/escape.jsonl"),
            format!(
                "{{\"type\":\"session_meta\",\"payload\":{{\"cwd\":{:?}}}}}\n\
                 {{\"type\":\"response_item\",\"payload\":{{\"type\":\"message\",\"role\":\"user\",\"content\":[{{\"type\":\"input_text\",\"text\":\"symlink-escape-marker\"}}]}}}}\n",
                escaped.to_string_lossy()
            )
            .as_bytes(),
        )?;
        registry.set_recall_settings(RecallSettings {
            include_codex_memories: false,
            include_codex_sessions: true,
        })?;

        let report = registry
            .refresh_context_documents(&codex_home, ContextDocumentRefreshOptions::default())?;
        assert_eq!(report.sessions.documents, 0);
        assert_eq!(report.sessions.skipped_outside_roots, 1);
        assert!(
            registry
                .search_context_documents("symlink-escape-marker", 10)?
                .is_empty()
        );
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn recursive_context_walk_never_follows_symlinks() -> Result<()> {
        use std::os::unix::fs::symlink;

        let (temp, mut registry, codex_home) = registry()?;
        let outside = temp.path().join("outside-memory.md");
        write(&outside, b"symlink-context-secret")?;
        fs::create_dir(codex_home.join("memories")).map_err(|source| Error::Io {
            path: codex_home.join("memories"),
            source,
        })?;
        symlink(&outside, codex_home.join("memories/link.md")).map_err(|source| Error::Io {
            path: codex_home.join("memories/link.md"),
            source,
        })?;
        registry.set_recall_settings(RecallSettings {
            include_codex_memories: true,
            include_codex_sessions: false,
        })?;
        let report = registry
            .refresh_context_documents(&codex_home, ContextDocumentRefreshOptions::default())?;
        assert_eq!(report.memories.documents, 0);
        assert!(
            registry
                .search_context_documents("symlink-context-secret", 10)?
                .is_empty()
        );
        Ok(())
    }
}
