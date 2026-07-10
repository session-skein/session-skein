use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;
use std::time::SystemTime;
use std::time::UNIX_EPOCH;

use rusqlite::Connection;
use rusqlite::OpenFlags;
use rusqlite::OptionalExtension;
use rusqlite::params;
use serde::Serialize;

use crate::Error;
use crate::GitMetadata;
use crate::Result;
use crate::SkeinPaths;
use crate::git;

const SCHEMA_VERSION: i64 = 2;

const PROJECT_SELECT: &str = "SELECT p.id, p.name, p.path, p.updated_at,
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
    connection: Connection,
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

    /// List all registered projects in stable name/path order.
    pub fn list_projects(&self) -> Result<Vec<Project>> {
        let query = format!("{PROJECT_SELECT} ORDER BY p.name COLLATE NOCASE, p.path");
        let mut statement = self.connection.prepare(&query)?;
        let projects = statement
            .query_map([], project_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(projects)
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
            return Ok(RefreshReport {
                status: RefreshStatus::Unchanged,
                project,
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

fn project_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
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

fn unix_timestamp() -> i64 {
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
        assert_eq!(registry.schema_version()?, 2);
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
        assert_eq!(registry.schema_version()?, 2);
        assert!(registry.list_projects()?.is_empty());
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

        let second = registry.refresh_project(&project_dir, false, false)?;
        assert_eq!(second.status, RefreshStatus::Unchanged);
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
