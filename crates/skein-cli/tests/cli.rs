use std::error::Error;
use std::io::BufRead;
use std::io::BufReader;
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
fn commands_are_human_readable_by_default_and_support_global_json_format()
-> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let data = temp.path().join("data");
    let config = temp.path().join("config");

    let human = skein(&data, &config).arg("doctor").output()?;
    assert!(human.status.success());
    let human = String::from_utf8(human.stdout)?;
    assert!(human.contains("database exists: no"));
    assert!(!human.trim_start().starts_with('{'));

    let json_output = skein(&data, &config)
        .args(["doctor", "--format", "json"])
        .output()?;
    assert!(json_output.status.success());
    let value: Value = serde_json::from_slice(&json_output.stdout)?;
    assert_eq!(value["database_exists"], false);
    Ok(())
}

#[test]
fn freshness_is_structured_and_non_tty_progress_stays_quiet() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let data = temp.path().join("data");
    let config = temp.path().join("config");
    let project = temp.path().join("project");
    std::fs::create_dir(&project)?;
    std::fs::write(project.join("README.md"), "# Synthetic freshness\n")?;

    assert!(skein(&data, &config).arg("init").output()?.status.success());
    assert!(
        skein(&data, &config)
            .arg("project")
            .arg("add")
            .arg(&project)
            .output()?
            .status
            .success()
    );
    let before = skein(&data, &config)
        .args(["freshness", "--format", "json"])
        .output()?;
    let before: Value = serde_json::from_slice(&before.stdout)?;
    assert_eq!(before["state"], "missing");
    assert_eq!(before["mayBeStale"], true);

    for format in ["human", "json"] {
        let output = skein(&data, &config)
            .arg("index")
            .arg("--project")
            .arg(&project)
            .args(["--format", format])
            .output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(output.stderr.is_empty(), "non-TTY progress must stay quiet");
        if format == "json" {
            let report: Value = serde_json::from_slice(&output.stdout)?;
            assert!(report["startedAt"].is_i64());
            assert!(report["completedAt"].is_i64());
            assert_eq!(
                report["mayBeStale"], true,
                "scoped global sources are deferred"
            );
        }
    }
    let after = skein(&data, &config)
        .args(["freshness", "--stale-after-hours", "24", "--format", "json"])
        .output()?;
    let after: Value = serde_json::from_slice(&after.stdout)?;
    assert_eq!(after["git"]["state"], "fresh");
    assert_eq!(after["documents"]["state"], "fresh");
    Ok(())
}

#[test]
fn deep_recall_requires_explicit_cli_opt_in() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let data = temp.path().join("data");
    let config = temp.path().join("config");
    let codex_home = temp.path().join("codex-home");
    std::fs::create_dir_all(codex_home.join("memories"))?;

    assert!(skein(&data, &config).arg("init").output()?.status.success());
    let initial = skein(&data, &config)
        .args(["context", "status", "--format", "json"])
        .output()?;
    let initial: Value = serde_json::from_slice(&initial.stdout)?;
    assert_eq!(initial["includeCodexMemories"], false);
    assert_eq!(initial["includeCodexSessions"], false);

    let enabled = skein(&data, &config)
        .args(["context", "memories", "enable", "--format", "json"])
        .output()?;
    assert!(enabled.status.success());
    let enabled: Value = serde_json::from_slice(&enabled.stdout)?;
    assert_eq!(enabled["includeCodexMemories"], true);

    std::fs::write(
        codex_home.join("memories/summary.md"),
        "# Synthetic memory\n\nDeep recall marker.\n",
    )?;
    let refreshed = skein(&data, &config)
        .args(["context", "refresh", "--codex-home"])
        .arg(&codex_home)
        .args(["--format", "json"])
        .output()?;
    assert!(
        refreshed.status.success(),
        "{}",
        String::from_utf8_lossy(&refreshed.stderr)
    );
    let refreshed: Value = serde_json::from_slice(&refreshed.stdout)?;
    assert_eq!(refreshed["memories"]["documents"], 1);

    let searched = skein(&data, &config)
        .args(["context", "search", "recall", "marker", "--format", "json"])
        .output()?;
    let searched: Value = serde_json::from_slice(&searched.stdout)?;
    assert_eq!(searched[0]["sourceKind"], "codex_memory");
    assert!(
        searched[0]["snippet"]
            .as_str()
            .is_some_and(|snippet| snippet.contains("[recall]"))
    );
    Ok(())
}

#[test]
fn recursively_indexes_an_approved_root_and_removes_it_after_disconnect()
-> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let data = temp.path().join("data");
    let config = temp.path().join("config");
    let root = temp.path().join("slow-workspace");
    let repository = root.join("group").join("nested-repository");
    std::fs::create_dir_all(&repository)?;
    assert!(
        std::process::Command::new("git")
            .arg("init")
            .arg(&repository)
            .output()?
            .status
            .success()
    );
    std::fs::write(
        repository.join("README.md"),
        "# Nested Aurora\n\nRecursive identity marker for recall.\n",
    )?;
    assert!(
        std::process::Command::new("git")
            .arg("-C")
            .arg(&repository)
            .args(["add", "README.md"])
            .output()?
            .status
            .success()
    );

    let added = skein(&data, &config)
        .args(["scan-root", "add"])
        .arg(&root)
        .args(["--recursive", "--max-depth", "4", "--format", "json"])
        .output()?;
    assert!(
        added.status.success(),
        "{}",
        String::from_utf8_lossy(&added.stderr)
    );
    let added: Value = serde_json::from_slice(&added.stdout)?;
    assert_eq!(added["root"]["recursive"], true);
    assert_eq!(added["root"]["max_depth"], 4);
    assert_eq!(added["discovery"]["newly_registered"], 1);
    let canonical_repository = std::fs::canonicalize(&repository)?;
    assert_eq!(
        added["discovery"]["discovered"][0]["path"],
        canonical_repository.to_string_lossy().as_ref()
    );

    let indexed = skein(&data, &config)
        .args(["index", "--format", "json"])
        .output()?;
    assert!(indexed.status.success());
    let indexed: Value = serde_json::from_slice(&indexed.stdout)?;
    assert_eq!(indexed["discovery"][0]["already_registered"], 1);
    assert!(
        indexed["documents"][0]["indexedBytes"]
            .as_u64()
            .unwrap_or(0)
            > 0
    );
    assert!(indexed["sessions"]["ok"].is_boolean());

    let searched = skein(&data, &config)
        .args(["search", "aurora", "identity", "--format", "json"])
        .output()?;
    assert!(searched.status.success());
    let searched: Value = serde_json::from_slice(&searched.stdout)?;
    assert_eq!(searched["documents"][0]["title"], "Nested Aurora");
    assert!(
        searched["documents"][0]["snippet"]
            .as_str()
            .is_some_and(|snippet| snippet.contains("[identity]"))
    );

    std::fs::rename(&root, temp.path().join("disconnected-workspace"))?;
    let offline_index = skein(&data, &config)
        .args(["index", "--format", "json"])
        .output()?;
    assert!(
        offline_index.status.success(),
        "{}",
        String::from_utf8_lossy(&offline_index.stderr)
    );
    let offline_index: Value = serde_json::from_slice(&offline_index.stdout)?;
    assert_eq!(offline_index["refreshed"][0]["ok"], false);
    assert_eq!(offline_index["documents"][0]["ok"], false);
    let removed = skein(&data, &config)
        .args(["scan-root", "remove"])
        .arg(&root)
        .args(["--format", "json"])
        .output()?;
    assert!(
        removed.status.success(),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );
    let removed: Value = serde_json::from_slice(&removed.stdout)?;
    assert_eq!(removed["path"], added["root"]["path"]);
    Ok(())
}

#[test]
fn scoped_index_refreshes_only_the_selected_project_and_rejects_mixed_scope()
-> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let data = temp.path().join("data");
    let config = temp.path().join("config");
    let first = temp.path().join("first-project");
    let sibling = temp.path().join("sibling-project");
    for (path, marker) in [(&first, "Selected Comet"), (&sibling, "Sibling Nebula")] {
        std::fs::create_dir_all(path)?;
        assert!(
            Command::new("git")
                .arg("init")
                .arg(path)
                .output()?
                .status
                .success()
        );
        std::fs::write(path.join("README.md"), format!("# {marker}\n"))?;
        assert!(
            skein(&data, &config)
                .args(["project", "add"])
                .arg(path)
                .output()?
                .status
                .success()
        );
    }

    let output = skein(&data, &config)
        .args(["index", "--project"])
        .arg(&first)
        .args(["--format", "json"])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let report: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(report["scope"]["kind"], "project");
    assert_eq!(report["discovery"].as_array().map(Vec::len), Some(0));
    assert_eq!(report["refreshed"].as_array().map(Vec::len), Some(1));
    assert_eq!(report["documents"].as_array().map(Vec::len), Some(1));
    assert_eq!(report["context"]["status"], "deferred");
    let sibling_search = skein(&data, &config)
        .args(["search", "nebula", "--format", "json"])
        .output()?;
    let sibling_search: Value = serde_json::from_slice(&sibling_search.stdout)?;
    assert!(
        sibling_search["documents"]
            .as_array()
            .is_some_and(Vec::is_empty)
    );

    let mixed = skein(&data, &config)
        .args(["index", "--project"])
        .arg(&first)
        .arg("--scan-root")
        .arg(temp.path())
        .output()?;
    assert!(!mixed.status.success());
    Ok(())
}

#[test]
fn scan_root_scope_isolated_offline_and_unregistered_roots() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let data = temp.path().join("data");
    let config = temp.path().join("config");
    let selected_root = temp.path().join("selected-root");
    let other_root = temp.path().join("other-root");
    let selected = selected_root.join("selected");
    let other = other_root.join("other");
    for (root, repository, title) in [
        (&selected_root, &selected, "Selected Root"),
        (&other_root, &other, "Other Root"),
    ] {
        std::fs::create_dir_all(repository)?;
        assert!(
            Command::new("git")
                .arg("init")
                .arg(repository)
                .output()?
                .status
                .success()
        );
        std::fs::write(repository.join("README.md"), format!("# {title}\n"))?;
        assert!(
            skein(&data, &config)
                .args(["scan-root", "add"])
                .arg(root)
                .args(["--recursive"])
                .output()?
                .status
                .success()
        );
    }

    let scoped = skein(&data, &config)
        .args(["index", "--scan-root"])
        .arg(&selected_root)
        .args(["--format", "json"])
        .output()?;
    assert!(scoped.status.success());
    let scoped: Value = serde_json::from_slice(&scoped.stdout)?;
    assert_eq!(scoped["discovery"].as_array().map(Vec::len), Some(1));
    assert_eq!(scoped["documents"].as_array().map(Vec::len), Some(1));
    assert_eq!(
        scoped["documents"][0]["projectPath"],
        std::fs::canonicalize(&selected)?.to_string_lossy().as_ref()
    );

    let unregistered = temp.path().join("unregistered");
    std::fs::create_dir_all(unregistered.join("hidden/.git"))?;
    let rejected = skein(&data, &config)
        .args(["index", "--scan-root"])
        .arg(&unregistered)
        .output()?;
    assert!(!rejected.status.success());

    let offline_path = selected_root.clone();
    std::fs::rename(&selected_root, temp.path().join("offline-root"))?;
    let offline = skein(&data, &config)
        .args(["index", "--scan-root"])
        .arg(&offline_path)
        .args(["--format", "json"])
        .output()?;
    assert!(
        offline.status.success(),
        "{}",
        String::from_utf8_lossy(&offline.stderr)
    );
    let offline: Value = serde_json::from_slice(&offline.stdout)?;
    assert_eq!(offline["discovery"][0]["unreachable"], true);
    assert_eq!(offline["deferred"][0]["cachedProjectsRetained"], true);
    assert_eq!(offline["documents"].as_array().map(Vec::len), Some(1));
    Ok(())
}

#[test]
fn tui_rejects_non_interactive_stdio_without_creating_state() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let data = temp.path().join("data");
    let config = temp.path().join("config");
    let output = skein(&data, &config).arg("tui").output()?;

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("interactive terminal"));
    assert!(!data.exists());
    assert!(!config.exists());
    Ok(())
}

#[test]
fn mcp_stdio_lists_legacy_tools_and_calls_read_and_write_paths() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let data = temp.path().join("data");
    let config = temp.path().join("config");
    let first = temp.path().join("first-project");
    let second = temp.path().join("second-project");
    std::fs::create_dir(&first)?;
    std::fs::create_dir(&second)?;
    assert!(
        std::process::Command::new("git")
            .arg("init")
            .arg(&second)
            .output()?
            .status
            .success()
    );
    assert!(
        skein(&data, &config)
            .arg("project")
            .arg("add")
            .arg(&first)
            .args(["--name", "Synthetic Recall", "--json"])
            .output()?
            .status
            .success()
    );

    let mut child = skein(&data, &config)
        .arg("mcp")
        .arg("--allow-control")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("MCP stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("MCP stdout"));

    let initialized = mcp_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "synthetic-test", "version": "1.0"}
            }
        }),
    )?;
    assert_eq!(initialized["id"], 1);
    assert!(
        initialized["result"]["instructions"]
            .as_str()
            .is_some_and(|value| value.contains("search_projects"))
    );
    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
    )?;
    stdin.flush()?;

    let listed = mcp_request(
        &mut stdin,
        &mut stdout,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list", "params": {}}),
    )?;
    let names = listed["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect::<Vec<_>>();
    let refresh_schema = listed["result"]["tools"]
        .as_array()
        .and_then(|tools| tools.iter().find(|tool| tool["name"] == "refresh_index"))
        .expect("refresh_index schema");
    assert_eq!(
        refresh_schema["inputSchema"]["properties"]["project"]["type"],
        "string"
    );
    assert_eq!(
        refresh_schema["inputSchema"]["properties"]["scan_root"]["type"],
        "string"
    );
    for name in [
        "search_projects",
        "search_sessions",
        "get_project",
        "suggest_codex_command",
        "add_scan_root",
        "conduct",
        "steer_run",
        "interrupt_run",
        "reconcile_run",
    ] {
        assert!(names.contains(&name));
    }

    let searched = mcp_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "search_projects",
                "arguments": {"query": "Synthetic Recall", "limit": 5}
            }
        }),
    )?;
    assert_eq!(searched["result"]["isError"], false);
    assert_eq!(
        searched["result"]["structuredContent"]["report"]["candidates"][0]["project"]["name"],
        "Synthetic Recall"
    );

    let added = mcp_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "add_scan_root",
                "arguments": {"path": second, "recursive": false}
            }
        }),
    )?;
    assert_eq!(added["result"]["isError"], false);
    assert_eq!(
        added["result"]["structuredContent"]["root"]["recursive"],
        false
    );

    let enabled_private_index = mcp_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "set_codex_session_indexing",
                "arguments": {"enabled": true}
            }
        }),
    )?;
    assert_eq!(enabled_private_index["result"]["isError"], false);
    assert_eq!(
        enabled_private_index["result"]["structuredContent"]["settings"]["includeCodexSessions"],
        true
    );

    let scoped_refresh = mcp_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 51,
            "method": "tools/call",
            "params": {
                "name": "refresh_index",
                "arguments": {"project": first}
            }
        }),
    )?;
    assert_eq!(scoped_refresh["result"]["isError"], false);
    assert_eq!(
        scoped_refresh["result"]["structuredContent"]["scope"]["kind"],
        "project"
    );
    assert_eq!(
        scoped_refresh["result"]["structuredContent"]["reports"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    assert_eq!(
        scoped_refresh["result"]["structuredContent"]["sessions"]["status"],
        "deferred"
    );

    let mixed_refresh = mcp_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 52,
            "method": "tools/call",
            "params": {
                "name": "refresh_index",
                "arguments": {"project": first, "scan_root": second}
            }
        }),
    )?;
    assert_eq!(mixed_refresh["result"]["isError"], true);

    let removed = mcp_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 6,
            "method": "tools/call",
            "params": {
                "name": "remove_scan_root",
                "arguments": {"path": second}
            }
        }),
    )?;
    assert_eq!(removed["result"]["isError"], false);
    assert_eq!(removed["result"]["structuredContent"]["removed"], true);

    drop(stdin);
    drop(stdout);
    assert!(child.wait()?.success());

    let roots = skein(&data, &config)
        .args(["scan-root", "list", "--json"])
        .output()?;
    let roots: Value = serde_json::from_slice(&roots.stdout)?;
    assert_eq!(roots.as_array().map(Vec::len), Some(0));
    let projects = skein(&data, &config)
        .args(["project", "list", "--json"])
        .output()?;
    let projects: Value = serde_json::from_slice(&projects.stdout)?;
    assert_eq!(projects.as_array().map(Vec::len), Some(2));
    Ok(())
}

#[test]
fn mcp_first_search_initializes_state_and_returns_complete_setup_guidance()
-> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let data = temp.path().join("data");
    let config = temp.path().join("config");
    let mut child = skein(&data, &config)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("MCP stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("MCP stdout"));
    mcp_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "first-use-test", "version": "1.0"}
            }
        }),
    )?;
    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
    )?;
    stdin.flush()?;

    let searched = mcp_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "search_projects",
                "arguments": {"query": "anything"}
            }
        }),
    )?;
    let result = &searched["result"]["structuredContent"];
    assert_eq!(result["setupRequired"], true);
    assert_eq!(result["setup_required"], true);
    assert!(
        result["setupHint"]
            .as_str()
            .is_some_and(|hint| hint.contains("add_scan_root") && hint.contains("refresh_index"))
    );
    assert!(data.join("skein.sqlite3").is_file());
    drop(stdin);
    assert!(child.wait()?.success());
    Ok(())
}

#[cfg(unix)]
#[test]
fn mcp_stdio_runs_session_sync_through_a_fake_codex_child() -> Result<(), Box<dyn Error>> {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir()?;
    let data = temp.path().join("data");
    let config = temp.path().join("config");
    let script = temp.path().join("fake-codex-mcp-sync");
    std::fs::write(
        &script,
        r#"#!/bin/sh
set -eu
read -r _
printf '%s\n' '{"id":1,"result":{"userAgent":"synthetic"}}'
read -r _
read -r _
printf '%s\n' '{"id":2,"result":{"data":[],"nextCursor":null}}'
"#,
    )?;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))?;

    let mut child = skein(&data, &config)
        .env("SKEIN_CODEX_BIN", &script)
        .arg("mcp")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("MCP stdin");
    let mut stdout = BufReader::new(child.stdout.take().expect("MCP stdout"));
    let initialized = mcp_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "synthetic-test", "version": "1.0"}
            }
        }),
    )?;
    assert_eq!(initialized["id"], 1);
    writeln!(
        stdin,
        "{}",
        json!({"jsonrpc": "2.0", "method": "notifications/initialized"})
    )?;
    stdin.flush()?;

    let synced = mcp_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "sync_codex_sessions",
                "arguments": {"limit": 1}
            }
        }),
    )?;
    assert_eq!(synced["result"]["isError"], false);
    assert_eq!(synced["result"]["structuredContent"]["observed"], 0);

    let refreshed = mcp_request(
        &mut stdin,
        &mut stdout,
        json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "refresh_activity",
                "arguments": {"since_days": 7, "session_limit": 1}
            }
        }),
    )?;
    assert_eq!(refreshed["result"]["isError"], false);
    assert_eq!(
        refreshed["result"]["structuredContent"]["sessions"]["sinceDays"],
        7
    );
    assert_eq!(
        refreshed["result"]["structuredContent"]["coverage"],
        "bounded_app_server_metadata_updated_within_requested_window"
    );

    drop(stdin);
    drop(stdout);
    assert!(child.wait()?.success());
    Ok(())
}

fn mcp_request(
    stdin: &mut impl Write,
    stdout: &mut impl BufRead,
    request: Value,
) -> Result<Value, Box<dyn Error>> {
    writeln!(stdin, "{request}")?;
    stdin.flush()?;
    let mut line = String::new();
    stdout.read_line(&mut line)?;
    if line.is_empty() {
        return Err("MCP server closed stdout before responding".into());
    }
    Ok(serde_json::from_str(&line)?)
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
fn session_search_is_read_only_and_reports_disabled_private_gate() -> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let data = temp.path().join("data");
    let config = temp.path().join("config");
    assert!(skein(&data, &config).arg("init").output()?.status.success());
    let database = data.join("skein.sqlite3");
    let before = std::fs::metadata(&database)?.modified()?;
    let output = skein(&data, &config)
        .args(["session", "search", "deploy", "aura.ai.pro.br", "--json"])
        .output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let value: Value = serde_json::from_slice(&output.stdout)?;
    assert_eq!(value["results"], json!([]));
    assert_eq!(value["returned"], 0);
    assert_eq!(value["privateSources"]["includeCodexSessions"], false);
    assert_eq!(std::fs::metadata(&database)?.modified()?, before);
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
    let steer_log = temp.path().join("steer.json");
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
read -r steer
printf '%s\n' "$steer" > '{}'
printf '%s\n' '{{"id":5,"result":{{"turnId":"worker-turn"}}}}'
read -r interrupt
printf '%s\n' "$interrupt" > '{}'
printf '%s\n' '{{"id":6,"result":{{}}}}'
printf '%s\n' '{{"method":"turn/completed","params":{{"threadId":"worker-thread","turn":{{"id":"worker-turn","status":"interrupted","items":[]}}}}}}'
read -r _ || true
"#,
            thread_response,
            steer_log.display(),
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

    let request_id = "00000000-0000-4000-8000-000000000456";
    let mut steering = skein(&data, &config)
        .args([
            "worker",
            "steer",
            &run_id.to_string(),
            "--request-id",
            request_id,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    steering
        .stdin
        .take()
        .expect("steer stdin")
        .write_all(b"WORKER_SECRET_STEER")?;
    let steered = steering.wait_with_output()?;
    assert!(
        steered.status.success(),
        "{}",
        String::from_utf8_lossy(&steered.stderr)
    );
    let mut retry = skein(&data, &config)
        .args([
            "worker",
            "steer",
            &run_id.to_string(),
            "--request-id",
            request_id,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    retry
        .stdin
        .take()
        .expect("retry stdin")
        .write_all(b"WORKER_SECRET_STEER")?;
    let retry = retry.wait_with_output()?;
    assert!(retry.status.success());
    assert!(String::from_utf8_lossy(&retry.stdout).contains("reused action"));
    for _ in 0..200 {
        if steer_log.exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    let steer_text = std::fs::read_to_string(&steer_log)
        .map_err(|error| format!("failed to read {}: {error}", steer_log.display()))?;
    let steer: Value = serde_json::from_str(&steer_text)?;
    assert_eq!(steer["method"], "turn/steer");
    assert_eq!(steer["params"]["threadId"], "worker-thread");
    assert_eq!(steer["params"]["expectedTurnId"], "worker-turn");
    assert_eq!(steer["params"]["clientUserMessageId"], request_id);
    assert_eq!(steer_text.lines().count(), 1);

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
    assert!(audit["actions"].as_array().is_some_and(|actions| {
        actions.iter().any(|action| {
            action["action_kind"] == "turn_steer"
                && action["state"] == "succeeded"
                && action["client_request_id"] == request_id
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
            .windows(19)
            .any(|value| value == b"WORKER_SECRET_STEER")
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
fn conducts_one_private_prompt_with_atomic_route_and_idempotent_status_lookup()
-> Result<(), Box<dyn Error>> {
    use std::os::unix::fs::PermissionsExt;

    let temp = tempfile::tempdir()?;
    let data = temp.path().join("data");
    let config = temp.path().join("config");
    let project = temp.path().join("alpha-renderer");
    std::fs::create_dir(&project)?;
    assert!(
        skein(&data, &config)
            .arg("project")
            .arg("add")
            .arg(&project)
            .args(["--name", "Alpha Renderer"])
            .output()?
            .status
            .success()
    );
    let project = project.canonicalize()?;
    let protocol_log = temp.path().join("conductor-protocol.jsonl");
    let script = temp.path().join("fake-codex-conductor");
    let thread_response = json!({
        "id": 3,
        "result": {
            "thread": {"id": "conductor-thread", "sessionId": "conductor-session"},
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
read -r initialize
printf '%s\n' "$initialize" >> '{}'
printf '%s\n' '{{"id":1,"result":{{"userAgent":"synthetic"}}}}'
read -r initialized
printf '%s\n' "$initialized" >> '{}'
read -r account
printf '%s\n' "$account" >> '{}'
printf '%s\n' '{{"id":2,"result":{{"requiresOpenaiAuth":true,"account":{{"type":"chatgpt","email":null,"planType":"pro"}}}}}}'
read -r thread || exit 0
printf '%s\n' "$thread" >> '{}'
printf '%s\n' '{}'
read -r turn
printf '%s\n' "$turn" >> '{}'
printf '%s\n' '{{"id":4,"result":{{"turn":{{"id":"conductor-turn","status":"inProgress","items":[]}}}}}}'
printf '%s\n' '{{"method":"turn/started","params":{{"threadId":"conductor-thread","turn":{{"id":"conductor-turn","status":"inProgress","items":[]}}}}}}'
printf '%s\n' '{{"method":"turn/completed","params":{{"threadId":"conductor-thread","turn":{{"id":"conductor-turn","status":"completed","items":[]}}}}}}'
read -r _ || true
"#,
            protocol_log.display(),
            protocol_log.display(),
            protocol_log.display(),
            protocol_log.display(),
            thread_response,
            protocol_log.display(),
        ),
    )?;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))?;

    let request_id = "30000000-0000-4000-8000-000000000001";
    let prompt = "continue Alpha Renderer CONDUCTOR_PRIVATE_SENTINEL";
    let mut child = skein(&data, &config)
        .env("SKEIN_CODEX_BIN", &script)
        .args([
            "conduct",
            "--full-access",
            "--request-id",
            request_id,
            "--jsonl",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .expect("conductor stdin")
        .write_all(prompt.as_bytes())?;
    let output = child.wait_with_output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout)?;
    assert!(!stdout.contains("CONDUCTOR_PRIVATE_SENTINEL"));
    let records = stdout
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(records[0]["type"], "route");
    assert_eq!(records[0]["decision"]["action"], "start");
    assert_eq!(records[0]["decision"]["confidence"], "high");
    assert!(
        records.iter().any(|record| {
            record["type"] == "worker_run" && record["run"]["state"] == "completed"
        })
    );

    let protocol = std::fs::read_to_string(&protocol_log)?;
    let requests = protocol
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(
        requests
            .iter()
            .filter(|request| request["method"] == "account/read")
            .count(),
        2
    );
    let turn = requests
        .iter()
        .find(|request| request["method"] == "turn/start")
        .expect("turn/start request");
    assert_eq!(turn["params"]["input"][0]["text"], prompt);
    assert_eq!(turn["params"]["sandboxPolicy"]["type"], "dangerFullAccess");
    assert_eq!(turn["params"]["approvalPolicy"], "never");

    let mut resumed = skein(&data, &config)
        .env("SKEIN_CODEX_BIN", &script)
        .args([
            "conduct",
            "--full-access",
            "--request-id",
            "30000000-0000-4000-8000-000000000002",
            "--jsonl",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    resumed
        .stdin
        .take()
        .expect("owned-thread resume stdin")
        .write_all(b"conductor-thread")?;
    let resumed = resumed.wait_with_output()?;
    assert!(
        resumed.status.success(),
        "{}",
        String::from_utf8_lossy(&resumed.stderr)
    );
    let resumed_records = String::from_utf8(resumed.stdout)?
        .lines()
        .map(serde_json::from_str::<Value>)
        .collect::<Result<Vec<_>, _>>()?;
    assert_eq!(resumed_records[0]["decision"]["action"], "resume");
    assert_eq!(
        resumed_records[0]["decision"]["sourceThreadId"],
        "conductor-thread"
    );
    let updated_protocol = std::fs::read_to_string(&protocol_log)?;
    assert!(updated_protocol.lines().any(|line| {
        serde_json::from_str::<Value>(line)
            .is_ok_and(|request| request["method"] == "thread/resume")
    }));

    let mut retry = skein(&data, &config)
        .env("SKEIN_CODEX_BIN", temp.path().join("must-not-run"))
        .args([
            "conduct",
            "--full-access",
            "--request-id",
            request_id,
            "--json",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    retry
        .stdin
        .take()
        .expect("retry stdin")
        .write_all(b"DIFFERENT_PRIVATE_RETRY")?;
    let retry = retry.wait_with_output()?;
    assert!(retry.status.success());
    let retry_text = String::from_utf8(retry.stdout)?;
    assert!(!retry_text.contains("DIFFERENT_PRIVATE_RETRY"));
    let retry: Value = serde_json::from_str(&retry_text)?;
    assert_eq!(retry["reused"], true);
    assert_eq!(retry["dispatched"], false);

    let database = std::fs::read(data.join("skein.sqlite3"))?;
    for sentinel in [prompt.as_bytes(), b"DIFFERENT_PRIVATE_RETRY".as_slice()] {
        assert!(
            !database
                .windows(sentinel.len())
                .any(|window| window == sentinel)
        );
    }
    Ok(())
}

#[cfg(unix)]
#[test]
fn conductor_refuses_ambiguous_and_unacknowledged_routes_before_codex_or_control_state()
-> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let data = temp.path().join("data");
    let config = temp.path().join("config");
    for name in ["one", "two"] {
        let project = temp.path().join(name);
        std::fs::create_dir(&project)?;
        assert!(
            skein(&data, &config)
                .arg("project")
                .arg("add")
                .arg(&project)
                .args(["--name", "Shared Project"])
                .output()?
                .status
                .success()
        );
    }

    let mut missing_ack = skein(&data, &config)
        .env("SKEIN_CODEX_BIN", temp.path().join("must-not-run"))
        .arg("conduct")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    missing_ack
        .stdin
        .take()
        .expect("missing ack stdin")
        .write_all(b"Shared Project")?;
    let missing_ack = missing_ack.wait_with_output()?;
    assert!(!missing_ack.status.success());
    assert!(String::from_utf8_lossy(&missing_ack.stderr).contains("--full-access"));

    let mut ambiguous = skein(&data, &config)
        .env("SKEIN_CODEX_BIN", temp.path().join("must-not-run"))
        .args(["conduct", "--full-access", "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    ambiguous
        .stdin
        .take()
        .expect("ambiguous stdin")
        .write_all(b"Shared Project")?;
    let ambiguous = ambiguous.wait_with_output()?;
    assert!(!ambiguous.status.success());
    let report: Value = serde_json::from_slice(&ambiguous.stdout)?;
    assert_eq!(report["recommendation"]["confidence"], "low");
    assert_eq!(report["recommendation"]["ambiguous"], true);
    assert_eq!(report["schemaVersion"], 2);
    assert_eq!(report["resolution"]["required"], true);
    assert_eq!(
        report["resolution"]["reason"],
        "multiple_plausible_candidates"
    );
    assert_eq!(report["candidates"].as_array().map(Vec::len), Some(2));
    for (index, candidate) in report["candidates"]
        .as_array()
        .expect("ranked candidates")
        .iter()
        .enumerate()
    {
        assert_eq!(candidate["rank"], index + 1);
        assert!(candidate["selection"]["projectId"].is_i64());
        assert!(candidate["evidence"].is_array());
    }

    let paths = skein_core::SkeinPaths::new(config, data);
    let registry = skein_core::Registry::open_read_only(&paths)?;
    assert!(registry.list_control_runs()?.is_empty());
    assert!(
        registry
            .conductor_decision_by_request_id("30000000-0000-4000-8000-000000000099")?
            .is_none()
    );
    Ok(())
}

#[cfg(unix)]
#[test]
fn conductor_launch_failure_returns_durable_auto_request_status() -> Result<(), Box<dyn Error>> {
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
            .args(["--name", "Synthetic Project"])
            .output()?
            .status
            .success()
    );
    let script = temp.path().join("fake-codex-auth-only");
    std::fs::write(
        &script,
        r#"#!/usr/bin/env bash
set -euo pipefail
read -r _
printf '%s\n' '{"id":1,"result":{"userAgent":"synthetic"}}'
read -r _
read -r _
printf '%s\n' '{"id":2,"result":{"requiresOpenaiAuth":true,"account":{"type":"chatgpt","email":null,"planType":"pro"}}}'
read -r _ || true
"#,
    )?;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))?;

    let mut child = skein(&data, &config)
        .env("SKEIN_CODEX_BIN", &script)
        .env("SKEIN_TEST_FAIL_WORKER_LAUNCH", "1")
        .args(["conduct", "--full-access", "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child
        .stdin
        .take()
        .expect("launch failure stdin")
        .write_all(b"continue Synthetic Project launch-private")?;
    let failed = child.wait_with_output()?;
    assert!(!failed.status.success());
    let status: Value = serde_json::from_slice(&failed.stdout)?;
    let request_id = status["requestId"].as_str().expect("auto request id");
    assert!(uuid::Uuid::parse_str(request_id).is_ok());
    assert_eq!(status["type"], "worker_launch_failed");
    assert_eq!(status["run"]["state"], "failed");
    assert_eq!(status["worker"]["state"], "exited");
    assert_eq!(status["promptReplayAllowed"], false);
    assert!(!String::from_utf8_lossy(&failed.stdout).contains("launch-private"));

    let mut retry = skein(&data, &config)
        .env("SKEIN_CODEX_BIN", temp.path().join("must-not-run"))
        .args([
            "conduct",
            "--full-access",
            "--request-id",
            request_id,
            "--json",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    retry
        .stdin
        .take()
        .expect("failure retry stdin")
        .write_all(b"must not replay")?;
    let retry = retry.wait_with_output()?;
    assert!(retry.status.success());
    let retry: Value = serde_json::from_slice(&retry.stdout)?;
    assert_eq!(retry["reused"], true);
    assert_eq!(retry["run"]["state"], "failed");
    Ok(())
}

#[test]
fn conductor_retry_recovers_an_expired_preallocated_worker_without_codex()
-> Result<(), Box<dyn Error>> {
    let temp = tempfile::tempdir()?;
    let data = temp.path().join("data");
    let config = temp.path().join("config");
    let project = temp.path().join("project");
    std::fs::create_dir(&project)?;
    let paths = skein_core::SkeinPaths::new(config.clone(), data.clone());
    let mut registry = skein_core::Registry::open(&paths)?;
    let project_id = registry.add_project(&project, Some("Expired Project"))?.id;
    let request_id = "30000000-0000-4000-8000-000000000099";
    let outcome = registry.plan_conductor_run(&skein_core::NewConductorRun {
        request_id,
        prompt: "continue Expired Project",
        include_session_text: false,
        full_access_acknowledged: true,
        expected: skein_core::ExpectedConductorRoute {
            project_id,
            action: "start",
            source_thread_id: None,
        },
        explicit_selection: false,
    })?;
    assert!(matches!(
        outcome,
        skein_core::ConductorPlanOutcome::Created { .. }
    ));
    drop(registry);

    std::thread::sleep(std::time::Duration::from_secs(11));
    let mut retry = skein(&data, &config)
        .env("SKEIN_CODEX_BIN", temp.path().join("must-not-run"))
        .args([
            "conduct",
            "--full-access",
            "--request-id",
            request_id,
            "--json",
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    retry
        .stdin
        .take()
        .expect("expired retry stdin")
        .write_all(b"status only")?;
    let retry = retry.wait_with_output()?;
    assert!(
        retry.status.success(),
        "{}",
        String::from_utf8_lossy(&retry.stderr)
    );
    let status: Value = serde_json::from_slice(&retry.stdout)?;
    assert_eq!(status["reused"], true);
    assert_eq!(status["dispatched"], false);
    assert_eq!(status["run"]["state"], "failed");
    assert_eq!(status["worker"]["state"], "lost");
    assert_eq!(status["worker"]["exit_kind"], "lease_lost");
    Ok(())
}

#[cfg(unix)]
#[test]
fn reads_and_reconciles_exact_lost_worker_turn_without_source_content() -> Result<(), Box<dyn Error>>
{
    use std::os::unix::fs::PermissionsExt;

    use skein_core::NewControlRun;
    use skein_core::Registry;
    use skein_core::SkeinPaths;
    use skein_core::WorkerState;

    let temp = tempfile::tempdir()?;
    let data = temp.path().join("data");
    let config = temp.path().join("config");
    let project = temp.path().join("project");
    std::fs::create_dir(&project)?;
    let paths = SkeinPaths::new(config.clone(), data.clone());
    let mut registry = Registry::open(&paths)?;
    registry.add_project(&project, Some("Synthetic"))?;
    let plan = registry.plan_control_run(&NewControlRun {
        project_path: &project,
        resume_thread_id: None,
        prompt: "RECOVERY_PRIVATE_INITIAL",
        full_access_acknowledged: true,
    })?;
    let claim = registry.create_control_worker(plan.run_id)?;
    registry.mark_worker_ready(&claim, "127.0.0.1:1", 42)?;
    registry.heartbeat_worker(&claim, WorkerState::Busy)?;
    registry.begin_owned_control_action(plan.thread_action_id, &claim)?;
    registry.acknowledge_owned_thread_action(
        plan.thread_action_id,
        "recovery-thread",
        Some("recovery-session"),
        &claim,
    )?;
    registry.begin_owned_control_action(plan.turn_action_id, &claim)?;
    registry.acknowledge_owned_turn_action(plan.turn_action_id, "recovery-turn", &claim)?;
    registry.mark_owned_control_uncertain(plan.run_id, &claim)?;
    registry.finish_worker(&claim, "clean")?;
    drop(registry);

    let log = temp.path().join("read.log");
    let script = temp.path().join("fake-codex-read");
    let response = json!({
        "id": 3,
        "result": {
            "thread": {
                "id": "recovery-thread",
                "sessionId": "recovery-session",
                "cwd": project.canonicalize()?,
                "createdAt": 1,
                "updatedAt": 2,
                "source": "appServer",
                "status": {"type": "notLoaded"},
                "modelProvider": "openai",
                "cliVersion": "0.144.1",
                "ephemeral": false,
                "preview": "RECOVERY_PRIVATE_PREVIEW",
                "turns": [{
                    "id": "recovery-turn",
                    "status": "completed",
                    "itemsView": "full",
                    "items": [{
                        "type": "userMessage",
                        "id": "recovery-item",
                        "clientId": plan.client_message_id,
                        "content": [{"type": "text", "text": "RECOVERY_PRIVATE_SOURCE"}]
                    }]
                }]
            }
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
read -r request
printf '%s\n' "$request" >> '{}'
printf '%s\n' '{}'
read -r _ || true
"#,
            log.display(),
            response
        ),
    )?;
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700))?;

    let reconciled = skein(&data, &config)
        .env("SKEIN_CODEX_BIN", &script)
        .args([
            "worker",
            "reconcile",
            &plan.run_id.to_string(),
            "--request-id",
            "00000000-0000-4000-8000-000000000789",
            "--json",
        ])
        .output()?;
    assert!(
        reconciled.status.success(),
        "{}",
        String::from_utf8_lossy(&reconciled.stderr)
    );
    let reconciled_text = String::from_utf8(reconciled.stdout)?;
    assert!(!reconciled_text.contains("RECOVERY_PRIVATE"));
    let reconciled: Value = serde_json::from_str(&reconciled_text)?;
    assert_eq!(reconciled["run"]["state"], "completed");
    assert_eq!(reconciled["source"]["turnId"], "recovery-turn");

    let retried = skein(&data, &config)
        .env("SKEIN_CODEX_BIN", temp.path().join("must-not-run"))
        .args([
            "worker",
            "reconcile",
            &plan.run_id.to_string(),
            "--request-id",
            "00000000-0000-4000-8000-000000000789",
            "--json",
        ])
        .output()?;
    assert!(retried.status.success());
    let retried: Value = serde_json::from_slice(&retried.stdout)?;
    assert_eq!(retried["reused"], true);
    assert_eq!(retried["run"]["state"], "completed");

    let read = skein(&data, &config)
        .env("SKEIN_CODEX_BIN", &script)
        .args(["worker", "read", &plan.run_id.to_string(), "--json"])
        .output()?;
    assert!(read.status.success());
    let read_text = String::from_utf8(read.stdout)?;
    assert!(!read_text.contains("RECOVERY_PRIVATE"));
    let read: Value = serde_json::from_str(&read_text)?;
    assert_eq!(read["turns"][0]["status"], "completed");
    assert_eq!(read["contentRedacted"], true);

    let requests = std::fs::read_to_string(&log)?;
    assert_eq!(requests.lines().count(), 2);
    for line in requests.lines() {
        let request: Value = serde_json::from_str(line)?;
        assert_eq!(request["method"], "thread/read");
        assert_eq!(request["params"]["threadId"], "recovery-thread");
        assert_eq!(request["params"]["includeTurns"], true);
    }
    let audit = skein(&data, &config)
        .args(["control", "show", &plan.run_id.to_string(), "--json"])
        .output()?;
    let audit: Value = serde_json::from_slice(&audit.stdout)?;
    assert!(audit["actions"].as_array().is_some_and(|actions| {
        actions.iter().any(|action| {
            action["action_kind"] == "status_reconcile" && action["state"] == "succeeded"
        })
    }));
    let database = std::fs::read(data.join("skein.sqlite3"))?;
    for sentinel in [
        b"RECOVERY_PRIVATE_INITIAL".as_slice(),
        b"RECOVERY_PRIVATE_PREVIEW".as_slice(),
        b"RECOVERY_PRIVATE_SOURCE".as_slice(),
    ] {
        assert!(
            !database
                .windows(sentinel.len())
                .any(|value| value == sentinel)
        );
    }
    Ok(())
}

#[test]
fn matches_and_summarizes_local_metadata_without_codex_or_query_persistence()
-> Result<(), Box<dyn Error>> {
    use skein_core::Registry;
    use skein_core::SessionObservation;
    use skein_core::SkeinPaths;

    let temp = tempfile::tempdir()?;
    let data = temp.path().join("data");
    let config = temp.path().join("config");
    let project = temp.path().join("checkout-service");
    std::fs::create_dir(&project)?;
    let paths = SkeinPaths::new(config.clone(), data.clone());
    let mut registry = Registry::open(&paths)?;
    registry.add_project(&project, Some("Checkout Service"))?;
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)?
        .as_secs() as i64;
    registry.import_sessions(&[SessionObservation {
        source_kind: "codex".to_owned(),
        source_thread_id: "01900000-0000-7000-8000-000000000321".to_owned(),
        source_session_id: Some("01900000-0000-7000-8000-000000000320".to_owned()),
        source_cwd: project.clone(),
        source_created_at: now - 60,
        source_updated_at: now,
        source_label: "cli".to_owned(),
        observed_status_label: "notLoaded".to_owned(),
        model_provider: Some("openai".to_owned()),
        source_version: Some("0.144.1".to_owned()),
        parent_source_thread_id: None,
        forked_from_source_thread_id: None,
        ephemeral: false,
        name: Some("SESSION_PRIVATE_TITLE".to_owned()),
        preview: Some("SESSION_PRIVATE_PREVIEW".to_owned()),
        text_imported: true,
    }])?;
    drop(registry);

    let query_sentinel = "QUERY_PRIVATE_NEVER_STORE";
    let mut child = skein(&data, &config)
        .env("SKEIN_CODEX_BIN", temp.path().join("must-not-run"))
        .args(["match", "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    child.stdin.take().expect("match stdin").write_all(
        format!("{query_sentinel} continue 01900000-0000-7000-8000-000000000321").as_bytes(),
    )?;
    let output = child.wait_with_output()?;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let text = String::from_utf8(output.stdout)?;
    assert!(!text.contains(query_sentinel));
    assert!(!text.contains("SESSION_PRIVATE_TITLE"));
    assert!(!text.contains("SESSION_PRIVATE_PREVIEW"));
    let report: Value = serde_json::from_str(&text)?;
    assert_eq!(report["recommendation"]["confidence"], "high");
    assert_eq!(report["recommendation"]["action"], "resume");
    assert_eq!(report["recommendation"]["dispatchable"], false);

    let mut with_text = skein(&data, &config)
        .env("SKEIN_CODEX_BIN", temp.path().join("must-not-run"))
        .args(["match", "--include-text", "--json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    with_text
        .stdin
        .take()
        .expect("match stdin")
        .write_all(b"SESSION_PRIVATE_TITLE checkout")?;
    let with_text = with_text.wait_with_output()?;
    assert!(with_text.status.success());
    let with_text = String::from_utf8(with_text.stdout)?;
    assert!(!with_text.contains("SESSION_PRIVATE_TITLE"));
    assert!(!with_text.contains("SESSION_PRIVATE_PREVIEW"));

    let card = skein(&data, &config)
        .env("SKEIN_CODEX_BIN", temp.path().join("must-not-run"))
        .arg("summary")
        .arg("project")
        .arg(&project)
        .arg("--json")
        .output()?;
    assert!(card.status.success());
    let card_text = String::from_utf8(card.stdout)?;
    assert!(card_text.contains("Checkout Service"));
    assert!(!card_text.contains("SESSION_PRIVATE"));

    let day = skein(&data, &config)
        .env("SKEIN_CODEX_BIN", temp.path().join("must-not-run"))
        .args(["summary", "day", "--json"])
        .output()?;
    assert!(day.status.success());
    let day: Value = serde_json::from_slice(&day.stdout)?;
    assert_eq!(day["persisted"], false);
    assert_eq!(day["coverage"]["externalShellWork"], false);

    let database = std::fs::read(data.join("skein.sqlite3"))?;
    assert!(
        !database
            .windows(query_sentinel.len())
            .any(|window| window == query_sentinel.as_bytes())
    );
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
