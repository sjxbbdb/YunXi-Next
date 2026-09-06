param(
    [string]$Workspace = (Split-Path -Parent $PSScriptRoot),
    [string]$ReleaseExe = "D:\DevTools\Rust\cargo\bin\yunxi-next.exe",
    [string]$LegacyExe = "D:\Apps\YunXi Agent\bin\yunxi.exe",
    [switch]$RunCargo,
    [switch]$SkipWeb,
    [switch]$KeepArtifacts
)

$ErrorActionPreference = "Stop"
$failures = [System.Collections.Generic.List[string]]::new()
$warnings = [System.Collections.Generic.List[string]]::new()
$webProcess = $null
$webInput = $null
$webStdout = $null
$webStderr = $null
$tempRoot = $null
$legacyHashBefore = $null

function Write-Pass([string]$Message) {
    Write-Host "[PASS] $Message" -ForegroundColor Green
}

function Write-Warn([string]$Message) {
    $warnings.Add($Message)
    Write-Host "[WARN] $Message" -ForegroundColor Yellow
}

function Write-Fail([string]$Message) {
    $failures.Add($Message)
    Write-Host "[FAIL] $Message" -ForegroundColor Red
}

function Assert-Condition([bool]$Condition, [string]$Pass, [string]$Fail) {
    if ($Condition) {
        Write-Pass $Pass
    } else {
        Write-Fail $Fail
    }
}

function Get-FreeLoopbackPort {
    $listener = [System.Net.Sockets.TcpListener]::new(
        [System.Net.IPAddress]::Loopback,
        0
    )
    try {
        $listener.Start()
        return ([System.Net.IPEndPoint]$listener.LocalEndpoint).Port
    } finally {
        $listener.Stop()
    }
}

function Invoke-Rpc([string]$BaseUrl, [string]$RpcId, [string]$Method) {
    $body = @{
        type = "client-request"
        rpcId = $RpcId
        method = $Method
        payload = @{}
    } | ConvertTo-Json -Compress
    return Invoke-WebRequest -UseBasicParsing -Method Post `
        -Uri "$BaseUrl/api/$Method" `
        -ContentType "application/json" `
        -Body $body
}

function Invoke-ExpectedHttpFailure([string]$Uri, [string]$Body) {
    try {
        $response = Invoke-WebRequest -UseBasicParsing -Method Post `
            -Uri $Uri -ContentType "application/json" -Body $Body
        return [pscustomobject]@{
            Status = [int]$response.StatusCode
            Content = [string]$response.Content
        }
    } catch {
        if ($_.Exception.Response) {
            return [pscustomobject]@{
                Status = [int]$_.Exception.Response.StatusCode
                Content = ""
            }
        }
        throw
    }
}

try {
    $workspace = (Resolve-Path -LiteralPath $Workspace).Path
    $expectedWorkspace = "D:\YunXi Next"
    Assert-Condition ($workspace -eq $expectedWorkspace) `
        "audit workspace is $workspace" `
        "audit must run against $expectedWorkspace, got $workspace"

    $branch = (& git -C $workspace symbolic-ref --short HEAD 2>$null).Trim()
    Assert-Condition ($branch -eq "codex/complete-plugin-runtime") `
        "branch is codex/complete-plugin-runtime" `
        "expected branch codex/complete-plugin-runtime, got $branch"

    $newBinary = Resolve-Path -LiteralPath $ReleaseExe -ErrorAction SilentlyContinue
    Assert-Condition ($null -ne $newBinary) `
        "release binary exists at $ReleaseExe" `
        "release binary is missing at $ReleaseExe"

    if ($newBinary) {
        $newHash = (Get-FileHash -LiteralPath $newBinary.Path -Algorithm SHA256).Hash
        Write-Host "[INFO] yunxi-next SHA256: $newHash"
        $help = (& $newBinary.Path --help 2>&1 | Out-String)
        Assert-Condition ($help -match "yunxi-next" -and $help -match "--once") `
            "release binary exposes the documented CLI help" `
            "release binary help is unavailable or incomplete"
        $webHelp = (& $newBinary.Path web --help 2>&1 | Out-String)
        Assert-Condition ($webHelp -match "--bind") `
            "release binary exposes web --bind" `
            "release binary web help does not expose --bind"

        if ($help -notmatch "status|diagnostics|enable|disable|reload") {
            Write-Warn "CLI management commands status/diagnostics/enable/disable/reload are not exposed"
        }
    }

    $legacy = Resolve-Path -LiteralPath $LegacyExe -ErrorAction SilentlyContinue
    if ($legacy) {
        $legacyHashBefore = (Get-FileHash -LiteralPath $legacy.Path -Algorithm SHA256).Hash
        Write-Host "[INFO] legacy yunxi SHA256 before audit: $legacyHashBefore"
        Write-Pass "legacy executable is present and was only read"
    } else {
        Write-Warn "legacy executable is unavailable; old-version isolation cannot be hash-verified"
    }

    if ($RunCargo) {
        Push-Location $workspace
        try {
            & cargo fmt --all -- --check
            Assert-Condition ($LASTEXITCODE -eq 0) "cargo fmt check passed" "cargo fmt check failed"
            & cargo clippy --workspace --all-targets -- -D warnings
            Assert-Condition ($LASTEXITCODE -eq 0) "strict clippy passed" "strict clippy failed"
            & cargo test --workspace --all-targets
            Assert-Condition ($LASTEXITCODE -eq 0) "workspace tests passed" "workspace tests failed"
        } finally {
            Pop-Location
        }
    } else {
        Write-Warn "cargo checks skipped; rerun with -RunCargo for fmt, clippy, and tests"
    }

    if (-not $SkipWeb -and $newBinary) {
        $port = Get-FreeLoopbackPort
        $baseUrl = "http://127.0.0.1:$port"
        $tempRoot = Join-Path ([System.IO.Path]::GetTempPath()) `
            ("yunxi-next-acceptance-{0}-{1}" -f $PID, ([guid]::NewGuid().ToString("N")))
        New-Item -ItemType Directory -Path $tempRoot | Out-Null

        $psi = [System.Diagnostics.ProcessStartInfo]::new()
        $psi.FileName = $newBinary.Path
        $psi.Arguments = "web --bind 127.0.0.1:$port"
        $psi.WorkingDirectory = $workspace
        $psi.UseShellExecute = $false
        $psi.CreateNoWindow = $true
        $psi.RedirectStandardInput = $true
        $psi.RedirectStandardOutput = $true
        $psi.RedirectStandardError = $true
        $psi.EnvironmentVariables["YUNXI_NEXT_HOME"] = $tempRoot
        $psi.EnvironmentVariables["YUNXI_NEXT_CONTEXT_ENABLED"] = "false"
        $psi.EnvironmentVariables["YUNXI_NEXT_PERSONA_ENABLED"] = "false"
        $psi.EnvironmentVariables["YUNXI_NEXT_MEMORY_ENABLED"] = "false"
        $psi.EnvironmentVariables["YUNXI_NEXT_STORAGE_ENABLED"] = "false"
        $psi.EnvironmentVariables["YUNXI_NEXT_COMPANION_ENABLED"] = "false"
        $psi.EnvironmentVariables["YUNXI_NEXT_MAILBOX_ENABLED"] = "false"
        $psi.EnvironmentVariables["YUNXI_NEXT_SCHEDULER_ENABLED"] = "false"
        $psi.EnvironmentVariables["YUNXI_NEXT_SHELL_ENABLED"] = "false"
        $psi.EnvironmentVariables["YUNXI_NEXT_PATCH_ENABLED"] = "false"
        $psi.EnvironmentVariables["YUNXI_NEXT_FILES_ENABLED"] = "false"
        $psi.EnvironmentVariables["YUNXI_NEXT_MCP_ENABLED"] = "false"
        $psi.EnvironmentVariables["YUNXI_NEXT_SKILLS_ENABLED"] = "false"
        $psi.EnvironmentVariables["YUNXI_NEXT_MULTI_AGENT_ENABLED"] = "false"
        $psi.EnvironmentVariables["YUNXI_NEXT_VOICE_ENABLED"] = "false"
        $psi.EnvironmentVariables["YUNXI_NEXT_WEIXIN_ENABLED"] = "false"
        foreach ($name in @(
            "YUNXI_PROVIDER_API_KEY", "YUNXI_PROVIDER_API_KEY_ENV",
            "DEEPSEEK_API_KEY", "OPENAI_API_KEY", "YUNXI_PROVIDER_BASE_URL",
            "OPENAI_BASE_URL", "YUNXI_MCP_HTTP_ENDPOINT", "YUNXI_MCP_HTTP_HEADERS"
        )) {
            [void]$psi.EnvironmentVariables.Remove($name)
        }
        # WebHost validates that a provider credential exists at startup. This
        # fixed marker is synthetic, never sent to a provider, and is not a
        # user secret. No real credential is inherited by the audit child.
        $psi.EnvironmentVariables["YUNXI_PROVIDER_API_KEY"] = "acceptance-only-not-secret"

        $webProcess = [System.Diagnostics.Process]::new()
        $webProcess.StartInfo = $psi
        [void]$webProcess.Start()
        # Keep the pipe writer alive. The Web command uses stdin EOF as its
        # explicit shutdown signal, so closing this writer before cleanup would
        # make a healthy server exit during readiness polling.
        $webInput = $webProcess.StandardInput
        $webStdout = $webProcess.StandardOutput.ReadToEndAsync()
        $webStderr = $webProcess.StandardError.ReadToEndAsync()

        $ready = $false
        for ($attempt = 0; $attempt -lt 60; $attempt++) {
            Start-Sleep -Milliseconds 100
            if ($webProcess.HasExited) {
                break
            }
            try {
                $rootResponse = Invoke-WebRequest -UseBasicParsing -Uri "$baseUrl/"
                if ([int]$rootResponse.StatusCode -eq 200) {
                    $ready = $true
                    break
                }
            } catch {
                # The listener may still be starting.
            }
        }
        Assert-Condition $ready `
            "web listener accepts loopback requests on $baseUrl" `
            "web listener did not become ready on $baseUrl"

        if ($webProcess.HasExited) {
            $outText = $webStdout.GetAwaiter().GetResult()
            $errText = $webStderr.GetAwaiter().GetResult()
            if ($outText.Trim()) { Write-Host "[INFO] web stdout: $($outText.Trim())" }
            if ($errText.Trim()) { Write-Host "[INFO] web stderr: $($errText.Trim())" }
        }

        if ($ready) {
            $health = Invoke-Rpc $baseUrl "audit-health" "health.status"
            $healthJson = $health.Content | ConvertFrom-Json
            Assert-Condition ($healthJson.result.ok -eq $true) `
                "health.status returns a successful bounded RPC" `
                "health.status did not return a successful RPC"

            $inventory = Invoke-Rpc $baseUrl "audit-inventory" "pluginInventory/list"
            $inventoryJson = $inventory.Content | ConvertFrom-Json
            $entries = @($inventoryJson.result.value.entries)
            $optionalDisabled = @($entries | Where-Object {
                $_.entryId -ne "model" -and $_.enabled -eq $false
            })
            Assert-Condition ($entries.Count -ge 1) `
                "plugin inventory is available" `
                "plugin inventory is empty"
            Assert-Condition ($optionalDisabled.Count -ge 1) `
                "optional capabilities remain disabled in the audit child" `
                "optional capability defaults were not observed as disabled"
            Assert-Condition (-not ($inventory.Content -match "secret|api_key|authorization|audioData")) `
                "plugin inventory does not expose obvious secret/audio fields" `
                "plugin inventory contains a possible secret or audio field"

            $badRequest = Invoke-ExpectedHttpFailure `
                "$baseUrl/api/health.status" `
                '{"type":"not-a-client-request"}'
            $rpcRejected = ($badRequest.Status -ge 400 -and $badRequest.Status -lt 500) `
                -or ($badRequest.Status -eq 200 -and $badRequest.Content -match '"ok"\s*:\s*false')
            Assert-Condition $rpcRejected `
                "malformed RPC is rejected with an HTTP or structured RPC client error" `
                "malformed RPC was not rejected (status $($badRequest.Status))"

            $sse = Invoke-WebRequest -UseBasicParsing -Uri "$baseUrl/api/events.mux"
            $sseContentType = [string]$sse.Headers["Content-Type"]
            Assert-Condition ($sseContentType -match "text/event-stream") `
                "events.mux uses the bounded SSE content type" `
                "events.mux did not return text/event-stream"
        }

        try {
            $webInput.Close()
        } catch {
            # The process may have already exited.
        }
        if (-not $webProcess.WaitForExit(5000) -and -not $webProcess.HasExited) {
            & taskkill.exe /PID $webProcess.Id /T /F | Out-Null
            Write-Warn "web process required forced cleanup after graceful shutdown timeout"
        }
    } elseif ($SkipWeb) {
        Write-Warn "web checks skipped by request"
    }

    if ($legacy -and $legacyHashBefore) {
        $legacyHashAfter = (Get-FileHash -LiteralPath $legacy.Path -Algorithm SHA256).Hash
        Assert-Condition ($legacyHashAfter -eq $legacyHashBefore) `
            "legacy executable hash is unchanged after audit" `
            "legacy executable hash changed during audit"
    }
} catch {
    Write-Fail "audit execution error: $($_.Exception.Message)"
} finally {
    if ($webProcess -and -not $webProcess.HasExited) {
        try { $webInput.Close() } catch {}
        if (-not $webProcess.WaitForExit(2000) -and -not $webProcess.HasExited) {
            & taskkill.exe /PID $webProcess.Id /T /F | Out-Null
        }
    }
    if ($tempRoot -and (Test-Path -LiteralPath $tempRoot)) {
        if ($KeepArtifacts) {
            Write-Host "[INFO] kept audit artifacts at $tempRoot"
        } else {
            Remove-Item -LiteralPath $tempRoot -Recurse -Force -ErrorAction SilentlyContinue
        }
    }
}

Write-Host "`nAcceptance summary: $($failures.Count) failure(s), $($warnings.Count) warning(s)."
if ($warnings.Count -gt 0) {
    Write-Host "Warnings:" -ForegroundColor Yellow
    $warnings | ForEach-Object { Write-Host "  - $_" }
}
if ($failures.Count -gt 0) {
    exit 1
}
exit 0
