use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn read(path: impl AsRef<Path>) -> String {
    fs::read_to_string(path).expect("read release contract file")
}

#[test]
fn release_workflow_is_pr_testable_tag_gated_and_least_privilege() {
    let workflow = read(root().join(".github/workflows/release.yml")).replace("\r\n", "\n");
    assert!(workflow.contains("pull_request:"));
    assert!(
        !workflow.contains("    paths:"),
        "every pull request must package every target"
    );
    assert!(workflow.contains("tags:\n      - \"v*.*.*\""));
    assert!(workflow.contains("if: needs.validate.outputs.publish == 'true'"));
    assert!(workflow.contains("permissions:\n  contents: read"));
    assert!(workflow.contains("contents: write\n      id-token: write\n      attestations: write"));
    assert!(workflow.contains("check-ref --ref \"$GITHUB_REF\" --event \"$GITHUB_EVENT_NAME\""));
    assert!(workflow.contains("gh release create \"$TAG\" --verify-tag --draft --prerelease"));
    assert!(workflow.contains("gh release upload \"$TAG\" release-assets/*"));
    assert!(workflow.contains("gh release edit \"$TAG\" --draft=false"));
    assert!(workflow.contains("subject-path: release-assets/*"));
    assert!(workflow.contains("python scripts/release.py assemble"));

    for target in [
        "x86_64-unknown-linux-gnu",
        "x86_64-apple-darwin",
        "aarch64-apple-darwin",
        "x86_64-pc-windows-msvc",
    ] {
        assert!(workflow.contains(target), "missing release target {target}");
    }
    for line in workflow.lines().filter(|line| line.contains("uses:")) {
        let revision = line
            .split('@')
            .nth(1)
            .and_then(|value| value.split_whitespace().next())
            .expect("action revision");
        assert_eq!(revision.len(), 40, "action is not pinned by SHA: {line}");
        assert!(revision.bytes().all(|byte| byte.is_ascii_hexdigit()));
    }
}

#[test]
fn release_identity_rejects_wrong_tags_and_never_publishes_pull_requests() {
    let script = root().join("scripts/release.py");
    let version = env!("CARGO_PKG_VERSION");
    let pr = Command::new("python")
        .arg(&script)
        .args([
            "check-ref",
            "--ref",
            "refs/pull/17/merge",
            "--event",
            "pull_request",
        ])
        .output()
        .expect("run release identity check");
    assert!(pr.status.success());
    let pr: Value = serde_json::from_slice(&pr.stdout).expect("PR identity JSON");
    assert_eq!(pr["publish"], false);

    let exact = Command::new("python")
        .arg(&script)
        .args([
            "check-ref",
            "--ref",
            &format!("refs/tags/v{version}"),
            "--event",
            "push",
        ])
        .output()
        .expect("run tag identity check");
    assert!(exact.status.success());
    let exact: Value = serde_json::from_slice(&exact.stdout).expect("tag identity JSON");
    assert_eq!(exact["publish"], true);

    let wrong = Command::new("python")
        .arg(script)
        .args(["check-ref", "--ref", "refs/tags/v9.9.9", "--event", "push"])
        .output()
        .expect("run mismatched tag check");
    assert!(!wrong.status.success());
}

#[test]
fn deterministic_packages_include_distribution_contract_and_assemble_checksums() {
    let temp = tempfile::tempdir().expect("temporary release directory");
    let script = root().join("scripts/release.py");
    let targets = [
        ("x86_64-unknown-linux-gnu", "skein"),
        ("x86_64-apple-darwin", "skein"),
        ("aarch64-apple-darwin", "skein"),
        ("x86_64-pc-windows-msvc", "skein.exe"),
    ];
    let mut first_hashes = Vec::new();
    for (index, (target, binary_name)) in targets.iter().enumerate() {
        let binary = temp
            .path()
            .join(format!("binary-{index}"))
            .join(binary_name);
        fs::create_dir_all(binary.parent().expect("binary parent")).expect("create binary parent");
        fs::write(&binary, b"synthetic release executable\n").expect("write synthetic binary");
        let first = temp.path().join("first").join(target);
        let second = temp.path().join("second").join(target);
        for output in [&first, &second] {
            let status = Command::new("python")
                .arg(&script)
                .args(["package", "--binary"])
                .arg(&binary)
                .args(["--target", target, "--output"])
                .arg(output)
                .status()
                .expect("package synthetic binary");
            assert!(status.success());
        }
        let first_archive = fs::read_dir(&first)
            .expect("first archive")
            .next()
            .expect("archive entry")
            .expect("archive path")
            .path();
        let second_archive = fs::read_dir(&second)
            .expect("second archive")
            .next()
            .expect("archive entry")
            .expect("archive path")
            .path();
        let first_bytes = fs::read(&first_archive).expect("first archive bytes");
        assert_eq!(
            first_bytes,
            fs::read(second_archive).expect("second archive bytes")
        );
        first_hashes.push(first_archive);
    }

    let assembled = temp.path().join("assembled");
    let status = Command::new("python")
        .arg(script)
        .args(["assemble", "--input"])
        .arg(temp.path().join("first"))
        .args(["--output"])
        .arg(&assembled)
        .status()
        .expect("assemble synthetic release");
    assert!(status.success());
    let manifest: Value = serde_json::from_str(&read(assembled.join("release-manifest.json")))
        .expect("release manifest JSON");
    assert_eq!(manifest["version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(manifest["assets"].as_array().map(Vec::len), Some(4));
    let checksums = read(assembled.join("SHA256SUMS"));
    assert_eq!(checksums.lines().count(), 5);
    assert!(checksums.contains("release-manifest.json"));
    assert_eq!(first_hashes.len(), 4);
}
