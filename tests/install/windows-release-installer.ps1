param(
    [Parameter(Mandatory = $true)]
    [string]$Binary
)

$ErrorActionPreference = 'Stop'
$RepoRoot = (Resolve-Path (Join-Path $PSScriptRoot '..\..')).Path
$Binary = (Resolve-Path $Binary).Path
$Version = ((& $Binary --version) -replace '^skein ', '').Trim()
$Target = 'x86_64-pc-windows-msvc'
$Tag = "v$Version"
$Archive = "session-skein-$Tag-$Target.zip"
$Root = Join-Path ([System.IO.Path]::GetTempPath()) ('skein-release-tests-' + [Guid]::NewGuid())
$WebRoot = Join-Path $Root 'web'
$AssetDir = Join-Path $WebRoot "releases\$Tag"
$ChannelDir = Join-Path $WebRoot 'channels'
$Server = $null
$OriginalPath = [Environment]::GetEnvironmentVariable('Path', 'User')

try {
    New-Item -ItemType Directory -Path $AssetDir, $ChannelDir -Force | Out-Null
    [System.IO.File]::WriteAllText((Join-Path $ChannelDir 'preview'), "$Version`n")
    & python (Join-Path $RepoRoot 'scripts\release.py') package --binary $Binary --target $Target --output $AssetDir | Out-Null
    if ($LASTEXITCODE -ne 0) { throw 'release packager failed' }
    $Hash = (Get-FileHash (Join-Path $AssetDir $Archive) -Algorithm SHA256).Hash.ToLowerInvariant()
    $Manifest = [ordered]@{
        schemaVersion = 1
        name = 'session-skein'
        version = $Version
        tag = $Tag
        assets = @([ordered]@{ name = $Archive; target = $Target; sha256 = $Hash })
    }
    [System.IO.File]::WriteAllText((Join-Path $AssetDir 'release-manifest.json'), ($Manifest | ConvertTo-Json -Depth 5))
    [System.IO.File]::WriteAllText((Join-Path $AssetDir 'SHA256SUMS'), "$Hash  $Archive`n")

    $Listener = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    $Listener.Start()
    $Port = ([System.Net.IPEndPoint]$Listener.LocalEndpoint).Port
    $Listener.Stop()
    $Server = Start-Process python -ArgumentList @('-m', 'http.server', $Port, '--bind', '127.0.0.1', '--directory', $WebRoot) -PassThru -WindowStyle Hidden
    Start-Sleep -Milliseconds 500

    $Case = Join-Path $Root 'case'
    $env:LOCALAPPDATA = $Case
    $env:CODEX_HOME = Join-Path $Case 'codex'
    $env:SKEIN_DATA_DIR = Join-Path $Case 'data'
    $env:SKEIN_CONFIG_DIR = Join-Path $Case 'config'
    $env:SKEIN_RELEASE_BASE_URL = "http://127.0.0.1:$Port/releases"
    $env:SKEIN_RELEASE_CHANNEL_URL = "http://127.0.0.1:$Port/channels"
    $env:SKEIN_ALLOW_INSECURE_TEST_DOWNLOADS = '1'
    $env:SKEIN_ALLOW_RELEASE_OVERRIDE = '1'
    $BinDir = Join-Path $Case 'bin'
    & (Join-Path $RepoRoot 'install.ps1') -BinDir $BinDir -NoMcp | Out-Null
    if ((& (Join-Path $BinDir 'skein.exe') --version) -ne "skein $Version") { throw 'remote binary version mismatch' }
    $Receipt = Get-Content (Join-Path $Case 'SessionSkein\install\receipt.json') -Raw | ConvertFrom-Json
    if ($Receipt.source -ne "release:${Tag}:$Target") { throw 'release provenance missing from receipt' }
    if (-not (Test-Path (Join-Path $Case 'codex\skills\session-skein\SKILL.md'))) { throw 'release skill was not installed' }
    if (-not (Test-Path (Join-Path $Case 'SessionSkein\install\installer.ps1'))) { throw 'updater installer snapshot missing' }

    # A mutable receipt cannot impersonate an older running binary.
    $ReceiptPath = Join-Path $Case 'SessionSkein\install\receipt.json'
    $GoodReceipt = Get-Content $ReceiptPath -Raw
    $Receipt.version = '0.5.0-alpha.8'
    [System.IO.File]::WriteAllText($ReceiptPath, ($Receipt | ConvertTo-Json -Depth 4))
    & (Join-Path $BinDir 'skein.exe') update --check 2>$null | Out-Null
    if ($LASTEXITCODE -eq 0) { throw 'modified receipt version unexpectedly passed' }
    [System.IO.File]::WriteAllText($ReceiptPath, $GoodReceipt)
    $Check = & (Join-Path $BinDir 'skein.exe') update --check --json | ConvertFrom-Json
    if ($Check.status -ne 'current' -or $Check.currentVersion -ne $Version) { throw 'Windows same-version check was inaccurate' }

    # Exercise the real parent-exit/file-lock handoff without falsifying versions.
    $ReceiptPublishedAt = (Get-Item -LiteralPath $ReceiptPath).LastWriteTimeUtc
    $HelperResult = Join-Path $Case 'SessionSkein\install\update-helper.result.json'
    $Scheduled = & (Join-Path $BinDir 'skein.exe') update --force --json | ConvertFrom-Json
    if ($Scheduled.status -ne 'scheduled' -or -not $Scheduled.scheduled) {
        throw 'Windows forced reinstall was not scheduled through the parent-exit helper'
    }
    $Deadline = [DateTime]::UtcNow.AddSeconds(30)
    $HelperStatus = $null
    do {
        Start-Sleep -Milliseconds 250
        if (Test-Path -LiteralPath $HelperResult) {
            $HelperStatus = Get-Content -LiteralPath $HelperResult -Raw | ConvertFrom-Json
        }
        $HelperDone = $HelperStatus -and $HelperStatus.status -in @('completed', 'failed')
        $ReceiptRepublished = (Get-Item -LiteralPath $ReceiptPath).LastWriteTimeUtc -gt $ReceiptPublishedAt
    } while ($HelperStatus.status -ne 'failed' -and (-not $HelperDone -or -not $ReceiptRepublished) -and [DateTime]::UtcNow -lt $Deadline)
    if ($HelperStatus.status -eq 'failed') { throw "Windows update helper failed: $($HelperStatus.error)" }
    if (-not $HelperDone -or -not $ReceiptRepublished) {
        $HelperScript = Join-Path $Case 'SessionSkein\install\update-helper.ps1'
        $HelperLog = Join-Path $Case 'SessionSkein\install\update-helper.log'
        $ResultDiagnostic = if (Test-Path $HelperResult) { Get-Content $HelperResult -Raw } else { '<missing>' }
        $LogDiagnostic = if (Test-Path $HelperLog) { Get-Content $HelperLog -Raw } else { '<missing>' }
        $ScriptDiagnostic = if (Test-Path $HelperScript) { Get-Content $HelperScript -Raw } else { '<missing>' }
        $ProcessDiagnostic = Get-CimInstance Win32_Process -Filter "Name = 'pwsh.exe'" -ErrorAction SilentlyContinue |
            Select-Object ProcessId, ParentProcessId, CommandLine | ConvertTo-Json -Compress
        throw "Windows update helper timeout. helperDone=$HelperDone receiptRepublished=$ReceiptRepublished result=$ResultDiagnostic log=$LogDiagnostic processes=$ProcessDiagnostic script=$ScriptDiagnostic"
    }
    if ($HelperStatus.status -ne 'completed') { throw "Windows update helper returned unexpected status: $($HelperStatus.status)" }
    $Receipt = Get-Content $ReceiptPath -Raw | ConvertFrom-Json
    if ($Receipt.version -ne $Version) { throw 'Windows forced reinstall published the wrong receipt version' }
    if ((& (Join-Path $BinDir 'skein.exe') --version) -ne "skein $Version") { throw 'Windows replaced executable is unhealthy' }
    if ((Get-FileHash $Receipt.binary -Algorithm SHA256).Hash -ine $Receipt.binaryHash) { throw 'Windows binary receipt ownership hash mismatch' }
    if ((Get-FileHash $Receipt.installer -Algorithm SHA256).Hash -ine $Receipt.installerHash) { throw 'Windows installer receipt ownership hash mismatch' }

    $BeforeBinary = (Get-FileHash (Join-Path $BinDir 'skein.exe') -Algorithm SHA256).Hash
    $BeforeInstaller = (Get-FileHash (Join-Path $Case 'SessionSkein\install\installer.ps1') -Algorithm SHA256).Hash
    $BeforeReceipt = (Get-FileHash $ReceiptPath -Algorithm SHA256).Hash
    $BeforeSkill = (Get-Item (Join-Path $Case 'codex\skills\session-skein') -Force).Target
    foreach ($Failure in @('SKEIN_TEST_FAIL_INSTALLER_SNAPSHOT', 'SKEIN_TEST_FAIL_INSTALLER_RECEIPT')) {
        [Environment]::SetEnvironmentVariable($Failure, '1', 'Process')
        try {
            & (Join-Path $RepoRoot 'install.ps1') -Version $Version -BinDir $BinDir -NoMcp | Out-Null
            throw "$Failure unexpectedly succeeded"
        } catch {
            if ($_.Exception.Message -eq "$Failure unexpectedly succeeded") { throw }
        } finally {
            [Environment]::SetEnvironmentVariable($Failure, $null, 'Process')
        }
        if ((Get-FileHash (Join-Path $BinDir 'skein.exe') -Algorithm SHA256).Hash -ne $BeforeBinary) { throw 'snapshot rollback changed binary' }
        if ((Get-FileHash (Join-Path $Case 'SessionSkein\install\installer.ps1') -Algorithm SHA256).Hash -ne $BeforeInstaller) { throw 'snapshot rollback changed installer' }
        if ((Get-FileHash $ReceiptPath -Algorithm SHA256).Hash -ne $BeforeReceipt) { throw 'snapshot rollback changed receipt' }
        if ((Get-Item (Join-Path $Case 'codex\skills\session-skein') -Force).Target -ne $BeforeSkill) { throw 'snapshot rollback changed skill' }
    }
    & (Join-Path $RepoRoot 'install.ps1') -Version $Version -BinDir $BinDir -NoMcp | Out-Null
    & (Join-Path $RepoRoot 'install.ps1') -Uninstall | Out-Null
    if (Test-Path (Join-Path $BinDir 'skein.exe')) { throw 'release binary survived owned uninstall' }

    $Bad = Join-Path $Root 'bad'
    $env:LOCALAPPDATA = $Bad
    $env:CODEX_HOME = Join-Path $Bad 'codex'
    $env:SKEIN_DATA_DIR = Join-Path $Bad 'data'
    $env:SKEIN_CONFIG_DIR = Join-Path $Bad 'config'
    [System.IO.File]::WriteAllText((Join-Path $AssetDir 'SHA256SUMS'), "$('0' * 64)  $Archive`n")
    try {
        & (Join-Path $RepoRoot 'install.ps1') -Version $Version -BinDir (Join-Path $Bad 'bin') -NoMcp | Out-Null
        throw 'checksum mismatch unexpectedly installed'
    } catch {
        if ($_.Exception.Message -eq 'checksum mismatch unexpectedly installed') { throw }
    }
    if (Test-Path (Join-Path $Bad 'bin\skein.exe')) { throw 'checksum failure mutated destination' }

    Write-Host 'Windows binary-first release installer tests passed.'
} finally {
    if ($Server -and -not $Server.HasExited) { Stop-Process -Id $Server.Id -Force -ErrorAction SilentlyContinue }
    [Environment]::SetEnvironmentVariable('Path', $OriginalPath, 'User')
    Remove-Item -LiteralPath $Root -Recurse -Force -ErrorAction SilentlyContinue
    Remove-Item Env:SKEIN_RELEASE_BASE_URL, Env:SKEIN_RELEASE_CHANNEL_URL, Env:SKEIN_ALLOW_INSECURE_TEST_DOWNLOADS, Env:SKEIN_ALLOW_RELEASE_OVERRIDE -ErrorAction SilentlyContinue
}
