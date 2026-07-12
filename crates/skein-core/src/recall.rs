use std::fs;
use std::fs::File;
use std::io::Read;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::process::Stdio;
use std::time::Duration;
use std::time::Instant;
use std::time::UNIX_EPOCH;

use rusqlite::OptionalExtension;
use rusqlite::TransactionBehavior;
use rusqlite::params;
use serde::Serialize;

use crate::Error;
use crate::Registry;
use crate::Result;
use crate::registry::unix_timestamp;

pub(crate) const CREATE_PROJECT_DOCUMENT_SCHEMA: &str =
    "CREATE TABLE IF NOT EXISTS project_documents (
    project_id INTEGER PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
    fingerprint TEXT NOT NULL,
    refreshed_at INTEGER NOT NULL,
    title TEXT NOT NULL,
    body TEXT NOT NULL,
    source_paths TEXT NOT NULL,
    indexed_bytes INTEGER NOT NULL CHECK(indexed_bytes >= 0 AND indexed_bytes <= 524288)
);
CREATE VIRTUAL TABLE IF NOT EXISTS project_documents_fts USING fts5(title, body);";

const MAX_FILES: usize = 40;
const MAX_FILE_BYTES: u64 = 64 * 1024;
const MAX_PROJECT_BYTES: usize = 512 * 1024;
const MAX_GIT_OUTPUT_BYTES: usize = 1024 * 1024;
const MAX_SNIPPET_BYTES: usize = 2 * 1024;
const GIT_TIMEOUT: Duration = Duration::from_secs(2);
const MAX_SEARCH_RESULTS: usize = 100;
const ROOT_IDENTITY_FILES: &[&str] = &[
    "AGENTS.md",
    "Cargo.toml",
    "pyproject.toml",
    "package.json",
    "go.mod",
];

/// Whether a project-document refresh rewrote the private index.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectDocumentRefreshStatus {
    /// The bounded source fingerprint matched the stored observation.
    Unchanged,
    /// The bounded private document and FTS row were replaced atomically.
    Updated,
}

/// Result of refreshing the bounded identity-document index for one project.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDocumentRefreshReport {
    /// Registered project identity.
    pub project_id: i64,
    /// Canonical registered project path.
    pub project_path: PathBuf,
    /// Derived display title stored in the private index.
    pub title: String,
    /// Relative identity files selected for the document.
    pub source_paths: Vec<PathBuf>,
    /// Raw source bytes admitted before lossy UTF-8 decoding.
    pub indexed_bytes: usize,
    /// Refresh decision.
    pub status: ProjectDocumentRefreshStatus,
}

/// Bounded public projection of one private project-document search hit.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectDocumentSearchResult {
    /// Registered project identity.
    pub project_id: i64,
    /// Canonical registered project path.
    pub project_path: PathBuf,
    /// Derived project title.
    pub title: String,
    /// FTS-generated bounded match context, never the full stored body.
    pub snippet: String,
    /// Relative files which contributed to the indexed document.
    pub source_paths: Vec<PathBuf>,
    /// SQLite FTS rank; lower values are stronger matches.
    pub rank: f64,
}

#[derive(Debug)]
struct CandidateSet {
    source: &'static str,
    paths: Vec<PathBuf>,
}

#[derive(Debug)]
struct SourceDocument {
    path: PathBuf,
    text: String,
    raw_bytes: usize,
}

impl Registry {
    /// Refresh the private FTS document for one explicitly registered project.
    pub fn refresh_project_documents(
        &mut self,
        path: &Path,
    ) -> Result<ProjectDocumentRefreshReport> {
        let project = self.get_project(path)?;
        let candidates = select_candidates(&project.path);
        let fingerprint = fingerprint_candidates(&project.path, &candidates)?;
        let existing = self
            .connection
            .query_row(
                "SELECT fingerprint, title, source_paths, indexed_bytes
                   FROM project_documents WHERE project_id = ?1",
                [project.id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, i64>(3)?,
                    ))
                },
            )
            .optional()?;
        if let Some((stored, title, source_paths, indexed_bytes)) = existing
            && stored == fingerprint
        {
            self.connection.execute(
                "UPDATE project_documents SET refreshed_at = ?1 WHERE project_id = ?2",
                params![unix_timestamp(), project.id],
            )?;
            return Ok(ProjectDocumentRefreshReport {
                project_id: project.id,
                project_path: project.path,
                title,
                source_paths: decode_source_paths(&source_paths),
                indexed_bytes: usize::try_from(indexed_bytes).unwrap_or(0),
                status: ProjectDocumentRefreshStatus::Unchanged,
            });
        }

        let documents = read_documents(&project.path, &candidates.paths)?;
        let title = derive_title(&project.name, &documents);
        let (body, indexed_bytes) = assemble_body(&documents);
        let source_paths = documents
            .iter()
            .map(|document| document.path.clone())
            .collect::<Vec<_>>();
        let encoded_paths = encode_source_paths(&source_paths);
        let refreshed_at = unix_timestamp();
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        transaction.execute(
            "INSERT INTO project_documents (
                project_id, fingerprint, refreshed_at, title, body, source_paths, indexed_bytes
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
             ON CONFLICT(project_id) DO UPDATE SET
                fingerprint = excluded.fingerprint,
                refreshed_at = excluded.refreshed_at,
                title = excluded.title,
                body = excluded.body,
                source_paths = excluded.source_paths,
                indexed_bytes = excluded.indexed_bytes",
            params![
                project.id,
                fingerprint,
                refreshed_at,
                title,
                body,
                encoded_paths,
                i64::try_from(indexed_bytes).unwrap_or(i64::MAX)
            ],
        )?;
        transaction.execute(
            "DELETE FROM project_documents_fts WHERE rowid = ?1",
            [project.id],
        )?;
        transaction.execute(
            "INSERT INTO project_documents_fts(rowid, title, body) VALUES (?1, ?2, ?3)",
            params![project.id, title, body],
        )?;
        transaction.commit()?;

        Ok(ProjectDocumentRefreshReport {
            project_id: project.id,
            project_path: project.path,
            title,
            source_paths,
            indexed_bytes,
            status: ProjectDocumentRefreshStatus::Updated,
        })
    }

    /// Refresh bounded identity documents for every registered project in stable order.
    pub fn refresh_all_project_documents(&mut self) -> Result<Vec<ProjectDocumentRefreshReport>> {
        let paths = self
            .list_projects()?
            .into_iter()
            .map(|project| project.path)
            .collect::<Vec<_>>();
        paths
            .iter()
            .map(|path| self.refresh_project_documents(path))
            .collect()
    }

    /// Search private project identity documents without returning complete stored bodies.
    pub fn search_project_documents(
        &self,
        query: &str,
        limit: usize,
    ) -> Result<Vec<ProjectDocumentSearchResult>> {
        let Some(query) = fts_query(query) else {
            return Ok(Vec::new());
        };
        let limit = limit.clamp(1, MAX_SEARCH_RESULTS);
        let mut statement = self.connection.prepare(
            "SELECT p.id, p.path, d.title,
                    snippet(project_documents_fts, 1, '[', ']', '...', 24),
                    d.source_paths, bm25(project_documents_fts)
               FROM project_documents_fts
               JOIN project_documents d ON d.project_id = project_documents_fts.rowid
               JOIN projects p ON p.id = d.project_id
              WHERE project_documents_fts MATCH ?1
              ORDER BY bm25(project_documents_fts), p.name COLLATE NOCASE, p.path
              LIMIT ?2",
        )?;
        statement
            .query_map(
                params![query, i64::try_from(limit).unwrap_or(i64::MAX)],
                |row| {
                    let source_paths: String = row.get(4)?;
                    let mut snippet: String = row.get(3)?;
                    truncate_utf8(&mut snippet, MAX_SNIPPET_BYTES);
                    Ok(ProjectDocumentSearchResult {
                        project_id: row.get(0)?,
                        project_path: PathBuf::from(row.get::<_, String>(1)?),
                        title: row.get(2)?,
                        snippet,
                        source_paths: decode_source_paths(&source_paths),
                        rank: row.get(5)?,
                    })
                },
            )?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Error::from)
    }
}

fn select_candidates(project: &Path) -> CandidateSet {
    git_candidates(project).unwrap_or_else(|| CandidateSet {
        source: "filesystem",
        paths: filesystem_candidates(project),
    })
}

fn git_candidates(project: &Path) -> Option<CandidateSet> {
    let pathspecs = [
        ":(top,glob)README*",
        ":(top,literal)AGENTS.md",
        ":(top,literal)Cargo.toml",
        ":(top,literal)pyproject.toml",
        ":(top,literal)package.json",
        ":(top,literal)go.mod",
        ":(top,glob)docs/*.md",
        ":(top,glob).codex/*.md",
    ];
    let mut command = Command::new("git");
    command
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .arg("-C")
        .arg(project)
        .args(["ls-files", "-z", "--"])
        .args(pathspecs);
    let output = run_bounded(command, GIT_TIMEOUT, MAX_GIT_OUTPUT_BYTES)?;
    if !output.status.success() || output.exceeded {
        return None;
    }
    let value = String::from_utf8(output.stdout).ok()?;
    let mut paths = value
        .split('\0')
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| allowed_relative_path(path))
        .collect::<Vec<_>>();
    paths.sort();
    paths.dedup();
    paths.truncate(MAX_FILES);
    // A successful empty result is authoritative: Git projects index tracked identity
    // files only. The filesystem allowlist is a resilience fallback for Git failure.
    Some(CandidateSet {
        source: "git",
        paths,
    })
}

fn filesystem_candidates(project: &Path) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(entries) = fs::read_dir(project) {
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else {
                continue;
            };
            if is_readme_variant(name) {
                paths.push(PathBuf::from(name));
            }
        }
    }
    paths.extend(ROOT_IDENTITY_FILES.iter().map(PathBuf::from));
    for directory in ["docs", ".codex"] {
        let root = project.join(directory);
        if !fs::symlink_metadata(&root).is_ok_and(|metadata| metadata.file_type().is_dir()) {
            continue;
        }
        if let Ok(entries) = fs::read_dir(root) {
            for entry in entries.flatten() {
                let relative = PathBuf::from(directory).join(entry.file_name());
                if allowed_relative_path(&relative) {
                    paths.push(relative);
                }
            }
        }
    }
    paths.sort();
    paths.dedup();
    paths.retain(|path| {
        fs::symlink_metadata(project.join(path))
            .is_ok_and(|metadata| metadata.file_type().is_file())
    });
    paths.truncate(MAX_FILES);
    paths
}

fn allowed_relative_path(path: &Path) -> bool {
    if path
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
    {
        return false;
    }
    let parts = path
        .components()
        .filter_map(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect::<Vec<_>>();
    match parts.as_slice() {
        [name] => is_readme_variant(name) || ROOT_IDENTITY_FILES.contains(name),
        [directory @ ("docs" | ".codex"), name] => {
            !directory.is_empty() && name.to_ascii_lowercase().ends_with(".md")
        }
        _ => false,
    }
}

fn is_readme_variant(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    upper == "README" || upper.starts_with("README.")
}

fn fingerprint_candidates(project: &Path, candidates: &CandidateSet) -> Result<String> {
    let mut fingerprint = StableFingerprint::new();
    fingerprint.write(candidates.source.as_bytes());
    for relative in &candidates.paths {
        let path = project.join(relative);
        let metadata = fs::symlink_metadata(&path).map_err(|source| Error::Io {
            path: path.clone(),
            source,
        })?;
        if !metadata.file_type().is_file() {
            continue;
        }
        let modified = metadata
            .modified()
            .ok()
            .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
            .map_or(0, |value| value.as_nanos());
        fingerprint.write(relative.to_string_lossy().as_bytes());
        fingerprint.write(&metadata.len().to_le_bytes());
        fingerprint.write(&modified.to_le_bytes());
    }
    Ok(fingerprint.finish())
}

fn read_documents(project: &Path, paths: &[PathBuf]) -> Result<Vec<SourceDocument>> {
    let mut documents = Vec::new();
    let mut remaining = MAX_PROJECT_BYTES;
    for relative in paths.iter().take(MAX_FILES) {
        if remaining == 0 {
            break;
        }
        let path = project.join(relative);
        let metadata = fs::symlink_metadata(&path).map_err(|source| Error::Io {
            path: path.clone(),
            source,
        })?;
        if !metadata.file_type().is_file() {
            continue;
        }
        let allowance = remaining.min(usize::try_from(MAX_FILE_BYTES).unwrap_or(usize::MAX));
        let mut bytes = Vec::with_capacity(allowance);
        File::open(&path)
            .map_err(|source| Error::Io {
                path: path.clone(),
                source,
            })?
            .take(u64::try_from(allowance).unwrap_or(u64::MAX))
            .read_to_end(&mut bytes)
            .map_err(|source| Error::Io {
                path: path.clone(),
                source,
            })?;
        remaining = remaining.saturating_sub(bytes.len());
        documents.push(SourceDocument {
            path: relative.clone(),
            text: String::from_utf8_lossy(&bytes).into_owned(),
            raw_bytes: bytes.len(),
        });
    }
    Ok(documents)
}

fn assemble_body(documents: &[SourceDocument]) -> (String, usize) {
    let indexed_bytes = documents.iter().map(|document| document.raw_bytes).sum();
    let mut body = String::new();
    for document in documents {
        body.push_str("\n\nsource: ");
        body.push_str(&document.path.to_string_lossy());
        body.push('\n');
        body.push_str(&document.text);
    }
    truncate_utf8(&mut body, MAX_PROJECT_BYTES);
    (body, indexed_bytes)
}

fn derive_title(fallback: &str, documents: &[SourceDocument]) -> String {
    for name in ["package.json", "Cargo.toml", "pyproject.toml", "go.mod"] {
        let Some(document) = documents
            .iter()
            .find(|document| document.path == Path::new(name))
        else {
            continue;
        };
        let title = match name {
            "package.json" => serde_json::from_str::<serde_json::Value>(&document.text)
                .ok()
                .and_then(|value| value.get("name")?.as_str().map(ToOwned::to_owned)),
            "Cargo.toml" => toml_section_name(&document.text, "package"),
            "pyproject.toml" => toml_section_name(&document.text, "project"),
            "go.mod" => document.text.lines().find_map(|line| {
                line.trim()
                    .strip_prefix("module ")
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
            }),
            _ => None,
        };
        if let Some(title) = title.filter(|value| !value.trim().is_empty()) {
            return title;
        }
    }
    documents
        .iter()
        .filter(|document| {
            document
                .path
                .file_name()
                .and_then(|value| value.to_str())
                .is_some_and(is_readme_variant)
        })
        .find_map(|document| {
            document.text.lines().find_map(|line| {
                line.trim()
                    .strip_prefix("# ")
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(ToOwned::to_owned)
            })
        })
        .unwrap_or_else(|| fallback.to_owned())
}

fn toml_section_name(text: &str, wanted: &str) -> Option<String> {
    let mut in_section = false;
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with('[') {
            in_section = line == format!("[{wanted}]");
            continue;
        }
        if !in_section {
            continue;
        }
        let Some(value) = line
            .strip_prefix("name")
            .and_then(|value| value.trim().strip_prefix('='))
        else {
            continue;
        };
        let value = value.trim().trim_matches(['\'', '"']);
        if !value.is_empty() {
            return Some(value.to_owned());
        }
    }
    None
}

fn fts_query(query: &str) -> Option<String> {
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

fn encode_source_paths(paths: &[PathBuf]) -> String {
    serde_json::to_string(paths).unwrap_or_else(|_| "[]".to_owned())
}

fn decode_source_paths(value: &str) -> Vec<PathBuf> {
    serde_json::from_str(value).unwrap_or_default()
}

fn truncate_utf8(value: &mut String, maximum: usize) {
    if value.len() <= maximum {
        return;
    }
    let mut boundary = maximum;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
}

struct StableFingerprint(u64);

impl StableFingerprint {
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

struct BoundedOutput {
    status: std::process::ExitStatus,
    stdout: Vec<u8>,
    exceeded: bool,
}

fn run_bounded(mut command: Command, timeout: Duration, maximum: usize) -> Option<BoundedOutput> {
    let mut child = command
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .ok()?;
    let stdout = child.stdout.take()?;
    let stderr = child.stderr.take()?;
    let stdout_thread = std::thread::spawn(move || read_bounded(stdout, maximum));
    let stderr_thread = std::thread::spawn(move || read_bounded(stderr, maximum));
    let deadline = Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(10));
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                return None;
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                let _ = stdout_thread.join();
                let _ = stderr_thread.join();
                return None;
            }
        }
    };
    let (stdout, stdout_exceeded) = stdout_thread.join().ok()?.ok()?;
    let (_, stderr_exceeded) = stderr_thread.join().ok()?.ok()?;
    Some(BoundedOutput {
        status,
        stdout,
        exceeded: stdout_exceeded || stderr_exceeded,
    })
}

fn read_bounded(mut reader: impl Read, maximum: usize) -> std::io::Result<(Vec<u8>, bool)> {
    let mut kept = Vec::new();
    let mut exceeded = false;
    let mut buffer = [0_u8; 8192];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        let remaining = maximum.saturating_sub(kept.len());
        let admitted = read.min(remaining);
        kept.extend_from_slice(&buffer[..admitted]);
        exceeded |= admitted < read;
    }
    Ok((kept, exceeded))
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use tempfile::TempDir;

    use super::*;
    use crate::SkeinPaths;

    fn registry() -> Result<(TempDir, Registry)> {
        let temp = tempfile::tempdir().map_err(|source| Error::Io {
            path: PathBuf::from("synthetic recall test directory"),
            source,
        })?;
        let paths = SkeinPaths::new(temp.path().join("config"), temp.path().join("data"));
        Ok((temp, Registry::open(&paths)?))
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
    fn indexes_only_registered_projects_and_bounded_identity_paths() -> Result<()> {
        let (temp, mut registry) = registry()?;
        let project = temp.path().join("registered");
        let outside = temp.path().join("outside");
        fs::create_dir_all(&project).map_err(|source| Error::Io {
            path: project.clone(),
            source,
        })?;
        fs::create_dir_all(&outside).map_err(|source| Error::Io {
            path: outside.clone(),
            source,
        })?;
        write(
            &project.join("README.md"),
            b"# Synthetic Atlas\nquartz recall marker",
        )?;
        write(&project.join("docs/guide.md"), b"bounded nebula identity")?;
        write(
            &project.join("docs/nested/ignored.md"),
            b"forbidden nested marker",
        )?;
        write(&project.join("secrets.txt"), b"forbidden secret marker")?;
        write(&outside.join("README.md"), b"unregistered outside marker")?;
        registry.add_project(&project, Some("Fallback"))?;

        let report = registry.refresh_project_documents(&project)?;
        assert_eq!(report.status, ProjectDocumentRefreshStatus::Updated);
        assert_eq!(report.title, "Synthetic Atlas");
        assert!(report.source_paths.contains(&PathBuf::from("README.md")));
        assert!(
            report
                .source_paths
                .contains(&PathBuf::from("docs/guide.md"))
        );
        assert!(registry.search_project_documents("quartz", 10)?.len() == 1);
        assert!(registry.search_project_documents("nebula", 10)?.len() == 1);
        assert!(
            registry
                .search_project_documents("forbidden", 10)?
                .is_empty()
        );
        assert!(registry.search_project_documents("outside", 10)?.is_empty());
        Ok(())
    }

    #[test]
    fn skips_unchanged_fingerprints_and_reindexes_changed_files() -> Result<()> {
        let (temp, mut registry) = registry()?;
        let project = temp.path().join("changing");
        fs::create_dir(&project).map_err(|source| Error::Io {
            path: project.clone(),
            source,
        })?;
        write(&project.join("README.md"), b"# First\nold-copper-token")?;
        registry.add_project(&project, None)?;
        let first = registry.refresh_project_documents(&project)?;
        registry.connection.execute(
            "UPDATE project_documents SET refreshed_at = 1 WHERE project_id = ?1",
            [first.project_id],
        )?;
        let unchanged = registry.refresh_project_documents(&project)?;
        assert_eq!(first.status, ProjectDocumentRefreshStatus::Updated);
        assert_eq!(unchanged.status, ProjectDocumentRefreshStatus::Unchanged);
        let observed: i64 = registry.connection.query_row(
            "SELECT refreshed_at FROM project_documents WHERE project_id = ?1",
            [first.project_id],
            |row| row.get(0),
        )?;
        assert!(observed > 1);

        write(
            &project.join("README.md"),
            b"# Second title\nnew-sapphire-token with a different length",
        )?;
        let changed = registry.refresh_project_documents(&project)?;
        assert_eq!(changed.status, ProjectDocumentRefreshStatus::Updated);
        assert_eq!(changed.title, "Second title");
        assert!(registry.search_project_documents("sapphire", 10)?.len() == 1);
        assert!(registry.search_project_documents("copper", 10)?.is_empty());
        Ok(())
    }

    #[test]
    fn git_projects_index_only_tracked_identity_files() -> Result<()> {
        let (temp, mut registry) = registry()?;
        let project = temp.path().join("tracked");
        fs::create_dir(&project).map_err(|source| Error::Io {
            path: project.clone(),
            source,
        })?;
        write(
            &project.join("README.md"),
            b"# Tracked Atlas\ntracked-identity-marker",
        )?;
        write(&project.join("AGENTS.md"), b"untracked-identity-marker")?;
        let initialized = Command::new("git")
            .args(["init", "--quiet"])
            .arg(&project)
            .status()
            .map_err(|source| Error::GitUnavailable {
                path: project.clone(),
                source,
            })?;
        assert!(initialized.success());
        let added = Command::new("git")
            .arg("-C")
            .arg(&project)
            .args(["add", "--", "README.md"])
            .status()
            .map_err(|source| Error::GitUnavailable {
                path: project.clone(),
                source,
            })?;
        assert!(added.success());
        registry.add_project(&project, None)?;

        let report = registry.refresh_project_documents(&project)?;
        assert_eq!(report.source_paths, [PathBuf::from("README.md")]);
        assert_eq!(
            registry
                .search_project_documents("tracked-identity-marker", 10)?
                .len(),
            1
        );
        assert!(
            registry
                .search_project_documents("untracked-identity-marker", 10)?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn caps_files_and_bytes_and_never_returns_full_bodies() -> Result<()> {
        let (temp, mut registry) = registry()?;
        let project = temp.path().join("bounded");
        fs::create_dir_all(project.join("docs")).map_err(|source| Error::Io {
            path: project.clone(),
            source,
        })?;
        for index in 0..50 {
            write(
                &project.join(format!("docs/{index:02}.md")),
                &vec![b'x'; usize::try_from(MAX_FILE_BYTES + 1024).unwrap_or(usize::MAX)],
            )?;
        }
        registry.add_project(&project, None)?;
        let report = registry.refresh_project_documents(&project)?;
        assert!(report.source_paths.len() <= MAX_FILES);
        assert!(!report.source_paths.is_empty());
        assert!(report.indexed_bytes <= MAX_PROJECT_BYTES);
        let body_bytes: i64 = registry.connection.query_row(
            "SELECT length(CAST(body AS BLOB)) FROM project_documents WHERE project_id = ?1",
            [report.project_id],
            |row| row.get(0),
        )?;
        assert!(body_bytes <= i64::try_from(MAX_PROJECT_BYTES).unwrap_or(i64::MAX));

        let unique = "bounded-search-marker";
        let mut readme = File::create(project.join("README.md")).map_err(|source| Error::Io {
            path: project.join("README.md"),
            source,
        })?;
        readme
            .write_all(unique.as_bytes())
            .map_err(|source| Error::Io {
                path: project.join("README.md"),
                source,
            })?;
        registry.refresh_project_documents(&project)?;
        let hit = registry.search_project_documents(unique, 10)?.remove(0);
        assert!(hit.snippet.len() <= MAX_SNIPPET_BYTES);
        assert!(hit.snippet.len() < usize::try_from(MAX_FILE_BYTES).unwrap_or(usize::MAX));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn filesystem_fallback_does_not_follow_symlinks() -> Result<()> {
        use std::os::unix::fs::symlink;

        let (temp, mut registry) = registry()?;
        let project = temp.path().join("links");
        fs::create_dir(&project).map_err(|source| Error::Io {
            path: project.clone(),
            source,
        })?;
        let target = temp.path().join("outside.md");
        write(&target, b"symlink-secret-marker")?;
        symlink(&target, project.join("README.md")).map_err(|source| Error::Io {
            path: project.join("README.md"),
            source,
        })?;
        registry.add_project(&project, None)?;
        let report = registry.refresh_project_documents(&project)?;
        assert!(report.source_paths.is_empty());
        assert!(
            registry
                .search_project_documents("symlink-secret-marker", 10)?
                .is_empty()
        );
        Ok(())
    }
}
