use std::error::Error;
use std::path::Path;
use std::process::Command;

use serde_json::Value;
use serde_json::json;

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

#[test]
fn codex_sync_failure_does_not_create_or_migrate_state() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let data = temp.path().join("data");
    let output = skein(&data, &temp.path().join("config"))
        .env("SKEIN_CODEX_BIN", temp.path().join("missing-codex"))
        .args(["session", "sync", "codex", "--json"])
        .output()?;

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("app-server I/O failed"));
    assert!(!data.exists());
    Ok(())
}

#[cfg(unix)]
#[test]
fn synchronizes_lists_and_explicitly_rebinds_codex_sessions() -> Result<(), Box<dyn Error>> {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir()?;
    let data = temp.path().join("data");
    let config = temp.path().join("config");
    let project = temp.path().join("project");
    let nested = project.join("nested");
    let alternate = temp.path().join("alternate");
    std::fs::create_dir_all(&nested)?;
    std::fs::create_dir(&alternate)?;

    for path in [&project, &alternate] {
        assert!(
            skein(&data, &config)
                .arg("project")
                .arg("add")
                .arg(path)
                .output()?
                .status
                .success()
        );
    }

    let thread = json!({
        "id": "synthetic-thread",
        "sessionId": "synthetic-tree",
        "cwd": nested,
        "createdAt": 10,
        "updatedAt": 20,
        "source": "cli",
        "status": {"type": "notLoaded"},
        "modelProvider": "synthetic-provider",
        "cliVersion": "1.2.3",
        "parentThreadId": null,
        "forkedFromId": null,
        "ephemeral": false,
        "name": "Synthetic title",
        "preview": "Synthetic preview",
        "turns": []
    });
    let response = json!({
        "id": 2,
        "result": {"data": [thread], "nextCursor": "opaque-next"}
    });
    let script = temp.path().join("fake-codex");
    std::fs::write(
        &script,
        format!(
            "#!/usr/bin/env bash\nset -euo pipefail\nread -r _\nprintf '%s\\n' '{}'\nread -r _\nread -r _\nprintf '%s\\n' '{}'\n",
            r#"{"id":1,"result":{"userAgent":"synthetic"}}"#, response
        ),
    )?;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))?;

    let synced = skein(&data, &config)
        .env("SKEIN_CODEX_BIN", &script)
        .args(["session", "sync", "codex", "--json"])
        .output()?;
    assert!(
        synced.status.success(),
        "{}",
        String::from_utf8_lossy(&synced.stderr)
    );
    let report: Value = serde_json::from_slice(&synced.stdout)?;
    assert_eq!(report["inserted"], 1);
    assert_eq!(report["linkedToProjects"], 1);
    assert_eq!(report["nextCursor"], "opaque-next");
    assert_eq!(report["textImported"], false);

    let listed = skein(&data, &config)
        .args(["session", "list", "--json"])
        .output()?;
    assert!(listed.status.success());
    let sessions: Value = serde_json::from_slice(&listed.stdout)?;
    assert_eq!(sessions[0]["source_thread_id"], "synthetic-thread");
    assert_eq!(sessions[0]["project_link_kind"], "automatic");
    assert!(sessions[0]["name"].is_null());
    assert!(sessions[0]["preview"].is_null());

    let rebound = skein(&data, &config)
        .arg("session")
        .arg("bind")
        .arg("synthetic-thread")
        .arg(&alternate)
        .arg("--json")
        .output()?;
    assert!(rebound.status.success());
    let rebound: Value = serde_json::from_slice(&rebound.stdout)?;
    assert_eq!(rebound["project_link_kind"], "manual");
    let alternate = alternate.canonicalize()?;
    assert_eq!(rebound["project_path"].as_str(), alternate.to_str());

    let with_text = skein(&data, &config)
        .env("SKEIN_CODEX_BIN", &script)
        .args(["session", "sync", "codex", "--include-text", "--json"])
        .output()?;
    assert!(with_text.status.success());
    let listed = skein(&data, &config)
        .args(["session", "list", "--json"])
        .output()?;
    let listed: Value = serde_json::from_slice(&listed.stdout)?;
    assert!(listed[0]["name"].is_null());
    assert!(listed[0]["preview"].is_null());
    assert_eq!(listed[0]["text_redacted"], true);

    let shown = skein(&data, &config)
        .args(["session", "show", "synthetic-thread", "--json"])
        .output()?;
    let shown: Value = serde_json::from_slice(&shown.stdout)?;
    assert_eq!(shown["project_link_kind"], "manual");
    assert!(shown["name"].is_null());
    assert!(shown["preview"].is_null());
    assert_eq!(shown["text_redacted"], true);

    let shown = skein(&data, &config)
        .args([
            "session",
            "show",
            "synthetic-thread",
            "--include-text",
            "--json",
        ])
        .output()?;
    let shown: Value = serde_json::from_slice(&shown.stdout)?;
    assert_eq!(shown["name"], "Synthetic title");
    assert_eq!(shown["preview"], "Synthetic preview");
    assert_eq!(shown["text_redacted"], false);

    let unbound = skein(&data, &config)
        .args(["session", "unbind", "synthetic-thread", "--json"])
        .output()?;
    assert!(unbound.status.success());
    let unbound: Value = serde_json::from_slice(&unbound.stdout)?;
    assert_eq!(unbound["project_link_kind"], "manual_unbound");
    assert!(unbound["project_id"].is_null());
    assert!(unbound["name"].is_null());
    assert!(unbound["preview"].is_null());
    assert_eq!(unbound["text_redacted"], true);
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
