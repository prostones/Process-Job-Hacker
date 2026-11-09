[CmdletBinding()]
param(
    [switch]$SkipTests,
    [switch]$SkipInstaller,
    [switch]$DynamicCrt,
    [string]$PfxPath,
    [string]$PfxPassword,
    [string]$TimestampUrl = 'http://timestamp.digicert.com'
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repoRoot = Split-Path -Parent $PSScriptRoot
$distDir = Join-Path $repoRoot 'dist'
$targetTriple = 'x86_64-pc-windows-msvc'
$targetDir = Join-Path $repoRoot "target\$targetTriple\release"
$exeName = 'process-job-hacker.exe'
$exePath = Join-Path $targetDir $exeName

function Invoke-Native {
    param(
        [Parameter(Mandatory)][string]$FilePath,
        [Parameter(Mandatory)][string[]]$Arguments
    )
    Write-Host "> $FilePath $($Arguments -join ' ')"
    & $FilePath @Arguments
    if ($LASTEXITCODE -ne 0) {
        throw "命令失败（退出码 $LASTEXITCODE）：$FilePath $($Arguments -join ' ')"
    }
}

function Get-PackageVersion {
    $metadataText = & cargo metadata --no-deps --format-version 1 --locked | Out-String
    if ($LASTEXITCODE -ne 0) {
        throw 'cargo metadata 失败。'
    }
    $metadata = $metadataText | ConvertFrom-Json
    $package = $metadata.packages | Where-Object { $_.name -eq 'process-job-hacker' } | Select-Object -First 1
    if (-not $package) {
        throw 'cargo metadata 中没有找到 process-job-hacker。'
    }
    return $package.version
}

function Find-SignTool {
    if ($env:SIGNTOOL -and (Test-Path -LiteralPath $env:SIGNTOOL)) {
        return $env:SIGNTOOL
    }
    $command = Get-Command signtool.exe -ErrorAction SilentlyContinue
    if ($command) {
        return $command.Source
    }
    $kitRoots = @(
        (Join-Path ${env:ProgramFiles(x86)} 'Windows Kits\10\bin'),
        (Join-Path $env:ProgramFiles 'Windows Kits\10\bin')
    ) | Where-Object { $_ -and (Test-Path -LiteralPath $_) }
    foreach ($kitRoot in $kitRoots) {
        $candidate = Get-ChildItem -LiteralPath $kitRoot -Recurse -Filter signtool.exe -ErrorAction SilentlyContinue |
            Where-Object { $_.FullName -match '\\x64\\signtool\.exe$' } |
            Sort-Object FullName -Descending |
            Select-Object -First 1
        if ($candidate) {
            return $candidate.FullName
        }
    }
    throw '未找到 signtool.exe，请设置 SIGNTOOL 环境变量或安装 Windows SDK。'
}

function Invoke-Sign {
    param([Parameter(Mandatory)][string]$Path)
    if (-not $PfxPath) {
        return
    }
    if (-not (Test-Path -LiteralPath $PfxPath)) {
        throw "找不到签名证书：$PfxPath"
    }
    $signtool = Find-SignTool
    $password = if ($PfxPassword) {
        $PfxPassword
    } elseif ($env:PJH_PFX_PASSWORD) {
        $env:PJH_PFX_PASSWORD
    } else {
        ''
    }
    $signArgs = @('sign', '/fd', 'SHA256', '/td', 'SHA256', '/tr', $TimestampUrl, '/f', $PfxPath)
    if ($password) {
        $signArgs += @('/p', $password)
    }
    $signArgs += $Path
    Invoke-Native $signtool $signArgs
    Invoke-Native $signtool @('verify', '/pa', '/v', $Path)
}

function New-Stage {
    param([Parameter(Mandatory)][string]$Version)

    $stageName = "ProcessJobHacker-$Version-win-x64"
    $stageDir = Join-Path $distDir $stageName
    if (Test-Path -LiteralPath $stageDir) {
        Remove-Item -LiteralPath $stageDir -Recurse -Force
    }
    New-Item -ItemType Directory -Path $stageDir -Force | Out-Null
    $stageAssets = Join-Path $stageDir 'assets'
    New-Item -ItemType Directory -Path $stageAssets -Force | Out-Null

    Copy-Item -LiteralPath $exePath -Destination (Join-Path $stageDir $exeName)
    Copy-Item -LiteralPath (Join-Path $repoRoot 'README.md') -Destination $stageDir
    Copy-Item -LiteralPath (Join-Path $repoRoot 'LICENSE') -Destination $stageDir
    Copy-Item -LiteralPath (Join-Path $repoRoot 'CHANGELOG.md') -Destination $stageDir
    Copy-Item -LiteralPath (Join-Path $repoRoot 'assets\main-window.png') -Destination $stageAssets

    $hash = (Get-FileHash -LiteralPath (Join-Path $stageDir $exeName) -Algorithm SHA256).Hash.ToLowerInvariant()
    $sums = "$hash  $exeName`r`n"
    [IO.File]::WriteAllText((Join-Path $stageDir 'SHA256SUMS.txt'), $sums, [Text.UTF8Encoding]::new($false))
    return $stageDir
}

function New-PortableZip {
    param(
        [Parameter(Mandatory)][string]$StageDir,
        [Parameter(Mandatory)][string]$Version
    )

    $zipName = "ProcessJobHacker-$Version-win-x64.zip"
    $zipPath = Join-Path $distDir $zipName
    if (Test-Path -LiteralPath $zipPath) {
        Remove-Item -LiteralPath $zipPath -Force
    }
    Compress-Archive -Path $StageDir -DestinationPath $zipPath -CompressionLevel Optimal -Force
    $hash = (Get-FileHash -LiteralPath $zipPath -Algorithm SHA256).Hash.ToLowerInvariant()
    [IO.File]::WriteAllText("$zipPath.sha256", "$hash  $zipName`r`n", [Text.UTF8Encoding]::new($false))
    return $zipPath
}

function New-Installer {
    param(
        [Parameter(Mandatory)][string]$Version,
        [Parameter(Mandatory)][string]$StageDir
    )

    $iscc = (& (Join-Path $PSScriptRoot 'Get-InnoSetup.ps1') | Select-Object -Last 1)
    $iss = Join-Path $repoRoot 'installer\process-job-hacker.iss'
    $setupName = "ProcessJobHacker-$Version-Setup.exe"
    $setupPath = Join-Path $distDir $setupName
    if (Test-Path -LiteralPath $setupPath) {
        Remove-Item -LiteralPath $setupPath -Force
    }
    Invoke-Native $iscc @('-qp', "-dAppVersion=$Version", "-dSourceDir=$StageDir", "-o$distDir", $iss)
    if (-not (Test-Path -LiteralPath $setupPath)) {
        throw "安装包未生成：$setupPath"
    }
    Invoke-Sign $setupPath
    $hash = (Get-FileHash -LiteralPath $setupPath -Algorithm SHA256).Hash.ToLowerInvariant()
    [IO.File]::WriteAllText("$setupPath.sha256", "$hash  $setupName`r`n", [Text.UTF8Encoding]::new($false))
    return $setupPath
}

$version = Get-PackageVersion
Write-Host "Process Job Hacker $version"

if (-not $SkipTests) {
    Invoke-Native cargo @('fmt', '--check')
    Invoke-Native cargo @('clippy', '--all-targets', '--', '-D', 'warnings')
    Invoke-Native cargo @('test')
}

$previousRustFlags = $env:RUSTFLAGS
try {
    # Strip the build machine's home directory and repo path out of the panic
    # location strings that end up in the binary, so released executables never
    # embed a local user name.
    $remapFlags = @(
        "--remap-path-prefix=$env:USERPROFILE=C:\build"
        "--remap-path-prefix=$repoRoot=C:\build\project"
    ) -join ' '
    $crtFlag = if ($DynamicCrt) { '' } else { '-C target-feature=+crt-static' }
    $releaseFlags = ("$crtFlag $remapFlags").Trim()
    $env:RUSTFLAGS = if ($previousRustFlags) { "$previousRustFlags $releaseFlags" } else { $releaseFlags }
    Invoke-Native cargo @('build', '--release', '--locked', '--target', $targetTriple)
} finally {
    if ($null -eq $previousRustFlags) {
        Remove-Item Env:RUSTFLAGS -ErrorAction SilentlyContinue
    } else {
        $env:RUSTFLAGS = $previousRustFlags
    }
}

if (-not (Test-Path -LiteralPath $exePath)) {
    throw "release exe 不存在：$exePath"
}

Invoke-Sign $exePath
New-Item -ItemType Directory -Path $distDir -Force | Out-Null
$stageDir = New-Stage -Version $version
$zipPath = New-PortableZip -StageDir $stageDir -Version $version

$setupPath = $null
if (-not $SkipInstaller) {
    $setupPath = New-Installer -Version $version -StageDir $stageDir
}

$releaseFiles = Get-ChildItem -LiteralPath $distDir -File |
    Where-Object { $_.Extension -ne '.sha256' -and $_.Name -ne 'SHA256SUMS.txt' } |
    Sort-Object Name
$releaseSums = $releaseFiles | ForEach-Object {
    '{0}  {1}' -f (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant(), $_.Name
}
[IO.File]::WriteAllText(
    (Join-Path $distDir 'SHA256SUMS.txt'),
    (($releaseSums -join "`r`n") + "`r`n"),
    [Text.UTF8Encoding]::new($false)
)

Write-Host ''
Write-Host '发布产物：'
Get-ChildItem -LiteralPath $distDir -File | Sort-Object Name | ForEach-Object {
    Write-Host ('  {0}  ({1:N1} MB)' -f $_.FullName, ($_.Length / 1MB))
}
if ($setupPath) {
    Write-Host "安装包：$setupPath"
}
