<#
.SYNOPSIS
Builds and installs only `yunxi-next.exe` without changing PATH or legacy YunXi.
#>

[CmdletBinding()]
param(
    [string]$InstallBin,
    [switch]$SkipBuild
)

$ErrorActionPreference = 'Stop'

if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw 'This installer currently supports Windows only.'
}

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$cargoExecutable = Get-Command cargo.exe -CommandType Application -ErrorAction Stop |
    Select-Object -First 1 -ExpandProperty Source
$commandBin = if ([string]::IsNullOrWhiteSpace($InstallBin)) {
    [IO.Path]::GetFullPath((Split-Path -Parent $cargoExecutable))
}
else {
    [IO.Path]::GetFullPath($InstallBin)
}
$sourceNext = Join-Path $repositoryRoot 'target\release\yunxi-next.exe'
$installedNext = Join-Path $commandBin 'yunxi-next.exe'

if ($commandBin -eq $repositoryRoot -or $commandBin -like ($repositoryRoot + '\*')) {
    throw "InstallBin must be outside the repository: $commandBin"
}

if (-not $SkipBuild) {
    Push-Location $repositoryRoot
    try {
        & $cargoExecutable build -p yunxi-cli --bin yunxi-next --release --locked
        if ($LASTEXITCODE -ne 0) {
            throw "Release build failed with exit code $LASTEXITCODE."
        }
    }
    finally {
        Pop-Location
    }
}

if (-not (Test-Path -LiteralPath $sourceNext -PathType Leaf)) {
    throw "Release `yunxi-next.exe` is missing: $sourceNext"
}

New-Item -ItemType Directory -Path $commandBin -Force | Out-Null
Copy-Item -LiteralPath $sourceNext -Destination $installedNext -Force

if ((Get-FileHash $sourceNext).Hash -ne (Get-FileHash $installedNext).Hash) {
    throw 'Installed YunXi Next hash does not match the release build.'
}

& $installedNext --version
if ($LASTEXITCODE -ne 0) {
    throw 'The installed `yunxi-next` command failed its version check.'
}

Write-Host "Installed YunXi Next: $installedNext"
Write-Host 'Only yunxi-next.exe was written; PATH, registry, services, and legacy YunXi were not changed.'
Write-Host "Run explicitly: & '$installedNext'"
