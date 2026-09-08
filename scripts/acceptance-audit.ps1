<##
.SYNOPSIS
Builds, installs, and locally accepts only YunXi Next.

.DESCRIPTION
This release gate reads the legacy executable and repository, but never starts
or writes them. It writes Cargo output, one private temporary install, and
private smoke homes. It never changes PATH, the registry, Windows services, or
the current checkout. Voice and Weixin external integration are manual only.
#>

[CmdletBinding()]
param(
    [string]$Workspace = '',
    [string]$LegacyExe = 'D:\Apps\YunXi Agent\bin\yunxi.exe',
    [string]$LegacyRepository = 'D:\YunXi Agent',
    [ValidatePattern('^[A-Fa-f0-9]{64}$')]
    [string]$ExpectedLegacySha256 = 'D252FD513B3B0AC846C1C36FC7DBDF6F0B514832A72310E113F5B2846B37A43F',
    [switch]$SkipWeb,
    [switch]$SkipConPty,
    [switch]$SkipClippy,
    [switch]$SkipTests,
    [switch]$SkipSourceAudit,
    [switch]$SkipPowerShell,
    [switch]$KeepArtifacts,
    [switch]$DryRun
)

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

if ([string]::IsNullOrWhiteSpace($Workspace)) { $Workspace = Split-Path -Parent $PSScriptRoot }
$workspacePath = [IO.Path]::GetFullPath($Workspace)
$legacyPath = [IO.Path]::GetFullPath($LegacyExe)
$legacyRepositoryPath = [IO.Path]::GetFullPath($LegacyRepository)
$failures = [System.Collections.Generic.List[string]]::new()
$warnings = [System.Collections.Generic.List[string]]::new()
$blocked = [System.Collections.Generic.List[string]]::new()
$smokeHome = $null
$installRoot = $null
$webHome = $null
$webProcess = $null
$webInput = $null
$legacyHashBefore = $null
$legacyFingerprintBefore = $null

$sensitiveEnvironment = @(
    'YUNXI_PROVIDER_API_KEY', 'YUNXI_PROVIDER_API_KEY_ENV',
    'DEEPSEEK_API_KEY', 'OPENAI_API_KEY', 'YUNXI_PROVIDER_BASE_URL',
    'OPENAI_BASE_URL', 'YUNXI_MCP_HTTP_ENDPOINT', 'YUNXI_MCP_HTTP_HEADERS',
    'YUNXI_MCP_HTTP_SECRETS', 'YUNXI_WEIXIN_TOKEN', 'YUNXI_WEIXIN_SECRET'
)
$optionalSwitches = @(
    'YUNXI_NEXT_CONTEXT_ENABLED', 'YUNXI_NEXT_PERSONA_ENABLED',
    'YUNXI_NEXT_MEMORY_ENABLED', 'YUNXI_NEXT_STORAGE_ENABLED',
    'YUNXI_NEXT_COMPANION_ENABLED', 'YUNXI_NEXT_MAILBOX_ENABLED',
    'YUNXI_NEXT_SCHEDULER_ENABLED', 'YUNXI_NEXT_SHELL_ENABLED',
    'YUNXI_NEXT_PATCH_ENABLED', 'YUNXI_NEXT_FILES_ENABLED',
    'YUNXI_NEXT_MCP_ENABLED', 'YUNXI_NEXT_SKILLS_ENABLED',
    'YUNXI_NEXT_MULTI_AGENT_ENABLED', 'YUNXI_NEXT_VOICE_ENABLED',
    'YUNXI_NEXT_WEIXIN_ENABLED'
)

function Write-Pass([string]$Message) { Write-Host "[PASS] $Message" -ForegroundColor Green }
function Write-Warn([string]$Message) {
    [void]$warnings.Add($Message)
    Write-Host "[WARN] $Message" -ForegroundColor Yellow
}
function Write-Blocked([string]$Message) {
    [void]$blocked.Add($Message)
    Write-Host "[BLOCKED] $Message" -ForegroundColor Yellow
}
function Write-Skip([string]$Message) { Write-Blocked $Message }
function Write-Fail([string]$Message) {
    [void]$failures.Add($Message)
    Write-Host "[FAIL] $Message" -ForegroundColor Red
}
function Assert-Condition([bool]$Condition, [string]$Pass, [string]$Fail) {
    if ($Condition) { Write-Pass $Pass } else { Write-Fail $Fail }
}

function Invoke-Cargo([string[]]$Arguments, [string]$Label) {
    if (-not (Get-Command cargo.exe -CommandType Application -ErrorAction SilentlyContinue)) {
        Write-Fail "$Label cannot run because cargo.exe is unavailable"
        throw 'cargo.exe is unavailable'
    }
    Push-Location -LiteralPath $workspacePath
    try { & cargo.exe @Arguments; $exitCode = $LASTEXITCODE }
    finally { Pop-Location }
    if ($exitCode -ne 0) {
        Write-Fail "$Label failed with exit code $exitCode"
        throw "$Label failed"
    }
    Write-Pass "$Label passed"
}

function Get-IsolatedEnvironment([string]$IsolatedHome) {
    $environment = @{}
    foreach ($entry in [Environment]::GetEnvironmentVariables().GetEnumerator()) {
        $environment[[string]$entry.Key] = [string]$entry.Value
    }
    $environment['YUNXI_NEXT_HOME'] = $IsolatedHome
    foreach ($name in $optionalSwitches) { $environment[$name] = 'false' }
    foreach ($name in $sensitiveEnvironment) { [void]$environment.Remove($name) }
    return $environment
}

function Set-ProcessEnvironment([Diagnostics.ProcessStartInfo]$StartInfo, [string]$IsolatedHome) {
    $StartInfo.EnvironmentVariables.Clear()
    foreach ($entry in (Get-IsolatedEnvironment $IsolatedHome).GetEnumerator()) {
        $StartInfo.EnvironmentVariables[[string]$entry.Key] = [string]$entry.Value
    }
}

function Invoke-CliProcess(
    [string]$BinaryPath,
    [string[]]$Arguments,
    [string]$IsolatedHome,
    [int]$TimeoutSeconds = 20
) {
    $psi = [Diagnostics.ProcessStartInfo]::new()
    $psi.FileName = $BinaryPath
    $psi.WorkingDirectory = $workspacePath
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    Set-ProcessEnvironment $psi $IsolatedHome
    foreach ($argument in $Arguments) { [void]$psi.ArgumentList.Add($argument) }

    $process = [Diagnostics.Process]::new()
    $process.StartInfo = $psi
    try {
        [void]$process.Start()
        $stdoutTask = $process.StandardOutput.ReadToEndAsync()
        $stderrTask = $process.StandardError.ReadToEndAsync()
        if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
            try { $process.Kill($true) } catch { }
            throw "child process timed out after $TimeoutSeconds seconds"
        }
        return [pscustomobject]@{
            ExitCode = $process.ExitCode
            Stdout = $stdoutTask.GetAwaiter().GetResult()
            Stderr = $stderrTask.GetAwaiter().GetResult()
        }
    }
    finally { $process.Dispose() }
}

function Invoke-CliSmoke([string[]]$Arguments, [string]$Label, [string[]]$RequiredText = @()) {
    $result = Invoke-CliProcess $releasePath $Arguments $smokeHome
    $combined = "$($result.Stdout)`n$($result.Stderr)"
    $markersPresent = @($RequiredText | Where-Object {
            $combined -notmatch [regex]::Escape($_)
        }).Count -eq 0
    Assert-Condition ($result.ExitCode -eq 0 -and $markersPresent) `
        "$Label passed" "$Label failed with exit code $($result.ExitCode) or missing marker"
}

function Get-FreeLoopbackPort {
    $listener = [Net.Sockets.TcpListener]::new([Net.IPAddress]::Loopback, 0)
    try { $listener.Start(); return ([Net.IPEndPoint]$listener.LocalEndpoint).Port }
    finally { $listener.Stop() }
}

function Invoke-Rpc([string]$BaseUrl, [string]$RpcId, [string]$Method, [hashtable]$Payload = @{}) {
    $body = @{
        type = 'client-request'
        rpcId = $RpcId
        method = $Method
        payload = $Payload
    } | ConvertTo-Json -Compress -Depth 20
    return Invoke-WebRequest -UseBasicParsing -Method Post `
        -Uri "$BaseUrl/api/$Method" -ContentType 'application/json' -Body $body
}

function Invoke-ExpectedHttpFailure([string]$Uri, [string]$Body) {
    try {
        $response = Invoke-WebRequest -UseBasicParsing -Method Post `
            -Uri $Uri -ContentType 'application/json' -Body $Body
        return [pscustomobject]@{ Status = [int]$response.StatusCode; Content = [string]$response.Content }
    }
    catch {
        if ($_.Exception.Response) {
            return [pscustomobject]@{ Status = [int]$_.Exception.Response.StatusCode; Content = '' }
        }
        throw
    }
}

function Invoke-WebSmoke {
    if ($SkipWeb) { Write-Skip 'Web smoke skipped by -SkipWeb'; return }
    $port = Get-FreeLoopbackPort
    $baseUrl = "http://127.0.0.1:$port"
    $webHome = Join-Path ([IO.Path]::GetTempPath()) ('yunxi-next-audit-web-{0}-{1}' -f $PID, [guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory -Path $webHome -Force | Out-Null
    $psi = [Diagnostics.ProcessStartInfo]::new()
    $psi.FileName = $releasePath
    $psi.WorkingDirectory = $workspacePath
    $psi.UseShellExecute = $false
    $psi.CreateNoWindow = $true
    $psi.RedirectStandardInput = $true
    $psi.RedirectStandardOutput = $true
    $psi.RedirectStandardError = $true
    Set-ProcessEnvironment $psi $webHome
    # The Web host validates that a provider is configured at startup. This
    # marker exists only in the isolated audit child and no request is sent.
    $psi.EnvironmentVariables['YUNXI_PROVIDER_API_KEY'] = 'acceptance-only-not-secret'
    [void]$psi.ArgumentList.Add('web')
    [void]$psi.ArgumentList.Add('--bind')
    [void]$psi.ArgumentList.Add("127.0.0.1:$port")
    $webProcess = [Diagnostics.Process]::new()
    $webProcess.StartInfo = $psi
    [void]$webProcess.Start()
    $webInput = $webProcess.StandardInput
    $webStdoutTask = $webProcess.StandardOutput.ReadToEndAsync()
    $webStderrTask = $webProcess.StandardError.ReadToEndAsync()
    try {
        $ready = $false
        for ($attempt = 0; $attempt -lt 80; $attempt++) {
            Start-Sleep -Milliseconds 100
            if ($webProcess.HasExited) { break }
            try {
                $root = Invoke-WebRequest -UseBasicParsing -Uri "$baseUrl/" -TimeoutSec 2
                if ([int]$root.StatusCode -eq 200) { $ready = $true; break }
            }
            catch { }
        }
        Assert-Condition $ready `
            "Web loopback listener accepts requests on $baseUrl" `
            "Web listener did not become ready on $baseUrl"
        if (-not $ready) { return }
        $health = Invoke-Rpc $baseUrl 'audit-health' 'health.status'
        $healthJson = $health.Content | ConvertFrom-Json
        Assert-Condition ($healthJson.result.ok -eq $true) `
            'Web health.status returns ok=true' 'Web health.status did not return ok=true'
        $inventory = Invoke-Rpc $baseUrl 'audit-inventory' 'pluginInventory/list'
        $inventoryJson = $inventory.Content | ConvertFrom-Json
        $entries = @($inventoryJson.result.value.entries)
        $optionalDisabled = @($entries | Where-Object {
                $_.entryId -ne 'yunxi.model.openai-compatible' -and $_.enabled -eq $false
            })
        Assert-Condition ($entries.Count -ge 1) 'Web inventory is available' 'Web inventory is empty'
        Assert-Condition ($optionalDisabled.Count -ge 1) `
            'Web optional entries remain disabled' 'Web optional entries were not disabled'
        Assert-Condition (-not ($inventory.Content -match '(?i)secret|api_key|authorization|audioData')) `
            'Web inventory contains no obvious secret/audio fields' `
            'Web inventory contains a possible secret/audio field'
        $badRequest = Invoke-ExpectedHttpFailure "$baseUrl/api/health.status" '{"type":"not-a-client-request"}'
        $rejected = ($badRequest.Status -ge 400 -and $badRequest.Status -lt 500) `
            -or ($badRequest.Status -eq 200 -and $badRequest.Content -match '"ok"\s*:\s*false')
        Assert-Condition $rejected `
            'Web malformed RPC is rejected' "Web malformed RPC was not rejected (status $($badRequest.Status))"
        $sse = Invoke-WebRequest -UseBasicParsing -Uri "$baseUrl/api/events.mux" -TimeoutSec 5
        $sseContentType = [string]$sse.Headers['Content-Type']
        Assert-Condition ($sseContentType -match 'text/event-stream') `
            'Web events.mux returns text/event-stream' 'Web events.mux returned the wrong content type'
    }
    finally {
        try { $webInput.Close() } catch { }
        if (-not $webProcess.WaitForExit(5000) -and -not $webProcess.HasExited) {
            try { $webProcess.Kill($true) } catch { }
            [void]$webProcess.WaitForExit(2000)
        }
        try { $webStdoutTask.GetAwaiter().GetResult() | Out-Null } catch { }
        try { $webStderrTask.GetAwaiter().GetResult() | Out-Null } catch { }
        $webProcess.Dispose()
    }
}

function Get-TreeFingerprint([string]$Root) {
    if (-not (Test-Path -LiteralPath $Root -PathType Container)) { return $null }
    $rootPrefix = $Root.TrimEnd('\') + '\'
    $builder = [Text.StringBuilder]::new()
    $files = Get-ChildItem -LiteralPath $Root -File -Recurse -Force |
        Where-Object { $_.FullName -notlike ($rootPrefix + '.git\*') } |
        Sort-Object FullName
    foreach ($file in $files) {
        [void]$builder.Append($file.FullName.Substring($rootPrefix.Length))
        [void]$builder.Append('|')
        [void]$builder.Append($file.Length)
        [void]$builder.Append('|')
        [void]$builder.Append($file.LastWriteTimeUtc.Ticks)
        [void]$builder.Append("`n")
    }
    return $builder.ToString()
}

function Invoke-SourceAudit {
    if ($SkipSourceAudit) { Write-Skip 'source audit skipped by -SkipSourceAudit'; return }
    $cratesPath = Join-Path $workspacePath 'crates'
    $allowedExtensions = @('.rs', '.toml', '.md', '.lock', '.gitignore')
    $files = @(Get-ChildItem -LiteralPath $cratesPath -File -Recurse -Force |
        Where-Object { $_.FullName -notmatch '\\target\\|\\.git\\' })
    $unexpected = @($files | Where-Object {
            # PowerShell exposes the extension of a dotfile such as
            # `.gitignore` as an empty string; compare its name explicitly.
            $_.Name -ne '.gitignore' -and
            $allowedExtensions -notcontains $_.Extension.ToLowerInvariant()
        })
    Assert-Condition ($unexpected.Count -eq 0) `
        'crate source tree has no unexpected generated/executable files' `
        ('crate source tree contains unexpected files: ' + (($unexpected | ForEach-Object FullName) -join ', '))
    $unsafeFiles = @($files | Where-Object {
            $_.Extension -eq '.rs' -and
            (Get-Content -LiteralPath $_.FullName -Raw) -match '(?m)^\s*(?:pub\s+)?unsafe\b|\bunsafe\s*\{'
        })
    Assert-Condition ($unsafeFiles.Count -eq 0) 'Rust source contains no unsafe item/block' `
        ('Rust source contains unsafe code: ' + (($unsafeFiles | ForEach-Object FullName) -join ', '))
    $rootCargo = Get-Content -LiteralPath (Join-Path $workspacePath 'Cargo.toml') -Raw
    Assert-Condition ($rootCargo -match 'unsafe_code\s*=\s*"forbid"') `
        'workspace lint forbids unsafe code' 'Cargo.toml does not forbid unsafe code'
}

function Invoke-FocusedAcceptanceTests {
    $cases = @(
        @{ Package = 'yunxi-plugin-host'; Test = 'process_runtime'; Filter = 'malformed_plugin_frame_isolated_from_a_healthy_sibling'; Label = 'plugin bad-frame isolation' },
        @{ Package = 'yunxi-plugin-host'; Test = 'process_runtime'; Filter = 'a_crashed_provider_is_removed_while_its_sibling_keeps_serving'; Label = 'plugin crash isolation' },
        @{ Package = 'yunxi-plugin-host'; Test = 'process_runtime'; Filter = 'timed_out_plugin_is_removed_while_a_healthy_sibling_keeps_serving'; Label = 'plugin timeout isolation' },
        @{ Package = 'yunxi-plugin-host'; Test = 'process_runtime'; Filter = 'fast_crashes_are_restarted_three_times_then_disabled'; Label = 'plugin bounded restart exhaustion' },
        @{ Package = 'yunxi-plugin-host'; Test = 'process_runtime'; Filter = 'explicit_disable_removes_routes_until_enable'; Label = 'plugin disable and re-enable' },
        @{ Package = 'yunxi-plugin-host'; Test = 'manager_runtime'; Filter = 'dynamic_reload_isolates_failure_and_supports_disable_enable_and_replace'; Label = 'plugin replace and disable lifecycle' },
        @{ Package = 'yunxi-plugin-host'; Test = 'manager_runtime'; Filter = 'failed_replacement_isolated_and_later_replacement_recovers'; Label = 'plugin failed replacement recovery' },
        @{ Package = 'yunxi-plugin-host'; Test = 'manager_runtime'; Filter = 'unload_does_not_remove_a_same_id_registration_owned_elsewhere'; Label = 'plugin unload ownership rollback' },
        @{ Package = 'yunxi-cli'; Test = 'cli_surface'; Filter = 'session_migration_command_is_explicit_read_only_and_reversible'; Label = 'migration apply and rollback' }
    )
    foreach ($case in $cases) {
        Invoke-Cargo @('test', '-p', $case.Package, '--test', $case.Test, $case.Filter, '--', '--exact') $case.Label
    }
}

function Add-ConPtyInterop {
    if ('YunXiConPtyInterop' -as [type]) { return }
    Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.IO;
using System.Runtime.InteropServices;
using System.Text;
using System.Threading;
using System.Threading.Tasks;

public static class YunXiConPtyInterop
{
    private const uint ExtendedStartupInfo = 0x00080000;
    private const uint UnicodeEnvironment = 0x00000400;
    private const uint UseStandardHandles = 0x00000100;
    private const int PseudoConsoleAttribute = 0x00020016;

    [StructLayout(LayoutKind.Sequential)] private struct Coord { public short X; public short Y; }
    [StructLayout(LayoutKind.Sequential)] private struct StartupInfo
    {
        public uint cb; public string reserved; public string desktop; public string title;
        public uint x; public uint y; public uint xSize; public uint ySize;
        public uint xCount; public uint yCount; public uint fill; public uint flags;
        public ushort show; public ushort reserved2; public IntPtr reserved3;
        public IntPtr input; public IntPtr output; public IntPtr error;
    }
    [StructLayout(LayoutKind.Sequential)] private struct StartupInfoEx
    {
        public StartupInfo StartupInfo; public IntPtr AttributeList;
    }
    [StructLayout(LayoutKind.Sequential)] private struct ProcessInfo
    {
        public IntPtr process; public IntPtr thread; public uint processId; public uint threadId;
    }

    [DllImport("kernel32.dll", SetLastError=true)] private static extern bool CreatePipe(out IntPtr read, out IntPtr write, IntPtr attrs, uint size);
    [DllImport("kernel32.dll", SetLastError=true)] private static extern int CreatePseudoConsole(Coord size, IntPtr input, IntPtr output, uint flags, out IntPtr console);
    [DllImport("kernel32.dll", SetLastError=true)] private static extern void ClosePseudoConsole(IntPtr console);
    [DllImport("kernel32.dll", SetLastError=true)] private static extern bool CloseHandle(IntPtr handle);
    [DllImport("kernel32.dll", SetLastError=true)] private static extern bool InitializeProcThreadAttributeList(IntPtr list, int count, int flags, ref IntPtr size);
    [DllImport("kernel32.dll", SetLastError=true)] private static extern bool UpdateProcThreadAttribute(IntPtr list, uint flags, IntPtr attribute, IntPtr value, IntPtr size, IntPtr previous, IntPtr returnedSize);
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] private static extern bool CreateProcess(string app, StringBuilder command, IntPtr processAttrs, IntPtr threadAttrs, bool inherit, uint flags, IntPtr environment, string directory, ref StartupInfoEx startup, out ProcessInfo info);
    [DllImport("kernel32.dll", SetLastError=true)] private static extern bool DeleteProcThreadAttributeList(IntPtr list);
    [DllImport("kernel32.dll", SetLastError=true)] private static extern uint WaitForSingleObject(IntPtr handle, uint milliseconds);
    [DllImport("kernel32.dll", SetLastError=true)] private static extern bool TerminateProcess(IntPtr handle, uint code);
    [DllImport("kernel32.dll", SetLastError=true)] private static extern bool GetExitCodeProcess(IntPtr handle, out uint exitCode);

    public static string Run(string applicationPath, string commandLine, string environmentBlock, string directory, int timeoutMilliseconds)
    {
        IntPtr inputRead = IntPtr.Zero, inputWrite = IntPtr.Zero;
        IntPtr outputRead = IntPtr.Zero, outputWrite = IntPtr.Zero;
        IntPtr pseudoConsole = IntPtr.Zero, attributeList = IntPtr.Zero, environment = IntPtr.Zero;
        ProcessInfo processInfo = new ProcessInfo();
        try
        {
            if (!CreatePipe(out inputRead, out inputWrite, IntPtr.Zero, 0)) ThrowLastError();
            if (!CreatePipe(out outputRead, out outputWrite, IntPtr.Zero, 0)) ThrowLastError();
            if (CreatePseudoConsole(new Coord { X = 120, Y = 30 }, inputRead, outputWrite, 0, out pseudoConsole) != 0) ThrowLastError();
            IntPtr attributeSize = IntPtr.Zero;
            InitializeProcThreadAttributeList(IntPtr.Zero, 1, 0, ref attributeSize);
            attributeList = Marshal.AllocHGlobal(attributeSize);
            if (!InitializeProcThreadAttributeList(attributeList, 1, 0, ref attributeSize)) ThrowLastError();
            // PROC_THREAD_ATTRIBUTE_PSEUDOCONSOLE is unusual: lpValue is the
            // HPCON value itself, not a pointer to a separately allocated
            // handle value.
            if (!UpdateProcThreadAttribute(attributeList, 0, (IntPtr)PseudoConsoleAttribute, pseudoConsole, (IntPtr)IntPtr.Size, IntPtr.Zero, IntPtr.Zero)) ThrowLastError();
            var startup = new StartupInfoEx();
            startup.StartupInfo.cb = (uint)Marshal.SizeOf<StartupInfoEx>();
            // When the host itself has redirected stdout/stderr, Windows can
            // otherwise reuse those standard-handle slots instead of routing
            // output through the ConPTY. Empty handles plus this flag make the
            // child use the pseudo console's console handles.
            startup.StartupInfo.flags = UseStandardHandles;
            startup.StartupInfo.input = IntPtr.Zero;
            startup.StartupInfo.output = IntPtr.Zero;
            startup.StartupInfo.error = IntPtr.Zero;
            startup.AttributeList = attributeList;
            environment = Marshal.StringToHGlobalUni(environmentBlock);
            if (!CreateProcess(applicationPath, new StringBuilder(commandLine), IntPtr.Zero, IntPtr.Zero, false, ExtendedStartupInfo | UnicodeEnvironment, environment, directory, ref startup, out processInfo)) ThrowLastError();
            // The endpoints passed to CreatePseudoConsole must remain valid
            // until the child is attached, then the host releases its copies.
            CloseHandle(inputRead); inputRead = IntPtr.Zero;
            CloseHandle(outputWrite); outputWrite = IntPtr.Zero;
            // Keep a dedicated synchronous reader on the ConPTY pipe. The
            // Windows console documentation recommends draining this channel
            // independently of process waiting to avoid losing final frames.
            var outputTask = Task.Run(() =>
            {
                using var stream = new FileStream(new Microsoft.Win32.SafeHandles.SafeFileHandle(outputRead, false), FileAccess.Read, 4096, false);
                using var buffer = new MemoryStream();
                stream.CopyTo(buffer);
                return Encoding.UTF8.GetString(buffer.ToArray());
            });
            if (WaitForSingleObject(processInfo.process, (uint)timeoutMilliseconds) != 0)
            {
                TerminateProcess(processInfo.process, 1);
                WaitForSingleObject(processInfo.process, 2000);
                throw new TimeoutException("ConPTY child process timed out");
            }
            if (!GetExitCodeProcess(processInfo.process, out uint exitCode)) ThrowLastError();
            if (exitCode != 0) throw new InvalidOperationException("ConPTY child exited with code " + exitCode);
            // ConPTY writes the child's final console records asynchronously
            // after the process handle is signalled. Give that bounded drain
            // window a chance to reach the output pipe before closing HPCON.
            Thread.Sleep(500);
            ClosePseudoConsole(pseudoConsole);
            pseudoConsole = IntPtr.Zero;
            if (!outputTask.Wait(5000)) throw new TimeoutException("ConPTY output drain timed out");
            return outputTask.GetAwaiter().GetResult();
        }
        finally
        {
            if (processInfo.thread != IntPtr.Zero) CloseHandle(processInfo.thread);
            if (processInfo.process != IntPtr.Zero) CloseHandle(processInfo.process);
            if (pseudoConsole != IntPtr.Zero) ClosePseudoConsole(pseudoConsole);
            if (attributeList != IntPtr.Zero) { DeleteProcThreadAttributeList(attributeList); Marshal.FreeHGlobal(attributeList); }
            if (environment != IntPtr.Zero) Marshal.FreeHGlobal(environment);
            if (inputRead != IntPtr.Zero) CloseHandle(inputRead);
            if (inputWrite != IntPtr.Zero) CloseHandle(inputWrite);
            if (outputRead != IntPtr.Zero) CloseHandle(outputRead);
            if (outputWrite != IntPtr.Zero) CloseHandle(outputWrite);
        }
    }

    private static void ThrowLastError() { throw new Win32Exception(Marshal.GetLastWin32Error()); }
}
'@
}

function ConvertTo-EnvironmentBlock([hashtable]$Environment) {
    $lines = @($Environment.GetEnumerator() | Sort-Object Key | ForEach-Object { "{0}={1}" -f $_.Key, $_.Value })
    return (($lines -join "`0") + "`0`0")
}

function Invoke-ConPtySmoke {
    if ($SkipConPty) { Write-Skip 'ConPTY smoke skipped by -SkipConPty'; return }
    if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) { Write-Fail 'ConPTY smoke requires Windows'; return }
    try {
        Add-ConPtyInterop
        $environment = Get-IsolatedEnvironment $smokeHome
        $quoted = '"' + $releasePath.Replace('"', '\"') + '" --version'
        $output = [YunXiConPtyInterop]::Run($releasePath, $quoted, (ConvertTo-EnvironmentBlock $environment), $workspacePath, 15000)
        $normalizedOutput = [string]$output -replace '[\x00-\x1F\x7F]', ' '
        Assert-Condition ($normalizedOutput -match 'yunxi-next\s+0\.1\.0') `
            'ConPTY launches yunxi-next and captures the version surface' `
            ("ConPTY output did not contain the expected version (length {0}, sample: {1})" -f $normalizedOutput.Length, $normalizedOutput.Trim())
    }
    catch { Write-Fail "ConPTY smoke failed: $($_.Exception.Message)" }
}

function Invoke-ExternalManualChecklist {
    Write-Blocked 'Voice external integration is manual: real sidecar, microphone/speaker permissions, codec, cancellation, backpressure, crash/timeout recovery, and text fallback.'
    Write-Blocked 'Weixin external integration is manual: test account, QR/login, network, signing/encryption, duplicate/retry/ACK, media, reconnect, logout, and redaction.'
    Write-Blocked 'OS resource controls and authenticated/non-loopback Web deployment are outside this local gate.'
}

function Remove-GeneratedDirectory([string]$Path) {
    if ([string]::IsNullOrWhiteSpace($Path)) { return }
    $tempPrefix = [IO.Path]::GetFullPath([IO.Path]::GetTempPath()).TrimEnd('\') + '\yunxi-next-audit-'
    $fullPath = [IO.Path]::GetFullPath($Path)
    if (($fullPath -like ($tempPrefix + '*')) -and (Test-Path -LiteralPath $fullPath -PathType Container)) {
        Remove-Item -LiteralPath $fullPath -Recurse -Force -ErrorAction SilentlyContinue
    }
}

try {
    $expectedWorkspace = [IO.Path]::GetFullPath('D:\YunXi Next')
    Assert-Condition ($workspacePath -eq $expectedWorkspace) `
        'audit is scoped to D:\YunXi Next' "audit workspace is outside D:\YunXi Next: $workspacePath"
    if ($workspacePath -ne $expectedWorkspace) { throw 'workspace scope precondition failed' }
    Assert-Condition (Test-Path -LiteralPath $legacyPath -PathType Leaf) `
        "legacy executable exists at $legacyPath" "legacy executable is missing: $legacyPath"
    if (-not (Test-Path -LiteralPath $legacyPath -PathType Leaf)) { throw 'legacy executable precondition failed' }
    Assert-Condition ($legacyPath -ne $workspacePath -and $legacyPath -notlike ($workspacePath + '\*')) `
        'legacy executable is outside the Next workspace' 'legacy executable overlaps the Next workspace'
    $legacyHashBefore = (Get-FileHash -LiteralPath $legacyPath -Algorithm SHA256).Hash.ToUpperInvariant()
    Write-Host "[INFO] legacy SHA256 before: $legacyHashBefore"
    Assert-Condition ($legacyHashBefore -eq $ExpectedLegacySha256.ToUpperInvariant()) `
        'legacy SHA256 matches the protected baseline' `
        "legacy SHA256 mismatch: expected $ExpectedLegacySha256, got $legacyHashBefore"
    if (Test-Path -LiteralPath $legacyRepositoryPath -PathType Container) {
        $legacyFingerprintBefore = Get-TreeFingerprint $legacyRepositoryPath
        Write-Pass 'legacy repository fingerprint captured read-only'
    }
    else { Write-Warn "legacy repository fingerprint unavailable: $legacyRepositoryPath" }

    if ($DryRun) {
        Write-Pass 'dry-run validated workspace, legacy path, and SHA256 scope without build/install/process launch'
        Write-Host '[DRY-RUN] would run PowerShell safety, fmt, clippy, tests, release build, safe install, CLI/Web/ConPTY smoke, and focused plugin tests'
    }
    else {
        if ($SkipPowerShell) { Write-Skip 'PowerShell parser/safety check skipped by -SkipPowerShell' }
        else {
            & (Join-Path $PSScriptRoot 'test-powershell.ps1')
            if ($LASTEXITCODE -ne 0) { throw 'PowerShell parser/safety check failed' }
            Write-Pass 'PowerShell parser/safety check passed'
        }
        Invoke-SourceAudit
        Invoke-Cargo @('fmt', '--all', '--', '--check') 'cargo fmt --check'
        if ($SkipClippy) { Write-Skip 'strict Clippy skipped by -SkipClippy' }
        else { Invoke-Cargo @('clippy', '--workspace', '--all-targets', '--', '-D', 'warnings') 'strict Clippy' }
        if ($SkipTests) { Write-Skip 'workspace tests skipped by -SkipTests' }
        else { Invoke-Cargo @('test', '--workspace', '--all-targets') 'workspace tests' }
        Invoke-FocusedAcceptanceTests
        Invoke-Cargo @('build', '-p', 'yunxi-cli', '--bin', 'yunxi-next', '--release', '--locked') 'yunxi-next release build'
        $sourceRelease = [IO.Path]::GetFullPath((Join-Path $workspacePath 'target\release\yunxi-next.exe'))
        Assert-Condition (Test-Path -LiteralPath $sourceRelease -PathType Leaf) `
            'release binary exists under target\release' 'release binary is missing under target\release'
        $releaseSourceHash = (Get-FileHash -LiteralPath $sourceRelease -Algorithm SHA256).Hash
        $installRoot = Join-Path ([IO.Path]::GetTempPath()) ('yunxi-next-audit-install-{0}-{1}' -f $PID, [guid]::NewGuid().ToString('N'))
        $installBin = Join-Path $installRoot 'bin'
        New-Item -ItemType Directory -Path $installBin -Force | Out-Null
        & (Join-Path $PSScriptRoot 'install-windows.ps1') -InstallBin $installBin -SkipBuild
        if ($LASTEXITCODE -ne 0) { throw 'safe yunxi-next install failed' }
        $releasePath = Join-Path $installBin 'yunxi-next.exe'
        Assert-Condition ((Get-FileHash -LiteralPath $releasePath -Algorithm SHA256).Hash -eq $releaseSourceHash) `
            'installed yunxi-next hash matches target release' 'installed yunxi-next hash differs from target release'
        $smokeHome = Join-Path ([IO.Path]::GetTempPath()) ('yunxi-next-audit-smoke-{0}-{1}' -f $PID, [guid]::NewGuid().ToString('N'))
        New-Item -ItemType Directory -Path $smokeHome -Force | Out-Null
        Invoke-CliSmoke @('--version') 'CLI --version' @('yunxi-next')
        Invoke-CliSmoke @('--help') 'CLI --help' @('status', 'web', 'tui')
        Invoke-CliSmoke @('web', '--help') 'CLI Web --help' @('--bind')
        Invoke-CliSmoke @('status', '--json') 'CLI status --json' @('"ok"')
        Invoke-CliSmoke @('diagnostics', '--json') 'CLI diagnostics --json' @('"ok"')
        Invoke-CliSmoke @('controls', 'status', '--json') 'CLI controls status --json' @('"ok"')
        Invoke-WebSmoke
        Invoke-ConPtySmoke
        $legacyHashAfter = (Get-FileHash -LiteralPath $legacyPath -Algorithm SHA256).Hash.ToUpperInvariant()
        Write-Host "[INFO] legacy SHA256 after:  $legacyHashAfter"
        Assert-Condition ($legacyHashAfter -eq $legacyHashBefore) `
            'legacy SHA256 is unchanged after build/install/smoke' 'legacy SHA256 changed during acceptance'
        if ($legacyFingerprintBefore) {
            $legacyFingerprintAfter = Get-TreeFingerprint $legacyRepositoryPath
            Assert-Condition ($legacyFingerprintAfter -ceq $legacyFingerprintBefore) `
                'legacy repository fingerprint is unchanged' 'legacy repository fingerprint changed'
        }
    }
    Invoke-ExternalManualChecklist
}
catch { Write-Fail "audit execution error: $($_.Exception.Message)" }
finally {
    if ($webProcess -and -not $webProcess.HasExited) {
        try { $webInput.Close() } catch { }
        if (-not $webProcess.WaitForExit(2000) -and -not $webProcess.HasExited) {
            try { $webProcess.Kill($true) } catch { }
        }
    }
    if (-not $KeepArtifacts) {
        Remove-GeneratedDirectory $webHome
        Remove-GeneratedDirectory $smokeHome
        Remove-GeneratedDirectory $installRoot
    }
    elseif ($installRoot -or $smokeHome -or $webHome) {
        Write-Host '[INFO] kept generated audit artifacts under the system temp directory'
    }
}

Write-Host "`nAcceptance summary: $($failures.Count) failure(s), $($warnings.Count) warning(s), $($blocked.Count) manual/excluded item(s)."
if ($warnings.Count -gt 0) { $warnings | ForEach-Object { Write-Host "  - $_" } }
if ($blocked.Count -gt 0) { $blocked | ForEach-Object { Write-Host "  - $_" } }
if ($failures.Count -gt 0) { exit 1 }
exit 0
