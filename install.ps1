<#
.SYNOPSIS
  Build and install one Session Skein source revision for local Codex on Windows.

.DESCRIPTION
  Preflights every destination, validates the binary identity and doctor JSON, and
  stores hashes/targets so uninstall preserves anything the user later replaces.
#>

[CmdletBinding()]
param(
    [switch]$CatalogOnly,
    [switch]$Control,
    [string]$Binary,
    [string]$Source,
    [string]$BinDir,
    [switch]$ReplaceBinary,
    [switch]$NoMcp,
    [switch]$NoSkill,
    [switch]$ReplaceMcp,
    [switch]$ReplaceSkill,
    [switch]$Update,
    [switch]$Uninstall,
    [switch]$Help
)

$ErrorActionPreference = 'Stop'
$OriginalParameters = @{}
foreach ($entry in $PSBoundParameters.GetEnumerator()) { $OriginalParameters[$entry.Key] = $entry.Value }
$RepoUrl = if ($env:SKEIN_REPO_URL) { $env:SKEIN_REPO_URL } else { 'https://github.com/session-skein/session-skein.git' }
$CodexHome = if ($env:CODEX_HOME) { $env:CODEX_HOME } else { Join-Path $HOME '.codex' }
$LocalAppDataRoot = if ($env:LOCALAPPDATA) { $env:LOCALAPPDATA } else { Join-Path $HOME 'AppData\Local' }
$InstallStateDir = Join-Path $LocalAppDataRoot 'SessionSkein\install'
$ReceiptPath = Join-Path $InstallStateDir 'receipt.json'
$McpBackupPath = Join-Path $InstallStateDir 'replaced-mcp.json'
$McpRollbackPath = Join-Path $InstallStateDir 'codex-config.rollback'
$McpJsonRollbackPath = Join-Path $InstallStateDir 'mcp.rollback.json'
$ReceiptRollbackPath = Join-Path $InstallStateDir 'receipt.rollback.json'
$BinaryRollbackPath = Join-Path $InstallStateDir 'binary.rollback.exe'
$CodexConfigPath = Join-Path $CodexHome 'config.toml'
$SkillSnapshotRoot = Join-Path $InstallStateDir 'skills'
$ManagedSource = if ($env:SKEIN_INSTALL_SOURCE) { $env:SKEIN_INSTALL_SOURCE } else { Join-Path $LocalAppDataRoot 'SessionSkein\repo' }
if (-not $BinDir) { $BinDir = Join-Path $LocalAppDataRoot 'Programs\SessionSkein\bin' }
$InstalledBinary = Join-Path $BinDir 'skein.exe'
$Profile = if ($Control) { 'control' } else { 'catalog' }

function Show-Usage {
@'
Session Skein installer

Usage:
  ./install.ps1 [-CatalogOnly | -Control] [options]

Options:
  -CatalogOnly       Register read/catalog MCP tools only (default)
  -Control           Expose audited conduct, steer, interrupt, and reconcile tools
  -Binary PATH       Install an already-built skein.exe
  -Source PATH       Build and install a Session Skein checkout
  -BinDir PATH       Override the executable destination
  -ReplaceBinary     Back up and replace an unowned destination binary
  -NoMcp             Do not change MCP configuration
  -NoSkill           Do not change the Codex skill
  -ReplaceMcp        Replace a conflicting MCP entry (a JSON backup is retained)
  -ReplaceSkill      Back up and replace a conflicting skill path
  -Update            Fast-forward the managed checkout and re-run its new installer
  -Uninstall         Remove only hash/target-matched installer-owned integration
  -Help              Show this help
'@
}

function Invoke-Native([string]$Command, [string[]]$Arguments) {
    & $Command @Arguments
    if ($LASTEXITCODE -ne 0) { throw "$Command exited with code $LASTEXITCODE" }
}

function Get-Receipt {
    if (-not (Test-Path -LiteralPath $ReceiptPath)) { return $null }
    return Get-Content -LiteralPath $ReceiptPath -Raw | ConvertFrom-Json
}

function Get-FileSha([string]$Path) {
    return (Get-FileHash -Algorithm SHA256 -LiteralPath $Path).Hash.ToLowerInvariant()
}

function Get-TextSha([string]$Text) {
    $bytes = [System.Text.Encoding]::UTF8.GetBytes($Text)
    $algorithm = [System.Security.Cryptography.SHA256]::Create()
    try {
        $hash = $algorithm.ComputeHash($bytes)
        return (($hash | ForEach-Object { $_.ToString('x2') }) -join '')
    } finally {
        $algorithm.Dispose()
    }
}

function Get-TreeSha([string]$Root) {
    $resolvedRoot = (Resolve-Path -LiteralPath $Root).Path
    $builder = [System.Text.StringBuilder]::new()
    $files = Get-ChildItem -LiteralPath $resolvedRoot -Recurse -Force -File |
        Sort-Object { [System.IO.Path]::GetRelativePath($resolvedRoot, $_.FullName) }
    foreach ($file in $files) {
        $relative = [System.IO.Path]::GetRelativePath($resolvedRoot, $file.FullName).Replace('\', '/')
        [void]$builder.Append($relative).Append("`n")
        [void]$builder.Append((Get-FileSha $file.FullName)).Append("`n")
    }
    return Get-TextSha $builder.ToString()
}

function Get-McpJson {
    if (-not (Get-Command codex -ErrorAction SilentlyContinue)) { return '' }
    $lines = & codex mcp get session-skein --json 2>$null
    if ($LASTEXITCODE -ne 0 -or -not $lines) { return '' }
    return ($lines -join "`n")
}

function Get-LinkTarget([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return '' }
    $item = Get-Item -LiteralPath $Path -Force
    if ($item.LinkType -ne 'Junction' -and $item.LinkType -ne 'SymbolicLink') { return '' }
    return [string](@($item.Target)[0])
}

function Remove-OwnedIntegration {
    $receipt = Get-Receipt
    if (-not $receipt) { throw "No installer receipt found at $ReceiptPath" }
    $preserved = $false

    if ($receipt.mcpHash) {
        $currentMcp = Get-McpJson
        if ($currentMcp) {
            if ((Get-TextSha $currentMcp) -eq $receipt.mcpHash) {
                Write-Host '→ Removing installer-owned Codex MCP registration'
                Invoke-Native 'codex' @('mcp', 'remove', 'session-skein')
            } else {
                Write-Warning 'Preserving modified session-skein MCP registration.'
                $preserved = $true
            }
        } else {
            Write-Warning 'Could not verify the installer-owned MCP registration; preserving its receipt.'
            $preserved = $true
        }
    }

    if ($receipt.skill) {
        $target = Get-LinkTarget ([string]$receipt.skill)
        if ($target -and $target -eq $receipt.skillSource) {
            Write-Host '→ Removing installer-owned skill link'
            (Get-Item -LiteralPath $receipt.skill -Force).Delete()
            if ($receipt.skillBackup -and (Test-Path -LiteralPath $receipt.skillBackup)) {
                New-Item -ItemType Directory -Path (Split-Path -Parent $receipt.skill) -Force | Out-Null
                Move-Item -LiteralPath $receipt.skillBackup -Destination $receipt.skill
                Write-Host '→ Restored the previous skill path'
            }
        } elseif (Test-Path -LiteralPath $receipt.skill) {
            Write-Warning "Preserving modified skill path $($receipt.skill)."
            $preserved = $true
        } elseif ($receipt.skillBackup -and (Test-Path -LiteralPath $receipt.skillBackup)) {
            New-Item -ItemType Directory -Path (Split-Path -Parent $receipt.skill) -Force | Out-Null
            Move-Item -LiteralPath $receipt.skillBackup -Destination $receipt.skill
            Write-Host '→ Restored the previous skill path'
        }
    }

    $binaryPreserved = $false
    if ($receipt.binary) {
        if ((Test-Path -LiteralPath $receipt.binary) -and
            (Get-FileSha ([string]$receipt.binary)) -eq $receipt.binaryHash) {
            Write-Host "→ Removing installer-owned binary $($receipt.binary)"
            Remove-Item -LiteralPath $receipt.binary -Force
            if ($receipt.binaryBackup -and (Test-Path -LiteralPath $receipt.binaryBackup)) {
                New-Item -ItemType Directory -Path (Split-Path -Parent $receipt.binary) -Force | Out-Null
                Move-Item -LiteralPath $receipt.binaryBackup -Destination $receipt.binary
                Write-Host '→ Restored the previous destination binary'
            }
        } elseif (Test-Path -LiteralPath $receipt.binary) {
            Write-Warning "Preserving modified binary $($receipt.binary)."
            $preserved = $true
            $binaryPreserved = $true
        } elseif ($receipt.binaryBackup -and (Test-Path -LiteralPath $receipt.binaryBackup)) {
            New-Item -ItemType Directory -Path (Split-Path -Parent $receipt.binary) -Force | Out-Null
            Move-Item -LiteralPath $receipt.binaryBackup -Destination $receipt.binary
            Write-Host '→ Restored the previous destination binary'
        }
    }

    if ($receipt.pathAdded -and $receipt.binDir -and -not $binaryPreserved) {
        $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
        $kept = @($userPath -split ';' | Where-Object {
            $_ -and $_.TrimEnd('\') -ine ([string]$receipt.binDir).TrimEnd('\')
        })
        [Environment]::SetEnvironmentVariable('Path', ($kept -join ';'), 'User')
    }

    if (-not $preserved) {
        Remove-Item -LiteralPath $ReceiptPath -Force
        Write-Host "`n✓ Session Skein integration removed."
    } else {
        Write-Warning "Modified paths were preserved; the receipt remains at $ReceiptPath."
    }
    Write-Host 'Private data and the source checkout were preserved.'
    if (Test-Path -LiteralPath $McpBackupPath) {
        Write-Host "A replaced MCP JSON backup remains at $McpBackupPath."
    }
}

if ($Help) { Show-Usage; return }
if ($CatalogOnly -and $Control) { throw 'Choose either -CatalogOnly or -Control.' }
if ($Binary -and $Source) { throw '-Binary and -Source are mutually exclusive.' }
if ($Uninstall) { Remove-OwnedIntegration; return }

$Previous = Get-Receipt
$SourceDir = ''
$SourceCommit = ''
$Reexecuted = $false

function Resolve-Source {
    if ($Source) {
        $candidate = (Resolve-Path -LiteralPath $Source).Path
    } elseif ($PSScriptRoot -and (Test-Path (Join-Path $PSScriptRoot 'Cargo.toml'))) {
        $candidate = $PSScriptRoot
    } else {
        $candidate = $ManagedSource
    }

    if (-not (Test-Path (Join-Path $candidate 'Cargo.toml'))) {
        if (-not (Get-Command git -ErrorAction SilentlyContinue)) { throw 'git is required to obtain Session Skein.' }
        Write-Host "→ Cloning Session Skein into $candidate"
        New-Item -ItemType Directory -Path (Split-Path -Parent $candidate) -Force | Out-Null
        Invoke-Native 'git' @('clone', '--depth', '1', $RepoUrl, $candidate)
    }

    if ($Update -and $env:SKEIN_UPDATE_REEXEC -ne '1') {
        if (-not (Test-Path (Join-Path $candidate '.git'))) { throw '-Update requires a Git checkout.' }
        Write-Host "→ Updating $candidate"
        Invoke-Native 'git' @('-C', $candidate, 'pull', '--ff-only')
        $updatedInstaller = Join-Path $candidate 'install.ps1'
        if (-not (Test-Path $updatedInstaller)) { throw 'Updated checkout has no install.ps1.' }
        $previousReexec = $env:SKEIN_UPDATE_REEXEC
        try {
            $env:SKEIN_UPDATE_REEXEC = '1'
            & $updatedInstaller @OriginalParameters
        } finally {
            $env:SKEIN_UPDATE_REEXEC = $previousReexec
        }
        $script:Reexecuted = $true
        return $candidate
    }

    $skill = Join-Path $candidate 'plugins\session-skein\skills\session-skein\SKILL.md'
    if (-not (Test-Path $skill)) { throw "Source is missing the bundled Session Skein skill: $candidate" }
    return (Resolve-Path -LiteralPath $candidate).Path
}

$NeedSource = (-not $Binary) -or (-not $NoSkill) -or $Update
if ($NeedSource) {
    $SourceDir = Resolve-Source
    if ($Reexecuted) { return }
    if (Test-Path (Join-Path $SourceDir '.git')) {
        $SourceCommit = (& git -C $SourceDir rev-parse HEAD 2>$null | Out-String).Trim()
    }
}

if ($Binary) {
    $BinarySource = (Resolve-Path -LiteralPath $Binary).Path
} else {
    if (-not (Get-Command cargo -ErrorAction SilentlyContinue)) { throw 'Rust 1.95+ is required for source installation.' }
    $rustHost = (& rustc -vV 2>$null | Where-Object { $_ -like 'host: *' } | Select-Object -First 1)
    if ($rustHost -like '*-msvc') {
        if (-not (Get-Command cl.exe -ErrorAction SilentlyContinue) -or
            -not (Get-Command link.exe -ErrorAction SilentlyContinue)) {
            throw 'MSVC Build Tools and the Windows SDK are required. Run this from a Developer PowerShell for Visual Studio.'
        }
    } elseif (-not (Get-Command gcc.exe -ErrorAction SilentlyContinue) -and
              -not (Get-Command clang.exe -ErrorAction SilentlyContinue)) {
        throw 'A native C compiler/linker toolchain is required for source installation.'
    }
    Write-Host '→ Building the locked source checkout'
    Invoke-Native 'cargo' @('build', '--manifest-path', (Join-Path $SourceDir 'Cargo.toml'), '--workspace', '--release', '--locked', '--target-dir', (Join-Path $SourceDir 'target'))
    $BinarySource = Join-Path $SourceDir 'target\release\skein.exe'
}
if (-not (Test-Path -LiteralPath $BinarySource)) { throw "Binary does not exist: $BinarySource" }

$versionOutput = (& $BinarySource --version 2>$null | Out-String).Trim()
if ($versionOutput -notmatch '^skein ([0-9]+\.[0-9]+\.[0-9]+(?:[-+][0-9A-Za-z.-]+)?)$') {
    throw "-Binary did not identify itself as 'skein VERSION'."
}
$ActualVersion = $Matches[1]
$validationDir = Join-Path ([System.IO.Path]::GetTempPath()) ("skein-validate-" + [Guid]::NewGuid())
$oldConfig = $env:SKEIN_CONFIG_DIR
$oldData = $env:SKEIN_DATA_DIR
try {
    $env:SKEIN_CONFIG_DIR = Join-Path $validationDir 'config'
    $env:SKEIN_DATA_DIR = Join-Path $validationDir 'data'
    $doctorText = (& $BinarySource --format json doctor 2>$null | Out-String).Trim()
    $doctor = $doctorText | ConvertFrom-Json
    if ($doctor.version -ne $ActualVersion) { throw 'Candidate binary version and doctor output disagree.' }
} finally {
    $env:SKEIN_CONFIG_DIR = $oldConfig
    $env:SKEIN_DATA_DIR = $oldData
    Remove-Item -LiteralPath $validationDir -Recurse -Force -ErrorAction SilentlyContinue
}

if ($SourceDir -and -not $NoSkill) {
    $plugin = Get-Content -LiteralPath (Join-Path $SourceDir 'plugins\session-skein\.codex-plugin\plugin.json') -Raw | ConvertFrom-Json
    if ($plugin.version -ne $ActualVersion) {
        throw "Binary $ActualVersion and bundled skill/plugin $($plugin.version) do not match."
    }
}
$IncomingHash = Get-FileSha $BinarySource

# Preflight all collisions before changing state.
if ($Previous -and $Previous.binary -and $Previous.binary -ne $InstalledBinary) {
    throw "Receipt owns $($Previous.binary); uninstall before changing -BinDir."
}
$BinaryAction = 'install'
$BinaryBackup = if ($Previous) { [string]$Previous.binaryBackup } else { '' }
if (Test-Path -LiteralPath $InstalledBinary) {
    $currentHash = Get-FileSha $InstalledBinary
    if ($Previous -and $Previous.binary -eq $InstalledBinary -and $Previous.binaryHash -eq $currentHash) {
        if ($currentHash -eq $IncomingHash) {
            $BinaryAction = 'keep-owned'
        } else {
            $BinaryAction = 'replace-owned'
        }
    } elseif ($ReplaceBinary) {
        $BinaryAction = 'backup-replace'
        $BinaryBackup = "$InstalledBinary.backup.$([DateTime]::UtcNow.ToString('yyyyMMddHHmmss'))"
    } else {
        throw "Destination binary is not installer-owned: $InstalledBinary (use -ReplaceBinary)."
    }
}

$SkillTarget = if ($Previous) { [string]$Previous.skill } else { '' }
$SkillSource = if ($Previous) { [string]$Previous.skillSource } else { '' }
$SkillBackup = if ($Previous) { [string]$Previous.skillBackup } else { '' }
$SkillAction = 'none'
if (-not $NoSkill) {
    $desiredSkillOrigin = Join-Path $SourceDir 'plugins\session-skein\skills\session-skein'
    $desiredSkillHash = Get-TreeSha $desiredSkillOrigin
    $desiredSkillSource = Join-Path $SkillSnapshotRoot "$ActualVersion-$desiredSkillHash"
    $desiredSkillTarget = Join-Path $CodexHome 'skills\session-skein'
    if ($Previous -and $Previous.skill -and $Previous.skill -ne $desiredSkillTarget) {
        throw "Receipt owns $($Previous.skill); uninstall before changing CODEX_HOME."
    }
    $SkillTarget = $desiredSkillTarget
    $SkillSource = $desiredSkillSource
    if (-not (Test-Path -LiteralPath $desiredSkillTarget)) {
        $SkillAction = 'create'
    } else {
        $currentTarget = Get-LinkTarget $desiredSkillTarget
        if ($currentTarget -eq $desiredSkillSource) {
            if (-not $Previous -or $Previous.skill -ne $desiredSkillTarget -or $Previous.skillSource -ne $desiredSkillSource) {
                $SkillTarget = ''
                $SkillSource = ''
            }
        } elseif ($Previous -and $Previous.skill -eq $desiredSkillTarget -and
                  $currentTarget -eq $Previous.skillSource) {
            $SkillAction = 'replace-owned'
        } elseif ($ReplaceSkill) {
            $SkillAction = 'backup-create'
            $SkillBackup = "$desiredSkillTarget.backup.$([DateTime]::UtcNow.ToString('yyyyMMddHHmmss'))"
        } else {
            throw "Skill path is not installer-owned: $desiredSkillTarget (use -ReplaceSkill)."
        }
    }
}

$McpProfile = if ($Previous) { [string]$Previous.mcpProfile } else { '' }
$McpHash = if ($Previous) { [string]$Previous.mcpHash } else { '' }
$McpSpecHash = if ($Previous) { [string]$Previous.mcpSpecHash } else { '' }
$McpAction = 'none'
$CurrentMcp = ''
if (-not $NoMcp) {
    if (-not (Get-Command codex -ErrorAction SilentlyContinue)) { throw 'codex CLI is required unless -NoMcp is used.' }
    $desiredMcpSpec = @(
        "command=$InstalledBinary"
        "profile=$Profile"
        "SKEIN_CONFIG_DIR=$($env:SKEIN_CONFIG_DIR)"
        "SKEIN_DATA_DIR=$($env:SKEIN_DATA_DIR)"
        "SKEIN_CODEX_BIN=$($env:SKEIN_CODEX_BIN)"
        "CODEX_HOME=$($env:CODEX_HOME)"
    ) -join "`n"
    $desiredMcpSpecHash = Get-TextSha $desiredMcpSpec
    $CurrentMcp = Get-McpJson
    if ($CurrentMcp) {
        $currentMcpHash = Get-TextSha $CurrentMcp
        if ($Previous -and $Previous.mcpHash -and $Previous.mcpHash -eq $currentMcpHash) {
            if ($Previous.mcpSpecHash -and $Previous.mcpSpecHash -eq $desiredMcpSpecHash) {
                $McpAction = 'none'
            } else {
                $McpAction = 'replace-owned'
            }
        } elseif ($ReplaceMcp) {
            $McpAction = 'backup-replace'
        } else {
            throw 'session-skein MCP registration is not installer-owned. Review it and use -ReplaceMcp.'
        }
    } else {
        $McpAction = 'add'
    }
}

New-Item -ItemType Directory -Path $BinDir, $InstallStateDir -Force | Out-Null
$PathAdded = if ($Previous -and $Previous.binDir -eq $BinDir) { [bool]$Previous.pathAdded } else { $false }
if (-not $NoSkill) {
    if (Test-Path -LiteralPath $desiredSkillSource) {
        if (-not (Get-Item -LiteralPath $desiredSkillSource -Force).PSIsContainer) {
            throw "Skill snapshot path is not a directory: $desiredSkillSource"
        }
        if ((Get-TreeSha $desiredSkillSource) -ne $desiredSkillHash) {
            throw 'Skill snapshot content does not match its content address.'
        }
    } else {
        New-Item -ItemType Directory -Path $SkillSnapshotRoot -Force | Out-Null
        $stagedSkill = Join-Path $SkillSnapshotRoot ('.session-skein.install.' + [Guid]::NewGuid())
        Copy-Item -LiteralPath $desiredSkillOrigin -Destination $stagedSkill -Recurse
        if ((Get-TreeSha $stagedSkill) -ne $desiredSkillHash) {
            Remove-Item -LiteralPath $stagedSkill -Recurse -Force -ErrorAction SilentlyContinue
            throw 'Copied skill snapshot failed content verification.'
        }
        try {
            Move-Item -LiteralPath $stagedSkill -Destination $desiredSkillSource
        } catch {
            Remove-Item -LiteralPath $stagedSkill -Recurse -Force -ErrorAction SilentlyContinue
            throw 'Could not install the immutable skill snapshot.'
        }
    }
}
Remove-Item -LiteralPath $ReceiptRollbackPath, $BinaryRollbackPath, $McpRollbackPath, $McpJsonRollbackPath -Force -ErrorAction SilentlyContinue
$HadReceipt = Test-Path -LiteralPath $ReceiptPath
if ($HadReceipt) { Copy-Item -LiteralPath $ReceiptPath -Destination $ReceiptRollbackPath }

function Restore-BinaryAndReceipt {
    if ($BinaryAction -ne 'keep-owned') {
        Remove-Item -LiteralPath $InstalledBinary -Force -ErrorAction SilentlyContinue
        if ($BinaryAction -eq 'replace-owned' -and (Test-Path -LiteralPath $BinaryRollbackPath)) {
            Move-Item -LiteralPath $BinaryRollbackPath -Destination $InstalledBinary
        } elseif ($BinaryAction -eq 'backup-replace' -and (Test-Path -LiteralPath $BinaryBackup)) {
            Move-Item -LiteralPath $BinaryBackup -Destination $InstalledBinary
        }
    }
    if ($HadReceipt -and (Test-Path -LiteralPath $ReceiptRollbackPath)) {
        Move-Item -LiteralPath $ReceiptRollbackPath -Destination $ReceiptPath -Force
    } else {
        Remove-Item -LiteralPath $ReceiptPath -Force -ErrorAction SilentlyContinue
    }
}

if ($BinaryAction -eq 'keep-owned') {
    $InstalledHash = $currentHash
    Write-Host "→ $InstalledBinary is already the requested build"
} else {
    $staged = Join-Path $BinDir (".skein.install." + [Guid]::NewGuid() + '.exe')
    Copy-Item -LiteralPath $BinarySource -Destination $staged
    try {
        $stagedVersion = (& $staged --version 2>$null | Out-String).Trim()
        if ($stagedVersion -ne "skein $ActualVersion") { throw 'Staged binary identity changed.' }
    } catch {
        Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
        throw 'Staged skein.exe failed identity validation.'
    }
    if ($BinaryAction -eq 'replace-owned') {
        Copy-Item -LiteralPath $InstalledBinary -Destination $BinaryRollbackPath
    } elseif ($BinaryAction -eq 'backup-replace') {
        Move-Item -LiteralPath $InstalledBinary -Destination $BinaryBackup
        Write-Host "→ Backed up existing binary to $BinaryBackup"
    }
    try {
        Move-Item -LiteralPath $staged -Destination $InstalledBinary -Force
    } catch {
        Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue
        Restore-BinaryAndReceipt
        throw 'Could not replace skein.exe. Close running Codex/Skein processes and retry.'
    }
    $InstalledHash = Get-FileSha $InstalledBinary
    Write-Host "→ Installed $InstalledBinary"
}

function Write-Receipt {
    $receipt = [ordered]@{
        version = $ActualVersion
        binary = $InstalledBinary
        binaryHash = $InstalledHash
        binaryBackup = $BinaryBackup
        binDir = $BinDir
        pathAdded = $PathAdded
        source = $SourceDir
        sourceCommit = $SourceCommit
        skill = $SkillTarget
        skillSource = $SkillSource
        skillBackup = $SkillBackup
        mcpProfile = $McpProfile
        mcpHash = $McpHash
        mcpSpecHash = $McpSpecHash
    }
    [System.IO.File]::WriteAllText($ReceiptPath, ($receipt | ConvertTo-Json -Depth 3))
}

# Provisional receipt makes later failures recoverable with -Uninstall.
try {
    Write-Receipt
} catch {
    Restore-BinaryAndReceipt
    throw 'Could not write the provisional installer receipt; the previous binary was restored.'
}

try {
    Invoke-Native $InstalledBinary @('init')
} catch {
    Restore-BinaryAndReceipt
    throw "Session Skein initialization failed; the previous binary and receipt were restored. $($_.Exception.Message)"
}

$skillSwitched = $false
$oldSkillSource = ''
try {
    if ($SkillAction -eq 'backup-create') {
        New-Item -ItemType Directory -Path (Split-Path -Parent $desiredSkillTarget) -Force | Out-Null
        Move-Item -LiteralPath $desiredSkillTarget -Destination $SkillBackup
        Write-Host "→ Backed up existing skill to $SkillBackup"
        try {
            New-Item -ItemType Junction -Path $desiredSkillTarget -Target $desiredSkillSource | Out-Null
            $skillSwitched = $true
        } catch {
            Move-Item -LiteralPath $SkillBackup -Destination $desiredSkillTarget
            throw
        }
    } elseif ($SkillAction -eq 'replace-owned') {
        $oldSkillSource = Get-LinkTarget $desiredSkillTarget
        (Get-Item -LiteralPath $desiredSkillTarget -Force).Delete()
        try {
            New-Item -ItemType Junction -Path $desiredSkillTarget -Target $desiredSkillSource | Out-Null
            $skillSwitched = $true
        } catch {
            New-Item -ItemType Junction -Path $desiredSkillTarget -Target $oldSkillSource -ErrorAction SilentlyContinue | Out-Null
            throw 'Could not switch the Codex skill snapshot.'
        }
    } elseif ($SkillAction -eq 'create') {
        New-Item -ItemType Directory -Path (Split-Path -Parent $desiredSkillTarget) -Force | Out-Null
        New-Item -ItemType Junction -Path $desiredSkillTarget -Target $desiredSkillSource | Out-Null
        $skillSwitched = $true
    }
    if ($SkillAction -ne 'none') {
        Write-Host "→ Installed Codex skill $desiredSkillTarget"
        Write-Receipt
    }
} catch {
    if ($skillSwitched) {
        (Get-Item -LiteralPath $desiredSkillTarget -Force).Delete()
        if ($SkillAction -eq 'replace-owned' -and $oldSkillSource) {
            New-Item -ItemType Junction -Path $desiredSkillTarget -Target $oldSkillSource -ErrorAction SilentlyContinue | Out-Null
        } elseif ($SkillAction -eq 'backup-create' -and (Test-Path -LiteralPath $SkillBackup)) {
            Move-Item -LiteralPath $SkillBackup -Destination $desiredSkillTarget -ErrorAction SilentlyContinue
        }
    }
    Restore-BinaryAndReceipt
    throw "Codex skill installation failed; the previous binary and receipt were restored. $($_.Exception.Message)"
}
Remove-Item -LiteralPath $BinaryRollbackPath, $ReceiptRollbackPath -Force -ErrorAction SilentlyContinue

$userPathParts = @([Environment]::GetEnvironmentVariable('Path', 'User') -split ';' | Where-Object { $_ })
if (-not ($userPathParts | Where-Object { $_.TrimEnd('\') -ieq $BinDir.TrimEnd('\') })) {
    [Environment]::SetEnvironmentVariable('Path', ((@($userPathParts) + $BinDir) -join ';'), 'User')
    $PathAdded = $true
    Write-Receipt
}
if (-not (($env:Path -split ';') | Where-Object { $_.TrimEnd('\') -ieq $BinDir.TrimEnd('\') })) {
    $env:Path = "$BinDir;$env:Path"
}

if ($McpAction -ne 'none') {
    $mcpConfigExisted = Test-Path -LiteralPath $CodexConfigPath
    if ($mcpConfigExisted) {
        Copy-Item -LiteralPath $CodexConfigPath -Destination $McpRollbackPath
    }
    if ($CurrentMcp) {
        [System.IO.File]::WriteAllText($McpJsonRollbackPath, $CurrentMcp)
    }
    if ($McpAction -eq 'backup-replace') {
        [System.IO.File]::WriteAllText($McpBackupPath, $CurrentMcp)
        Write-Host "→ Backed up previous MCP JSON to $McpBackupPath"
    }

    function Restore-CodexConfig {
        if ($mcpConfigExisted -and (Test-Path -LiteralPath $McpRollbackPath)) {
            New-Item -ItemType Directory -Path (Split-Path -Parent $CodexConfigPath) -Force | Out-Null
            Copy-Item -LiteralPath $McpRollbackPath -Destination $CodexConfigPath -Force
        } else {
            Remove-Item -LiteralPath $CodexConfigPath -Force -ErrorAction SilentlyContinue
        }
    }

    $mcpArgs = @('mcp', 'add', 'session-skein')
    if ($env:SKEIN_CONFIG_DIR) { $mcpArgs += @('--env', "SKEIN_CONFIG_DIR=$($env:SKEIN_CONFIG_DIR)") }
    if ($env:SKEIN_DATA_DIR) { $mcpArgs += @('--env', "SKEIN_DATA_DIR=$($env:SKEIN_DATA_DIR)") }
    if ($env:SKEIN_CODEX_BIN) { $mcpArgs += @('--env', "SKEIN_CODEX_BIN=$($env:SKEIN_CODEX_BIN)") }
    if ($env:CODEX_HOME) { $mcpArgs += @('--env', "CODEX_HOME=$($env:CODEX_HOME)") }
    $mcpArgs += @('--', $InstalledBinary, 'mcp')
    if ($Profile -eq 'control') { $mcpArgs += '--allow-control' }
    try {
        Invoke-Native 'codex' $mcpArgs
        $configuredMcp = Get-McpJson
        if (-not $configuredMcp) { throw 'Codex did not return the configured MCP server.' }
    } catch {
        Restore-CodexConfig
        Remove-Item -LiteralPath $McpRollbackPath, $McpJsonRollbackPath -Force -ErrorAction SilentlyContinue
        throw "Codex MCP registration failed; the previous Codex configuration was restored. $($_.Exception.Message)"
    }
    $McpProfile = $Profile
    $McpHash = Get-TextSha $configuredMcp
    $McpSpecHash = $desiredMcpSpecHash
    Write-Receipt
    Remove-Item -LiteralPath $McpRollbackPath, $McpJsonRollbackPath -Force -ErrorAction SilentlyContinue
    Write-Host "→ Registered the $Profile Session Skein MCP profile"
}

Write-Host "`n✓ Session Skein is installed."
Invoke-Native $InstalledBinary @('--version')
Invoke-Native $InstalledBinary @('doctor')
if (-not $NoMcp) { Write-Output (Get-McpJson) }
Write-Host "`nStart a new Codex session so it discovers the skill and MCP server."
Write-Host 'No scan root, private context source, daemon, or worker was enabled.'
