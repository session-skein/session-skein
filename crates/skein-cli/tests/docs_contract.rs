use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("crate must live under the workspace root")
        .to_path_buf()
}

fn read(path: impl AsRef<Path>) -> String {
    let path = path.as_ref();
    fs::read_to_string(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn collect_files(root: &Path, extension: &str, files: &mut Vec<PathBuf>) {
    let mut entries = fs::read_dir(root)
        .unwrap_or_else(|error| panic!("read_dir {}: {error}", root.display()))
        .collect::<Result<Vec<_>, _>>()
        .expect("directory entries must be readable");
    entries.sort_by_key(std::fs::DirEntry::file_name);
    for entry in entries {
        let path = entry.path();
        let name = entry.file_name();
        if path.is_dir() {
            if name == ".git" || name == "target" {
                continue;
            }
            collect_files(&path, extension, files);
        } else if path.extension().and_then(|value| value.to_str()) == Some(extension) {
            files.push(path);
        }
    }
}

#[test]
fn handbook_has_required_entry_points() {
    let root = repo_root();
    for path in [
        "README.md",
        "llms.txt",
        "INSTALL.md",
        "docs/index.md",
        "docs/getting-started.md",
        "docs/concepts.md",
        "docs/codebase-map.md",
        "docs/indexing-and-search.md",
        "docs/cli-reference.md",
        "docs/mcp-reference.md",
        "docs/state-and-configuration.md",
        "docs/maintenance.md",
        "docs/troubleshooting.md",
        "docs/releases.md",
        "docs/privacy.md",
        "docs/architecture.md",
    ] {
        assert!(root.join(path).is_file(), "missing handbook page: {path}");
    }
}

#[test]
fn pages_site_reuses_canonical_markdown_and_deploys_only_from_main() {
    let root = repo_root();
    for path in [
        "site/index.md",
        "site/_config.yml",
        "site/_data/navigation.yml",
        "site/_layouts/default.html",
        "site/assets/site.css",
        "scripts/stage-pages.py",
        "scripts/check-pages-links.py",
        ".github/workflows/pages.yml",
    ] {
        assert!(root.join(path).is_file(), "missing Pages contract: {path}");
    }
    let stage = read(root.join("scripts/stage-pages.py"));
    assert!(stage.contains(".rglob(\"*.md\")"));
    assert!(stage.contains("canonical_source"));
    let workflow = read(root.join(".github/workflows/pages.yml")).replace("\r\n", "\n");
    assert!(workflow.contains("pull_request:"));
    assert!(workflow.contains("github.event_name != 'pull_request'"));
    assert!(workflow.contains("github.ref == 'refs/heads/main'"));
    assert!(workflow.contains("pages: write"));
    assert!(workflow.contains("id-token: write"));
    assert!(workflow.contains("permissions:\n  contents: read"));
    assert!(workflow.contains("scripts/check-pages-links.py _site"));
}

#[test]
fn local_markdown_links_resolve() {
    let root = repo_root();
    let mut markdown = Vec::new();
    collect_files(&root, "md", &mut markdown);
    collect_files(&root, "txt", &mut markdown);
    let mut failures = Vec::new();

    for file in markdown {
        let source = read(&file);
        for (line_index, line) in source.lines().enumerate() {
            let mut rest = line;
            while let Some(start) = rest.find("](") {
                rest = &rest[start + 2..];
                let Some(end) = rest.find(')') else {
                    break;
                };
                let raw = rest[..end].trim().trim_matches(['<', '>']);
                rest = &rest[end + 1..];
                if raw.is_empty()
                    || raw.starts_with('#')
                    || raw.starts_with("http://")
                    || raw.starts_with("https://")
                    || raw.starts_with("mailto:")
                {
                    continue;
                }
                let path_part = raw.split('#').next().unwrap_or_default();
                if path_part.is_empty() {
                    continue;
                }
                let parent = if file.starts_with(root.join("site")) {
                    root.as_path()
                } else {
                    file.parent().expect("markdown parent")
                };
                let target = parent.join(path_part);
                if !target.exists() {
                    failures.push(format!(
                        "{}:{} -> {}",
                        file.strip_prefix(&root).unwrap_or(&file).display(),
                        line_index + 1,
                        raw
                    ));
                }
            }
        }
    }

    assert!(
        failures.is_empty(),
        "broken local Markdown links:\n{}",
        failures.join("\n")
    );
}

fn command_names(args: &[&str]) -> Vec<String> {
    let output = Command::new(env!("CARGO_BIN_EXE_skein"))
        .args(args)
        .arg("--help")
        .output()
        .expect("run skein help");
    assert!(output.status.success(), "skein help must succeed");
    let stdout = String::from_utf8(output.stdout).expect("help is utf-8");
    let mut in_commands = false;
    let mut names = Vec::new();
    for line in stdout.lines() {
        if line == "Commands:" {
            in_commands = true;
            continue;
        }
        if in_commands && line.is_empty() {
            break;
        }
        if in_commands {
            let name = line.split_whitespace().next().unwrap_or_default();
            if !name.is_empty() && name != "help" {
                names.push(name.to_owned());
            }
        }
    }
    names
}

#[test]
fn cli_reference_covers_generated_public_commands() {
    let cli = read(repo_root().join("docs/cli-reference.md"));
    let groups: &[&[&str]] = &[
        &[],
        &["project"],
        &["scan-root"],
        &["context"],
        &["import"],
        &["session"],
        &["control"],
        &["worker"],
        &["summary"],
    ];
    for group in groups {
        let names = command_names(group);
        assert!(!names.is_empty(), "expected commands for {group:?}");
        for name in names {
            assert!(
                cli.contains(&format!("`{name}")) || cli.contains(&format!(" {name}")),
                "CLI reference is missing {group:?} command {name}"
            );
        }
    }
}

fn mcp_tool_names(source: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut after_tool = false;
    for line in source.lines() {
        let trimmed = line.trim();
        if after_tool {
            if let Some(name) = trimmed
                .strip_prefix('"')
                .and_then(|value| value.strip_suffix("\","))
            {
                names.push(name.to_owned());
            }
            after_tool = false;
        }
        if trimmed == "tool(" {
            after_tool = true;
        }
    }
    names
}

#[test]
fn mcp_reference_covers_the_runtime_catalog() {
    let source = read(repo_root().join("crates/skein-cli/src/mcp.rs"));
    let reference = read(repo_root().join("docs/mcp-reference.md"));
    let tools = mcp_tool_names(&source);
    assert_eq!(tools.len(), 25, "update the documented MCP catalog count");
    for tool in tools {
        assert!(
            reference.contains(&format!("`{tool}`")),
            "MCP reference is missing {tool}"
        );
    }
}

#[test]
fn public_environment_variables_are_documented() {
    let state = read(repo_root().join("docs/state-and-configuration.md"));
    for variable in [
        "SKEIN_CONFIG_DIR",
        "SKEIN_DATA_DIR",
        "SKEIN_CODEX_BIN",
        "CODEX_HOME",
    ] {
        assert!(
            state.contains(&format!("`{variable}`")),
            "missing {variable}"
        );
    }
    assert!(
        !state.contains("SKEIN_GUARDED_CODEX_BIN")
            && !state.contains("SKEIN_TEST_FAIL_WORKER_LAUNCH"),
        "internal variables must not be advertised as public configuration"
    );
}

#[test]
fn plugin_skill_and_installer_versions_match_the_crate() {
    let root = repo_root();
    let manifest: Value = serde_json::from_str(&read(
        root.join("plugins/session-skein/.codex-plugin/plugin.json"),
    ))
    .expect("valid plugin JSON");
    assert_eq!(manifest["name"], "session-skein");
    assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(manifest["skills"], "./skills/");
    assert_eq!(manifest["mcpServers"], "./.mcp.json");

    let marketplace: Value =
        serde_json::from_str(&read(root.join(".agents/plugins/marketplace.json")))
            .expect("valid marketplace JSON");
    assert_eq!(marketplace["name"], "session-skein");
    assert_eq!(marketplace["plugins"][0]["name"], "session-skein");
    assert_eq!(
        marketplace["plugins"][0]["source"]["path"],
        "./plugins/session-skein"
    );

    let mcp: Value = serde_json::from_str(&read(root.join("plugins/session-skein/.mcp.json")))
        .expect("valid plugin MCP JSON");
    assert_eq!(mcp["mcpServers"]["session-skein"]["command"], "skein");
    assert_eq!(
        mcp["mcpServers"]["session-skein"]["args"],
        serde_json::json!(["mcp"])
    );

    let skill_path = root.join("plugins/session-skein/skills/session-skein/SKILL.md");
    let skill = read(&skill_path);
    let normalized_skill = skill.replace("\r\n", "\n");
    assert!(normalized_skill.starts_with("---\nname: session-skein\ndescription: "));
    assert!(
        normalized_skill.matches('\n').count() < 500,
        "skill must stay concise"
    );
    assert!(
        !normalized_skill.contains("TODO"),
        "skill contains a placeholder"
    );
    assert!(
        skill_path
            .with_file_name("agents")
            .join("openai.yaml")
            .is_file()
    );

    let shell = read(root.join("install.sh"));
    let powershell = read(root.join("install.ps1"));
    assert!(shell.contains("--binary did not identify itself as 'skein VERSION'"));
    assert!(powershell.contains("-Binary did not identify itself as 'skein VERSION'"));
    assert!(
        shell.contains("SKILL_SNAPSHOT_ROOT") && powershell.contains("SkillSnapshotRoot"),
        "direct installers must isolate the active skill from mutable source checkouts"
    );
}

#[test]
fn public_text_has_no_developer_machine_paths() {
    let root = repo_root();
    let mut failures = Vec::new();
    let forbidden_values = [
        format!("/home/{}{}", "sa", "bino"),
        format!("/media/{}{}", "sa", "bino"),
        format!("SABINO_{}", "EXT4"),
    ];
    for extension in [
        "md", "txt", "rs", "toml", "json", "yaml", "yml", "sh", "ps1",
    ] {
        let mut files = Vec::new();
        collect_files(&root, extension, &mut files);
        for file in files {
            let source = read(&file);
            for forbidden in &forbidden_values {
                if source.contains(forbidden) {
                    failures.push(format!(
                        "{} contains {forbidden}",
                        file.strip_prefix(&root).unwrap_or(&file).display()
                    ));
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn installers_expose_parallel_public_controls() {
    let root = repo_root();
    let shell = read(root.join("install.sh"));
    let powershell = read(root.join("install.ps1"));
    for (shell_option, powershell_option) in [
        ("--catalog-only", "-CatalogOnly"),
        ("--control", "-Control"),
        ("--binary", "-Binary"),
        ("--source", "-Source"),
        ("--version", "-Version"),
        ("--channel", "-Channel"),
        ("--bin-dir", "-BinDir"),
        ("--replace-binary", "-ReplaceBinary"),
        ("--no-mcp", "-NoMcp"),
        ("--no-skill", "-NoSkill"),
        ("--replace-mcp", "-ReplaceMcp"),
        ("--replace-skill", "-ReplaceSkill"),
        ("--update", "-Update"),
        ("--uninstall", "-Uninstall"),
    ] {
        assert!(shell.contains(shell_option), "missing {shell_option}");
        assert!(
            powershell.contains(powershell_option),
            "missing {powershell_option}"
        );
    }
}

#[test]
fn binary_first_installers_enforce_release_supply_chain_contract() {
    let root = repo_root();
    let shell = read(root.join("install.sh"));
    let powershell = read(root.join("install.ps1"));
    let ci = read(root.join(".github/workflows/ci.yml"));
    let channel = read(root.join("release-channels/preview"));
    let manifest: Value = serde_json::from_str(&read(
        root.join("plugins/session-skein/.codex-plugin/plugin.json"),
    ))
    .expect("plugin manifest");

    assert_eq!(channel.trim(), manifest["version"]);
    for target in [
        "x86_64-unknown-linux-gnu",
        "x86_64-apple-darwin",
        "aarch64-apple-darwin",
    ] {
        assert!(shell.contains(target), "Unix installer lacks {target}");
    }
    assert!(powershell.contains("x86_64-pc-windows-msvc"));
    for contract in [
        "release-manifest.json",
        "SHA256SUMS",
        "release-package.json",
    ] {
        assert!(shell.contains(contract));
        assert!(powershell.contains(contract));
    }
    assert!(shell.contains("checksum verification failed"));
    assert!(shell.contains("unsafe path"));
    assert!(powershell.contains("Checksum verification failed"));
    assert!(powershell.contains("unsafe path"));
    assert!(
        !shell.contains("eval "),
        "installer must not evaluate downloaded text"
    );
    assert!(!powershell.contains("Invoke-Expression"));
    assert!(ci.contains("tests/install/unix-release-installer.sh"));
    assert!(ci.contains("tests/install/windows-release-installer.ps1"));
}
