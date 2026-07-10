//! Fast, local-first state primitives for Session Skein.

mod paths;
mod registry;

pub use paths::SkeinPaths;
pub use registry::Project;
pub use registry::Registry;

/// Errors returned by Session Skein's core library.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// No supported per-user data directories could be discovered.
    #[error("could not determine per-user config and data directories")]
    MissingUserDirectories,

    /// A filesystem operation failed.
    #[error("filesystem operation failed for {path}: {source}")]
    Io {
        /// Path involved in the failed operation.
        path: std::path::PathBuf,
        /// Underlying I/O error.
        source: std::io::Error,
    },

    /// A SQLite operation failed.
    #[error("state database operation failed: {0}")]
    Sqlite(#[from] rusqlite::Error),

    /// The database was created by an incompatible Session Skein version.
    #[error("unsupported state schema version {found}; this build supports {supported}")]
    UnsupportedSchema {
        /// Version stored in the database.
        found: i64,
        /// Version understood by this build.
        supported: i64,
    },

    /// A project path was not an existing directory.
    #[error("project path is not an existing directory: {0}")]
    InvalidProjectPath(std::path::PathBuf),

    /// An existing directory had no usable final path component.
    #[error("project path has no usable name: {0}")]
    MissingProjectName(std::path::PathBuf),
}

/// Result type used throughout the core library.
pub type Result<T> = std::result::Result<T, Error>;
