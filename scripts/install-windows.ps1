<#
.SYNOPSIS
Installs the native `yunxi next` route without replacing legacy YunXi.
#>

[CmdletBinding()]
param(
    [string]$InstallRoot = (Join-Path $env:LOCALAPPDATA 'YunXi Next'),
    [string]$RouterRoot = (Join-Path $env:LOCALAPPDATA 'YunXi'),
    [string]$LegacyExecutable
)

$ErrorActionPreference = 'Stop'

if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
    throw 'This installer currently supports Windows only.'
}

$repositoryRoot = [IO.Path]::GetFullPath((Join-Path $PSScriptRoot '..'))
$installBin = [IO.Path]::GetFullPath((Join-Path $InstallRoot 'bin'))
$routerBin = [IO.Path]::GetFullPath((Join-Path $RouterRoot 'bin'))
$sourceNext = Join-Path $repositoryRoot 'target\release\yunxi.exe'
$sourceLauncher = Join-Path $repositoryRoot 'target\release\yunxi-launcher.exe'
$installedNext = Join-Path $installBin 'yunxi-next.exe'
$installedLauncher = Join-Path $routerBin 'yunxi.exe'
$nextRouteFile = Join-Path $routerBin 'yunxi-next.path'
$legacyRouteFile = Join-Path $routerBin 'yunxi-legacy.path'
$obsoleteBatchRouter = Join-Path $routerBin 'yunxi.cmd'

if ([string]::IsNullOrWhiteSpace($LegacyExecutable)) {
    $LegacyExecutable = Get-Command yunxi.exe -All -ErrorAction SilentlyContinue |
        Where-Object {
            $_.CommandType -eq 'Application' -and
            -not $_.Source.StartsWith($installBin, [StringComparison]::OrdinalIgnoreCase) -and
            -not $_.Source.StartsWith($routerBin, [StringComparison]::OrdinalIgnoreCase)
        } |
        Select-Object -First 1 -ExpandProperty Source
}

if (-not [string]::IsNullOrWhiteSpace($LegacyExecutable)) {
    $LegacyExecutable = (Resolve-Path -LiteralPath $LegacyExecutable).Path
}

Push-Location $repositoryRoot
try {
    & cargo build -p yunxi-cli -p yunxi-launcher --release
    if ($LASTEXITCODE -ne 0) {
        throw "Release build failed with exit code $LASTEXITCODE."
    }
}
finally {
    Pop-Location
}

& $sourceNext next --version
if ($LASTEXITCODE -ne 0) {
    throw 'The release executable did not accept the `next` subcommand.'
}

New-Item -ItemType Directory -Path $installBin -Force | Out-Null
New-Item -ItemType Directory -Path $routerBin -Force | Out-Null
Copy-Item -LiteralPath $sourceNext -Destination $installedNext -Force
Copy-Item -LiteralPath $sourceLauncher -Destination $installedLauncher -Force

if ((Get-FileHash $sourceNext).Hash -ne (Get-FileHash $installedNext).Hash) {
    throw 'Installed YunXi Next hash does not match the release build.'
}
if ((Get-FileHash $sourceLauncher).Hash -ne (Get-FileHash $installedLauncher).Hash) {
    throw 'Installed launcher hash does not match the release build.'
}

$utf8WithoutBom = New-Object Text.UTF8Encoding($false)
[IO.File]::WriteAllText($nextRouteFile, $installedNext, $utf8WithoutBom)
if ([string]::IsNullOrWhiteSpace($LegacyExecutable)) {
    if (Test-Path -LiteralPath $legacyRouteFile) {
        Remove-Item -LiteralPath $legacyRouteFile -Force
    }
    $legacyLabel = 'not installed'
}
else {
    [IO.File]::WriteAllText($legacyRouteFile, $LegacyExecutable, $utf8WithoutBom)
    $legacyLabel = $LegacyExecutable
}

if (Test-Path -LiteralPath $obsoleteBatchRouter) {
    Remove-Item -LiteralPath $obsoleteBatchRouter -Force
}

function Add-PathPrefix([string]$CurrentPath, [string]$Prefix) {
    $segments = @($CurrentPath -split ';' | Where-Object { -not [string]::IsNullOrWhiteSpace($_) })
    $segments = @($segments | Where-Object {
        -not $_.TrimEnd('\').Equals($Prefix.TrimEnd('\'), [StringComparison]::OrdinalIgnoreCase)
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
$newUserPath = Add-PathPrefix $userPath $routerBin
[Environment]::SetEnvironmentVariable('Path', $newUserPath, 'User')
$env:Path = Add-PathPrefix $env:Path $routerBin
Publish-EnvironmentChange

& $installedLauncher next --version
if ($LASTEXITCODE -ne 0) {
    throw 'The installed `yunxi next` route failed its version check.'
}

Write-Host "Installed YunXi Next: $installedNext"
Write-Host "Registered native launcher: $installedLauncher"
Write-Host "Legacy YunXi route: $legacyLabel"
Write-Host 'Open a new terminal, then run: yunxi next'
