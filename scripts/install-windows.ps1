<#
.SYNOPSIS
Installs `yunxi-next` beside Cargo without replacing legacy YunXi.
#>

[CmdletBinding()]
param(
    [string]$InstallBin,
    [string]$PreviousInstallRoot = (Join-Path $env:LOCALAPPDATA 'YunXi Next'),
    [string]$ObsoleteRouterRoot = (Join-Path $env:LOCALAPPDATA 'YunXi')
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
$previousInstallBin = [IO.Path]::GetFullPath((Join-Path $PreviousInstallRoot 'bin'))
$obsoleteRouterBin = [IO.Path]::GetFullPath((Join-Path $ObsoleteRouterRoot 'bin'))
$sourceNext = Join-Path $repositoryRoot 'target\release\yunxi-next.exe'
$installedNext = Join-Path $commandBin 'yunxi-next.exe'
$previousInstalledNext = Join-Path $previousInstallBin 'yunxi-next.exe'
$obsoleteNextRouteFile = Join-Path $obsoleteRouterBin 'yunxi-next.path'
$obsoleteRouterDetected = Test-Path -LiteralPath $obsoleteNextRouteFile -PathType Leaf
$obsoleteRouterFiles = @(
    (Join-Path $obsoleteRouterBin 'yunxi.exe'),
    (Join-Path $obsoleteRouterBin 'yunxi.cmd'),
    $obsoleteNextRouteFile,
    (Join-Path $obsoleteRouterBin 'yunxi-legacy.path')
)

Push-Location $repositoryRoot
try {
    & $cargoExecutable build -p yunxi-cli --bin yunxi-next --release
    if ($LASTEXITCODE -ne 0) {
        throw "Release build failed with exit code $LASTEXITCODE."
    }
}
finally {
    Pop-Location
}

& $sourceNext --version
if ($LASTEXITCODE -ne 0) {
    throw 'The release `yunxi-next` executable failed its version check.'
}

New-Item -ItemType Directory -Path $commandBin -Force | Out-Null
Copy-Item -LiteralPath $sourceNext -Destination $installedNext -Force

if ((Get-FileHash $sourceNext).Hash -ne (Get-FileHash $installedNext).Hash) {
    throw 'Installed YunXi Next hash does not match the release build.'
}

if (
    -not $commandBin.Equals($previousInstallBin, [StringComparison]::OrdinalIgnoreCase) -and
    (Test-Path -LiteralPath $previousInstalledNext -PathType Leaf)
) {
    Remove-Item -LiteralPath $previousInstalledNext -Force
}

if ($obsoleteRouterDetected) {
    foreach ($obsoleteRouterFile in $obsoleteRouterFiles) {
        if (Test-Path -LiteralPath $obsoleteRouterFile -PathType Leaf) {
            Remove-Item -LiteralPath $obsoleteRouterFile -Force
        }
    }
}

function Set-PathPrefix(
    [string]$CurrentPath,
    [string]$Prefix,
    [string[]]$RemovePaths
) {
    $segments = @($CurrentPath -split ';' | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    $segments = @($segments | Where-Object {
        $candidate = $_.Trim().Trim('"').TrimEnd('\')
        -not ($RemovePaths | Where-Object {
            $candidate.Equals($_.TrimEnd('\'), [StringComparison]::OrdinalIgnoreCase)
        })
    })
    return (@($Prefix) + $segments) -join ';'
}

function Publish-EnvironmentChange {
    if (-not ('YunXiEnvironmentBroadcast' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;

public static class YunXiEnvironmentBroadcast
{
    [DllImport("user32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
    private static extern IntPtr SendMessageTimeout(
        IntPtr window,
        uint message,
        UIntPtr wordParameter,
        string longParameter,
        uint flags,
        uint timeout,
        out UIntPtr result);

    public static void Publish()
    {
        UIntPtr result;
        SendMessageTimeout(
            new IntPtr(0xffff),
            0x001a,
            UIntPtr.Zero,
            "Environment",
            0x0002,
            5000,
            out result);
    }
}
'@
    }
    [YunXiEnvironmentBroadcast]::Publish()
}

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$pathsToReplace = @($commandBin, $previousInstallBin)
if ($obsoleteRouterDetected) {
    $pathsToReplace += $obsoleteRouterBin
}
$newUserPath = Set-PathPrefix $userPath $commandBin $pathsToReplace
[Environment]::SetEnvironmentVariable('Path', $newUserPath, 'User')
$env:Path = Set-PathPrefix $env:Path $commandBin $pathsToReplace
Publish-EnvironmentChange

& $installedNext --version
if ($LASTEXITCODE -ne 0) {
    throw 'The installed `yunxi-next` command failed its version check.'
}

Write-Host "Installed YunXi Next: $installedNext"
Write-Host 'Legacy YunXi was not modified.'
Write-Host 'Run: yunxi-next'
