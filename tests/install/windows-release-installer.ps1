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
    $BinDir = Join-Path $Case 'bin'
    & (Join-Path $RepoRoot 'install.ps1') -BinDir $BinDir -NoMcp | Out-Null
    if ((& (Join-Path $BinDir 'skein.exe') --version) -ne "skein $Version") { throw 'remote binary version mismatch' }
    $Receipt = Get-Content (Join-Path $Case 'SessionSkein\install\receipt.json') -Raw | ConvertFrom-Json
    if ($Receipt.source -ne "release:${Tag}:$Target") { throw 'release provenance missing from receipt' }
    if (-not (Test-Path (Join-Path $Case 'codex\skills\session-skein\SKILL.md'))) { throw 'release skill was not installed' }
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
    Remove-Item Env:SKEIN_RELEASE_BASE_URL, Env:SKEIN_RELEASE_CHANNEL_URL, Env:SKEIN_ALLOW_INSECURE_TEST_DOWNLOADS -ErrorAction SilentlyContinue
}
