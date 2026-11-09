# Gracefully stops a running Process Job Hacker copy, or force-stops it with -Force.
# The installer first runs this without -Force (no UAC); -Force runs elevated and leaves
# the --guard process alive so it can clear Job limits and restore affinity.
[CmdletBinding()]
param(
    [switch]$Force
)

$ErrorActionPreference = 'Stop'

$mutexName = 'Local\ProcessJobHacker.Instance'
$quitEventName = 'Local\ProcessJobHacker.Quit'
$journalPath = Join-Path $env:LOCALAPPDATA 'prostones\ProcessJobHacker\data\active_jobs.json'

function Test-InstanceRunning {
    try {
        $mutex = [System.Threading.Mutex]::OpenExisting($mutexName)
        $mutex.Dispose()
        return $true
    } catch [System.Threading.WaitHandleCannotBeOpenedException] {
        return $false
    }
}

function Wait-ForGuardExit {
    if (-not (Test-Path -LiteralPath $journalPath)) {
        return
    }
    try {
        $journal = Get-Content -LiteralPath $journalPath -Raw | ConvertFrom-Json
        $guardPid = [int]$journal.guard_pid
        $guardStart = [int64]$journal.guard_start_time
        if ($guardPid -le 0) {
            return
        }
        $guard = Get-Process -Id $guardPid -ErrorAction SilentlyContinue
        if (-not $guard) {
            return
        }
        $actualStart = [int64]([DateTimeOffset]$guard.StartTime).ToUnixTimeSeconds()
        if ($guardStart -le 0 -or $actualStart -eq $guardStart) {
            $guard.WaitForExit(15000) | Out-Null
        }
    } catch {
        # A malformed or missing journal must not mask the stop result.
    }
}

function Wait-ForInstanceExit {
    for ($attempt = 0; $attempt -lt 150; $attempt++) {
        if (-not (Test-InstanceRunning)) {
            return $true
        }
        Start-Sleep -Milliseconds 100
    }
    return -not (Test-InstanceRunning)
}

if (-not (Test-InstanceRunning)) {
    exit 0
}

if (-not $Force) {
    try {
        $quitEvent = [System.Threading.EventWaitHandle]::OpenExisting($quitEventName)
        $quitEvent.Set() | Out-Null
        $quitEvent.Dispose()
    } catch [System.Threading.WaitHandleCannotBeOpenedException] {
        # An older version has no quit event; let the installer request elevation.
        exit 2
    }
    if (-not (Wait-ForInstanceExit)) {
        exit 3
    }
    Wait-ForGuardExit
    exit 0
}

$identity = [System.Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object System.Security.Principal.WindowsPrincipal($identity)
if (-not $principal.IsInRole([System.Security.Principal.WindowsBuiltInRole]::Administrator)) {
    exit 2
}

$processes = @(Get-CimInstance Win32_Process -Filter "Name='process-job-hacker.exe'")
if ($processes.Count -eq 0) {
    exit 0
}

foreach ($process in $processes) {
    $commandLine = [string]$process.CommandLine
    if ($commandLine -match '--guard') {
        continue
    }
    try {
        Stop-Process -Id $process.ProcessId -Force -ErrorAction Stop
    } catch {
        # The process may have exited between enumeration and termination.
    }
}

if (-not (Wait-ForInstanceExit)) {
    exit 3
}
Wait-ForGuardExit
exit 0
