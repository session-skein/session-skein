//! Fast, local-first state primitives for Session Skein.

mod control;
mod git;
mod paths;
mod registry;
mod session;
mod worker;

pub use control::ControlAction;
pub use control::ControlActionKind;
pub use control::ControlActionState;
pub use control::ControlPlan;
pub use control::ControlRun;
pub use control::ControlRunDetail;
pub use control::ControlRunState;
pub use control::InterruptPlan;
pub use control::NewControlRun;
pub use control::ReconciliationObservation;
pub use control::ReconciliationPlan;
pub use control::SteerPlan;
pub use git::GitMetadata;
pub use paths::SkeinPaths;
pub use registry::Project;
pub use registry::RefreshReport;
pub use registry::RefreshStatus;
pub use registry::Registry;
pub use session::ProjectLinkKind;
pub use session::Session;
pub use session::SessionImportReport;
pub use session::SessionObservation;
pub use worker::ControlWorker;
pub use worker::WorkerClaim;
pub use worker::WorkerConnection;
pub use worker::WorkerState;

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

    /// A path was not present in the explicit project registry.
    #[error("project is not registered: {0}")]
    ProjectNotRegistered(std::path::PathBuf),

    /// An adapter observation violated the durable session contract.
    #[error("invalid session observation: {0}")]
    InvalidSessionObservation(String),

    /// No durable session matched the adapter-owned identity.
    #[error("session is not registered: {source_kind}:{source_thread_id}")]
    SessionNotFound {
        /// Adapter identity, such as `codex`.
        source_kind: String,
        /// Opaque thread identifier owned by the adapter.
        source_thread_id: String,
    },

    /// A control request violated a policy or state-machine invariant.
    #[error("invalid control request: {0}")]
    InvalidControlRequest(String),

    /// A conditional control transition lost a race or used the wrong state.
    #[error("control state conflict: {0}")]
    ControlStateConflict(String),

    /// Git could not be started on this machine.
    #[error("could not start Git for {path}: {source}")]
    GitUnavailable {
        /// Project path passed to Git.
        path: std::path::PathBuf,
        /// Underlying process-spawn error.
        source: std::io::Error,
    },

    /// A read-only Git command failed.
    #[error("Git inspection failed for {path} with status {status}: {stderr}")]
    GitCommand {
        /// Project path passed to Git.
        path: std::path::PathBuf,
        /// Process exit status, or `-1` when unavailable.
        status: i32,
        /// Sanitized standard error from Git.
        stderr: String,
    },

    /// A Git administrative file exceeded the bounded metadata-read limit.
    #[error("Git metadata file exceeds the {limit}-byte read limit: {path}")]
    GitMetadataTooLarge {
        /// Administrative file that exceeded the limit.
        path: std::path::PathBuf,
        /// Maximum accepted size in bytes.
        limit: u64,
    },
}

/// Result type used throughout the core library.
pub type Result<T> = std::result::Result<T, Error>;
