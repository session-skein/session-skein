use std::collections::BTreeMap;
use std::fs;
use std::io::Read;
#[cfg(windows)]
use std::os::windows::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(windows)]
use std::process::Stdio;

use semver::Version;
use serde::Serialize;
use sha2::{Digest, Sha256};

#[derive(Debug)]
pub(crate) struct UpdateOptions {
    pub version: Option<String>,
    pub check: bool,
    pub force: bool,
    pub allow_downgrade: bool,
    pub json: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct UpdateReport {
    current_version: String,
    target_version: String,
    update_available: bool,
    check_only: bool,
    status: &'static str,
    scheduled: bool,
}

#[derive(Default)]
struct Receipt {
    version: String,
    binary: PathBuf,
    binary_hash: String,
    source: String,
    installer: PathBuf,
    installer_hash: String,
    skill: Option<PathBuf>,
    skill_source: Option<PathBuf>,
    mcp_profile: Option<String>,
    mcp_hash: Option<String>,
}

pub(crate) fn run(options: UpdateOptions) -> Result<(), Box<dyn std::error::Error>> {
    let state = install_state_dir()?;
    let receipt_path = state.join(receipt_name());
    let receipt = read_receipt(&receipt_path)?;
    let current = preflight(&receipt)?;

    let target = inspect_target(&receipt.installer, options.version.as_deref())?;
    let target_version = Version::parse(&target)?;
    if target_version < current && !options.allow_downgrade {
        return Err(format!(
            "refusing downgrade from {current} to {target_version}; pass --allow-downgrade only after reviewing schema compatibility"
        )
        .into());
    }
    let same = target_version == current;
    if same && !options.force && !options.check {
        return Err(format!(
            "Session Skein {current} is already installed; pass --force to reinstall intentionally"
        )
        .into());
    }
    if options.check || (same && !options.force) {
        return print_report(
            UpdateReport {
                current_version: current.to_string(),
                target_version: target_version.to_string(),
                update_available: target_version > current,
                check_only: true,
                status: if target_version > current {
                    "available"
                } else {
                    "current"
                },
                scheduled: false,
            },
            options.json,
        );
    }

    let mut mcp_env = current_mcp(&receipt)?;
    if let Some(skill) = &receipt.skill
        && let Some(codex_home) = skill.parent().and_then(Path::parent)
    {
        mcp_env.insert(
            "CODEX_HOME".to_owned(),
            codex_home.to_string_lossy().into_owned(),
        );
    }
    for key in [
        "SKEIN_CONFIG_DIR",
        "SKEIN_DATA_DIR",
        "SKEIN_CODEX_BIN",
        "SKEIN_RELEASE_BASE_URL",
        "SKEIN_RELEASE_CHANNEL_URL",
        "SKEIN_ALLOW_INSECURE_TEST_DOWNLOADS",
        "SKEIN_ALLOW_RELEASE_OVERRIDE",
    ] {
        if let Ok(value) = std::env::var(key) {
            mcp_env.entry(key.to_owned()).or_insert(value);
        }
    }
    let args = installer_args(&receipt, &target_version.to_string());
    #[cfg(windows)]
    let scheduled = schedule_windows(
        &state,
        &receipt.installer,
        &receipt.installer_hash,
        &args,
        &mcp_env,
    )?;
    #[cfg(not(windows))]
    let scheduled = {
        let mut command = Command::new(&receipt.installer);
        command.args(&args).envs(&mcp_env);
        if options.json {
            let output = command.output()?;
            if !output.status.success() {
                return Err(format!(
                    "verified installer exited with {}: {}",
                    output.status,
                    String::from_utf8_lossy(&output.stderr).trim()
                )
                .into());
            }
        } else {
            let status = command.status()?;
            if !status.success() {
                return Err(format!("verified installer exited with {status}").into());
            }
        }
        false
    };
    print_report(
        UpdateReport {
            current_version: current.to_string(),
            target_version: target_version.to_string(),
            update_available: target_version > current,
            check_only: false,
            status: if scheduled { "scheduled" } else { "updated" },
            scheduled,
        },
        options.json,
    )
}

fn preflight(receipt: &Receipt) -> Result<Version, Box<dyn std::error::Error>> {
    if !receipt.source.starts_with("release:v") {
        return Err("product update owns only release-based installations; source contributors should run install.sh --update or install.ps1 -Update from their checkout".into());
    }
    let current = fs::canonicalize(std::env::current_exe()?)?;
    let binary = fs::canonicalize(&receipt.binary).map_err(|_| "receipt binary is missing")?;
    if current != binary {
        return Err(format!(
            "running executable {current:?} disagrees with receipt binary {binary:?}"
        )
        .into());
    }
    let compiled = Version::parse(env!("CARGO_PKG_VERSION"))?;
    let recorded = Version::parse(&receipt.version)
        .map_err(|_| "installation receipt contains an invalid version")?;
    if recorded != compiled {
        return Err(format!(
            "installation receipt version {recorded} disagrees with running binary version {compiled}; refusing modified or inconsistent ownership metadata"
        )
        .into());
    }
    require_hash(&receipt.binary, &receipt.binary_hash, "binary")?;
    if receipt.installer.as_os_str().is_empty() {
        return Err("this release installation predates the updater snapshot; reinstall alpha.9 once with the binary-first installer".into());
    }
    require_hash(
        &receipt.installer,
        &receipt.installer_hash,
        "installer snapshot",
    )?;
    if let (Some(skill), Some(source)) = (&receipt.skill, &receipt.skill_source) {
        let observed =
            fs::read_link(skill).map_err(|_| "installed skill is not the receipt-owned link")?;
        if observed != *source {
            return Err("installed skill ownership drift detected".into());
        }
    }
    Ok(compiled)
}

fn inspect_target(
    installer: &Path,
    version: Option<&str>,
) -> Result<String, Box<dyn std::error::Error>> {
    let mut command = installer_command(installer);
    command.arg("--check").arg("--json");
    if let Some(version) = version {
        command.arg(version_flag()).arg(version);
    }
    let output = command.output()?;
    if !output.status.success() {
        return Err(format!(
            "release verification failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )
        .into());
    }
    let stdout = String::from_utf8(output.stdout)?;
    let json = stdout
        .lines()
        .rev()
        .find(|line| line.trim_start().starts_with('{'))
        .ok_or("installer check omitted JSON output")?;
    let value: serde_json::Value = serde_json::from_str(json)?;
    if value["verified"].as_bool() != Some(true) {
        return Err("installer check did not affirm verified=true".into());
    }
    value["targetVersion"]
        .as_str()
        .map(str::to_owned)
        .ok_or_else(|| "installer check omitted targetVersion".into())
}

fn current_mcp(receipt: &Receipt) -> Result<BTreeMap<String, String>, Box<dyn std::error::Error>> {
    let Some(expected) = receipt
        .mcp_hash
        .as_deref()
        .filter(|value| !value.is_empty())
    else {
        return Ok(BTreeMap::new());
    };
    let output = Command::new("codex")
        .args(["mcp", "get", "session-skein", "--json"])
        .output()?;
    if !output.status.success() {
        return Err("could not verify installer-owned MCP registration".into());
    }
    let text = String::from_utf8(output.stdout)?.trim_end().to_owned();
    let actual = hex_hash(text.as_bytes());
    if actual != expected {
        return Err("MCP ownership drift detected".into());
    }
    let value: serde_json::Value = serde_json::from_str(&text)?;
    let env = value
        .pointer("/transport/env")
        .or_else(|| value.get("env"))
        .and_then(serde_json::Value::as_object)
        .map(|values| {
            values
                .iter()
                .filter_map(|(key, value)| {
                    if matches!(
                        key.as_str(),
                        "SKEIN_CONFIG_DIR" | "SKEIN_DATA_DIR" | "SKEIN_CODEX_BIN" | "CODEX_HOME"
                    ) {
                        value.as_str().map(|value| (key.clone(), value.to_owned()))
                    } else {
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    Ok(env)
}

fn installer_args(receipt: &Receipt, version: &str) -> Vec<String> {
    let mut args = vec![
        version_flag().to_owned(),
        version.to_owned(),
        bin_flag().to_owned(),
    ];
    args.push(
        receipt
            .binary
            .parent()
            .unwrap_or(Path::new("."))
            .to_string_lossy()
            .into_owned(),
    );
    if receipt.skill.is_none() {
        args.push(no_skill_flag().to_owned());
    }
    if receipt.mcp_hash.as_deref().is_none_or(str::is_empty) {
        args.push(no_mcp_flag().to_owned());
    } else if receipt.mcp_profile.as_deref() == Some("control") {
        args.push(control_flag().to_owned());
    } else {
        args.push(catalog_flag().to_owned());
    }
    args
}

fn print_report(report: UpdateReport, json: bool) -> Result<(), Box<dyn std::error::Error>> {
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if report.status == "available" {
        println!(
            "Session Skein {} is available (current {}).",
            report.target_version, report.current_version
        );
    } else if report.status == "current" {
        println!("Session Skein {} is current.", report.current_version);
    } else if report.scheduled {
        println!(
            "Session Skein {} update is scheduled and will continue after this process exits.",
            report.target_version
        );
    } else {
        println!(
            "Session Skein updated from {} to {}.",
            report.current_version, report.target_version
        );
    }
    Ok(())
}

fn require_hash(
    path: &Path,
    expected: &str,
    label: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut file = fs::File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    if format!("{:x}", digest.finalize()) != expected {
        return Err(format!("{label} ownership hash mismatch: {}", path.display()).into());
    }
    Ok(())
}

fn hex_hash(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

#[cfg(unix)]
fn read_receipt(path: &Path) -> Result<Receipt, Box<dyn std::error::Error>> {
    let content = fs::read_to_string(path)?;
    let values = content
        .lines()
        .filter_map(|line| line.split_once('='))
        .collect::<BTreeMap<_, _>>();
    Ok(Receipt {
        version: values.get("version").unwrap_or(&"").to_string(),
        binary: PathBuf::from(values.get("binary").unwrap_or(&"")),
        binary_hash: values.get("binary_hash").unwrap_or(&"").to_string(),
        source: values.get("source").unwrap_or(&"").to_string(),
        installer: PathBuf::from(values.get("installer").unwrap_or(&"")),
        installer_hash: values.get("installer_hash").unwrap_or(&"").to_string(),
        skill: optional_path(values.get("skill").copied()),
        skill_source: optional_path(values.get("skill_source").copied()),
        mcp_profile: optional_string(values.get("mcp_profile").copied()),
        mcp_hash: optional_string(values.get("mcp_hash").copied()),
    })
}

#[cfg(windows)]
fn read_receipt(path: &Path) -> Result<Receipt, Box<dyn std::error::Error>> {
    let value: serde_json::Value = serde_json::from_slice(&fs::read(path)?)?;
    let get = |key| value[key].as_str().unwrap_or_default().to_owned();
    Ok(Receipt {
        version: get("version"),
        binary: get("binary").into(),
        binary_hash: get("binaryHash"),
        source: get("source"),
        installer: get("installer").into(),
        installer_hash: get("installerHash"),
        skill: optional_path(value["skill"].as_str()),
        skill_source: optional_path(value["skillSource"].as_str()),
        mcp_profile: optional_string(value["mcpProfile"].as_str()),
        mcp_hash: optional_string(value["mcpHash"].as_str()),
    })
}

fn optional_path(value: Option<&str>) -> Option<PathBuf> {
    value.filter(|v| !v.is_empty()).map(PathBuf::from)
}
fn optional_string(value: Option<&str>) -> Option<String> {
    value.filter(|v| !v.is_empty()).map(str::to_owned)
}

fn install_state_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    #[cfg(windows)]
    {
        Ok(PathBuf::from(std::env::var("LOCALAPPDATA")?).join("SessionSkein/install"))
    }
    #[cfg(not(windows))]
    {
        let root = if let Some(path) = std::env::var_os("XDG_STATE_HOME") {
            PathBuf::from(path)
        } else {
            PathBuf::from(std::env::var("HOME")?).join(".local/state")
        };
        Ok(root.join("session-skein/install"))
    }
}
#[cfg(unix)]
fn receipt_name() -> &'static str {
    "receipt"
}
#[cfg(windows)]
fn receipt_name() -> &'static str {
    "receipt.json"
}
#[cfg(unix)]
fn installer_command(path: &Path) -> Command {
    Command::new(path)
}
#[cfg(windows)]
fn installer_command(path: &Path) -> Command {
    let mut c = Command::new("pwsh");
    c.arg("-NoProfile").arg("-File").arg(path);
    c
}
#[cfg(unix)]
fn version_flag() -> &'static str {
    "--version"
}
#[cfg(windows)]
fn version_flag() -> &'static str {
    "-Version"
}
#[cfg(unix)]
fn bin_flag() -> &'static str {
    "--bin-dir"
}
#[cfg(windows)]
fn bin_flag() -> &'static str {
    "-BinDir"
}
#[cfg(unix)]
fn no_skill_flag() -> &'static str {
    "--no-skill"
}
#[cfg(windows)]
fn no_skill_flag() -> &'static str {
    "-NoSkill"
}
#[cfg(unix)]
fn no_mcp_flag() -> &'static str {
    "--no-mcp"
}
#[cfg(windows)]
fn no_mcp_flag() -> &'static str {
    "-NoMcp"
}
#[cfg(unix)]
fn control_flag() -> &'static str {
    "--control"
}
#[cfg(windows)]
fn control_flag() -> &'static str {
    "-Control"
}
#[cfg(unix)]
fn catalog_flag() -> &'static str {
    "--catalog-only"
}
#[cfg(windows)]
fn catalog_flag() -> &'static str {
    "-CatalogOnly"
}

#[cfg(windows)]
fn schedule_windows(
    state: &Path,
    installer: &Path,
    installer_hash: &str,
    args: &[String],
    env: &BTreeMap<String, String>,
) -> Result<bool, Box<dyn std::error::Error>> {
    let helper = state.join("update-helper.ps1");
    let result = state.join("update-helper.result.json");
    let log = state.join("update-helper.log");
    let _ = fs::remove_file(&result);
    let _ = fs::remove_file(&log);
    let quote = |value: &str| value.replace('\'', "''");
    let mut script = format!(
        r#"$ErrorActionPreference='Stop'
$result='{}'
$log='{}'
$installer='{}'
$expectedHash='{}'
$parentPid={}
$phase='starting'
function Publish([string]$status,[string]$errorText='') {{
  $temporary="$result.$PID.tmp"
  [ordered]@{{status=$status;phase=$phase;parentPid=$parentPid;helperPid=$PID;timestamp=[DateTime]::UtcNow.ToString('o');error=$errorText}} | ConvertTo-Json -Compress | Set-Content -LiteralPath $temporary -NoNewline -Encoding utf8
  Move-Item -LiteralPath $temporary -Destination $result -Force
}}
function Log([string]$message) {{ Add-Content -LiteralPath $log -Value ("{{0:o}} {{1}}" -f [DateTime]::UtcNow,$message) -Encoding utf8 }}
try {{
Publish 'running'
$phase='waiting-for-parent'
Log "helper $PID locating parent $parentPid"
$parent=Get-Process -Id $parentPid -ErrorAction SilentlyContinue
if ($null -ne $parent) {{ Log "waiting for exact parent process $parentPid"; Wait-Process -InputObject $parent }} else {{ Log "parent $parentPid already exited" }}
$phase='verifying-installer'
$actualHash=(Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash.ToLowerInvariant()
if ($actualHash -ne $expectedHash.ToLowerInvariant()) {{ throw "installer snapshot hash changed: $actualHash" }}
"#,
        quote(&result.to_string_lossy()),
        quote(&log.to_string_lossy()),
        quote(&installer.to_string_lossy()),
        quote(installer_hash),
        std::process::id(),
    );
    for (key, value) in env {
        script.push_str(&format!("$env:{}='{}'\n", key, quote(value)));
    }
    script.push_str(
        "$phase='running-installer'\nLog \"executing verified installer snapshot\"\n& $installer",
    );
    for arg in args {
        script.push_str(&format!(" '{}'", quote(arg)));
    }
    script.push_str("\nif ($LASTEXITCODE -ne 0) { throw \"installer exited with $LASTEXITCODE\" }\n$phase='completed'\nLog 'installer completed successfully'\nPublish 'completed'\n} catch {\n$failure=$_.Exception.ToString()\nLog \"failed during $phase`: $failure\"\ntry { Publish 'failed' $failure } catch { Add-Content -LiteralPath $log -Value $_.Exception.ToString() -Encoding utf8 }\nexit 1\n}\n");
    fs::write(&helper, script)?;
    let parser = Command::new("pwsh")
        .args([
            "-NoProfile",
            "-Command",
            "[void][scriptblock]::Create((Get-Content -LiteralPath $env:SKEIN_HELPER_PARSE_PATH -Raw))",
        ])
        .env("SKEIN_HELPER_PARSE_PATH", &helper)
        .stdin(Stdio::null())
        .output()?;
    if !parser.status.success() {
        return Err(format!(
            "generated Windows update helper failed PowerShell parsing: {}",
            String::from_utf8_lossy(&parser.stderr).trim()
        )
        .into());
    }
    let mut command = Command::new("pwsh");
    command
        .args(["-NoProfile", "-File"])
        .arg(helper)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        // DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP: the helper must outlive
        // the updating executable and must not retain CI/pipeline console handles.
        .creation_flags(0x0000_0008 | 0x0000_0200);
    command.spawn()?;
    Ok(true)
}
