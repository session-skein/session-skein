use std::error::Error;
use std::path::Path;
use std::process::Command;

use serde_json::Value;

fn skein(data_dir: &Path, config_dir: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_skein"));
    command
        .env("SKEIN_DATA_DIR", data_dir)
        .env("SKEIN_CONFIG_DIR", config_dir);
    command
}

#[test]
fn doctor_does_not_create_state() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let data = temp.path().join("data");
    let config = temp.path().join("config");
    let output = skein(&data, &config).args(["doctor", "--json"]).output()?;

    assert!(output.status.success());
    let report: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(report["database_exists"], false);
    assert!(!data.exists());
    assert!(!config.exists());
    Ok(())
}

#[test]
fn initializes_adds_and_lists_a_project() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let data = temp.path().join("data");
    let config = temp.path().join("config");
    let project = temp.path().join("project");
    std::fs::create_dir(&project)?;

    let init = skein(&data, &config).args(["init", "--json"]).output()?;
    assert!(init.status.success());

    let add = skein(&data, &config)
        .arg("project")
        .arg("add")
        .arg(&project)
        .args(["--name", "Synthetic Project", "--json"])
        .output()?;
    assert!(add.status.success());

    let list = skein(&data, &config)
        .args(["project", "list", "--json"])
        .output()?;
    assert!(list.status.success());
    let projects: Value = serde_json::from_slice(&list.stdout)?;
    assert_eq!(projects[0]["name"], "Synthetic Project");
    let canonical_project = project.canonicalize()?;
    assert_eq!(projects[0]["path"].as_str(), canonical_project.to_str());

    let refreshed = skein(&data, &config)
        .args(["project", "refresh", "--all", "--json"])
        .output()?;
    assert!(refreshed.status.success());
    let reports: Value = serde_json::from_slice(&refreshed.stdout)?;
    assert_eq!(reports[0]["status"], "updated");
    assert!(reports[0]["project"]["git"].is_null());
    Ok(())
}

#[test]
fn refreshes_and_shows_bounded_git_metadata() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let data = temp.path().join("data");
    let config = temp.path().join("config");
    let project = temp.path().join("synthetic-repository");
    std::fs::create_dir(&project)?;
    git(&project, ["init", "-b", "main"])?;
    git(&project, ["config", "user.name", "Synthetic User"])?;
    git(
        &project,
        ["config", "user.email", "synthetic@example.invalid"],
    )?;
    let tracked = project.join("tracked.txt");
    std::fs::write(&tracked, "initial\n")?;
    git(&project, ["add", "tracked.txt"])?;
    git(&project, ["commit", "-m", "Synthetic snapshot"])?;

    assert!(
        skein(&data, &config)
            .arg("project")
            .arg("add")
            .arg(&project)
            .output()?
            .status
            .success()
    );
    let refreshed = skein(&data, &config)
        .arg("project")
        .arg("refresh")
        .arg(&project)
        .arg("--json")
        .output()?;
    assert!(refreshed.status.success());
    let report: Value = serde_json::from_slice(&refreshed.stdout)?;
    assert_eq!(report["status"], "updated");
    assert_eq!(report["project"]["git"]["head_ref"], "main");
    assert_eq!(
        report["project"]["git"]["last_commit_subject"],
        "Synthetic snapshot"
    );
    assert!(report["project"]["git"]["tracked_dirty"].is_null());

    let unchanged = skein(&data, &config)
        .arg("project")
        .arg("refresh")
        .arg(&project)
        .arg("--json")
        .output()?;
    assert!(unchanged.status.success());
    let report: Value = serde_json::from_slice(&unchanged.stdout)?;
    assert_eq!(report["status"], "unchanged");

    std::fs::write(tracked, "changed\n")?;
    let checked = skein(&data, &config)
        .arg("project")
        .arg("refresh")
        .arg(&project)
        .args(["--working-tree", "--json"])
        .output()?;
    assert!(checked.status.success());
    let report: Value = serde_json::from_slice(&checked.stdout)?;
    assert_eq!(report["project"]["git"]["tracked_dirty"], true);

    let shown = skein(&data, &config)
        .arg("project")
        .arg("show")
        .arg(&project)
        .arg("--json")
        .output()?;
    assert!(shown.status.success());
    let shown: Value = serde_json::from_slice(&shown.stdout)?;
    assert_eq!(shown["git"]["tracked_dirty"], true);
    Ok(())
}

#[test]
fn refresh_requires_an_explicit_scope() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let output = skein(&temp.path().join("data"), &temp.path().join("config"))
        .args(["project", "refresh"])
        .output()?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("required arguments"));
    Ok(())
}

#[test]
fn codex_preview_rejects_an_out_of_range_limit_before_launch() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let output = skein(&temp.path().join("data"), &temp.path().join("config"))
        .env("SKEIN_CODEX_BIN", temp.path().join("must-not-run"))
        .args(["import", "codex", "preview", "--limit", "0"])
        .output()?;

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("1..=1000"));
    assert!(!temp.path().join("data").exists());
    Ok(())
}

#[test]
fn codex_preview_reports_a_missing_configured_executable() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let output = skein(&temp.path().join("data"), &temp.path().join("config"))
        .env("SKEIN_CODEX_BIN", temp.path().join("missing-codex"))
        .args(["import", "codex", "preview", "--json"])
        .output()?;

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("app-server I/O failed"));
    assert!(!temp.path().join("data").exists());
    Ok(())
}

fn git<const N: usize>(project: &Path, args: [&str; N]) -> Result<(), Box<dyn Error>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(project)
        .args(args)
        .output()?;
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "Git fixture failed: {}",
        String::from_utf8_lossy(&output.stderr)
    )
    .into())
}
