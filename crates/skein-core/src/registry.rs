use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use rusqlite::Connection;
use rusqlite::OpenFlags;
use rusqlite::OptionalExtension;
use rusqlite::TransactionBehavior;
use rusqlite::params;
use serde::Serialize;

use crate::Error;
use crate::GitMetadata;
use crate::Result;
use crate::SkeinPaths;
use crate::git;

const SCHEMA_VERSION: i64 = 12;

const CREATE_CONDUCTOR_SCHEMA: &str = "CREATE TABLE conductor_decisions (
    id INTEGER PRIMARY KEY,
    request_id TEXT NOT NULL UNIQUE,
    run_id INTEGER NOT NULL UNIQUE REFERENCES control_runs(id) ON DELETE RESTRICT,
    created_at INTEGER NOT NULL,
    matched_at INTEGER NOT NULL,
    match_schema_version INTEGER NOT NULL CHECK(match_schema_version > 0),
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE RESTRICT,
    source_thread_id TEXT,
    action TEXT NOT NULL CHECK(action IN ('start', 'resume')),
    confidence TEXT NOT NULL CHECK(confidence IN ('none', 'low', 'medium', 'high')),
    ambiguous INTEGER NOT NULL CHECK(ambiguous IN (0, 1)),
    resolution_kind TEXT NOT NULL CHECK(resolution_kind IN
        ('automatic', 'explicit_project', 'explicit_thread', 'user_selected')),
    score INTEGER NOT NULL CHECK(score >= 0),
    runner_up_margin INTEGER NOT NULL CHECK(runner_up_margin >= 0),
    candidate_count INTEGER NOT NULL CHECK(candidate_count > 0),
    query_bytes INTEGER NOT NULL CHECK(query_bytes > 0 AND query_bytes <= 65536),
    query_tokens INTEGER NOT NULL CHECK(query_tokens > 0),
    include_text INTEGER NOT NULL CHECK(include_text IN (0, 1)),
    CHECK((action = 'start' AND source_thread_id IS NULL)
       OR (action = 'resume' AND source_thread_id IS NOT NULL)),
    CHECK(ambiguous = 0 OR resolution_kind = 'user_selected'),
    CHECK(resolution_kind != 'automatic'
       OR (ambiguous = 0 AND confidence = 'high'))
);
CREATE INDEX conductor_decisions_project_created
    ON conductor_decisions(project_id, created_at DESC);
CREATE TABLE conductor_decision_evidence (
    id INTEGER PRIMARY KEY,
    decision_id INTEGER NOT NULL REFERENCES conductor_decisions(id) ON DELETE RESTRICT,
    ordinal INTEGER NOT NULL,
    scope TEXT NOT NULL CHECK(scope IN ('project', 'session')),
    family TEXT NOT NULL CHECK(family IN
        ('project_identity', 'project_path', 'git', 'session_identity', 'session_path',
         'session_text', 'session_link', 'session_recency', 'control')),
    kind TEXT NOT NULL,
    points INTEGER NOT NULL CHECK(points > 0),
    matches INTEGER NOT NULL CHECK(matches > 0),
    UNIQUE(decision_id, ordinal)
);
CREATE INDEX conductor_evidence_decision
    ON conductor_decision_evidence(decision_id, ordinal);";

const CREATE_RECONCILIATION_SCHEMA: &str =
    "ALTER TABLE control_actions ADD COLUMN client_request_id TEXT;
ALTER TABLE control_actions ADD COLUMN input_bytes INTEGER;
ALTER TABLE control_actions ADD COLUMN expected_source_turn_id TEXT;
CREATE UNIQUE INDEX control_actions_client_request
    ON control_actions(run_id, client_request_id) WHERE client_request_id IS NOT NULL;
CREATE TABLE control_reconciliations (
    id INTEGER PRIMARY KEY,
    action_id INTEGER NOT NULL UNIQUE REFERENCES control_actions(id) ON DELETE RESTRICT,
    run_id INTEGER NOT NULL REFERENCES control_runs(id) ON DELETE RESTRICT,
    observed_at INTEGER NOT NULL,
    source_thread_id TEXT NOT NULL,
    expected_source_turn_id TEXT,
    thread_status TEXT NOT NULL CHECK(thread_status IN
        ('not_loaded', 'idle', 'system_error', 'active', 'unknown')),
    turn_found INTEGER NOT NULL CHECK(turn_found IN (0, 1)),
    observed_turn_status TEXT CHECK(observed_turn_status IS NULL OR observed_turn_status IN
        ('in_progress', 'completed', 'failed', 'interrupted', 'unknown')),
    initial_message_observed INTEGER NOT NULL CHECK(initial_message_observed IN (0, 1)),
    steer_messages_observed INTEGER NOT NULL CHECK(steer_messages_observed >= 0),
    outcome TEXT NOT NULL CHECK(outcome IN
        ('terminal_confirmed', 'turn_still_running', 'turn_missing',
         'identity_unavailable', 'unsupported_status')),
    CHECK((turn_found = 1 AND observed_turn_status IS NOT NULL)
       OR (turn_found = 0 AND observed_turn_status IS NULL))
);
CREATE INDEX control_reconciliations_run
    ON control_reconciliations(run_id, observed_at DESC);";

const CREATE_WORKER_SCHEMA: &str = "CREATE TABLE control_workers (
    id INTEGER PRIMARY KEY,
    worker_key TEXT NOT NULL UNIQUE,
    run_id INTEGER NOT NULL UNIQUE REFERENCES control_runs(id) ON DELETE RESTRICT,
    runtime_kind TEXT NOT NULL CHECK(runtime_kind = 'codex'),
    protocol_version INTEGER NOT NULL CHECK(protocol_version = 1),
    endpoint TEXT,
    pid INTEGER,
    process_started_at INTEGER NOT NULL,
    state TEXT NOT NULL CHECK(state IN ('starting', 'ready', 'busy', 'stopping', 'exited', 'lost')),
    lease_epoch INTEGER NOT NULL CHECK(lease_epoch > 0),
    lease_acquired_at INTEGER NOT NULL,
    lease_expires_at INTEGER NOT NULL,
    heartbeat_at INTEGER NOT NULL,
    terminal_at INTEGER,
    exit_kind TEXT CHECK(exit_kind IN ('clean', 'worker_error', 'codex_exit', 'lease_lost', 'forced', 'unknown')),
    last_error_class TEXT,
    last_error_message TEXT,
    CHECK(lease_expires_at >= lease_acquired_at),
    CHECK(heartbeat_at >= lease_acquired_at),
    CHECK(
        (state IN ('exited', 'lost') AND terminal_at IS NOT NULL)
        OR (state NOT IN ('exited', 'lost') AND terminal_at IS NULL)
    )
);
CREATE INDEX control_workers_lease ON control_workers(state, lease_expires_at);
ALTER TABLE control_runs ADD COLUMN ownership_mode TEXT NOT NULL DEFAULT 'foreground'
    CHECK(ownership_mode IN ('foreground', 'worker'));
ALTER TABLE control_actions ADD COLUMN worker_id INTEGER REFERENCES control_workers(id) ON DELETE RESTRICT;
ALTER TABLE control_actions ADD COLUMN worker_lease_epoch INTEGER;
CREATE INDEX control_actions_worker ON control_actions(worker_id, worker_lease_epoch, state);
UPDATE control_actions SET state = 'uncertain', terminal_at = unixepoch(),
    error_class = 'legacy_owner_unknown',
    error_message = 'pre-worker mutation ownership cannot be recovered'
WHERE state IN ('dispatching', 'acknowledged');
INSERT INTO control_action_events (action_id, sequence, event_kind, recorded_at, detail)
SELECT a.id,
    COALESCE((SELECT MAX(e.sequence) FROM control_action_events e WHERE e.action_id = a.id), 0) + 1,
    'uncertain', unixepoch(), NULL
FROM control_actions a WHERE a.error_class = 'legacy_owner_unknown';
UPDATE control_turns SET state = 'uncertain', terminal_at = unixepoch()
WHERE state IN ('dispatching', 'running');
UPDATE control_runs SET state = 'recovery_required', updated_at = unixepoch(),
    last_error_class = 'legacy_owner_unknown',
    last_error_message = 'read-only reconciliation is required before retry'
WHERE state IN ('planned', 'starting', 'active');";

const CREATE_CONTROL_SCHEMA: &str = "CREATE TABLE control_policies (
    id INTEGER PRIMARY KEY,
    created_at INTEGER NOT NULL,
    sandbox_mode TEXT NOT NULL CHECK(sandbox_mode = 'danger_full_access'),
    approval_mode TEXT NOT NULL CHECK(approval_mode = 'never'),
    network_access INTEGER NOT NULL CHECK(network_access IN (0, 1)),
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE RESTRICT,
    working_directory TEXT NOT NULL,
    acknowledged_at INTEGER NOT NULL,
    acknowledgement_source TEXT NOT NULL CHECK(acknowledgement_source = 'cli_flag')
);
CREATE TABLE control_runs (
    id INTEGER PRIMARY KEY,
    run_key TEXT NOT NULL UNIQUE,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE RESTRICT,
    session_id INTEGER REFERENCES sessions(id) ON DELETE SET NULL,
    policy_id INTEGER NOT NULL REFERENCES control_policies(id) ON DELETE RESTRICT,
    runtime_kind TEXT NOT NULL CHECK(runtime_kind = 'codex'),
    working_directory TEXT NOT NULL,
    state TEXT NOT NULL CHECK(state IN
        ('planned', 'starting', 'active', 'completed', 'failed', 'interrupted', 'recovery_required')),
    source_thread_id TEXT,
    source_session_id TEXT,
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    terminal_at INTEGER,
    last_error_class TEXT,
    last_error_message TEXT,
    CHECK(
        (state IN ('completed', 'failed', 'interrupted') AND terminal_at IS NOT NULL)
        OR (state NOT IN ('completed', 'failed', 'interrupted') AND terminal_at IS NULL)
    )
);
CREATE INDEX control_runs_state_updated ON control_runs(state, updated_at);
CREATE INDEX control_runs_project_updated ON control_runs(project_id, updated_at DESC);
CREATE TABLE control_turns (
    id INTEGER PRIMARY KEY,
    run_id INTEGER NOT NULL REFERENCES control_runs(id) ON DELETE RESTRICT,
    turn_number INTEGER NOT NULL,
    state TEXT NOT NULL CHECK(state IN
        ('planned', 'dispatching', 'running', 'completed', 'failed', 'interrupted', 'uncertain')),
    input_bytes INTEGER NOT NULL CHECK(input_bytes > 0),
    terminal_condition_version INTEGER NOT NULL CHECK(terminal_condition_version = 1),
    client_message_id TEXT NOT NULL UNIQUE,
    source_turn_id TEXT,
    created_at INTEGER NOT NULL,
    terminal_at INTEGER,
    UNIQUE(run_id, turn_number),
    CHECK(
        (state IN ('completed', 'failed', 'interrupted', 'uncertain') AND terminal_at IS NOT NULL)
        OR (state NOT IN ('completed', 'failed', 'interrupted', 'uncertain') AND terminal_at IS NULL)
    )
);
CREATE TABLE control_actions (
    id INTEGER PRIMARY KEY,
    action_key TEXT NOT NULL UNIQUE,
    run_id INTEGER NOT NULL REFERENCES control_runs(id) ON DELETE RESTRICT,
    turn_id INTEGER REFERENCES control_turns(id) ON DELETE RESTRICT,
    policy_id INTEGER NOT NULL REFERENCES control_policies(id) ON DELETE RESTRICT,
    action_kind TEXT NOT NULL CHECK(action_kind IN
        ('thread_start', 'thread_resume', 'turn_start', 'turn_steer', 'turn_interrupt', 'status_reconcile')),
    state TEXT NOT NULL CHECK(state IN
        ('planned', 'dispatching', 'acknowledged', 'succeeded', 'failed', 'uncertain')),
    request_method TEXT NOT NULL,
    request_fingerprint TEXT NOT NULL,
    source_result_id TEXT,
    created_at INTEGER NOT NULL,
    dispatch_started_at INTEGER,
    terminal_at INTEGER,
    error_class TEXT,
    error_message TEXT,
    CHECK(
        (state IN ('succeeded', 'failed', 'uncertain') AND terminal_at IS NOT NULL)
        OR (state NOT IN ('succeeded', 'failed', 'uncertain') AND terminal_at IS NULL)
    )
);
CREATE INDEX control_actions_recovery ON control_actions(state, created_at);
CREATE INDEX control_actions_run ON control_actions(run_id, created_at, id);
CREATE TABLE control_action_events (
    id INTEGER PRIMARY KEY,
    action_id INTEGER NOT NULL REFERENCES control_actions(id) ON DELETE RESTRICT,
    sequence INTEGER NOT NULL,
    event_kind TEXT NOT NULL,
    recorded_at INTEGER NOT NULL,
    detail TEXT,
    UNIQUE(action_id, sequence)
);";

const CREATE_SESSIONS_SCHEMA: &str = "CREATE TABLE sessions (
    id INTEGER PRIMARY KEY,
    source_kind TEXT NOT NULL,
    source_thread_id TEXT NOT NULL,
    source_session_id TEXT,
    project_id INTEGER REFERENCES projects(id) ON DELETE SET NULL,
    project_link_kind TEXT NOT NULL CHECK(project_link_kind IN
        ('automatic', 'manual', 'manual_unbound', 'unmatched')),
    source_cwd TEXT NOT NULL,
    source_created_at INTEGER NOT NULL,
    source_updated_at INTEGER NOT NULL,
    first_seen_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    source_label TEXT NOT NULL,
    status_label TEXT NOT NULL,
    model_provider TEXT,
    source_version TEXT,
    parent_source_thread_id TEXT,
    forked_from_source_thread_id TEXT,
    ephemeral INTEGER NOT NULL CHECK(ephemeral IN (0, 1)),
    name TEXT,
    preview TEXT,
    text_imported INTEGER NOT NULL DEFAULT 0 CHECK(text_imported IN (0, 1)),
    UNIQUE(source_kind, source_thread_id),
    CHECK((text_imported = 0 AND name IS NULL AND preview IS NULL) OR text_imported = 1)
);
CREATE INDEX sessions_project_updated
    ON sessions(project_id, source_updated_at DESC);
CREATE INDEX sessions_source_updated
    ON sessions(source_kind, source_updated_at DESC);
CREATE INDEX sessions_source_session
    ON sessions(source_kind, source_session_id);";

pub(crate) const PROJECT_SELECT: &str = "SELECT p.id, p.name, p.path, p.updated_at,
            m.refreshed_at, m.vcs_kind, m.head_ref, m.head_oid,
            m.last_commit_at, m.last_commit_subject, m.tracked_dirty
     FROM projects p
     LEFT JOIN project_metadata m ON m.project_id = p.id";

/// A project explicitly registered with Session Skein.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Project {
    /// Stable database identifier.
    pub id: i64,
    /// Human-readable project name.
    pub name: String,
    /// Canonical absolute project path.
    pub path: PathBuf,
    /// Unix timestamp of the latest registry update.
    pub updated_at: i64,
    /// Unix timestamp of the latest metadata observation, if refreshed.
    pub metadata_refreshed_at: Option<i64>,
    /// Git metadata, or `None` when unrefreshed or not a Git repository.
    pub git: Option<GitMetadata>,
}

/// Whether a refresh performed Git inspection or reused its stored fingerprint.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RefreshStatus {
    /// Metadata was inspected and stored.
    Updated,
    /// The bounded Git fingerprint was unchanged, so Git was not invoked.
    Unchanged,
}

/// Result of refreshing one explicitly registered project.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct RefreshReport {
    /// Refresh decision made for this project.
    pub status: RefreshStatus,
    /// Current project record after the refresh decision.
    pub project: Project,
}

/// Versioned local project registry.
pub struct Registry {
    pub(crate) connection: Connection,
}

impl Registry {
    /// Create private state directories, open the registry, and apply migrations.
    pub fn open(paths: &SkeinPaths) -> Result<Self> {
        create_private_dir(&paths.data_dir)?;
        let database = paths.database();
        let connection = Connection::open(&database)?;
        set_private_file_permissions(&database)?;
        connection.busy_timeout(Duration::from_secs(5))?;
        connection.execute_batch("PRAGMA foreign_keys = ON; PRAGMA journal_mode = WAL;")?;

        let mut registry = Self { connection };
        registry.migrate()?;
        Ok(registry)
    }

    /// Open an existing registry without creating files, migrating, or enabling WAL.
    pub fn open_read_only(paths: &SkeinPaths) -> Result<Self> {
        let connection = Connection::open_with_flags(
            paths.database(),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        connection.busy_timeout(Duration::from_secs(5))?;
        let registry = Self { connection };
        registry.ensure_supported_schema()?;
        Ok(registry)
    }

    /// Return the current schema version without modifying state.
    pub fn schema_version(&self) -> Result<i64> {
        self.connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(Error::from)
    }

    /// Add a project or refresh its name and update timestamp.
    pub fn add_project(&self, path: &Path, name: Option<&str>) -> Result<Project> {
        if !path.is_dir() {
            return Err(Error::InvalidProjectPath(path.to_path_buf()));
        }

        let canonical_path = fs::canonicalize(path).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let project_name = match name {
            Some(value) if !value.trim().is_empty() => value.trim().to_owned(),
            _ => canonical_path
                .file_name()
                .and_then(|value| value.to_str())
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
                .ok_or_else(|| Error::MissingProjectName(canonical_path.clone()))?,
        };
        let timestamp = unix_timestamp();

        self.connection.execute(
            "INSERT INTO projects (name, path, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?3)
             ON CONFLICT(path) DO UPDATE SET name = excluded.name, updated_at = excluded.updated_at",
            params![project_name, canonical_path.to_string_lossy(), timestamp],
        )?;

        self.project_by_path(&canonical_path)?
            .ok_or_else(|| Error::Sqlite(rusqlite::Error::QueryReturnedNoRows))
    }

    pub(crate) fn add_discovered_project(&self, path: &Path) -> Result<(Project, bool)> {
        let canonical_path = canonical_project_path(path)?;
        if let Some(project) = self.project_by_path(&canonical_path)? {
            return Ok((project, false));
        }
        let project_name = canonical_path
            .file_name()
            .and_then(|value| value.to_str())
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned)
            .ok_or_else(|| Error::MissingProjectName(canonical_path.clone()))?;
        let timestamp = unix_timestamp();
        let inserted = self.connection.execute(
            "INSERT OR IGNORE INTO projects (name, path, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?3)",
            params![project_name, canonical_path.to_string_lossy(), timestamp],
        )?;
        let project = self
            .project_by_path(&canonical_path)?
            .ok_or(rusqlite::Error::QueryReturnedNoRows)?;
        Ok((project, inserted == 1))
    }

    /// Remove an explicitly registered project only when no durable session or run refers to it.
    pub fn remove_project_if_unused(&mut self, path: &Path) -> Result<Project> {
        let canonical_path = canonical_project_path(path)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let query = format!("{PROJECT_SELECT} WHERE p.path = ?1");
        let project = transaction
            .query_row(&query, [canonical_path.to_string_lossy()], project_from_row)
            .optional()?
            .ok_or_else(|| Error::ProjectNotRegistered(canonical_path.clone()))?;
        let session_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM sessions WHERE project_id = ?1",
            [project.id],
            |row| row.get(0),
        )?;
        let run_count: i64 = transaction.query_row(
            "SELECT COUNT(*) FROM control_runs WHERE project_id = ?1",
            [project.id],
            |row| row.get(0),
        )?;
        if session_count > 0 || run_count > 0 {
            return Err(Error::InvalidControlRequest(
                "project cannot be removed while durable sessions or control runs refer to it"
                    .to_owned(),
            ));
        }
        let removed = transaction.execute("DELETE FROM projects WHERE id = ?1", [project.id])?;
        if removed != 1 {
            return Err(Error::ControlStateConflict(format!(
                "project {} disappeared before removal",
                project.id
            )));
        }
        transaction.commit()?;
        Ok(project)
    }

    /// List all registered projects in stable name/path order.
    pub fn list_projects(&self) -> Result<Vec<Project>> {
        list_projects_on(&self.connection)
    }

    /// Return one project by canonical path.
    pub fn get_project(&self, path: &Path) -> Result<Project> {
        let canonical_path = canonical_project_path(path)?;
        self.project_by_path(&canonical_path)?
            .ok_or(Error::ProjectNotRegistered(canonical_path))
    }

    /// Refresh one registered project without recursively discovering anything.
    pub fn refresh_project(
        &self,
        path: &Path,
        working_tree: bool,
        force: bool,
    ) -> Result<RefreshReport> {
        let canonical_path = canonical_project_path(path)?;
        let project = self
            .project_by_path(&canonical_path)?
            .ok_or_else(|| Error::ProjectNotRegistered(canonical_path.clone()))?;
        let probe = git::probe(&canonical_path)?;
        let new_fingerprint = probe
            .as_ref()
            .map_or_else(|| "none".to_owned(), |value| value.fingerprint.clone());

        if !working_tree
            && !force
            && self.metadata_fingerprint(project.id)?.as_deref() == Some(&new_fingerprint)
        {
            self.connection.execute(
                "UPDATE project_metadata SET refreshed_at = ?1 WHERE project_id = ?2",
                params![unix_timestamp(), project.id],
            )?;
            return Ok(RefreshReport {
                status: RefreshStatus::Unchanged,
                project: self
                    .project_by_path(&canonical_path)?
                    .ok_or_else(|| Error::ProjectNotRegistered(canonical_path.clone()))?,
            });
        }

        let timestamp = unix_timestamp();
        match probe {
            Some(probe) => {
                let observation = git::inspect(&canonical_path, probe, working_tree)?;
                self.store_metadata(
                    project.id,
                    "git",
                    &observation.fingerprint,
                    timestamp,
                    Some(&observation.metadata),
                )?;
            }
            None => {
                self.store_metadata(project.id, "none", "none", timestamp, None)?;
            }
        }

        Ok(RefreshReport {
            status: RefreshStatus::Updated,
            project: self
                .project_by_path(&canonical_path)?
                .ok_or(Error::ProjectNotRegistered(canonical_path))?,
        })
    }

    /// Refresh every registered project sequentially in stable display order.
    pub fn refresh_all(&self, working_tree: bool, force: bool) -> Result<Vec<RefreshReport>> {
        self.list_projects()?
            .iter()
            .map(|project| self.refresh_project(&project.path, working_tree, force))
            .collect()
    }

    fn project_by_path(&self, path: &Path) -> Result<Option<Project>> {
        let query = format!("{PROJECT_SELECT} WHERE p.path = ?1");
        self.connection
            .query_row(&query, [path.to_string_lossy()], project_from_row)
            .optional()
            .map_err(Error::from)
    }

    fn metadata_fingerprint(&self, project_id: i64) -> Result<Option<String>> {
        self.connection
            .query_row(
                "SELECT fingerprint FROM project_metadata WHERE project_id = ?1",
                [project_id],
                |row| row.get(0),
            )
            .optional()
            .map_err(Error::from)
    }

    fn store_metadata(
        &self,
        project_id: i64,
        vcs_kind: &str,
        fingerprint: &str,
        refreshed_at: i64,
        git: Option<&GitMetadata>,
    ) -> Result<()> {
        let head_ref = git.and_then(|value| value.head_ref.as_deref());
        let head_oid = git.and_then(|value| value.head_oid.as_deref());
        let last_commit_at = git.and_then(|value| value.last_commit_at);
        let last_commit_subject = git.and_then(|value| value.last_commit_subject.as_deref());
        let tracked_dirty = git.and_then(|value| value.tracked_dirty).map(i64::from);

        self.connection.execute(
            "INSERT INTO project_metadata (
                project_id, vcs_kind, fingerprint, refreshed_at, head_ref, head_oid,
                last_commit_at, last_commit_subject, tracked_dirty
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
             ON CONFLICT(project_id) DO UPDATE SET
                vcs_kind = excluded.vcs_kind,
                fingerprint = excluded.fingerprint,
                refreshed_at = excluded.refreshed_at,
                head_ref = excluded.head_ref,
                head_oid = excluded.head_oid,
                last_commit_at = excluded.last_commit_at,
                last_commit_subject = excluded.last_commit_subject,
                tracked_dirty = excluded.tracked_dirty",
            params![
                project_id,
                vcs_kind,
                fingerprint,
                refreshed_at,
                head_ref,
                head_oid,
                last_commit_at,
                last_commit_subject,
                tracked_dirty
            ],
        )?;
        Ok(())
    }

    fn migrate(&mut self) -> Result<()> {
        let transaction = self.connection.transaction()?;
        let current: i64 = transaction.query_row("PRAGMA user_version", [], |row| row.get(0))?;

        if current == 0 {
            transaction.execute_batch(
                "CREATE TABLE projects (
                    id INTEGER PRIMARY KEY,
                    name TEXT NOT NULL,
                    path TEXT NOT NULL UNIQUE,
                    created_at INTEGER NOT NULL,
                    updated_at INTEGER NOT NULL
                );
                CREATE TABLE project_metadata (
                    project_id INTEGER PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                    vcs_kind TEXT NOT NULL CHECK(vcs_kind IN ('none', 'git')),
                    fingerprint TEXT NOT NULL,
                    refreshed_at INTEGER NOT NULL,
                    head_ref TEXT,
                    head_oid TEXT,
                    last_commit_at INTEGER,
                    last_commit_subject TEXT,
                    tracked_dirty INTEGER CHECK(tracked_dirty IN (0, 1))
                );
                PRAGMA user_version = 2;",
            )?;
        } else if current == 1 {
            transaction.execute_batch(
                "CREATE TABLE project_metadata (
                    project_id INTEGER PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                    vcs_kind TEXT NOT NULL CHECK(vcs_kind IN ('none', 'git')),
                    fingerprint TEXT NOT NULL,
                    refreshed_at INTEGER NOT NULL,
                    head_ref TEXT,
                    head_oid TEXT,
                    last_commit_at INTEGER,
                    last_commit_subject TEXT,
                    tracked_dirty INTEGER CHECK(tracked_dirty IN (0, 1))
                );
                PRAGMA user_version = 2;",
            )?;
        }

        if current <= 2 {
            transaction.execute_batch(CREATE_SESSIONS_SCHEMA)?;
            transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        }

        if current <= 3 {
            transaction.execute_batch(CREATE_CONTROL_SCHEMA)?;
            transaction.pragma_update(None, "user_version", 4)?;
        }

        if current <= 4 {
            transaction.execute_batch(CREATE_WORKER_SCHEMA)?;
            transaction.pragma_update(None, "user_version", 5)?;
        }

        if current <= 5 {
            transaction.execute_batch(CREATE_RECONCILIATION_SCHEMA)?;
            transaction.pragma_update(None, "user_version", 6)?;
        }

        if current <= 6 {
            transaction.execute_batch(CREATE_CONDUCTOR_SCHEMA)?;
            transaction.pragma_update(None, "user_version", 7)?;
        }

        if current <= 7 {
            transaction.execute_batch(crate::scan::CREATE_SCAN_ROOT_SCHEMA)?;
            transaction.pragma_update(None, "user_version", 8)?;
        }

        if current <= 8 {
            transaction.execute_batch(crate::recall::CREATE_PROJECT_DOCUMENT_SCHEMA)?;
            transaction.pragma_update(None, "user_version", 9)?;
        }

        if current <= 9 {
            transaction.execute_batch(crate::context::CREATE_CONTEXT_DOCUMENT_SCHEMA)?;
            transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        } else if current == 10 {
            transaction.execute_batch(
                "ALTER TABLE context_documents ADD COLUMN source_bytes INTEGER NOT NULL DEFAULT 0
                    CHECK(source_bytes >= 0 AND source_bytes <= 1048576);",
            )?;
            migrate_context_documents_v12(&transaction)?;
            transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        } else if current == 11 {
            migrate_context_documents_v12(&transaction)?;
            transaction.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        } else if current != SCHEMA_VERSION {
            return Err(Error::UnsupportedSchema {
                found: current,
                supported: SCHEMA_VERSION,
            });
        }

        transaction.commit()?;
        Ok(())
    }

    fn ensure_supported_schema(&self) -> Result<()> {
        let found = self.schema_version()?;
        if !(1..=SCHEMA_VERSION).contains(&found) {
            return Err(Error::UnsupportedSchema {
                found,
                supported: SCHEMA_VERSION,
            });
        }
        Ok(())
    }
}

fn migrate_context_documents_v12(transaction: &rusqlite::Transaction<'_>) -> Result<()> {
    transaction.execute_batch(
        "DROP TABLE context_documents_fts;
         ALTER TABLE context_documents RENAME TO context_documents_v11;
         CREATE TABLE context_documents (
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
            source_bytes INTEGER NOT NULL DEFAULT 0 CHECK(source_bytes >= 0),
            source_modified_at_ns INTEGER NOT NULL DEFAULT 0 CHECK(source_modified_at_ns >= 0),
            source_thread_id TEXT,
            source_created_at INTEGER,
            source_updated_at INTEGER,
            UNIQUE(source_kind, source_path)
         );
         INSERT INTO context_documents (
            id, source_kind, source_path, project_id, context_path, fingerprint,
            refreshed_at, title, body, imported_bytes, source_bytes
         ) SELECT id, source_kind, source_path, project_id, context_path, fingerprint,
                  refreshed_at, title, body, imported_bytes, source_bytes
             FROM context_documents_v11;
         DROP TABLE context_documents_v11;
         CREATE INDEX context_documents_project ON context_documents(project_id, source_kind);
         CREATE VIRTUAL TABLE context_documents_fts USING fts5(title, body);
         INSERT INTO context_documents_fts(rowid, title, body)
              SELECT id, title, body FROM context_documents;",
    )?;
    Ok(())
}

pub(crate) fn list_projects_on(connection: &Connection) -> Result<Vec<Project>> {
    let query = format!("{PROJECT_SELECT} ORDER BY p.name COLLATE NOCASE, p.path");
    let mut statement = connection.prepare(&query)?;
    statement
        .query_map([], project_from_row)?
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Error::from)
}

pub(crate) fn project_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
    let refreshed_at: Option<i64> = row.get(4)?;
    let vcs_kind: Option<String> = row.get(5)?;
    let git = if vcs_kind.as_deref() == Some("git") {
        Some(GitMetadata {
            head_ref: row.get(6)?,
            head_oid: row.get(7)?,
            last_commit_at: row.get(8)?,
            last_commit_subject: row.get(9)?,
            tracked_dirty: row.get::<_, Option<i64>>(10)?.map(|value| value != 0),
        })
    } else {
        None
    };
    Ok(Project {
        id: row.get(0)?,
        name: row.get(1)?,
        path: PathBuf::from(row.get::<_, String>(2)?),
        updated_at: row.get(3)?,
        metadata_refreshed_at: refreshed_at,
        git,
    })
}

fn canonical_project_path(path: &Path) -> Result<PathBuf> {
    if !path.is_dir() {
        return Err(Error::InvalidProjectPath(path.to_path_buf()));
    }
    fs::canonicalize(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

pub(crate) fn unix_timestamp() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| {
            i64::try_from(duration.as_secs()).unwrap_or(i64::MAX)
        })
}

fn create_private_dir(path: &Path) -> Result<()> {
    fs::create_dir_all(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    set_private_dir_permissions(path)
}

#[cfg(unix)]
fn set_private_dir_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(not(unix))]
fn set_private_dir_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::process::Command;

    use tempfile::TempDir;

    use super::*;

    fn isolated_registry() -> Result<(TempDir, Registry)> {
        let temp = tempfile::tempdir().map_err(|source| Error::Io {
            path: PathBuf::from("temporary test directory"),
            source,
        })?;
        let paths = SkeinPaths::new(temp.path().join("config"), temp.path().join("data"));
        let registry = Registry::open(&paths)?;
        Ok((temp, registry))
    }

    #[test]
    fn initializes_current_schema() -> Result<()> {
        let (_temp, registry) = isolated_registry()?;
        assert_eq!(registry.schema_version()?, SCHEMA_VERSION);
        Ok(())
    }

    #[test]
    fn migrates_schema_version_one_in_place() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|source| Error::Io {
            path: PathBuf::from("temporary test directory"),
            source,
        })?;
        let paths = SkeinPaths::new(temp.path().join("config"), temp.path().join("data"));
        fs::create_dir_all(&paths.data_dir).map_err(|source| Error::Io {
            path: paths.data_dir.clone(),
            source,
        })?;
        let connection = Connection::open(paths.database())?;
        connection.execute_batch(
            "CREATE TABLE projects (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                path TEXT NOT NULL UNIQUE,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            PRAGMA user_version = 1;",
        )?;
        drop(connection);

        let read_only = Registry::open_read_only(&paths)?;
        assert_eq!(read_only.schema_version()?, 1);
        drop(read_only);

        let registry = Registry::open(&paths)?;
        assert_eq!(registry.schema_version()?, SCHEMA_VERSION);
        assert!(registry.list_projects()?.is_empty());
        Ok(())
    }

    #[test]
    fn migrates_schema_version_two_without_changing_projects() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|source| Error::Io {
            path: PathBuf::from("temporary test directory"),
            source,
        })?;
        let paths = SkeinPaths::new(temp.path().join("config"), temp.path().join("data"));
        fs::create_dir_all(&paths.data_dir).map_err(|source| Error::Io {
            path: paths.data_dir.clone(),
            source,
        })?;
        let connection = Connection::open(paths.database())?;
        connection.execute_batch(
            "CREATE TABLE projects (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                path TEXT NOT NULL UNIQUE,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE project_metadata (
                project_id INTEGER PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                vcs_kind TEXT NOT NULL CHECK(vcs_kind IN ('none', 'git')),
                fingerprint TEXT NOT NULL,
                refreshed_at INTEGER NOT NULL,
                head_ref TEXT,
                head_oid TEXT,
                last_commit_at INTEGER,
                last_commit_subject TEXT,
                tracked_dirty INTEGER CHECK(tracked_dirty IN (0, 1))
            );
            INSERT INTO projects (name, path, created_at, updated_at)
                VALUES ('Synthetic', '/synthetic/project', 1, 2);
            PRAGMA user_version = 2;",
        )?;
        drop(connection);

        let registry = Registry::open(&paths)?;
        assert_eq!(registry.schema_version()?, SCHEMA_VERSION);
        assert_eq!(registry.list_projects()?.len(), 1);
        assert!(registry.list_sessions()?.is_empty());
        Ok(())
    }

    #[test]
    fn migrates_schema_version_three_without_changing_session_catalog() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|source| Error::Io {
            path: PathBuf::from("temporary test directory"),
            source,
        })?;
        let paths = SkeinPaths::new(temp.path().join("config"), temp.path().join("data"));
        let project_dir = temp.path().join("project");
        fs::create_dir(&project_dir).map_err(|source| Error::Io {
            path: project_dir.clone(),
            source,
        })?;
        let mut registry = Registry::open(&paths)?;
        registry.add_project(&project_dir, None)?;
        registry.import_sessions(&[crate::SessionObservation {
            source_kind: "codex".to_owned(),
            source_thread_id: "synthetic-thread".to_owned(),
            source_session_id: Some("synthetic-session".to_owned()),
            source_cwd: project_dir,
            source_created_at: 1,
            source_updated_at: 2,
            source_label: "cli".to_owned(),
            observed_status_label: "notLoaded".to_owned(),
            model_provider: Some("synthetic".to_owned()),
            source_version: Some("1.0.0".to_owned()),
            parent_source_thread_id: None,
            forked_from_source_thread_id: None,
            ephemeral: false,
            name: None,
            preview: None,
            text_imported: false,
        }])?;
        registry.connection.execute_batch(
            "DROP TABLE conductor_decision_evidence;
             DROP TABLE conductor_decisions;
             DROP TABLE control_reconciliations;
             DROP TABLE control_workers;
             DROP TABLE control_action_events;
             DROP TABLE control_actions;
             DROP TABLE control_turns;
             DROP TABLE control_runs;
             DROP TABLE control_policies;
             PRAGMA user_version = 3;",
        )?;
        drop(registry);

        let registry = Registry::open(&paths)?;
        assert_eq!(registry.schema_version()?, SCHEMA_VERSION);
        assert_eq!(registry.list_projects()?.len(), 1);
        assert_eq!(registry.list_sessions()?.len(), 1);
        assert!(registry.list_control_runs()?.is_empty());
        Ok(())
    }

    #[test]
    fn migrates_schema_version_four_without_changing_control_history() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|source| Error::Io {
            path: PathBuf::from("temporary test directory"),
            source,
        })?;
        let paths = SkeinPaths::new(temp.path().join("config"), temp.path().join("data"));
        fs::create_dir_all(&paths.data_dir).map_err(|source| Error::Io {
            path: paths.data_dir.clone(),
            source,
        })?;
        let connection = Connection::open(paths.database())?;
        connection.execute_batch(
            "CREATE TABLE projects (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                path TEXT NOT NULL UNIQUE,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE project_metadata (
                project_id INTEGER PRIMARY KEY REFERENCES projects(id) ON DELETE CASCADE,
                vcs_kind TEXT NOT NULL CHECK(vcs_kind IN ('none', 'git')),
                fingerprint TEXT NOT NULL,
                refreshed_at INTEGER NOT NULL,
                head_ref TEXT,
                head_oid TEXT,
                last_commit_at INTEGER,
                last_commit_subject TEXT,
                tracked_dirty INTEGER CHECK(tracked_dirty IN (0, 1))
            );",
        )?;
        connection.execute_batch(CREATE_SESSIONS_SCHEMA)?;
        connection.execute_batch(CREATE_CONTROL_SCHEMA)?;
        connection.execute_batch(
            "INSERT INTO projects (id, name, path, created_at, updated_at)
                VALUES (1, 'Synthetic', '/synthetic/project', 1, 1);
             INSERT INTO control_policies (
                id, created_at, sandbox_mode, approval_mode, network_access, project_id,
                working_directory, acknowledged_at, acknowledgement_source
             ) VALUES (1, 1, 'danger_full_access', 'never', 1, 1,
                '/synthetic/project', 1, 'cli_flag');
             INSERT INTO control_runs (
                id, run_key, project_id, policy_id, runtime_kind, working_directory,
                state, created_at, updated_at, terminal_at
             ) VALUES (1, 'historical-run', 1, 1, 'codex', '/synthetic/project',
                'completed', 1, 2, 2);
             INSERT INTO control_runs (
                id, run_key, project_id, policy_id, runtime_kind, working_directory,
                state, source_thread_id, created_at, updated_at
             ) VALUES (2, 'inflight-run', 1, 1, 'codex', '/synthetic/project',
                'active', 'synthetic-thread', 3, 4);
             INSERT INTO control_turns (
                id, run_id, turn_number, state, input_bytes, terminal_condition_version,
                client_message_id, source_turn_id, created_at
             ) VALUES (1, 2, 1, 'running', 10, 1, 'synthetic-message',
                'synthetic-turn', 3);
             INSERT INTO control_actions (
                id, action_key, run_id, turn_id, policy_id, action_kind, state,
                request_method, request_fingerprint, source_result_id,
                created_at, dispatch_started_at
             ) VALUES (1, 'inflight-action', 2, 1, 1, 'turn_start', 'acknowledged',
                'turn/start', 'turn/start:text', 'synthetic-turn', 3, 3);
             PRAGMA user_version = 4;",
        )?;
        drop(connection);

        let registry = Registry::open(&paths)?;
        assert_eq!(registry.schema_version()?, SCHEMA_VERSION);
        assert_eq!(
            registry.control_run(1)?.expect("historical run").run_key,
            "historical-run"
        );
        assert!(registry.control_worker(1)?.is_none());
        let inflight = registry.control_run(2)?.expect("inflight run");
        assert_eq!(inflight.state, crate::ControlRunState::RecoveryRequired);
        assert_eq!(inflight.ownership_mode, "foreground");
        let turn_state: String = registry.connection.query_row(
            "SELECT state FROM control_turns WHERE run_id = 2",
            [],
            |row| row.get(0),
        )?;
        let action_state: String = registry.connection.query_row(
            "SELECT state FROM control_actions WHERE run_id = 2",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(turn_state, "uncertain");
        assert_eq!(action_state, "uncertain");
        let columns: String = registry.connection.query_row(
            "SELECT group_concat(name, ',') FROM pragma_table_info('control_actions')",
            [],
            |row| row.get(0),
        )?;
        assert!(columns.contains("worker_id"));
        assert!(columns.contains("worker_lease_epoch"));
        Ok(())
    }

    #[test]
    fn migrates_schema_version_five_without_changing_worker_history() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|source| Error::Io {
            path: PathBuf::from("temporary test directory"),
            source,
        })?;
        let paths = SkeinPaths::new(temp.path().join("config"), temp.path().join("data"));
        let project = temp.path().join("project");
        fs::create_dir(&project).map_err(|source| Error::Io {
            path: project.clone(),
            source,
        })?;
        let registry = Registry::open(&paths)?;
        registry.add_project(&project, Some("Synthetic"))?;
        registry.connection.execute_batch(
            "DROP TABLE conductor_decision_evidence;
             DROP TABLE conductor_decisions;
             DROP TABLE control_reconciliations;
             DROP INDEX control_actions_client_request;
             ALTER TABLE control_actions DROP COLUMN expected_source_turn_id;
             ALTER TABLE control_actions DROP COLUMN input_bytes;
             ALTER TABLE control_actions DROP COLUMN client_request_id;
             PRAGMA user_version = 5;",
        )?;
        drop(registry);

        let registry = Registry::open(&paths)?;
        assert_eq!(registry.schema_version()?, SCHEMA_VERSION);
        assert_eq!(registry.list_projects()?.len(), 1);
        let columns: String = registry.connection.query_row(
            "SELECT group_concat(name, ',') FROM pragma_table_info('control_actions')",
            [],
            |row| row.get(0),
        )?;
        assert!(columns.contains("client_request_id"));
        let reconciliation_table: i64 = registry.connection.query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name = 'control_reconciliations'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(reconciliation_table, 1);
        Ok(())
    }

    #[test]
    fn migrates_schema_version_six_without_rewriting_existing_history() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|source| Error::Io {
            path: PathBuf::from("temporary schema-six migration"),
            source,
        })?;
        let paths = SkeinPaths::new(temp.path().join("config"), temp.path().join("data"));
        let project = temp.path().join("project");
        fs::create_dir(&project).map_err(|source| Error::Io {
            path: project.clone(),
            source,
        })?;
        let mut registry = Registry::open(&paths)?;
        registry.add_project(&project, Some("Synthetic"))?;
        let plan = registry.plan_control_run(&crate::NewControlRun {
            project_path: &project,
            resume_thread_id: None,
            prompt: "private migration prompt",
            full_access_acknowledged: true,
        })?;
        registry.connection.execute_batch(
            "DROP TABLE conductor_decision_evidence;
             DROP TABLE conductor_decisions;
             PRAGMA user_version = 6;",
        )?;
        drop(registry);

        let registry = Registry::open(&paths)?;
        assert_eq!(registry.schema_version()?, SCHEMA_VERSION);
        assert_eq!(registry.list_projects()?.len(), 1);
        assert_eq!(registry.list_control_runs()?.len(), 1);
        assert_eq!(registry.list_control_runs()?[0].id, plan.run_id);
        let decisions: i64 = registry.connection.query_row(
            "SELECT COUNT(*) FROM conductor_decisions",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(decisions, 0);
        let foreign_key_violations: i64 = registry.connection.query_row(
            "SELECT COUNT(*) FROM pragma_foreign_key_check",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(foreign_key_violations, 0);
        Ok(())
    }

    #[test]
    fn migrates_schema_version_seven_by_adding_empty_scan_roots() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|source| Error::Io {
            path: PathBuf::from("temporary schema-seven migration"),
            source,
        })?;
        let paths = SkeinPaths::new(temp.path().join("config"), temp.path().join("data"));
        let registry = Registry::open(&paths)?;
        registry.connection.execute_batch(
            "DROP TABLE scan_root_projects;
             DROP TABLE scan_roots;
             PRAGMA user_version = 7;",
        )?;
        drop(registry);

        let registry = Registry::open(&paths)?;
        assert_eq!(registry.schema_version()?, SCHEMA_VERSION);
        assert!(registry.list_scan_roots()?.is_empty());
        let tables: i64 = registry.connection.query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE type = 'table' AND name IN ('scan_roots', 'scan_root_projects')",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(tables, 2);
        Ok(())
    }

    #[test]
    fn migrates_schema_version_eight_by_adding_empty_project_documents() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|source| Error::Io {
            path: PathBuf::from("temporary schema-eight migration"),
            source,
        })?;
        let paths = SkeinPaths::new(temp.path().join("config"), temp.path().join("data"));
        let registry = Registry::open(&paths)?;
        registry.connection.execute_batch(
            "DROP TABLE project_documents_fts;
             DROP TABLE project_documents;
             PRAGMA user_version = 8;",
        )?;
        drop(registry);

        let registry = Registry::open(&paths)?;
        assert_eq!(registry.schema_version()?, SCHEMA_VERSION);
        let tables: i64 = registry.connection.query_row(
            "SELECT COUNT(*) FROM sqlite_master
             WHERE name IN ('project_documents', 'project_documents_fts')",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(tables, 2);
        assert!(
            registry
                .search_project_documents("synthetic", 10)?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn migrates_schema_version_nine_with_private_recall_disabled() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|source| Error::Io {
            path: PathBuf::from("temporary schema-nine migration"),
            source,
        })?;
        let paths = SkeinPaths::new(temp.path().join("config"), temp.path().join("data"));
        let registry = Registry::open(&paths)?;
        registry.connection.execute_batch(
            "DROP TABLE context_documents_fts;
             DROP TABLE context_documents;
             DROP TABLE recall_settings;
             PRAGMA user_version = 9;",
        )?;
        drop(registry);

        let registry = Registry::open(&paths)?;
        assert_eq!(registry.schema_version()?, SCHEMA_VERSION);
        assert_eq!(
            registry.get_recall_settings()?,
            crate::RecallSettings::default()
        );
        assert!(
            registry
                .search_context_documents("synthetic", 10)?
                .is_empty()
        );
        Ok(())
    }

    #[test]
    fn migrates_schema_version_ten_with_safe_empty_incremental_checkpoints() -> Result<()> {
        let temp = tempfile::tempdir().expect("temporary state");
        let paths = SkeinPaths::new(temp.path().join("config"), temp.path().join("data"));
        let registry = Registry::open(&paths)?;
        registry.connection.execute_batch(
            "INSERT INTO context_documents (
                 source_kind, source_path, project_id, context_path, fingerprint,
                 refreshed_at, title, body, imported_bytes, source_bytes
             ) VALUES (
                 'codex_session', 'sessions/synthetic.jsonl', NULL, '/synthetic/project',
                 'fnv1a64:0123456789abcdef', 1234567890, 'Synthetic session',
                 'private synthetic body', 22, 314
             );
             ALTER TABLE context_documents DROP COLUMN source_bytes;
             PRAGMA user_version = 10;",
        )?;
        drop(registry);

        let migrated = Registry::open(&paths)?;
        assert_eq!(migrated.schema_version()?, SCHEMA_VERSION);
        let migrated_row: (
            String,
            String,
            Option<i64>,
            String,
            String,
            i64,
            String,
            String,
            i64,
            i64,
        ) = migrated.connection.query_row(
            "SELECT source_kind, source_path, project_id, context_path, fingerprint,
                        refreshed_at, title, body, imported_bytes, source_bytes
                 FROM context_documents
                 WHERE source_path = 'sessions/synthetic.jsonl'",
            [],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                    row.get(9)?,
                ))
            },
        )?;
        assert_eq!(
            migrated_row,
            (
                "codex_session".to_owned(),
                "sessions/synthetic.jsonl".to_owned(),
                None,
                "/synthetic/project".to_owned(),
                "fnv1a64:0123456789abcdef".to_owned(),
                1_234_567_890,
                "Synthetic session".to_owned(),
                "private synthetic body".to_owned(),
                22,
                0,
            )
        );
        Ok(())
    }

    #[test]
    fn migrates_schema_version_eleven_preserving_private_rows_and_fts() -> Result<()> {
        let temp = tempfile::tempdir().expect("temporary state");
        let paths = SkeinPaths::new(temp.path().join("config"), temp.path().join("data"));
        let registry = Registry::open(&paths)?;
        registry.connection.execute_batch(
            "INSERT INTO context_documents (
                 source_kind, source_path, project_id, context_path, fingerprint,
                 refreshed_at, title, body, imported_bytes, source_bytes
             ) VALUES (
                 'codex_session', 'sessions/preserved.jsonl', NULL, '/synthetic/project',
                 'fnv1a64:fedcba9876543210', 1234567890, 'Preserved session',
                 'private migration marker', 24, 987654
             );
             INSERT INTO context_documents_fts(rowid, title, body)
                  VALUES (last_insert_rowid(), 'Preserved session', 'private migration marker');
             ALTER TABLE context_documents DROP COLUMN source_modified_at_ns;
             ALTER TABLE context_documents DROP COLUMN source_thread_id;
             ALTER TABLE context_documents DROP COLUMN source_created_at;
             ALTER TABLE context_documents DROP COLUMN source_updated_at;
             PRAGMA user_version = 11;",
        )?;
        drop(registry);

        let migrated = Registry::open(&paths)?;
        assert_eq!(migrated.schema_version()?, SCHEMA_VERSION);
        #[derive(Debug, PartialEq)]
        struct MigratedContextRow {
            source_path: String,
            fingerprint: String,
            body: String,
            source_bytes: i64,
            source_modified_at_ns: i64,
            source_thread_id: Option<String>,
            source_created_at: Option<i64>,
            source_updated_at: Option<i64>,
        }
        let row = migrated.connection.query_row(
            "SELECT source_path, fingerprint, body, source_bytes,
                        source_modified_at_ns, source_thread_id,
                        source_created_at, source_updated_at
                   FROM context_documents WHERE source_path = 'sessions/preserved.jsonl'",
            [],
            |row| {
                Ok(MigratedContextRow {
                    source_path: row.get(0)?,
                    fingerprint: row.get(1)?,
                    body: row.get(2)?,
                    source_bytes: row.get(3)?,
                    source_modified_at_ns: row.get(4)?,
                    source_thread_id: row.get(5)?,
                    source_created_at: row.get(6)?,
                    source_updated_at: row.get(7)?,
                })
            },
        )?;
        assert_eq!(
            row,
            MigratedContextRow {
                source_path: "sessions/preserved.jsonl".to_owned(),
                fingerprint: "fnv1a64:fedcba9876543210".to_owned(),
                body: "private migration marker".to_owned(),
                source_bytes: 987_654,
                source_modified_at_ns: 0,
                source_thread_id: None,
                source_created_at: None,
                source_updated_at: None,
            }
        );
        migrated.set_recall_settings(crate::RecallSettings {
            include_codex_memories: false,
            include_codex_sessions: true,
        })?;
        assert_eq!(
            migrated
                .search_context_documents("migration marker", 10)?
                .len(),
            1
        );
        Ok(())
    }

    #[test]
    fn adds_and_updates_a_canonical_project() -> Result<()> {
        let (temp, registry) = isolated_registry()?;
        let project_dir = temp.path().join("example");
        fs::create_dir(&project_dir).map_err(|source| Error::Io {
            path: project_dir.clone(),
            source,
        })?;

        let first = registry.add_project(&project_dir, None)?;
        assert_eq!(first.name, "example");

        let updated = registry.add_project(&project_dir, Some("Example Project"))?;
        assert_eq!(updated.id, first.id);
        assert_eq!(updated.name, "Example Project");
        assert_eq!(registry.list_projects()?.len(), 1);
        Ok(())
    }

    #[test]
    fn removes_only_an_unused_registered_project() -> Result<()> {
        let (temp, mut registry) = isolated_registry()?;
        let project_dir = temp.path().join("removable");
        fs::create_dir(&project_dir).map_err(|source| Error::Io {
            path: project_dir.clone(),
            source,
        })?;
        let added = registry.add_project(&project_dir, None)?;
        let removed = registry.remove_project_if_unused(&project_dir)?;
        assert_eq!(removed.id, added.id);
        assert!(registry.list_projects()?.is_empty());
        Ok(())
    }

    #[test]
    fn rejects_a_missing_project_directory() -> Result<()> {
        let (temp, registry) = isolated_registry()?;
        let missing = temp.path().join("missing");
        assert!(matches!(
            registry.add_project(&missing, None),
            Err(Error::InvalidProjectPath(path)) if path == missing
        ));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn creates_private_state_permissions() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let temp = tempfile::tempdir().map_err(|source| Error::Io {
            path: PathBuf::from("temporary test directory"),
            source,
        })?;
        let paths = SkeinPaths::new(temp.path().join("config"), temp.path().join("data"));
        let _registry = Registry::open(&paths)?;

        assert_eq!(
            fs::metadata(&paths.data_dir)
                .map_err(|source| Error::Io {
                    path: paths.data_dir.clone(),
                    source,
                })?
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(paths.database())
                .map_err(|source| Error::Io {
                    path: paths.database(),
                    source,
                })?
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
        Ok(())
    }

    #[test]
    fn opens_initialized_registry_read_only() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|source| Error::Io {
            path: PathBuf::from("temporary test directory"),
            source,
        })?;
        let paths = SkeinPaths::new(temp.path().join("config"), temp.path().join("data"));
        let registry = Registry::open(&paths)?;
        assert_eq!(registry.schema_version()?, SCHEMA_VERSION);
        drop(registry);

        let read_only = Registry::open_read_only(&paths)?;
        assert_eq!(read_only.schema_version()?, SCHEMA_VERSION);
        Ok(())
    }

    #[test]
    fn refreshes_git_metadata_incrementally_and_checks_tracked_files_on_request() -> Result<()> {
        let (temp, registry) = isolated_registry()?;
        let project_dir = temp.path().join("synthetic-repository");
        fs::create_dir(&project_dir).map_err(|source| Error::Io {
            path: project_dir.clone(),
            source,
        })?;
        run_git(&project_dir, ["init", "-b", "main"])?;
        run_git(&project_dir, ["config", "user.name", "Synthetic User"])?;
        run_git(
            &project_dir,
            ["config", "user.email", "synthetic@example.invalid"],
        )?;
        let readme = project_dir.join("README.md");
        fs::write(&readme, "synthetic project\n").map_err(|source| Error::Io {
            path: readme.clone(),
            source,
        })?;
        run_git(&project_dir, ["add", "README.md"])?;
        run_git(&project_dir, ["commit", "-m", "Initial synthetic commit"])?;
        registry.add_project(&project_dir, None)?;

        let first = registry.refresh_project(&project_dir, false, false)?;
        assert_eq!(first.status, RefreshStatus::Updated);
        let first_git = first.project.git.as_ref().expect("Git metadata");
        assert_eq!(first_git.head_ref.as_deref(), Some("main"));
        assert_eq!(
            first_git.last_commit_subject.as_deref(),
            Some("Initial synthetic commit")
        );
        assert_eq!(first_git.tracked_dirty, None);

        let unchanged = registry.refresh_project(&project_dir, false, false)?;
        assert_eq!(unchanged.status, RefreshStatus::Unchanged);

        fs::write(&readme, "changed tracked content\n").map_err(|source| Error::Io {
            path: readme,
            source,
        })?;
        let still_fast = registry.refresh_project(&project_dir, false, false)?;
        assert_eq!(still_fast.status, RefreshStatus::Unchanged);

        let checked = registry.refresh_project(&project_dir, true, false)?;
        assert_eq!(checked.status, RefreshStatus::Updated);
        assert_eq!(
            checked.project.git.and_then(|value| value.tracked_dirty),
            Some(true)
        );

        run_git(&project_dir, ["add", "README.md"])?;
        run_git(&project_dir, ["commit", "-m", "Update synthetic content"])?;
        let committed = registry.refresh_project(&project_dir, false, false)?;
        assert_eq!(committed.status, RefreshStatus::Updated);
        let committed_git = committed.project.git.expect("Git metadata");
        assert_eq!(
            committed_git.last_commit_subject.as_deref(),
            Some("Update synthetic content")
        );
        assert_eq!(committed_git.tracked_dirty, None);
        Ok(())
    }

    #[test]
    fn records_non_git_directories_without_error() -> Result<()> {
        let (temp, registry) = isolated_registry()?;
        let project_dir = temp.path().join("plain-directory");
        fs::create_dir(&project_dir).map_err(|source| Error::Io {
            path: project_dir.clone(),
            source,
        })?;
        registry.add_project(&project_dir, None)?;

        let first = registry.refresh_project(&project_dir, false, false)?;
        assert_eq!(first.status, RefreshStatus::Updated);
        assert!(first.project.git.is_none());
        assert!(first.project.metadata_refreshed_at.is_some());

        registry.connection.execute(
            "UPDATE project_metadata SET refreshed_at = 1 WHERE project_id = ?1",
            [first.project.id],
        )?;
        let second = registry.refresh_project(&project_dir, false, false)?;
        assert_eq!(second.status, RefreshStatus::Unchanged);
        assert!(
            second
                .project
                .metadata_refreshed_at
                .is_some_and(|value| value > 1)
        );
        Ok(())
    }

    #[test]
    fn refreshes_an_unborn_git_repository() -> Result<()> {
        let (temp, registry) = isolated_registry()?;
        let project_dir = temp.path().join("empty-repository");
        fs::create_dir(&project_dir).map_err(|source| Error::Io {
            path: project_dir.clone(),
            source,
        })?;
        run_git(&project_dir, ["init", "-b", "main"])?;
        registry.add_project(&project_dir, None)?;

        let report = registry.refresh_project(&project_dir, false, false)?;
        assert_eq!(report.status, RefreshStatus::Updated);
        let git = report.project.git.expect("Git metadata");
        assert_eq!(git.head_ref.as_deref(), Some("main"));
        assert_eq!(git.head_oid, None);
        assert_eq!(git.last_commit_at, None);
        assert_eq!(git.last_commit_subject, None);
        Ok(())
    }

    fn run_git<const N: usize>(project: &Path, args: [&str; N]) -> Result<()> {
        let output = Command::new("git")
            .arg("-C")
            .arg(project)
            .args(args)
            .output()
            .map_err(|source| Error::GitUnavailable {
                path: project.to_path_buf(),
                source,
            })?;
        if output.status.success() {
            return Ok(());
        }
        Err(Error::GitCommand {
            path: project.to_path_buf(),
            status: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        })
    }
}
