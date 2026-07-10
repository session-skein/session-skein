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
use crate::Result;
use crate::SkeinPaths;

const SCHEMA_VERSION: i64 = 1;

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
        let mut statement = self.connection.prepare(
            "SELECT id, name, path, updated_at FROM projects ORDER BY name COLLATE NOCASE, path",
        )?;
        let projects = statement
            .query_map([], project_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(projects)
    }

    fn project_by_path(&self, path: &Path) -> Result<Option<Project>> {
        self.connection
            .query_row(
                "SELECT id, name, path, updated_at FROM projects WHERE path = ?1",
                [path.to_string_lossy()],
                project_from_row,
            )
            .optional()
            .map_err(Error::from)
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
                PRAGMA user_version = 1;",
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
        if found != SCHEMA_VERSION {
            return Err(Error::UnsupportedSchema {
                found,
                supported: SCHEMA_VERSION,
            });
        }
        Ok(())
    }
}

fn project_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Project> {
    Ok(Project {
        id: row.get(0)?,
        name: row.get(1)?,
        path: PathBuf::from(row.get::<_, String>(2)?),
        updated_at: row.get(3)?,
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
    fn initializes_schema_version_one() -> Result<()> {
        let (_temp, registry) = isolated_registry()?;
        assert_eq!(registry.schema_version()?, 1);
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
}
