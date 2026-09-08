<#
.SYNOPSIS
Runs parser and safety checks for repository PowerShell scripts.

.DESCRIPTION
This check uses only the PowerShell AST and read-only source inspection. It
does not invoke installation, PATH, registry, service, or legacy operations.
#>

[CmdletBinding()]
param()

Set-StrictMode -Version Latest
$ErrorActionPreference = 'Stop'

$repositoryRoot = Split-Path -Parent $PSScriptRoot
$failures = [System.Collections.Generic.List[string]]::new()

function Write-Pass([string]$Message) { Write-Host "[PASS] $Message" -ForegroundColor Green }
function Write-Fail([string]$Message) {
    [void]$failures.Add($Message)
    Write-Host "[FAIL] $Message" -ForegroundColor Red
}

function Assert-Condition([bool]$Condition, [string]$Pass, [string]$Fail) {
    if ($Condition) { Write-Pass $Pass } else { Write-Fail $Fail }
}

$scriptFiles = @(Get-ChildItem -LiteralPath $PSScriptRoot -Filter '*.ps1' -File | Sort-Object Name)
Assert-Condition ($scriptFiles.Count -ge 3) `
    'PowerShell test set contains the repository scripts' `
    'PowerShell test set is unexpectedly incomplete'

foreach ($scriptFile in $scriptFiles) {
    $tokens = $null
    $parseErrors = $null
    [System.Management.Automation.Language.Parser]::ParseFile(
        $scriptFile.FullName,
        [ref]$tokens,
        [ref]$parseErrors
    ) | Out-Null
    Assert-Condition (@($parseErrors).Count -eq 0) `
        "$($scriptFile.Name) parses on PowerShell $($PSVersionTable.PSVersion)" `
        "$($scriptFile.Name) has PowerShell parse errors"
}

$auditPath = Join-Path $PSScriptRoot 'acceptance-audit.ps1'
$auditText = Get-Content -LiteralPath $auditPath -Raw
$requiredPatterns = @(
    'Set-StrictMode',
    'ExpectedLegacySha256',
    'Get-TreeFingerprint',
    'Invoke-SourceAudit',
    'Invoke-ExternalManualChecklist',
    'cargo',
    'clippy',
    'test',
    'web --help',
    'events\.mux',
    'Invoke-ConPtySmoke',
    'DryRun',
    'Invoke-FocusedAcceptanceTests'
)
foreach ($pattern in $requiredPatterns) {
    Assert-Condition ($auditText -match $pattern) `
        "acceptance-audit.ps1 contains $pattern" `
        "acceptance-audit.ps1 is missing required check: $pattern"
}

$forbiddenAuditPatterns = @(
    '\[Environment\]::SetEnvironmentVariable',
    'Set-ItemProperty',
    'New-ItemProperty',
    '\bHKLM:\\',
    '\bHKCU:\\',
    '(?i)git\s+(reset|checkout)',
    '(?i)taskkill(?:\.exe)?',
    '(?i)Start-Service|Stop-Service|Restart-Service|New-Service|Remove-Service',
    'ForEach-Object\s+-Parallel',
    '\?\?',
    '\?\.'
)
foreach ($pattern in $forbiddenAuditPatterns) {
    $matches = @($scriptFiles | Where-Object {
            $_.Name -ne 'test-powershell.ps1' -and
            (Get-Content -LiteralPath $_.FullName -Raw) -match $pattern
        })
    Assert-Condition ($matches.Count -eq 0) `
        "repository scripts avoid forbidden operation pattern $pattern" `
        ("repository script contains forbidden operation pattern {0}: {1}" -f $pattern, (($matches | ForEach-Object FullName) -join ', '))
}

$cratesPath = Join-Path $repositoryRoot 'crates'
$allowedExtensions = @('.rs', '.toml', '.md', '.lock', '.gitignore')
$unexpected = @(Get-ChildItem -LiteralPath $cratesPath -File -Recurse -Force |
    Where-Object {
        $_.FullName -notmatch '\\target\\|\\.git\\' -and
        # PowerShell reports an empty Extension for dotfiles.
        $_.Name -ne '.gitignore' -and
        $allowedExtensions -notcontains $_.Extension.ToLowerInvariant()
    })
Assert-Condition ($unexpected.Count -eq 0) `
    'crate tree remains Rust-only apart from manifests and documentation' `
    ('crate tree contains unexpected executable source: ' +
    (($unexpected | ForEach-Object { $_.FullName }) -join ', '))

$unsafeFiles = @(Get-ChildItem -LiteralPath $cratesPath -Filter '*.rs' -File -Recurse -Force |
    Where-Object {
        $_.FullName -notmatch '\\target\\|\\.git\\' -and
        (Get-Content -LiteralPath $_.FullName -Raw) -match '(?m)^\s*(?:pub\s+)?unsafe\b|\bunsafe\s*\{'
    })
Assert-Condition ($unsafeFiles.Count -eq 0) `
    'crate Rust files contain no unsafe item or block' `
    ('crate Rust files contain unsafe code: ' +
    (($unsafeFiles | ForEach-Object { $_.FullName }) -join ', '))

$cargoText = Get-Content -LiteralPath (Join-Path $repositoryRoot 'Cargo.toml') -Raw
Assert-Condition ($cargoText -match 'unsafe_code\s*=\s*"forbid"') `
    'workspace Cargo.toml forbids unsafe code' `
    'workspace Cargo.toml does not forbid unsafe code'

Write-Host "`nPowerShell compatibility summary: $($failures.Count) failure(s)."
if ($failures.Count -gt 0) {
    $failures | ForEach-Object { Write-Host "  - $_" }
    exit 1
}
exit 0
