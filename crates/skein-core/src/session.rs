use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::path::PathBuf;

use rusqlite::OptionalExtension;
use rusqlite::params;
use serde::Serialize;

use crate::Error;
use crate::Project;
use crate::Registry;
use crate::Result;
use crate::registry::unix_timestamp;

const SESSION_SELECT: &str = "SELECT s.id, s.source_kind, s.source_thread_id,
    s.source_session_id, s.project_id, p.name, p.path, s.project_link_kind,
    s.source_cwd, s.source_created_at, s.source_updated_at, s.first_seen_at,
    s.last_seen_at, s.source_label, s.status_label, s.model_provider,
    s.source_version, s.parent_source_thread_id, s.forked_from_source_thread_id,
    s.ephemeral, s.name, s.preview, s.text_imported
    FROM sessions s LEFT JOIN projects p ON p.id = s.project_id";

/// Durable metadata for one externally owned agent session.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Session {
    pub id: i64,
    pub source_kind: String,
    pub source_thread_id: String,
    pub source_session_id: Option<String>,
    pub project_id: Option<i64>,
    pub project_name: Option<String>,
    pub project_path: Option<PathBuf>,
    pub project_link_kind: ProjectLinkKind,
    pub source_cwd: PathBuf,
    pub source_created_at: i64,
    pub source_updated_at: i64,
    pub first_seen_at: i64,
    pub last_seen_at: i64,
    pub source_label: String,
    pub observed_status_label: String,
    pub model_provider: Option<String>,
    pub source_version: Option<String>,
    pub parent_source_thread_id: Option<String>,
    pub forked_from_source_thread_id: Option<String>,
    pub ephemeral: bool,
    pub name: Option<String>,
    pub preview: Option<String>,
    pub text_imported: bool,
}

/// How a durable session is associated with a registered project.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectLinkKind {
    Automatic,
    Manual,
    ManualUnbound,
    Unmatched,
}

impl ProjectLinkKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Automatic => "automatic",
            Self::Manual => "manual",
            Self::ManualUnbound => "manual_unbound",
            Self::Unmatched => "unmatched",
        }
    }

    fn from_str(value: &str) -> rusqlite::Result<Self> {
        match value {
            "automatic" => Ok(Self::Automatic),
            "manual" => Ok(Self::Manual),
            "manual_unbound" => Ok(Self::ManualUnbound),
            "unmatched" => Ok(Self::Unmatched),
            _ => Err(rusqlite::Error::InvalidQuery),
        }
    }
}

/// Source-neutral metadata observed from an adapter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SessionObservation {
    pub source_kind: String,
    pub source_thread_id: String,
    pub source_session_id: Option<String>,
    pub source_cwd: PathBuf,
    pub source_created_at: i64,
    pub source_updated_at: i64,
    pub source_label: String,
    pub observed_status_label: String,
    pub model_provider: Option<String>,
    pub source_version: Option<String>,
    pub parent_source_thread_id: Option<String>,
    pub forked_from_source_thread_id: Option<String>,
    pub ephemeral: bool,
    pub name: Option<String>,
    pub preview: Option<String>,
    pub text_imported: bool,
}

/// Counts from one atomic metadata synchronization.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionImportReport {
    pub observed: usize,
    pub inserted: usize,
    pub updated: usize,
    pub unchanged: usize,
    pub linked_to_projects: usize,
    pub unassigned: usize,
}

impl Registry {
    /// Atomically insert or refresh a bounded set of adapter observations.
    pub fn import_sessions(
        &mut self,
        observations: &[SessionObservation],
    ) -> Result<SessionImportReport> {
        validate_observations(observations)?;
        let projects = self.list_projects()?;
        let seen_at = unix_timestamp();
        let transaction = self.connection.transaction()?;
        let mut report = SessionImportReport {
            observed: observations.len(),
            inserted: 0,
            updated: 0,
            unchanged: 0,
            linked_to_projects: 0,
            unassigned: 0,
        };

        for observation in observations {
            let existing = session_by_source_connection(
                &transaction,
                &observation.source_kind,
                &observation.source_thread_id,
            )?;
            if let Some(current) = existing.as_ref()
                && observation.source_updated_at < current.source_updated_at
            {
                if current.project_id.is_some() {
                    report.linked_to_projects += 1;
                } else {
                    report.unassigned += 1;
                }
                report.unchanged += 1;
                transaction.execute(
                    "UPDATE sessions SET last_seen_at = ?1 WHERE id = ?2",
                    params![seen_at, current.id],
                )?;
                continue;
            }
            let (project_id, link_kind) =
                match existing.as_ref().map(|value| value.project_link_kind) {
                    Some(ProjectLinkKind::Manual) => (
                        existing.as_ref().and_then(|value| value.project_id),
                        ProjectLinkKind::Manual,
                    ),
                    Some(ProjectLinkKind::ManualUnbound) => (None, ProjectLinkKind::ManualUnbound),
                    _ => match_project(&projects, &observation.source_cwd)
                        .map_or((None, ProjectLinkKind::Unmatched), |project| {
                            (Some(project.id), ProjectLinkKind::Automatic)
                        }),
                };

            if project_id.is_some() {
                report.linked_to_projects += 1;
            } else {
                report.unassigned += 1;
            }

            match existing {
                None => report.inserted += 1,
                Some(ref current)
                    if observation_changes(current, observation, project_id, link_kind) =>
                {
                    report.updated += 1;
                }
                Some(_) => report.unchanged += 1,
            }

            let source_cwd = observation.source_cwd.to_str().ok_or_else(|| {
                Error::InvalidSessionObservation("source cwd must be valid UTF-8".to_owned())
            })?;
            transaction.execute(
                "INSERT INTO sessions (
                    source_kind, source_thread_id, source_session_id, project_id,
                    project_link_kind, source_cwd, source_created_at, source_updated_at,
                    first_seen_at, last_seen_at, source_label, status_label, model_provider,
                    source_version, parent_source_thread_id, forked_from_source_thread_id,
                    ephemeral, name, preview, text_imported
                 ) VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?9, ?10, ?11, ?12,
                    ?13, ?14, ?15, ?16, ?17, ?18, ?19
                 ) ON CONFLICT(source_kind, source_thread_id) DO UPDATE SET
                    source_session_id = excluded.source_session_id,
                    project_id = excluded.project_id,
                    project_link_kind = excluded.project_link_kind,
                    source_cwd = excluded.source_cwd,
                    source_created_at = excluded.source_created_at,
                    source_updated_at = excluded.source_updated_at,
                    last_seen_at = excluded.last_seen_at,
                    source_label = excluded.source_label,
                    status_label = excluded.status_label,
                    model_provider = excluded.model_provider,
                    source_version = excluded.source_version,
                    parent_source_thread_id = excluded.parent_source_thread_id,
                    forked_from_source_thread_id = excluded.forked_from_source_thread_id,
                    ephemeral = excluded.ephemeral,
                    name = CASE WHEN excluded.text_imported = 1 THEN excluded.name ELSE sessions.name END,
                    preview = CASE WHEN excluded.text_imported = 1 THEN excluded.preview ELSE sessions.preview END,
                    text_imported = MAX(sessions.text_imported, excluded.text_imported)",
                params![
                    observation.source_kind,
                    observation.source_thread_id,
                    observation.source_session_id,
                    project_id,
                    link_kind.as_str(),
                    source_cwd,
                    observation.source_created_at,
                    observation.source_updated_at,
                    seen_at,
                    observation.source_label,
                    observation.observed_status_label,
                    observation.model_provider,
                    observation.source_version,
                    observation.parent_source_thread_id,
                    observation.forked_from_source_thread_id,
                    i64::from(observation.ephemeral),
                    observation.name,
                    observation.preview,
                    i64::from(observation.text_imported),
                ],
            )?;
        }

        transaction.commit()?;
        Ok(report)
    }

    /// List all durable sessions newest-first with stable source-id tie breaking.
    pub fn list_sessions(&self) -> Result<Vec<Session>> {
        let query = format!(
            "{SESSION_SELECT} ORDER BY s.source_updated_at DESC, s.source_kind, s.source_thread_id"
        );
        let mut statement = self.connection.prepare(&query)?;
        statement
            .query_map([], session_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Error::from)
    }

    /// Find one durable session by its adapter-owned identity.
    pub fn session_by_source(
        &self,
        source_kind: &str,
        source_thread_id: &str,
    ) -> Result<Option<Session>> {
        session_by_source_connection(&self.connection, source_kind, source_thread_id)
    }

    /// Bind a session explicitly to an already registered project.
    pub fn bind_session(
        &self,
        source_kind: &str,
        source_thread_id: &str,
        project_path: &Path,
    ) -> Result<Session> {
        let project = self.get_project(project_path)?;
        let changed = self.connection.execute(
            "UPDATE sessions SET project_id = ?1, project_link_kind = 'manual'
             WHERE source_kind = ?2 AND source_thread_id = ?3",
            params![project.id, source_kind, source_thread_id],
        )?;
        if changed == 0 {
            return Err(Error::SessionNotFound {
                source_kind: source_kind.to_owned(),
                source_thread_id: source_thread_id.to_owned(),
            });
        }
        self.session_by_source(source_kind, source_thread_id)?
            .ok_or_else(|| Error::SessionNotFound {
                source_kind: source_kind.to_owned(),
                source_thread_id: source_thread_id.to_owned(),
            })
    }

    /// Remove a project association and preserve that explicit choice during sync.
    pub fn unbind_session(&self, source_kind: &str, source_thread_id: &str) -> Result<Session> {
        let changed = self.connection.execute(
            "UPDATE sessions SET project_id = NULL, project_link_kind = 'manual_unbound'
             WHERE source_kind = ?1 AND source_thread_id = ?2",
            params![source_kind, source_thread_id],
        )?;
        if changed == 0 {
            return Err(Error::SessionNotFound {
                source_kind: source_kind.to_owned(),
                source_thread_id: source_thread_id.to_owned(),
            });
        }
        self.session_by_source(source_kind, source_thread_id)?
            .ok_or_else(|| Error::SessionNotFound {
                source_kind: source_kind.to_owned(),
                source_thread_id: source_thread_id.to_owned(),
            })
    }
}

fn validate_observations(observations: &[SessionObservation]) -> Result<()> {
    let mut identities = HashSet::with_capacity(observations.len());
    for observation in observations {
        if observation.source_kind.trim().is_empty()
            || observation.source_thread_id.trim().is_empty()
        {
            return Err(Error::InvalidSessionObservation(
                "source kind and thread id must be non-empty".to_owned(),
            ));
        }
        if observation.source_cwd.to_str().is_none() {
            return Err(Error::InvalidSessionObservation(
                "source cwd must be valid UTF-8".to_owned(),
            ));
        }
        if !observation.text_imported
            && (observation.name.is_some() || observation.preview.is_some())
        {
            return Err(Error::InvalidSessionObservation(
                "redacted observations cannot contain text".to_owned(),
            ));
        }
        let identity = (&observation.source_kind, &observation.source_thread_id);
        if !identities.insert(identity) {
            return Err(Error::InvalidSessionObservation(format!(
                "duplicate session identity {}:{}",
                observation.source_kind, observation.source_thread_id
            )));
        }
    }
    Ok(())
}

fn match_project<'a>(projects: &'a [Project], cwd: &Path) -> Option<&'a Project> {
    if !cwd.is_absolute() {
        return None;
    }
    let canonical_cwd = fs::canonicalize(cwd).ok()?;
    projects
        .iter()
        .filter(|project| canonical_cwd.starts_with(&project.path))
        .max_by_key(|project| project.path.components().count())
}

fn observation_changes(
    current: &Session,
    observation: &SessionObservation,
    project_id: Option<i64>,
    link_kind: ProjectLinkKind,
) -> bool {
    current.source_session_id != observation.source_session_id
        || current.project_id != project_id
        || current.project_link_kind != link_kind
        || current.source_cwd != observation.source_cwd
        || current.source_created_at != observation.source_created_at
        || current.source_updated_at != observation.source_updated_at
        || current.source_label != observation.source_label
        || current.observed_status_label != observation.observed_status_label
        || current.model_provider != observation.model_provider
        || current.source_version != observation.source_version
        || current.parent_source_thread_id != observation.parent_source_thread_id
        || current.forked_from_source_thread_id != observation.forked_from_source_thread_id
        || current.ephemeral != observation.ephemeral
        || (observation.text_imported
            && (current.name != observation.name
                || current.preview != observation.preview
                || !current.text_imported))
}

fn session_by_source_connection(
    connection: &rusqlite::Connection,
    source_kind: &str,
    source_thread_id: &str,
) -> Result<Option<Session>> {
    let query = format!("{SESSION_SELECT} WHERE s.source_kind = ?1 AND s.source_thread_id = ?2");
    connection
        .query_row(
            &query,
            params![source_kind, source_thread_id],
            session_from_row,
        )
        .optional()
        .map_err(Error::from)
}

fn session_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Session> {
    Ok(Session {
        id: row.get(0)?,
        source_kind: row.get(1)?,
        source_thread_id: row.get(2)?,
        source_session_id: row.get(3)?,
        project_id: row.get(4)?,
        project_name: row.get(5)?,
        project_path: row.get::<_, Option<String>>(6)?.map(PathBuf::from),
        project_link_kind: ProjectLinkKind::from_str(&row.get::<_, String>(7)?)?,
        source_cwd: PathBuf::from(row.get::<_, String>(8)?),
        source_created_at: row.get(9)?,
        source_updated_at: row.get(10)?,
        first_seen_at: row.get(11)?,
        last_seen_at: row.get(12)?,
        source_label: row.get(13)?,
        observed_status_label: row.get(14)?,
        model_provider: row.get(15)?,
        source_version: row.get(16)?,
        parent_source_thread_id: row.get(17)?,
        forked_from_source_thread_id: row.get(18)?,
        ephemeral: row.get::<_, i64>(19)? != 0,
        name: row.get(20)?,
        preview: row.get(21)?,
        text_imported: row.get::<_, i64>(22)? != 0,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SkeinPaths;

    fn observation(cwd: &Path, id: &str) -> SessionObservation {
        SessionObservation {
            source_kind: "codex".to_owned(),
            source_thread_id: id.to_owned(),
            source_session_id: Some("synthetic-tree".to_owned()),
            source_cwd: cwd.to_path_buf(),
            source_created_at: 10,
            source_updated_at: 20,
            source_label: "cli".to_owned(),
            observed_status_label: "notLoaded".to_owned(),
            model_provider: Some("synthetic-provider".to_owned()),
            source_version: Some("1.2.3".to_owned()),
            parent_source_thread_id: None,
            forked_from_source_thread_id: None,
            ephemeral: false,
            name: None,
            preview: None,
            text_imported: false,
        }
    }

    fn registry() -> Result<(tempfile::TempDir, Registry)> {
        let temp = tempfile::tempdir().map_err(|source| Error::Io {
            path: PathBuf::from("temporary test directory"),
            source,
        })?;
        let paths = SkeinPaths::new(temp.path().join("config"), temp.path().join("data"));
        let registry = Registry::open(&paths)?;
        Ok((temp, registry))
    }

    #[test]
    fn imports_sessions_idempotently_and_links_the_longest_project() -> Result<()> {
        let (temp, mut registry) = registry()?;
        let outer = temp.path().join("workspace");
        let inner = outer.join("project");
        let cwd = inner.join("nested");
        fs::create_dir_all(&cwd).map_err(|source| Error::Io {
            path: cwd.clone(),
            source,
        })?;
        registry.add_project(&outer, Some("Workspace"))?;
        let project = registry.add_project(&inner, Some("Project"))?;

        let first = registry.import_sessions(&[observation(&cwd, "thread-1")])?;
        assert_eq!(first.inserted, 1);
        assert_eq!(first.linked_to_projects, 1);
        let stored = registry
            .session_by_source("codex", "thread-1")?
            .expect("stored session");
        assert_eq!(stored.project_id, Some(project.id));
        assert_eq!(stored.project_link_kind, ProjectLinkKind::Automatic);

        let second = registry.import_sessions(&[observation(&cwd, "thread-1")])?;
        assert_eq!(second.unchanged, 1);
        assert_eq!(registry.list_sessions()?.len(), 1);
        assert_eq!(
            registry
                .session_by_source("codex", "thread-1")?
                .expect("stored session")
                .id,
            stored.id
        );
        Ok(())
    }

    #[test]
    fn does_not_match_similar_path_prefixes() -> Result<()> {
        let (temp, mut registry) = registry()?;
        let registered = temp.path().join("app");
        let other = temp.path().join("application");
        fs::create_dir(&registered).map_err(|source| Error::Io {
            path: registered.clone(),
            source,
        })?;
        fs::create_dir(&other).map_err(|source| Error::Io {
            path: other.clone(),
            source,
        })?;
        registry.add_project(&registered, None)?;

        let report = registry.import_sessions(&[observation(&other, "thread-2")])?;
        assert_eq!(report.unassigned, 1);
        assert_eq!(
            registry
                .session_by_source("codex", "thread-2")?
                .expect("stored session")
                .project_link_kind,
            ProjectLinkKind::Unmatched
        );

        let relative = observation(Path::new("app"), "thread-relative");
        assert_eq!(registry.import_sessions(&[relative])?.unassigned, 1);
        Ok(())
    }

    #[test]
    fn preserves_explicit_text_and_manual_project_choices() -> Result<()> {
        let (temp, mut registry) = registry()?;
        let first_project = temp.path().join("first");
        let second_project = temp.path().join("second");
        fs::create_dir(&first_project).map_err(|source| Error::Io {
            path: first_project.clone(),
            source,
        })?;
        fs::create_dir(&second_project).map_err(|source| Error::Io {
            path: second_project.clone(),
            source,
        })?;
        registry.add_project(&first_project, None)?;
        let second = registry.add_project(&second_project, None)?;
        let mut with_text = observation(&first_project, "thread-3");
        with_text.name = Some("Synthetic title".to_owned());
        with_text.preview = Some("Synthetic preview".to_owned());
        with_text.text_imported = true;
        registry.import_sessions(&[with_text])?;

        let bound = registry.bind_session("codex", "thread-3", &second_project)?;
        assert_eq!(bound.project_id, Some(second.id));
        assert_eq!(bound.project_link_kind, ProjectLinkKind::Manual);

        registry.import_sessions(&[observation(&first_project, "thread-3")])?;
        let preserved = registry
            .session_by_source("codex", "thread-3")?
            .expect("stored session");
        assert_eq!(preserved.project_id, Some(second.id));
        assert_eq!(preserved.name.as_deref(), Some("Synthetic title"));
        assert_eq!(preserved.preview.as_deref(), Some("Synthetic preview"));
        assert!(preserved.text_imported);

        registry.unbind_session("codex", "thread-3")?;
        registry.import_sessions(&[observation(&first_project, "thread-3")])?;
        let unbound = registry
            .session_by_source("codex", "thread-3")?
            .expect("stored session");
        assert_eq!(unbound.project_id, None);
        assert_eq!(unbound.project_link_kind, ProjectLinkKind::ManualUnbound);
        Ok(())
    }

    #[test]
    fn rejects_an_invalid_page_without_partial_writes() -> Result<()> {
        let (temp, mut registry) = registry()?;
        let valid = observation(temp.path(), "thread-4");
        let mut invalid = observation(temp.path(), "");
        invalid.name = Some("must not persist".to_owned());

        assert!(matches!(
            registry.import_sessions(&[valid, invalid]),
            Err(Error::InvalidSessionObservation(_))
        ));
        assert!(registry.list_sessions()?.is_empty());
        Ok(())
    }

    #[test]
    fn updates_transient_status_without_a_newer_source_timestamp() -> Result<()> {
        let (temp, mut registry) = registry()?;
        let original = observation(temp.path(), "thread-5");
        registry.import_sessions(std::slice::from_ref(&original))?;
        let mut changed = original;
        changed.observed_status_label = "active".to_owned();

        let report = registry.import_sessions(&[changed])?;
        assert_eq!(report.updated, 1);
        assert_eq!(
            registry
                .session_by_source("codex", "thread-5")?
                .expect("stored session")
                .observed_status_label,
            "active"
        );
        Ok(())
    }

    #[test]
    fn stale_observations_cannot_regress_newer_metadata_or_text() -> Result<()> {
        let (temp, mut registry) = registry()?;
        let mut newer = observation(temp.path(), "thread-6");
        newer.source_updated_at = 200;
        newer.observed_status_label = "active".to_owned();
        newer.name = Some("Newer title".to_owned());
        newer.preview = Some("Newer preview".to_owned());
        newer.text_imported = true;
        registry.import_sessions(&[newer])?;

        let mut stale = observation(temp.path(), "thread-6");
        stale.source_updated_at = 100;
        stale.observed_status_label = "idle".to_owned();
        stale.name = Some("Stale title".to_owned());
        stale.preview = Some("Stale preview".to_owned());
        stale.text_imported = true;
        let report = registry.import_sessions(&[stale])?;
        assert_eq!(report.unchanged, 1);

        let stored = registry
            .session_by_source("codex", "thread-6")?
            .expect("stored session");
        assert_eq!(stored.source_updated_at, 200);
        assert_eq!(stored.observed_status_label, "active");
        assert_eq!(stored.name.as_deref(), Some("Newer title"));
        assert_eq!(stored.preview.as_deref(), Some("Newer preview"));
        Ok(())
    }

    #[test]
    fn rejects_duplicate_identities_before_writing() -> Result<()> {
        let (temp, mut registry) = registry()?;
        let first = observation(temp.path(), "duplicate");
        let second = first.clone();
        assert!(matches!(
            registry.import_sessions(&[first, second]),
            Err(Error::InvalidSessionObservation(message)) if message.contains("duplicate")
        ));
        assert!(registry.list_sessions()?.is_empty());
        Ok(())
    }
}
