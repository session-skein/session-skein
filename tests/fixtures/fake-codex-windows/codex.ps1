param(
    [Parameter(ValueFromRemainingArguments = $true)]
    [string[]]$CommandArgs
)

$ErrorActionPreference = 'Stop'
$state = $env:FAKE_CODEX_STATE
if (-not $state) { throw 'FAKE_CODEX_STATE is required' }
if ($CommandArgs.Count -lt 2 -or $CommandArgs[0] -ne 'mcp') { exit 2 }

switch ($CommandArgs[1]) {
    'get' {
        if ($CommandArgs.Count -lt 3 -or $CommandArgs[2] -ne 'session-skein') { exit 2 }
        if ($env:FAKE_CODEX_FAIL_GET -eq '1') { exit 1 }
        if (Test-Path -LiteralPath "$state.fail-get-once") {
            Remove-Item -LiteralPath "$state.fail-get-once" -Force
            exit 1
        }
        if (-not (Test-Path -LiteralPath $state)) { exit 1 }
        [Console]::Out.Write([System.IO.File]::ReadAllText($state))
    }
    'remove' {
        if ($CommandArgs.Count -lt 3 -or $CommandArgs[2] -ne 'session-skein') { exit 2 }
        Remove-Item -LiteralPath $state -Force -ErrorAction SilentlyContinue
    }
    'add' {
        if ($CommandArgs.Count -lt 5 -or $CommandArgs[2] -ne 'session-skein') { exit 2 }
        if ($env:FAKE_CODEX_FAIL_ADD -eq 'before') { exit 1 }
        $separator = [Array]::IndexOf($CommandArgs, '--')
        if ($separator -lt 3 -or $separator + 2 -ge $CommandArgs.Count) { exit 2 }
        $command = $CommandArgs[$separator + 1]
        $serverArgs = @($CommandArgs[($separator + 2)..($CommandArgs.Count - 1)])
        $payload = [ordered]@{
            name = 'session-skein'
            transport = [ordered]@{
                type = 'stdio'
                command = $command
                args = $serverArgs
            }
        }
        $parent = Split-Path -Parent $state
        New-Item -ItemType Directory -Path $parent -Force | Out-Null
        [System.IO.File]::WriteAllText($state, ($payload | ConvertTo-Json -Compress -Depth 4))
        if ($env:FAKE_CODEX_FAIL_VERIFY -eq '1') {
            New-Item -ItemType File -Path "$state.fail-get-once" -Force | Out-Null
        }
        if ($env:FAKE_CODEX_FAIL_ADD -eq 'after') { exit 1 }
    }
    default { exit 2 }
}
