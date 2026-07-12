use std::collections::VecDeque;
use std::fs;
use std::path::Component;
use std::path::Path;
use std::path::PathBuf;

use rusqlite::OptionalExtension;
use rusqlite::TransactionBehavior;
use rusqlite::params;
use serde::Serialize;

use crate::Error;
use crate::Project;
use crate::Registry;
use crate::Result;
use crate::registry::unix_timestamp;

/// Default depth for an explicitly recursive root. The root itself is depth zero.
pub const DEFAULT_RECURSIVE_MAX_DEPTH: u16 = 16;
/// Hard safety bound for recursive discovery.
pub const MAX_SCAN_DEPTH: u16 = 64;

const EXCLUDED_DIRECTORY_NAMES: &[&str] = &[
    ".git",
    ".cache",
    ".mypy_cache",
    ".pytest_cache",
    ".tox",
    ".venv",
    "__pycache__",
    "build",
    "cache",
    "dist",
    "node_modules",
    "target",
    "vendor",
    "venv",
];

pub(crate) const CREATE_SCAN_ROOT_SCHEMA: &str = "CREATE TABLE IF NOT EXISTS scan_roots (
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL UNIQUE,
    recursive INTEGER NOT NULL CHECK(recursive IN (0, 1)),
    max_depth INTEGER NOT NULL CHECK(max_depth >= 0 AND max_depth <= 64),
    created_at INTEGER NOT NULL,
    updated_at INTEGER NOT NULL,
    last_scanned_at INTEGER
);
CREATE TABLE IF NOT EXISTS scan_root_projects (
    scan_root_id INTEGER NOT NULL REFERENCES scan_roots(id) ON DELETE CASCADE,
    project_id INTEGER NOT NULL REFERENCES projects(id) ON DELETE CASCADE,
    first_discovered_at INTEGER NOT NULL,
    last_seen_at INTEGER NOT NULL,
    PRIMARY KEY(scan_root_id, project_id)
);
CREATE INDEX IF NOT EXISTS scan_root_projects_project ON scan_root_projects(project_id);";

const SCAN_ROOT_SELECT: &str =
    "SELECT id, path, recursive, max_depth, created_at, updated_at, last_scanned_at
     FROM scan_roots";

/// Durable options for one explicitly authorized filesystem root.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize)]
pub struct ScanRootOptions {
    /// Descend beneath the root when true. False examines only the root itself.
    pub recursive: bool,
    /// Maximum child-directory depth. `None` selects 16 for recursive roots and 0 otherwise.
    pub max_depth: Option<u16>,
}

/// An explicitly registered directory from which Git repositories may be discovered.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct ScanRoot {
    /// Stable database identifier.
    pub id: i64,
    /// Normalized absolute path captured when the root was registered.
    pub path: PathBuf,
    /// Whether discovery descends beneath this directory.
    pub recursive: bool,
    /// Maximum depth, where the root itself is depth zero.
    pub max_depth: u16,
    /// Unix timestamp of initial registration.
    pub created_at: i64,
    /// Unix timestamp of the latest option update.
    pub updated_at: i64,
    /// Unix timestamp of the latest completed discovery attempt.
    pub last_scanned_at: Option<i64>,
}

/// Why a filesystem entry was deliberately not traversed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DiscoverySkipReason {
    /// A fixed, expensive dependency/build/cache directory.
    Excluded,
    /// A symbolic link, which is never followed.
    Symlink,
    /// A directory beyond the root's configured depth.
    MaxDepth,
}

/// One deliberately skipped filesystem entry.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DiscoverySkip {
    /// Entry path.
    pub path: PathBuf,
    /// Stable skip classification.
    pub reason: DiscoverySkipReason,
}

/// One non-fatal filesystem discovery error.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DiscoveryError {
    /// Path which could not be inspected.
    pub path: PathBuf,
    /// Portable I/O error category.
    pub kind: String,
    /// Local operating-system diagnostic.
    pub message: String,
}

/// Complete result of one root discovery attempt.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DiscoveryReport {
    /// Root and effective options used for this attempt.
    pub root: ScanRoot,
    /// Repositories observed, in stable path order.
    pub discovered: Vec<Project>,
    /// Number of newly inserted project records.
    pub newly_registered: usize,
    /// Number of repositories which already existed in the project registry.
    pub already_registered: usize,
    /// Whether the root itself was unavailable.
    pub unreachable: bool,
    /// Deliberately pruned entries.
    pub skipped: Vec<DiscoverySkip>,
    /// Non-fatal filesystem failures. Discovery continues after each entry failure.
    pub errors: Vec<DiscoveryError>,
}

impl Registry {
    /// Add a scan root, or update its explicit recursive options.
    pub fn add_scan_root(&self, path: &Path, options: ScanRootOptions) -> Result<ScanRoot> {
        if !path.is_dir() {
            return Err(Error::InvalidScanRoot(path.to_path_buf()));
        }
        let canonical_path = fs::canonicalize(path).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })?;
        let max_depth = effective_max_depth(options)?;
        let timestamp = unix_timestamp();
        self.connection.execute(
            "INSERT INTO scan_roots (
                path, recursive, max_depth, created_at, updated_at, last_scanned_at
             ) VALUES (?1, ?2, ?3, ?4, ?4, NULL)
             ON CONFLICT(path) DO UPDATE SET
                recursive = excluded.recursive,
                max_depth = excluded.max_depth,
                updated_at = excluded.updated_at",
            params![
                canonical_path.to_string_lossy(),
                i64::from(options.recursive),
                i64::from(max_depth),
                timestamp,
            ],
        )?;
        self.scan_root_by_path(&canonical_path)?
            .ok_or(rusqlite::Error::QueryReturnedNoRows.into())
    }

    /// List roots in stable path order without touching the filesystem.
    pub fn list_scan_roots(&self) -> Result<Vec<ScanRoot>> {
        let query = format!("{SCAN_ROOT_SELECT} ORDER BY path");
        let mut statement = self.connection.prepare(&query)?;
        statement
            .query_map([], scan_root_from_row)?
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Error::from)
    }

    /// Remove a root by its stored normalized path, even when it is now unavailable.
    pub fn remove_scan_root(&mut self, path: &Path) -> Result<ScanRoot> {
        let normalized = normalize_absolute_path(path)?;
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)?;
        let query = format!("{SCAN_ROOT_SELECT} WHERE path = ?1");
        let root = transaction
            .query_row(&query, [normalized.to_string_lossy()], scan_root_from_row)
            .optional()?
            .ok_or_else(|| Error::ScanRootNotRegistered(normalized.clone()))?;
        let removed = transaction.execute("DELETE FROM scan_roots WHERE id = ?1", [root.id])?;
        if removed != 1 {
            return Err(Error::ControlStateConflict(format!(
                "scan root {} disappeared before removal",
                root.id
            )));
        }
        transaction.commit()?;
        Ok(root)
    }

    /// Discover repositories beneath one registered root and register them idempotently.
    pub fn discover_scan_root(&self, path: &Path) -> Result<DiscoveryReport> {
        let normalized = if path.exists() {
            fs::canonicalize(path).map_err(|source| Error::Io {
                path: path.to_path_buf(),
                source,
            })?
        } else {
            normalize_absolute_path(path)?
        };
        let root = self
            .scan_root_by_path(&normalized)?
            .ok_or_else(|| Error::ScanRootNotRegistered(normalized.clone()))?;
        self.discover_root(root)
    }

    /// Discover all registered roots sequentially in stable path order.
    pub fn discover_all_scan_roots(&self) -> Result<Vec<DiscoveryReport>> {
        self.list_scan_roots()?
            .into_iter()
            .map(|root| self.discover_root(root))
            .collect()
    }

    fn discover_root(&self, root: ScanRoot) -> Result<DiscoveryReport> {
        let mut report = DiscoveryReport {
            root,
            discovered: Vec::new(),
            newly_registered: 0,
            already_registered: 0,
            unreachable: false,
            skipped: Vec::new(),
            errors: Vec::new(),
        };
        if !report.root.path.is_dir() {
            report.unreachable = true;
            match fs::metadata(&report.root.path) {
                Err(error) => report
                    .errors
                    .push(discovery_error(&report.root.path, &error)),
                Ok(_) => report.errors.push(DiscoveryError {
                    path: report.root.path.clone(),
                    kind: "not_a_directory".to_owned(),
                    message: "scan root is no longer a directory".to_owned(),
                }),
            }
            self.finish_discovery(&mut report)?;
            return Ok(report);
        }

        let mut pending = VecDeque::from([(report.root.path.clone(), 0_u16)]);
        while let Some((directory, depth)) = pending.pop_front() {
            if is_git_repository(&directory) {
                match self.add_discovered_project(&directory) {
                    Ok((project, inserted)) => {
                        if inserted {
                            report.newly_registered += 1;
                        } else {
                            report.already_registered += 1;
                        }
                        report.discovered.push(project);
                    }
                    Err(error) => report.errors.push(DiscoveryError {
                        path: directory.clone(),
                        kind: "registration".to_owned(),
                        message: error.to_string(),
                    }),
                }
            }

            if !report.root.recursive {
                continue;
            }
            let entries = match fs::read_dir(&directory) {
                Ok(entries) => entries,
                Err(error) => {
                    report.errors.push(discovery_error(&directory, &error));
                    continue;
                }
            };
            for entry in entries {
                let entry = match entry {
                    Ok(entry) => entry,
                    Err(error) => {
                        report.errors.push(discovery_error(&directory, &error));
                        continue;
                    }
                };
                let path = entry.path();
                let file_type = match entry.file_type() {
                    Ok(file_type) => file_type,
                    Err(error) => {
                        report.errors.push(discovery_error(&path, &error));
                        continue;
                    }
                };
                if file_type.is_symlink() {
                    report.skipped.push(DiscoverySkip {
                        path,
                        reason: DiscoverySkipReason::Symlink,
                    });
                    continue;
                }
                if !file_type.is_dir() {
                    continue;
                }
                if is_excluded_directory(&entry.file_name()) {
                    report.skipped.push(DiscoverySkip {
                        path,
                        reason: DiscoverySkipReason::Excluded,
                    });
                } else if depth >= report.root.max_depth {
                    report.skipped.push(DiscoverySkip {
                        path,
                        reason: DiscoverySkipReason::MaxDepth,
                    });
                } else {
                    pending.push_back((path, depth + 1));
                }
            }
        }

        report
            .discovered
            .sort_by(|left, right| left.path.cmp(&right.path));
        self.finish_discovery(&mut report)?;
        Ok(report)
    }

    fn finish_discovery(&self, report: &mut DiscoveryReport) -> Result<()> {
        let timestamp = unix_timestamp();
        for project in &report.discovered {
            self.connection.execute(
                "INSERT INTO scan_root_projects (
                    scan_root_id, project_id, first_discovered_at, last_seen_at
                 ) VALUES (?1, ?2, ?3, ?3)
                 ON CONFLICT(scan_root_id, project_id) DO UPDATE SET
                    last_seen_at = excluded.last_seen_at",
                params![report.root.id, project.id, timestamp],
            )?;
        }
        self.connection.execute(
            "UPDATE scan_roots SET last_scanned_at = ?1 WHERE id = ?2",
            params![timestamp, report.root.id],
        )?;
        report.root.last_scanned_at = Some(timestamp);
        Ok(())
    }

    fn scan_root_by_path(&self, path: &Path) -> Result<Option<ScanRoot>> {
        let query = format!("{SCAN_ROOT_SELECT} WHERE path = ?1");
        self.connection
            .query_row(&query, [path.to_string_lossy()], scan_root_from_row)
            .optional()
            .map_err(Error::from)
    }
}

fn effective_max_depth(options: ScanRootOptions) -> Result<u16> {
    if !options.recursive {
        return match options.max_depth {
            Some(depth) => Err(Error::ScanDepthRequiresRecursive(depth)),
            None => Ok(0),
        };
    }
    if let Some(depth) = options.max_depth.filter(|depth| *depth > MAX_SCAN_DEPTH) {
        return Err(Error::InvalidScanDepth {
            found: depth,
            maximum: MAX_SCAN_DEPTH,
        });
    }
    Ok(options.max_depth.unwrap_or(DEFAULT_RECURSIVE_MAX_DEPTH))
}

fn scan_root_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<ScanRoot> {
    Ok(ScanRoot {
        id: row.get(0)?,
        path: PathBuf::from(row.get::<_, String>(1)?),
        recursive: row.get::<_, i64>(2)? != 0,
        max_depth: u16::try_from(row.get::<_, i64>(3)?).unwrap_or(MAX_SCAN_DEPTH),
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
        last_scanned_at: row.get(6)?,
    })
}

fn is_git_repository(path: &Path) -> bool {
    let marker = path.join(".git");
    fs::symlink_metadata(marker).is_ok_and(|metadata| metadata.is_dir() || metadata.is_file())
}

fn is_excluded_directory(name: &std::ffi::OsStr) -> bool {
    name.to_str()
        .is_some_and(|name| EXCLUDED_DIRECTORY_NAMES.contains(&name))
}

fn discovery_error(path: &Path, error: &std::io::Error) -> DiscoveryError {
    DiscoveryError {
        path: path.to_path_buf(),
        kind: format!("{:?}", error.kind()).to_lowercase(),
        message: error.to_string(),
    }
}

fn normalize_absolute_path(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|source| Error::Io {
                path: PathBuf::from("."),
                source,
            })?
            .join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SkeinPaths;
    use tempfile::TempDir;

    fn isolated_registry() -> Result<(TempDir, Registry)> {
        let temp = tempfile::tempdir().map_err(|source| Error::Io {
            path: PathBuf::from("temporary test directory"),
            source,
        })?;
        let paths = SkeinPaths::new(temp.path().join("config"), temp.path().join("data"));
        Ok((temp, Registry::open(&paths)?))
    }

    fn create_repository(path: &Path) -> Result<()> {
        fs::create_dir_all(path.join(".git")).map_err(|source| Error::Io {
            path: path.to_path_buf(),
            source,
        })
    }

    #[test]
    fn recursively_discovers_nested_repositories() -> Result<()> {
        let (temp, registry) = isolated_registry()?;
        let root = temp.path().join("workspace");
        let first = root.join("first");
        let second = root.join("group").join("second");
        create_repository(&first)?;
        create_repository(&second)?;
        registry.add_scan_root(
            &root,
            ScanRootOptions {
                recursive: true,
                max_depth: None,
            },
        )?;

        let report = registry.discover_scan_root(&root)?;
        assert_eq!(report.newly_registered, 2);
        assert_eq!(report.already_registered, 0);
        assert_eq!(
            report
                .discovered
                .iter()
                .map(|project| project.path.clone())
                .collect::<Vec<_>>(),
            vec![first, second]
        );
        Ok(())
    }

    #[test]
    fn excludes_heavy_directories_and_never_follows_symlinks() -> Result<()> {
        let (temp, registry) = isolated_registry()?;
        let root = temp.path().join("workspace");
        let visible = root.join("visible");
        let excluded = root.join("node_modules").join("hidden");
        let linked_target = temp.path().join("linked-target");
        create_repository(&visible)?;
        create_repository(&excluded)?;
        create_repository(&linked_target)?;
        #[cfg(unix)]
        std::os::unix::fs::symlink(&linked_target, root.join("linked")).map_err(|source| {
            Error::Io {
                path: root.join("linked"),
                source,
            }
        })?;
        registry.add_scan_root(
            &root,
            ScanRootOptions {
                recursive: true,
                max_depth: None,
            },
        )?;

        let report = registry.discover_scan_root(&root)?;
        assert_eq!(report.discovered.len(), 1);
        assert_eq!(report.discovered[0].path, visible);
        assert!(report.skipped.iter().any(|skip| {
            skip.path.ends_with("node_modules") && skip.reason == DiscoverySkipReason::Excluded
        }));
        #[cfg(unix)]
        assert!(report.skipped.iter().any(|skip| {
            skip.path.ends_with("linked") && skip.reason == DiscoverySkipReason::Symlink
        }));
        Ok(())
    }

    #[test]
    fn nonrecursive_root_examines_only_the_exact_directory() -> Result<()> {
        let (temp, registry) = isolated_registry()?;
        let root = temp.path().join("repository");
        let nested = root.join("nested");
        create_repository(&root)?;
        create_repository(&nested)?;
        let stored = registry.add_scan_root(&root, ScanRootOptions::default())?;
        assert!(!stored.recursive);
        assert_eq!(stored.max_depth, 0);

        let report = registry.discover_scan_root(&root)?;
        assert_eq!(report.discovered.len(), 1);
        assert_eq!(report.discovered[0].path, root);
        Ok(())
    }

    #[test]
    fn missing_root_is_reported_and_can_be_removed_by_stored_path() -> Result<()> {
        let (temp, mut registry) = isolated_registry()?;
        let root = temp.path().join("disconnected-volume");
        fs::create_dir(&root).map_err(|source| Error::Io {
            path: root.clone(),
            source,
        })?;
        let stored = registry.add_scan_root(
            &root,
            ScanRootOptions {
                recursive: true,
                max_depth: None,
            },
        )?;
        fs::remove_dir(&root).map_err(|source| Error::Io {
            path: root.clone(),
            source,
        })?;

        let report = registry.discover_scan_root(&stored.path)?;
        assert!(report.unreachable);
        assert!(report.discovered.is_empty());
        assert_eq!(report.errors.len(), 1);
        let removed = registry.remove_scan_root(&stored.path)?;
        assert_eq!(removed.id, stored.id);
        assert!(registry.list_scan_roots()?.is_empty());
        Ok(())
    }

    #[test]
    fn recognizes_worktree_git_file() -> Result<()> {
        let (temp, registry) = isolated_registry()?;
        let root = temp.path().join("worktree");
        fs::create_dir(&root).map_err(|source| Error::Io {
            path: root.clone(),
            source,
        })?;
        fs::write(
            root.join(".git"),
            "gitdir: ../metadata/worktrees/synthetic\n",
        )
        .map_err(|source| Error::Io {
            path: root.join(".git"),
            source,
        })?;
        registry.add_scan_root(&root, ScanRootOptions::default())?;

        let report = registry.discover_scan_root(&root)?;
        assert_eq!(report.newly_registered, 1);
        assert_eq!(report.discovered.len(), 1);
        Ok(())
    }

    #[test]
    fn repeated_discovery_is_idempotent_and_preserves_custom_names() -> Result<()> {
        let (temp, registry) = isolated_registry()?;
        let root = temp.path().join("workspace");
        let repository = root.join("repository");
        create_repository(&repository)?;
        registry.add_project(&repository, Some("Custom name"))?;
        registry.add_scan_root(
            &root,
            ScanRootOptions {
                recursive: true,
                max_depth: Some(4),
            },
        )?;

        let first = registry.discover_scan_root(&root)?;
        let second = registry.discover_scan_root(&root)?;
        assert_eq!(first.newly_registered, 0);
        assert_eq!(first.already_registered, 1);
        assert_eq!(second.newly_registered, 0);
        assert_eq!(second.already_registered, 1);
        assert_eq!(registry.list_projects()?.len(), 1);
        assert_eq!(registry.list_projects()?[0].name, "Custom name");
        Ok(())
    }

    #[test]
    fn honors_depth_and_rejects_depths_above_the_bound() -> Result<()> {
        let (temp, registry) = isolated_registry()?;
        let root = temp.path().join("workspace");
        let too_deep = root.join("one").join("two");
        create_repository(&too_deep)?;
        registry.add_scan_root(
            &root,
            ScanRootOptions {
                recursive: true,
                max_depth: Some(1),
            },
        )?;
        let report = registry.discover_scan_root(&root)?;
        assert!(report.discovered.is_empty());
        assert!(
            report
                .skipped
                .iter()
                .any(|skip| skip.reason == DiscoverySkipReason::MaxDepth)
        );

        assert!(matches!(
            registry.add_scan_root(
                &root,
                ScanRootOptions {
                    recursive: true,
                    max_depth: Some(MAX_SCAN_DEPTH + 1),
                }
            ),
            Err(Error::InvalidScanDepth { .. })
        ));
        assert!(matches!(
            registry.add_scan_root(
                &root,
                ScanRootOptions {
                    recursive: false,
                    max_depth: Some(0),
                }
            ),
            Err(Error::ScanDepthRequiresRecursive(0))
        ));
        Ok(())
    }
}
