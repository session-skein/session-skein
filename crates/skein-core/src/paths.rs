use std::path::PathBuf;

use directories::BaseDirs;
use serde::Serialize;

use crate::Error;
use crate::Result;

/// All user-specific filesystem locations used by Session Skein.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct SkeinPaths {
    /// Directory for user-editable configuration.
    pub config_dir: PathBuf,
    /// Directory for private databases and generated state.
    pub data_dir: PathBuf,
}

impl SkeinPaths {
    /// Resolve paths from explicit overrides or platform-standard user directories.
    pub fn discover() -> Result<Self> {
        let base_dirs = BaseDirs::new().ok_or(Error::MissingUserDirectories)?;

        let config_dir = std::env::var_os("SKEIN_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| base_dirs.config_dir().join("session-skein"));
        let data_dir = std::env::var_os("SKEIN_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| base_dirs.data_local_dir().join("session-skein"));

        Ok(Self {
            config_dir,
            data_dir,
        })
    }

    /// Construct paths explicitly, primarily for embedding and isolated tests.
    #[must_use]
    pub fn new(config_dir: PathBuf, data_dir: PathBuf) -> Self {
        Self {
            config_dir,
            data_dir,
        }
    }

    /// Location of the versioned state database.
    #[must_use]
    pub fn database(&self) -> PathBuf {
        self.data_dir.join("skein.sqlite3")
    }
}
