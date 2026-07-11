use std::error::Error;
use std::io::Write;
use std::path::Path;
use std::process::Command;
use std::process::Stdio;

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
fn oversized_control_prompt_is_rejected_before_codex_or_control_state() -> Result<(), Box<dyn Error>>
{
    let temp = tempfile::tempdir()?;
    let data = temp.path().join("data");
    let config = temp.path().join("config");
    let project = temp.path().join("project");
    std::fs::create_dir(&project)?;
    assert!(
        skein(&data, &config)
            .arg("project")
            .arg("add")
            .arg(&project)
            .output()?
            .status
            .success()
    );

    let mut child = skein(&data, &config)
        .env("SKEIN_CODEX_BIN", temp.path().join("must-not-run"))
        .arg("control")
        .arg("codex")
        .arg(&project)
        .arg("--full-access")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(&vec![b'x'; 1024 * 1024 + 1])?;
    let output = child.wait_with_output()?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("prompt exceeds"));

    let runs = skein(&data, &config)
        .args(["control", "list", "--json"])
        .output()?;
    assert!(runs.status.success());
    assert_eq!(serde_json::from_slice::<Value>(&runs.stdout)?, json!([]));
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

#[cfg(unix)]
#[test]
fn runs_an_audited_full_access_codex_turn_without_persisting_content() -> Result<(), Box<dyn Error>>
{
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir()?;
    let data = temp.path().join("data");
    let config = temp.path().join("config");
    let project = temp.path().join("project");
    std::fs::create_dir(&project)?;
    assert!(
        skein(&data, &config)
            .arg("project")
            .arg("add")
            .arg(&project)
            .output()?
            .status
            .success()
    );
    let project = project.canonicalize()?;
    let log = temp.path().join("protocol.log");
    let script = temp.path().join("fake-codex-control");
    let thread_response = json!({
        "id": 3,
        "result": {
            "thread": {
                "id": "synthetic-thread",
                "sessionId": "synthetic-session",
                "cwd": project,
                "createdAt": 10,
                "updatedAt": 10,
                "source": "appServer",
                "status": {"type": "idle"},
                "modelProvider": "synthetic-provider",
                "cliVersion": "1.2.3",
                "ephemeral": false,
                "name": null,
                "preview": "",
                "turns": []
            },
            "model": "synthetic-model",
            "modelProvider": "synthetic-provider",
            "cwd": project,
            "approvalPolicy": "never",
            "approvalsReviewer": "user",
            "sandbox": {"type": "dangerFullAccess"}
        }
    });
    std::fs::write(
        &script,
        format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
read -r init; printf '%s\n' "$init" >> '{}'
printf '%s\n' '{{"id":1,"result":{{"userAgent":"synthetic"}}}}'
read -r initialized; printf '%s\n' "$initialized" >> '{}'
read -r account; printf '%s\n' "$account" >> '{}'
printf '%s\n' '{{"id":2,"result":{{"requiresOpenaiAuth":false,"account":{{"type":"chatgpt","email":null,"planType":"pro"}}}}}}'
read -r thread; printf '%s\n' "$thread" >> '{}'
printf '%s\n' '{}'
read -r turn; printf '%s\n' "$turn" >> '{}'
printf '%s\n' '{{"id":4,"result":{{"turn":{{"id":"synthetic-turn","status":"inProgress","items":[]}}}}}}'
printf '%s\n' '{{"method":"item/agentMessage/delta","params":{{"threadId":"synthetic-thread","turnId":"other-turn","itemId":"other-message","delta":"Unrelated answer"}}}}'
printf '%s\n' '{{"method":"turn/started","params":{{"threadId":"synthetic-thread","turn":{{"id":"synthetic-turn","status":"inProgress","items":[]}}}}}}'
printf '%s\n' '{{"method":"item/agentMessage/delta","params":{{"threadId":"synthetic-thread","turnId":"synthetic-turn","itemId":"message","delta":"Sensitive synthetic answer"}}}}'
printf '%s\n' '{{"method":"turn/completed","params":{{"threadId":"synthetic-thread","turn":{{"id":"synthetic-turn","status":"completed","items":[]}}}}}}'
read -r _ || true
"#,
            log.display(),
            log.display(),
            log.display(),
            log.display(),
            thread_response,
            log.display()
        ),
    )?;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))?;

    let mut child = skein(&data, &config)
        .env("SKEIN_CODEX_BIN", &script)
        .arg("control")
        .arg("codex")
        .arg(&project)
        .args(["--full-access", "--jsonl"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(b"Sensitive synthetic prompt")?;
    let output = child.wait_with_output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout)?;
    assert!(!stdout.contains("Sensitive synthetic answer"));
    assert!(!stdout.contains("other-turn"));
    assert!(stdout.contains("\"contentRedacted\":true"));
    assert!(stdout.lines().any(|line| {
        serde_json::from_str::<Value>(line).is_ok_and(|value| value["type"] == "control_run")
    }));

    let protocol = std::fs::read_to_string(log)?;
    assert!(protocol.contains("\"method\":\"account/read\""));
    assert!(protocol.contains("\"sandbox\":\"danger-full-access\""));
    assert!(protocol.contains("\"approvalPolicy\":\"never\""));
    assert!(protocol.contains("\"type\":\"dangerFullAccess\""));

    let detail = skein(&data, &config)
        .args(["control", "show", "1", "--json"])
        .output()?;
    assert!(detail.status.success());
    let detail: Value = serde_json::from_slice(&detail.stdout)?;
    assert_eq!(detail["state"], "completed");
    assert_eq!(detail["input_bytes"], "Sensitive synthetic prompt".len());
    let serialized = serde_json::to_string(&detail)?;
    assert!(!serialized.contains("Sensitive synthetic prompt"));
    assert!(!serialized.contains("Sensitive synthetic answer"));
    Ok(())
}

#[cfg(unix)]
#[test]
fn rejects_a_server_request_while_waiting_for_a_response() -> Result<(), Box<dyn Error>> {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir()?;
    let data = temp.path().join("data");
    let config = temp.path().join("config");
    let project = temp.path().join("project");
    std::fs::create_dir(&project)?;
    assert!(
        skein(&data, &config)
            .arg("project")
            .arg("add")
            .arg(&project)
            .output()?
            .status
            .success()
    );
    let script = temp.path().join("fake-codex-server-request");
    std::fs::write(
        &script,
        r#"#!/usr/bin/env bash
set -euo pipefail
read -r _
printf '%s\n' '{"id":1,"result":{"userAgent":"synthetic"}}'
read -r _
read -r _
printf '%s\n' '{"id":99,"method":"item/tool/requestUserInput","params":{}}'
read -r _
"#,
    )?;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))?;

    let mut child = skein(&data, &config)
        .env("SKEIN_CODEX_BIN", &script)
        .arg("control")
        .arg("codex")
        .arg(&project)
        .args(["--full-access", "--jsonl"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(b"Synthetic prompt")?;
    let output = child.wait_with_output()?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unsupported interactive input"));
    let runs = skein(&data, &config)
        .args(["control", "list", "--json"])
        .output()?;
    assert!(runs.status.success());
    assert_eq!(serde_json::from_slice::<Value>(&runs.stdout)?, json!([]));
    Ok(())
}

#[cfg(unix)]
#[test]
fn reconnects_to_a_worker_after_the_starting_client_exits() -> Result<(), Box<dyn Error>> {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir()?;
    let data = temp.path().join("data");
    let config = temp.path().join("config");
    let project = temp.path().join("project");
    std::fs::create_dir(&project)?;
    assert!(
        skein(&data, &config)
            .arg("project")
            .arg("add")
            .arg(&project)
            .output()?
            .status
            .success()
    );
    let project = project.canonicalize()?;
    let script = temp.path().join("fake-codex-worker");
    let interrupt_log = temp.path().join("interrupt.json");
    let thread_response = json!({
        "id": 3,
        "result": {
            "thread": {"id": "worker-thread", "sessionId": "worker-session"},
            "model": "synthetic-model",
            "modelProvider": "synthetic-provider",
            "cwd": project,
            "approvalPolicy": "never",
            "sandbox": {"type": "dangerFullAccess"}
        }
    });
    std::fs::write(
        &script,
        format!(
            r#"#!/usr/bin/env bash
set -euo pipefail
read -r _
printf '%s\n' '{{"id":1,"result":{{"userAgent":"synthetic"}}}}'
read -r _
read -r _
printf '%s\n' '{{"id":2,"result":{{"requiresOpenaiAuth":true,"account":{{"type":"chatgpt","email":null,"planType":"pro"}}}}}}'
read -r _ || exit 0
printf '%s\n' '{}'
read -r _
printf '%s\n' '{{"id":4,"result":{{"turn":{{"id":"worker-turn","status":"inProgress","items":[]}}}}}}'
printf '%s\n' '{{"method":"turn/started","params":{{"threadId":"worker-thread","turn":{{"id":"worker-turn","status":"inProgress","items":[]}}}}}}'
for _ in $(seq 1 520); do
  printf '%s\n' '{{"method":"item/agentMessage/delta","params":{{"threadId":"worker-thread","turnId":"worker-turn","itemId":"message","delta":"WORKER_SECRET_OUTPUT"}}}}'
done
read -r interrupt
printf '%s\n' "$interrupt" > '{}'
printf '%s\n' '{{"id":5,"result":{{}}}}'
printf '%s\n' '{{"method":"turn/completed","params":{{"threadId":"worker-thread","turn":{{"id":"worker-turn","status":"interrupted","items":[]}}}}}}'
read -r _ || true
"#,
            thread_response,
            interrupt_log.display()
        ),
    )?;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))?;

    let mut start = skein(&data, &config)
        .env("SKEIN_CODEX_BIN", &script)
        .arg("worker")
        .arg("start")
        .arg(&project)
        .args(["--full-access", "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    start
        .stdin
        .take()
        .expect("child stdin")
        .write_all(b"WORKER_SECRET_PROMPT")?;
    let output = start.wait_with_output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let started: Value = serde_json::from_slice(&output.stdout)?;
    let run_id = started["run"]["id"].as_i64().expect("run id");

    let mut status = Value::Null;
    for _ in 0..40 {
        let output = skein(&data, &config)
            .args(["worker", "status", &run_id.to_string(), "--json"])
            .output()?;
        assert!(output.status.success());
        status = serde_json::from_slice(&output.stdout)?;
        if status["run"]["state"] == "active" {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    assert_eq!(status["run"]["id"], run_id);
    assert_eq!(status["run"]["state"], "active");
    assert!(status["liveEventsAvailable"].as_bool().unwrap_or(false));
    let listed = skein(&data, &config)
        .args(["worker", "list", "--active", "--json"])
        .output()?;
    assert!(listed.status.success());
    let listed: Value = serde_json::from_slice(&listed.stdout)?;
    assert_eq!(listed[0]["run"]["id"], run_id);

    let refused_stop = skein(&data, &config)
        .args(["worker", "stop", &run_id.to_string()])
        .output()?;
    assert!(!refused_stop.status.success());
    assert!(String::from_utf8_lossy(&refused_stop.stderr).contains("active_run"));

    let capability_path = data
        .join("workers")
        .join(format!("run-{run_id}.capability"));
    let capability = std::fs::read_to_string(&capability_path)?;
    assert_eq!(
        std::fs::metadata(&capability_path)?.permissions().mode() & 0o777,
        0o600
    );
    std::fs::write(&capability_path, "00000000-0000-4000-8000-000000000000")?;
    let rejected = skein(&data, &config)
        .args(["worker", "interrupt", &run_id.to_string()])
        .output()?;
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("authentication_failed"));
    std::fs::write(&capability_path, &capability)?;
    std::fs::set_permissions(&capability_path, std::fs::Permissions::from_mode(0o600))?;

    let interrupted = skein(&data, &config)
        .args(["worker", "interrupt", &run_id.to_string()])
        .output()?;
    assert!(
        interrupted.status.success(),
        "{}",
        String::from_utf8_lossy(&interrupted.stderr)
    );

    let watched = skein(&data, &config)
        .args(["worker", "watch", &run_id.to_string(), "--jsonl"])
        .output()?;
    assert!(
        watched.status.success(),
        "{}",
        String::from_utf8_lossy(&watched.stderr)
    );
    let stdout = String::from_utf8(watched.stdout)?;
    assert!(stdout.contains("agent_message_delta"));
    assert!(stdout.contains("\"type\":\"event_gap\""));
    assert!(stdout.contains("\"delta_bytes\":20"));
    assert!(!stdout.contains("WORKER_SECRET_OUTPUT"));
    assert!(stdout.contains("\"state\":\"interrupted\""), "{stdout}");
    let interrupt: Value = serde_json::from_str(&std::fs::read_to_string(&interrupt_log)?)?;
    assert_eq!(interrupt["method"], "turn/interrupt");
    assert_eq!(interrupt["params"]["threadId"], "worker-thread");
    assert_eq!(interrupt["params"]["turnId"], "worker-turn");
    let audit = skein(&data, &config)
        .args(["control", "show", &run_id.to_string(), "--json"])
        .output()?;
    assert!(audit.status.success());
    let audit: Value = serde_json::from_slice(&audit.stdout)?;
    assert!(audit["actions"].as_array().is_some_and(|actions| {
        actions.iter().any(|action| {
            action["action_kind"] == "turn_interrupt" && action["state"] == "succeeded"
        })
    }));
    let repeated = skein(&data, &config)
        .args(["worker", "interrupt", &run_id.to_string()])
        .output()?;
    assert!(repeated.status.success());
    assert_eq!(std::fs::read_to_string(interrupt_log)?.lines().count(), 1);

    let database = std::fs::read(data.join("skein.sqlite3"))?;
    assert!(
        !database
            .windows(20)
            .any(|value| value == b"WORKER_SECRET_PROMPT")
    );
    assert!(
        !database
            .windows(20)
            .any(|value| value == b"WORKER_SECRET_OUTPUT")
    );
    assert!(
        !database
            .windows(capability.len())
            .any(|value| value == capability.as_bytes())
    );

    let stopped = skein(&data, &config)
        .args(["worker", "stop", &run_id.to_string()])
        .output()?;
    assert!(stopped.status.success());
    for _ in 0..20 {
        if !capability_path.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(!capability_path.exists());

    let unknown = skein(&data, &config)
        .args(["worker", "status", "999999", "--json"])
        .output()?;
    assert!(!unknown.status.success());
    Ok(())
}

#[cfg(unix)]
#[test]
fn closed_control_connection_fails_before_state_mutation() -> Result<(), Box<dyn Error>> {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir()?;
    let data = temp.path().join("data");
    let config = temp.path().join("config");
    let script = temp.path().join("fake-codex-eof");
    std::fs::write(
        &script,
        r#"#!/usr/bin/env bash
set -euo pipefail
read -r _
printf '%s\n' '{"id":1,"result":{"userAgent":"synthetic"}}'
read -r _
read -r _
"#,
    )?;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))?;

    let mut child = skein(&data, &config)
        .env("SKEIN_CODEX_BIN", &script)
        .args(["control", "codex", "/synthetic/project", "--full-access"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .expect("child stdin")
        .write_all(b"Synthetic prompt")?;
    let output = child.wait_with_output()?;
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("closed the control connection"));
    assert!(!data.exists());
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
