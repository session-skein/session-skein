param(
    [Parameter(Mandatory = $true)]
    [string]$Binary
)

$ErrorActionPreference = 'Stop'
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$Binary = (Resolve-Path $Binary).Path
$Root = Join-Path ([System.IO.Path]::GetTempPath()) ("skein-installer-tests-" + [Guid]::NewGuid())
$OriginalUserPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$OriginalProcessPath = $env:Path

function Set-CaseEnvironment([string]$CaseRoot) {
    $env:LOCALAPPDATA = $CaseRoot
    $env:CODEX_HOME = Join-Path $CaseRoot 'codex'
    $env:SKEIN_DATA_DIR = Join-Path $CaseRoot 'data'
    $env:SKEIN_CONFIG_DIR = Join-Path $CaseRoot 'config'
    $env:FAKE_CODEX_STATE = Join-Path $CaseRoot 'codex\config.toml'
    Remove-Item Env:FAKE_CODEX_FAIL_GET, Env:FAKE_CODEX_FAIL_ADD, Env:FAKE_CODEX_FAIL_VERIFY -ErrorAction SilentlyContinue
    $env:Path = (Join-Path $RepoRoot 'tests\fixtures\fake-codex-windows') + ';' + $OriginalProcessPath
}

function Invoke-Installer(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$Arguments
) {
    $parameters = @{}
    for ($index = 0; $index -lt $Arguments.Count; $index++) {
        $option = [string]$Arguments[$index]
        if (-not $option.StartsWith('-')) { throw "Invalid installer test option: $option" }
        $name = $option.TrimStart('-')
        if ($index + 1 -lt $Arguments.Count -and -not ([string]$Arguments[$index + 1]).StartsWith('-')) {
            $parameters[$name] = $Arguments[$index + 1]
            $index++
        } else {
            $parameters[$name] = $true
        }
    }
    & (Join-Path $RepoRoot 'install.ps1') @parameters
}

function Invoke-Git(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$Arguments
) {
    & git @Arguments | Out-Null
    if ($LASTEXITCODE -ne 0) { throw "git exited with code $LASTEXITCODE" }
}

try {
    New-Item -ItemType Directory -Path $Root | Out-Null

    # Clean control install, repeat with integration disabled, then uninstall. The
    # second pass must retain PATH ownership from the first.
    $clean = Join-Path $Root 'clean'
    Set-CaseEnvironment $clean
    $cleanBin = Join-Path $clean 'bin'
    Invoke-Installer @('-Binary', $Binary, '-BinDir', $cleanBin, '-Control') | Out-Null
    Invoke-Installer @('-Binary', $Binary, '-BinDir', $cleanBin, '-NoSkill', '-NoMcp') | Out-Null
    Invoke-Installer @('-Uninstall') | Out-Null
    if (Test-Path (Join-Path $cleanBin 'skein.exe')) { throw 'clean binary was not removed' }
    if (Test-Path (Join-Path $clean 'codex\skills\session-skein')) { throw 'clean skill was not removed' }
    if (Test-Path (Join-Path $clean 'codex\config.toml')) { throw 'clean MCP was not removed' }
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (@($userPath -split ';') -contains $cleanBin) { throw 'installer-added PATH entry was not removed' }

    # Unowned binary refusal and explicit backup/restore.
    $collision = Join-Path $Root 'binary-collision'
    Set-CaseEnvironment $collision
    $collisionBin = Join-Path $collision 'bin'
    New-Item -ItemType Directory -Path $collisionBin -Force | Out-Null
    Copy-Item "$env:WINDIR\System32\cmd.exe" (Join-Path $collisionBin 'skein.exe')
    $oldHash = (Get-FileHash (Join-Path $collisionBin 'skein.exe') -Algorithm SHA256).Hash
    try {
        Invoke-Installer @('-Binary', $Binary, '-BinDir', $collisionBin, '-NoMcp') | Out-Null
        throw 'unowned binary collision unexpectedly succeeded'
    } catch {
        if ($_.Exception.Message -eq 'unowned binary collision unexpectedly succeeded') { throw }
    }
    if (Test-Path (Join-Path $collision 'codex\skills\session-skein')) { throw 'skill changed before collision refusal' }
    Invoke-Installer @('-Binary', $Binary, '-BinDir', $collisionBin, '-NoMcp', '-ReplaceBinary') | Out-Null
    Invoke-Installer @('-Uninstall') | Out-Null
    if ((Get-FileHash (Join-Path $collisionBin 'skein.exe') -Algorithm SHA256).Hash -ne $oldHash) {
        throw 'previous binary was not restored'
    }

    # Generic executable identity rejection.
    $identity = Join-Path $Root 'identity'
    Set-CaseEnvironment $identity
    try {
        Invoke-Installer @('-Binary', "$env:WINDIR\System32\cmd.exe", '-BinDir', (Join-Path $identity 'bin'), '-NoSkill', '-NoMcp') | Out-Null
        throw 'non-Skein binary unexpectedly succeeded'
    } catch {
        if ($_.Exception.Message -eq 'non-Skein binary unexpectedly succeeded') { throw }
    }

    # Skill collision is preflighted before binary mutation.
    $skillCollision = Join-Path $Root 'skill-collision'
    Set-CaseEnvironment $skillCollision
    $skillPath = Join-Path $skillCollision 'codex\skills\session-skein'
    New-Item -ItemType Directory -Path $skillPath -Force | Out-Null
    New-Item -ItemType File -Path (Join-Path $skillPath 'user-owned') | Out-Null
    try {
        Invoke-Installer @('-Binary', $Binary, '-BinDir', (Join-Path $skillCollision 'bin'), '-NoMcp') | Out-Null
        throw 'unowned skill collision unexpectedly succeeded'
    } catch {
        if ($_.Exception.Message -eq 'unowned skill collision unexpectedly succeeded') { throw }
    }
    if (Test-Path (Join-Path $skillCollision 'bin\skein.exe')) { throw 'binary changed before skill collision refusal' }

    # MCP collision, explicit backup/replace, and owned uninstall.
    $mcpCollision = Join-Path $Root 'mcp-collision'
    Set-CaseEnvironment $mcpCollision
    New-Item -ItemType Directory -Path (Join-Path $mcpCollision 'codex') -Force | Out-Null
    [System.IO.File]::WriteAllText($env:FAKE_CODEX_STATE, '{"name":"session-skein","transport":{"command":"other","args":[]}}')
    try {
        Invoke-Installer @('-Binary', $Binary, '-BinDir', (Join-Path $mcpCollision 'bin'), '-NoSkill') | Out-Null
        throw 'unowned MCP collision unexpectedly succeeded'
    } catch {
        if ($_.Exception.Message -eq 'unowned MCP collision unexpectedly succeeded') { throw }
    }
    if (Test-Path (Join-Path $mcpCollision 'bin\skein.exe')) { throw 'binary changed before MCP collision refusal' }
    Invoke-Installer @('-Binary', $Binary, '-BinDir', (Join-Path $mcpCollision 'bin'), '-NoSkill', '-ReplaceMcp', '-Control') | Out-Null
    if (-not (Test-Path (Join-Path $mcpCollision 'SessionSkein\install\replaced-mcp.json'))) { throw 'MCP backup missing' }
    Invoke-Installer @('-Uninstall') | Out-Null

    # A fresh initialization failure rolls back its binary and receipt.
    $partial = Join-Path $Root 'partial'
    Set-CaseEnvironment $partial
    $blocked = Join-Path $partial 'blocked'
    New-Item -ItemType Directory -Path $partial -Force | Out-Null
    New-Item -ItemType File -Path $blocked | Out-Null
    $env:SKEIN_DATA_DIR = Join-Path $blocked 'child'
    try {
        Invoke-Installer @('-Binary', $Binary, '-BinDir', (Join-Path $partial 'bin'), '-NoSkill', '-NoMcp') | Out-Null
        throw 'forced init failure unexpectedly succeeded'
    } catch {
        if ($_.Exception.Message -eq 'forced init failure unexpectedly succeeded') { throw }
    }
    if (Test-Path (Join-Path $partial 'SessionSkein\install\receipt.json')) { throw 'failed install receipt was not rolled back' }
    if (Test-Path (Join-Path $partial 'bin\skein.exe')) { throw 'failed install binary was not rolled back' }

    # An owned update retains the previous executable and receipt until real-state
    # initialization succeeds.
    $rollback = Join-Path $Root 'rollback'
    Set-CaseEnvironment $rollback
    $rollbackCandidate = Join-Path $rollback 'old-skein.exe'
    New-Item -ItemType Directory -Path $rollback -Force | Out-Null
    Copy-Item -LiteralPath $Binary -Destination $rollbackCandidate
    $stream = [System.IO.File]::Open($rollbackCandidate, [System.IO.FileMode]::Append, [System.IO.FileAccess]::Write)
    try { $stream.WriteByte(0) } finally { $stream.Dispose() }
    $rollbackBin = Join-Path $rollback 'bin'
    Invoke-Installer @('-Binary', $rollbackCandidate, '-BinDir', $rollbackBin, '-NoSkill', '-NoMcp') | Out-Null
    $oldInstalledHash = (Get-FileHash (Join-Path $rollbackBin 'skein.exe') -Algorithm SHA256).Hash
    $rollbackReceipt = Join-Path $rollback 'SessionSkein\install\receipt.json'
    $oldReceiptHash = (Get-FileHash $rollbackReceipt -Algorithm SHA256).Hash
    $blocked = Join-Path $rollback 'blocked'
    New-Item -ItemType File -Path $blocked | Out-Null
    $env:SKEIN_DATA_DIR = Join-Path $blocked 'child'
    try {
        Invoke-Installer @('-Binary', $Binary, '-BinDir', $rollbackBin, '-NoSkill', '-NoMcp') | Out-Null
        throw 'owned update with forced init failure unexpectedly succeeded'
    } catch {
        if ($_.Exception.Message -eq 'owned update with forced init failure unexpectedly succeeded') { throw }
    }
    if ((Get-FileHash (Join-Path $rollbackBin 'skein.exe') -Algorithm SHA256).Hash -ne $oldInstalledHash) {
        throw 'owned binary was not restored after initialization failure'
    }
    if ((Get-FileHash $rollbackReceipt -Algorithm SHA256).Hash -ne $oldReceiptHash) {
        throw 'owned receipt was not restored after initialization failure'
    }
    Set-CaseEnvironment $rollback
    Invoke-Installer @('-Uninstall') | Out-Null

    # Reinstalling against another Codex home is refused before the old skill is
    # orphaned.
    $changedHome = Join-Path $Root 'changed-home'
    Set-CaseEnvironment $changedHome
    $changedHomeBin = Join-Path $changedHome 'bin'
    Invoke-Installer @('-Binary', $Binary, '-BinDir', $changedHomeBin, '-NoMcp') | Out-Null
    $env:CODEX_HOME = Join-Path $changedHome 'other-codex'
    try {
        Invoke-Installer @('-Binary', $Binary, '-BinDir', $changedHomeBin, '-NoMcp') | Out-Null
        throw 'CODEX_HOME change unexpectedly succeeded'
    } catch {
        if ($_.Exception.Message -eq 'CODEX_HOME change unexpectedly succeeded') { throw }
    }
    if (-not (Test-Path (Join-Path $changedHome 'codex\skills\session-skein'))) { throw 'original skill was orphaned' }
    if (Test-Path (Join-Path $changedHome 'other-codex\skills\session-skein')) { throw 'new Codex home was mutated' }
    Set-CaseEnvironment $changedHome
    Invoke-Installer @('-Uninstall') | Out-Null

    # Missing installed replacements still restore their user-owned backups.
    $missing = Join-Path $Root 'missing-replacements'
    Set-CaseEnvironment $missing
    $missingBin = Join-Path $missing 'bin'
    $missingSkill = Join-Path $missing 'codex\skills\session-skein'
    New-Item -ItemType Directory -Path $missingBin, $missingSkill -Force | Out-Null
    Copy-Item "$env:WINDIR\System32\cmd.exe" (Join-Path $missingBin 'skein.exe')
    New-Item -ItemType File -Path (Join-Path $missingSkill 'user-owned') | Out-Null
    $missingHash = (Get-FileHash (Join-Path $missingBin 'skein.exe') -Algorithm SHA256).Hash
    Invoke-Installer @('-Binary', $Binary, '-BinDir', $missingBin, '-NoMcp', '-ReplaceBinary', '-ReplaceSkill') | Out-Null
    Remove-Item -LiteralPath (Join-Path $missingBin 'skein.exe') -Force
    (Get-Item -LiteralPath $missingSkill -Force).Delete()
    Invoke-Installer @('-Uninstall') | Out-Null
    if ((Get-FileHash (Join-Path $missingBin 'skein.exe') -Algorithm SHA256).Hash -ne $missingHash) {
        throw 'missing binary replacement backup was not restored'
    }
    if (-not (Test-Path (Join-Path $missingSkill 'user-owned'))) { throw 'missing skill replacement backup was not restored' }

    # MCP writes roll back if Codex fails after changing its config or cannot
    # verify the new entry.
    $mcpFailure = Join-Path $Root 'mcp-failure'
    Set-CaseEnvironment $mcpFailure
    New-Item -ItemType Directory -Path (Split-Path -Parent $env:FAKE_CODEX_STATE) -Force | Out-Null
    [System.IO.File]::WriteAllText($env:FAKE_CODEX_STATE, '{"name":"session-skein","transport":{"command":"original","args":[]}}')
    $originalMcpHash = (Get-FileHash $env:FAKE_CODEX_STATE -Algorithm SHA256).Hash
    $env:FAKE_CODEX_FAIL_ADD = 'after'
    try {
        Invoke-Installer @('-Binary', $Binary, '-BinDir', (Join-Path $mcpFailure 'bin'), '-NoSkill', '-ReplaceMcp') | Out-Null
        throw 'MCP add failure unexpectedly succeeded'
    } catch {
        if ($_.Exception.Message -eq 'MCP add failure unexpectedly succeeded') { throw }
    } finally {
        Remove-Item Env:FAKE_CODEX_FAIL_ADD -ErrorAction SilentlyContinue
    }
    if ((Get-FileHash $env:FAKE_CODEX_STATE -Algorithm SHA256).Hash -ne $originalMcpHash) {
        throw 'previous MCP config was not restored after add failure'
    }
    if (Test-Path (Join-Path $mcpFailure 'bin\skein.exe')) { throw 'fresh binary survived MCP add rollback' }
    if (Test-Path (Join-Path $mcpFailure 'SessionSkein\install\receipt.json')) { throw 'fresh receipt survived MCP add rollback' }

    $mcpVerifyFailure = Join-Path $Root 'mcp-verify-failure'
    Set-CaseEnvironment $mcpVerifyFailure
    $env:FAKE_CODEX_FAIL_VERIFY = '1'
    try {
        Invoke-Installer @('-Binary', $Binary, '-BinDir', (Join-Path $mcpVerifyFailure 'bin'), '-NoSkill') | Out-Null
        throw 'MCP verification failure unexpectedly succeeded'
    } catch {
        if ($_.Exception.Message -eq 'MCP verification failure unexpectedly succeeded') { throw }
    } finally {
        Remove-Item Env:FAKE_CODEX_FAIL_VERIFY -ErrorAction SilentlyContinue
    }
    if (Test-Path $env:FAKE_CODEX_STATE) { throw 'fresh MCP config was not rolled back after verification failure' }
    if (Test-Path (Join-Path $mcpVerifyFailure 'bin\skein.exe')) { throw 'fresh binary survived MCP verification rollback' }
    if (Test-Path (Join-Path $mcpVerifyFailure 'SessionSkein\install\receipt.json')) { throw 'fresh receipt survived MCP verification rollback' }

    # Uninstall keeps ownership when Codex cannot answer authoritatively.
    $unqueryable = Join-Path $Root 'unqueryable-mcp'
    Set-CaseEnvironment $unqueryable
    Invoke-Installer @('-Binary', $Binary, '-BinDir', (Join-Path $unqueryable 'bin'), '-NoSkill') | Out-Null
    $env:FAKE_CODEX_FAIL_GET = '1'
    Invoke-Installer @('-Uninstall') | Out-Null
    Remove-Item Env:FAKE_CODEX_FAIL_GET -ErrorAction SilentlyContinue
    if (-not (Test-Path $env:FAKE_CODEX_STATE)) { throw 'unqueryable MCP config was removed' }
    if (-not (Test-Path (Join-Path $unqueryable 'SessionSkein\install\receipt.json'))) { throw 'unqueryable MCP receipt was removed' }

    # A real Git update that fails in the refreshed installer cannot change the
    # live content-addressed skill snapshot, and the re-exec sentinel is restored.
    $updateCase = Join-Path $Root 'update-failure'
    $updateFixture = Join-Path $Root 'update-fixture'
    $updateWork = Join-Path $updateFixture 'work'
    $updateRemote = Join-Path $updateFixture 'remote.git'
    $updateManaged = Join-Path $updateFixture 'managed'
    New-Item -ItemType Directory -Path (Join-Path $updateWork 'plugins\session-skein') -Force | Out-Null
    Invoke-Git @('init', '--bare', $updateRemote)
    Invoke-Git @('init', '-b', 'main', $updateWork)
    Copy-Item -LiteralPath (Join-Path $RepoRoot 'install.ps1'), (Join-Path $RepoRoot 'Cargo.toml') -Destination $updateWork
    Copy-Item -LiteralPath (Join-Path $RepoRoot 'plugins\session-skein\.codex-plugin'), (Join-Path $RepoRoot 'plugins\session-skein\skills') -Destination (Join-Path $updateWork 'plugins\session-skein') -Recurse
    Invoke-Git @('-C', $updateWork, 'add', '.')
    Invoke-Git @('-C', $updateWork, '-c', 'user.name=Installer Test', '-c', 'user.email=installer@example.invalid', 'commit', '-m', 'initial')
    Invoke-Git @('-C', $updateWork, 'remote', 'add', 'origin', $updateRemote)
    Invoke-Git @('-C', $updateWork, 'push', '-u', 'origin', 'main')
    Invoke-Git @('--git-dir', $updateRemote, 'symbolic-ref', 'HEAD', 'refs/heads/main')
    Invoke-Git @('clone', $updateRemote, $updateManaged)
    Set-CaseEnvironment $updateCase
    $updateBin = Join-Path $updateCase 'bin'
    & (Join-Path $updateManaged 'install.ps1') -Binary $Binary -BinDir $updateBin -NoMcp | Out-Null
    $updateReceipt = Join-Path $updateCase 'SessionSkein\install\receipt.json'
    $updateReceiptHash = (Get-FileHash $updateReceipt -Algorithm SHA256).Hash
    $updateSkill = Join-Path $updateCase 'codex\skills\session-skein'
    $updateSkillTarget = [string](@((Get-Item -LiteralPath $updateSkill -Force).Target)[0])
    Add-Content -LiteralPath (Join-Path $updateWork 'plugins\session-skein\skills\session-skein\SKILL.md') -Value "`nupdate-marker-must-not-go-live"
    [System.IO.File]::WriteAllText((Join-Path $updateWork 'install.ps1'), "throw 'forced updated installer failure'`n")
    Invoke-Git @('-C', $updateWork, 'add', '.')
    Invoke-Git @('-C', $updateWork, '-c', 'user.name=Installer Test', '-c', 'user.email=installer@example.invalid', 'commit', '-m', 'failing-update')
    Invoke-Git @('-C', $updateWork, 'push')
    $previousReexec = $env:SKEIN_UPDATE_REEXEC
    try {
        & (Join-Path $updateManaged 'install.ps1') -Binary $Binary -BinDir $updateBin -NoMcp -Update | Out-Null
        throw 'failing Git update unexpectedly succeeded'
    } catch {
        if ($_.Exception.Message -eq 'failing Git update unexpectedly succeeded') { throw }
    }
    if ($env:SKEIN_UPDATE_REEXEC -ne $previousReexec) { throw 'update re-exec sentinel leaked into the caller' }
    if ((Get-FileHash $updateReceipt -Algorithm SHA256).Hash -ne $updateReceiptHash) { throw 'failed update changed the receipt' }
    $currentUpdateSkillTarget = [string](@((Get-Item -LiteralPath $updateSkill -Force).Target)[0])
    if ($currentUpdateSkillTarget -ne $updateSkillTarget) { throw 'failed update switched the active skill snapshot' }
    if ((Get-Content -LiteralPath (Join-Path $updateSkill 'SKILL.md') -Raw) -match 'update-marker-must-not-go-live') {
        throw 'failed update changed the active skill content'
    }
    Set-CaseEnvironment $updateCase
    Invoke-Installer @('-Uninstall') | Out-Null

    # A user-replaced executable survives uninstall and keeps its receipt.
    $modified = Join-Path $Root 'modified'
    Set-CaseEnvironment $modified
    $modifiedBin = Join-Path $modified 'bin'
    Invoke-Installer @('-Binary', $Binary, '-BinDir', $modifiedBin, '-NoSkill', '-NoMcp') | Out-Null
    Copy-Item "$env:WINDIR\System32\cmd.exe" (Join-Path $modifiedBin 'skein.exe') -Force
    Invoke-Installer @('-Uninstall') | Out-Null
    if (-not (Test-Path (Join-Path $modifiedBin 'skein.exe'))) { throw 'modified binary was deleted' }
    if (-not (Test-Path (Join-Path $modified 'SessionSkein\install\receipt.json'))) { throw 'modified receipt was removed' }
    $userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
    if (-not (@($userPath -split ';') | Where-Object { $_.TrimEnd('\') -ieq $modifiedBin.TrimEnd('\') })) {
        throw 'PATH ownership was removed while a modified binary was preserved'
    }

    Write-Host 'Windows installer lifecycle and collision tests passed.'
} finally {
    [Environment]::SetEnvironmentVariable('Path', $OriginalUserPath, 'User')
    $env:Path = $OriginalProcessPath
    Remove-Item -LiteralPath $Root -Recurse -Force -ErrorAction SilentlyContinue
}
