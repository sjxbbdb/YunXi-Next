[CmdletBinding()]
param(
    [string]$Source = "D:\deepseek-harness-reference",
    [switch]$KeepWorktree
)

$ErrorActionPreference = "Stop"
Set-StrictMode -Version Latest

$expectedCommit = "b150a551b8d465e31e418e1b2eaf5e79bbb7d28e"
$repositoryRoot = [System.IO.Path]::GetFullPath((Join-Path $PSScriptRoot "..\.."))
$webRoot = [System.IO.Path]::GetFullPath((Join-Path $repositoryRoot "web"))
$distRoot = [System.IO.Path]::GetFullPath((Join-Path $webRoot "dist"))
$stageRoot = [System.IO.Path]::GetFullPath((Join-Path $webRoot "dist.stage"))
$sourceRoot = (Resolve-Path -LiteralPath $Source).Path
$adapter = Join-Path $webRoot "adapter\web-api-client.ts"
$adapterTest = Join-Path $webRoot "adapter\web-api-client.spec.ts"
$inventoryAdapterRoot = Join-Path $webRoot "adapter\plugin-inventory"
$temporaryRoot = Join-Path ([System.IO.Path]::GetTempPath()) ("yunxi-next-dsh-{0}" -f [guid]::NewGuid().ToString("N"))

foreach ($path in @($distRoot, $stageRoot)) {
    if (-not $path.StartsWith("$webRoot\", [System.StringComparison]::OrdinalIgnoreCase)) {
        throw "refusing to manage a Web output outside $webRoot"
    }
}

function Invoke-Checked {
    param(
        [Parameter(Mandatory)] [string]$FilePath,
        [Parameter(Mandatory)] [string[]]$Arguments,
        [Parameter(Mandatory)] [string]$WorkingDirectory
    )

    Push-Location $WorkingDirectory
    try {
        & $FilePath @Arguments
        if ($LASTEXITCODE -ne 0) {
            throw "$FilePath exited with code $LASTEXITCODE"
        }
    }
    finally {
        Pop-Location
    }
}

function Set-ExactText {
    param(
        [Parameter(Mandatory)] [string]$Path,
        [Parameter(Mandatory)] [string]$OldValue,
        [Parameter(Mandatory)] [string]$NewValue
    )

    $content = [System.IO.File]::ReadAllText($Path)
    if ($content.IndexOf($OldValue, [System.StringComparison]::Ordinal) -lt 0) {
        throw "expected branding text was not found in $Path"
    }
    [System.IO.File]::WriteAllText(
        $Path,
        $content.Replace($OldValue, $NewValue),
        [System.Text.UTF8Encoding]::new($false)
    )
}

$server = $null
try {
    $sourceCommit = (& git -C $sourceRoot rev-parse HEAD).Trim()
    if ($LASTEXITCODE -ne 0 -or $sourceCommit -ne $expectedCommit) {
        throw "dsh source must be exactly $expectedCommit; found $sourceCommit"
    }

    Invoke-Checked -FilePath "git" -Arguments @(
        "clone", "--no-hardlinks", "--quiet", $sourceRoot, $temporaryRoot
    ) -WorkingDirectory $repositoryRoot
    Invoke-Checked -FilePath "git" -Arguments @(
        "checkout", "--detach", "--quiet", $expectedCommit
    ) -WorkingDirectory $temporaryRoot

    $adapterTarget = Join-Path $temporaryRoot "packages\client\connection\src\client\web-api-client.ts"
    Copy-Item -LiteralPath $adapter -Destination $adapterTarget -Force
    $adapterTestTarget = Join-Path $temporaryRoot "packages\client\connection\tests\web-api-client.yunxi.spec.ts"
    Copy-Item -LiteralPath $adapterTest -Destination $adapterTestTarget -Force
    Set-ExactText `
        -Path $adapterTestTarget `
        -OldValue "'./web-api-client.ts'" `
        -NewValue "'../src/client/web-api-client.ts'"
    $inventoryTargetRoot = Join-Path $temporaryRoot "packages\client\ui-settings-plugin-inventory\src\client"
    foreach ($file in @(
        "index.ts",
        "locales.ts",
        "PluginInventorySettingsTab.tsx",
        "PluginInventorySettingsTab.module.css"
    )) {
        Copy-Item `
            -LiteralPath (Join-Path $inventoryAdapterRoot $file) `
            -Destination (Join-Path $inventoryTargetRoot $file) `
            -Force
    }
    Set-ExactText `
        -Path (Join-Path $temporaryRoot "packages\client\ui-renderer\src\client\DocumentTitle.tsx") `
        -OldValue "'DSH Local Build'" `
        -NewValue "'YunXi Next'"
    Set-ExactText `
        -Path (Join-Path $temporaryRoot "packages\client\ui-sidebar\src\client\SidebarRoot.tsx") `
        -OldValue ">DSH Local Build</span>" `
        -NewValue ">YunXi Next</span>"

    Invoke-Checked -FilePath "pnpm" -Arguments @("install", "--frozen-lockfile") -WorkingDirectory $temporaryRoot
    Invoke-Checked -FilePath "pnpm" -Arguments @(
        "exec", "vitest", "run", "packages/client/connection/tests/web-api-client.yunxi.spec.ts"
    ) -WorkingDirectory $temporaryRoot
    Remove-Item -LiteralPath $adapterTestTarget -Force
    Invoke-Checked -FilePath "pnpm" -Arguments @("run", "build") -WorkingDirectory $temporaryRoot

    $probe = [System.Net.Sockets.TcpListener]::new([System.Net.IPAddress]::Loopback, 0)
    $probe.Start()
    $port = ([System.Net.IPEndPoint]$probe.LocalEndpoint).Port
    $probe.Stop()

    $processInfo = [System.Diagnostics.ProcessStartInfo]::new()
    $processInfo.FileName = (Get-Command node).Source
    $processInfo.WorkingDirectory = $temporaryRoot
    $processInfo.UseShellExecute = $false
    $processInfo.CreateNoWindow = $true
    $processInfo.RedirectStandardOutput = $true
    $processInfo.RedirectStandardError = $true
    $isolatedDshHome = Join-Path $temporaryRoot ".dsh-home"
    New-Item -ItemType Directory -Path $isolatedDshHome | Out-Null
    $processInfo.Environment["DSH_HOME"] = $isolatedDshHome
    $cliEntry = Join-Path $temporaryRoot "apps\cli\lib\bin.js"
    $processInfo.Arguments = '"{0}" --profile web --no-open --host 127.0.0.1 --port {1}' -f $cliEntry, $port
    $server = [System.Diagnostics.Process]::Start($processInfo)
    if ($null -eq $server) {
        throw "failed to start the temporary dsh Web host"
    }

    $baseUrl = "http://127.0.0.1:$port"
    $indexResponse = $null
    for ($attempt = 0; $attempt -lt 120; $attempt++) {
        if ($server.HasExited) {
            $stderr = $server.StandardError.ReadToEnd()
            throw "temporary dsh Web host exited early: $stderr"
        }
        try {
            $indexResponse = Invoke-WebRequest -UseBasicParsing -Uri "$baseUrl/" -TimeoutSec 2
            break
        }
        catch {
            Start-Sleep -Milliseconds 250
        }
    }
    if ($null -eq $indexResponse) {
        throw "temporary dsh Web host did not become ready"
    }

    if (Test-Path -LiteralPath $stageRoot) {
        Remove-Item -LiteralPath $stageRoot -Recurse -Force
    }
    New-Item -ItemType Directory -Path $stageRoot | Out-Null
    Copy-Item -Path (Join-Path $temporaryRoot "apps\web\dist\*") -Destination $stageRoot -Recurse -Force
    Get-ChildItem -LiteralPath $stageRoot -Recurse -File -Filter "*.map" |
        Remove-Item -Force

    $manifestPath = Join-Path $stageRoot "manifest.webmanifest"
    $manifest = Get-Content -LiteralPath $manifestPath -Raw | ConvertFrom-Json
    $manifest.name = "YunXi Next"
    $manifest.short_name = "YunXi Next"
    [System.IO.File]::WriteAllText(
        $manifestPath,
        "$(ConvertTo-Json $manifest -Depth 10)`n",
        [System.Text.UTF8Encoding]::new($false)
    )

    $index = $indexResponse.Content.Replace("<title>DSH Local Build</title>", "<title>YunXi Next</title>")
    [System.IO.File]::WriteAllText(
        (Join-Path $stageRoot "index.html"),
        $index,
        [System.Text.UTF8Encoding]::new($false)
    )

    $bundleUrls = [System.Collections.Generic.HashSet[string]]::new([System.StringComparer]::Ordinal)
    foreach ($match in [regex]::Matches($index, '/plugins/[^"'']+?/client\.js\?rev=[0-9a-f]+')) {
        [void]$bundleUrls.Add($match.Value)
    }
    if ($bundleUrls.Count -lt 40) {
        throw "captured boot manifest referenced only $($bundleUrls.Count) client bundles"
    }

    foreach ($bundleUrl in $bundleUrls) {
        $uri = [uri]"$baseUrl$bundleUrl"
        $relative = [uri]::UnescapeDataString($uri.AbsolutePath).TrimStart('/').Replace('/', '\')
        $destination = Join-Path $stageRoot $relative
        $destinationDirectory = Split-Path -Parent $destination
        New-Item -ItemType Directory -Path $destinationDirectory -Force | Out-Null
        Invoke-WebRequest -UseBasicParsing -Uri $uri.AbsoluteUri -OutFile $destination
    }

    $record = [ordered]@{
        upstream = "https://github.com/deepseek-ai/deepseek-harness"
        commit = $expectedCommit
        version = "0.1.1-rc.2"
        clientBundles = $bundleUrls.Count
        adapter = "bounded-sse-polling+capability-switches"
    } | ConvertTo-Json
    [System.IO.File]::WriteAllText(
        (Join-Path $stageRoot "yunxi-web-build.json"),
        "$record`n",
        [System.Text.UTF8Encoding]::new($false)
    )

    if (Test-Path -LiteralPath $distRoot) {
        Remove-Item -LiteralPath $distRoot -Recurse -Force
    }
    Move-Item -LiteralPath $stageRoot -Destination $distRoot
    Write-Host "Imported $($bundleUrls.Count) dsh client bundles into $distRoot"
}
finally {
    if ($null -ne $server -and -not $server.HasExited) {
        $server.Kill()
        $server.WaitForExit()
    }
    if (-not $KeepWorktree -and (Test-Path -LiteralPath $temporaryRoot)) {
        $resolvedTemporary = [System.IO.Path]::GetFullPath($temporaryRoot)
        $tempBase = [System.IO.Path]::GetFullPath([System.IO.Path]::GetTempPath())
        if (-not $resolvedTemporary.StartsWith($tempBase, [System.StringComparison]::OrdinalIgnoreCase)) {
            throw "refusing to remove a worktree outside the temporary directory"
        }
        for ($attempt = 0; $attempt -lt 5; $attempt++) {
            try {
                Remove-Item -LiteralPath $resolvedTemporary -Recurse -Force -ErrorAction Stop
                break
            }
            catch {
                if ($attempt -eq 4) {
                    Write-Warning "temporary checkout cleanup was incomplete: $resolvedTemporary"
                    break
                }
                Start-Sleep -Milliseconds 200
            }
        }
    }
}
