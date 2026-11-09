# Restores the Inno Setup compiler pinned in installer/InnoSetup.csproj and returns the path of ISCC.exe.
$ErrorActionPreference = 'Stop'

$project = Join-Path (Split-Path $PSScriptRoot -Parent) 'installer/InnoSetup.csproj'
if (!(Test-Path -LiteralPath $project)) {
    throw "找不到 Inno Setup 项目：$project"
}

dotnet restore $project --nologo | Out-Host
if ($LASTEXITCODE -ne 0) {
    throw 'Inno Setup restore failed.'
}

$version = ([xml](Get-Content -Raw -LiteralPath $project)).Project.ItemGroup.PackageDownload.Version.Trim('[', ']')
$packages = if ($env:NUGET_PACKAGES) {
    $env:NUGET_PACKAGES
} else {
    Join-Path $env:USERPROFILE '.nuget\packages'
}
$iscc = Join-Path $packages "tools.innosetup/$version/tools/ISCC.exe"
if (!(Test-Path -LiteralPath $iscc)) {
    throw "ISCC.exe not found at $iscc"
}
$iscc
