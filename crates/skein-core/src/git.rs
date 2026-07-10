use std::ffi::OsStr;
use std::fs;
use std::path::Path;
use std::path::PathBuf;
use std::process::Command;
use std::time::UNIX_EPOCH;

use serde::Serialize;

use crate::Error;
use crate::Result;

const MAX_ADMIN_FILE_BYTES: u64 = 64 * 1024;

/// A bounded snapshot of local Git metadata.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct GitMetadata {
    /// Short branch name, or `None` for a detached or unborn head.
    pub head_ref: Option<String>,
    /// Full object ID for `HEAD`, when the repository has a commit.
    pub head_oid: Option<String>,
    /// Unix timestamp of the latest commit.
    pub last_commit_at: Option<i64>,
    /// Subject of the latest commit. This remains in the private local database.
    pub last_commit_subject: Option<String>,
    /// Whether tracked files differ from `HEAD`; `None` means no working-tree check ran.
    pub tracked_dirty: Option<bool>,
}

pub(crate) struct GitProbe {
    pub(crate) fingerprint: String,
    head_ref: Option<String>,
}

pub(crate) struct GitObservation {
    pub(crate) fingerprint: String,
    pub(crate) metadata: GitMetadata,
}

pub(crate) fn probe(project: &Path) -> Result<Option<GitProbe>> {
    let Some(git_dir) = locate_git_dir(project)? else {
        return Ok(None);
    };
    let common_dir = locate_common_dir(&git_dir)?;
    let head_path = git_dir.join("HEAD");
    let head = read_small_text(&head_path)?;
    let head_ref_path = head.strip_prefix("ref: ").map(str::trim);
    let head_ref = head_ref_path
        .and_then(|value| value.strip_prefix("refs/heads/"))
        .map(ToOwned::to_owned);

    let mut parts = vec![format!("head={head}"), file_stamp(&git_dir.join("index"))?];
    if let Some(reference) = head_ref_path {
        parts.push(file_content_fingerprint(&common_dir.join(reference))?);
    }
    parts.push(file_stamp(&common_dir.join("packed-refs"))?);

    Ok(Some(GitProbe {
        fingerprint: parts.join("|"),
        head_ref,
    }))
}

pub(crate) fn inspect(
    project: &Path,
    probe: GitProbe,
    working_tree: bool,
) -> Result<GitObservation> {
    let head_output = run_git(project, ["rev-parse", "--verify", "HEAD"])?;
    let head_oid = if head_output.status.success() {
        nonempty_line(&head_output.stdout)
    } else {
        None
    };

    let (last_commit_at, last_commit_subject) = match head_oid.as_deref() {
        Some(oid) => {
            let output = run_git(project, ["show", "-s", "--format=%ct%x00%s", oid])?;
            ensure_success(project, &output)?;
            parse_commit(&output.stdout)
        }
        None => (None, None),
    };

    let tracked_dirty = if working_tree {
        let output = if head_oid.is_some() {
            run_git(
                project,
                [
                    "diff-index",
                    "--quiet",
                    "--ignore-submodules=all",
                    "HEAD",
                    "--",
                ],
            )?
        } else {
            run_git(
                project,
                [
                    "diff",
                    "--cached",
                    "--quiet",
                    "--ignore-submodules=all",
                    "--",
                ],
            )?
        };
        Some(dirty_exit_status(project, &output)?)
    } else {
        None
    };

    Ok(GitObservation {
        fingerprint: probe.fingerprint,
        metadata: GitMetadata {
            head_ref: probe.head_ref,
            head_oid,
            last_commit_at,
            last_commit_subject,
            tracked_dirty,
        },
    })
}

fn locate_git_dir(project: &Path) -> Result<Option<PathBuf>> {
    let dot_git = project.join(".git");
    if dot_git.is_dir() {
        return Ok(Some(dot_git));
    }
    if !dot_git.is_file() {
        return Ok(None);
    }

    let content = read_small_text(&dot_git)?;
    let Some(value) = content.trim().strip_prefix("gitdir:") else {
        return Ok(None);
    };
    let value = PathBuf::from(value.trim());
    let git_dir = if value.is_absolute() {
        value
    } else {
        project.join(value)
    };
    Ok(git_dir.is_dir().then_some(git_dir))
}

fn locate_common_dir(git_dir: &Path) -> Result<PathBuf> {
    let path = git_dir.join("commondir");
    if !path.is_file() {
        return Ok(git_dir.to_path_buf());
    }
    let value = PathBuf::from(read_small_text(&path)?.trim());
    Ok(if value.is_absolute() {
        value
    } else {
        git_dir.join(value)
    })
}

fn read_small_text(path: &Path) -> Result<String> {
    let metadata = fs::metadata(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })?;
    if metadata.len() > MAX_ADMIN_FILE_BYTES {
        return Err(Error::GitMetadataTooLarge {
            path: path.to_path_buf(),
            limit: MAX_ADMIN_FILE_BYTES,
        });
    }
    fs::read_to_string(path).map_err(|source| Error::Io {
        path: path.to_path_buf(),
        source,
    })
}

fn file_stamp(path: &Path) -> Result<String> {
    let metadata = match fs::metadata(path) {
        Ok(value) => value,
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            return Ok(format!("{}=missing", path.display()));
        }
        Err(source) => {
            return Err(Error::Io {
                path: path.to_path_buf(),
                source,
            });
        }
    };
    let modified = metadata
        .modified()
        .ok()
        .and_then(|value| value.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |value| value.as_nanos());
    Ok(format!(
        "{}={}:{}",
        path.display(),
        metadata.len(),
        modified
    ))
}

fn file_content_fingerprint(path: &Path) -> Result<String> {
    match fs::metadata(path) {
        Ok(_) => Ok(format!(
            "{}={}",
            path.display(),
            read_small_text(path)?.trim()
        )),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            Ok(format!("{}=missing", path.display()))
        }
        Err(source) => Err(Error::Io {
            path: path.to_path_buf(),
            source,
        }),
    }
}

fn run_git<I, S>(project: &Path, args: I) -> Result<std::process::Output>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    Command::new("git")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("LC_ALL", "C")
        .arg("-C")
        .arg(project)
        .args(args)
        .output()
        .map_err(|source| Error::GitUnavailable {
            path: project.to_path_buf(),
            source,
        })
}

fn ensure_success(project: &Path, output: &std::process::Output) -> Result<()> {
    if output.status.success() {
        return Ok(());
    }
    Err(Error::GitCommand {
        path: project.to_path_buf(),
        status: output.status.code().unwrap_or(-1),
        stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
    })
}

fn dirty_exit_status(project: &Path, output: &std::process::Output) -> Result<bool> {
    match output.status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => Err(Error::GitCommand {
            path: project.to_path_buf(),
            status: output.status.code().unwrap_or(-1),
            stderr: String::from_utf8_lossy(&output.stderr).trim().to_owned(),
        }),
    }
}

fn nonempty_line(bytes: &[u8]) -> Option<String> {
    String::from_utf8_lossy(bytes)
        .lines()
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn parse_commit(bytes: &[u8]) -> (Option<i64>, Option<String>) {
    let value = String::from_utf8_lossy(bytes);
    let mut fields = value.trim_end().splitn(2, '\0');
    let timestamp = fields.next().and_then(|value| value.parse().ok());
    let subject = fields
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);
    (timestamp, subject)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_oversized_git_administrative_files() -> Result<()> {
        let temp = tempfile::tempdir().map_err(|source| Error::Io {
            path: PathBuf::from("temporary test directory"),
            source,
        })?;
        let git_dir = temp.path().join(".git");
        fs::create_dir(&git_dir).map_err(|source| Error::Io {
            path: git_dir.clone(),
            source,
        })?;
        let head = git_dir.join("HEAD");
        fs::write(&head, vec![b'x'; (MAX_ADMIN_FILE_BYTES + 1) as usize]).map_err(|source| {
            Error::Io {
                path: head.clone(),
                source,
            }
        })?;

        assert!(matches!(
            probe(temp.path()),
            Err(Error::GitMetadataTooLarge { path, limit })
                if path == head && limit == MAX_ADMIN_FILE_BYTES
        ));
        Ok(())
    }
}
