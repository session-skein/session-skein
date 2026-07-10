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
    Ok(())
}
