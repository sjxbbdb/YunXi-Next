# Repository scripts

This directory contains explicit developer and installation operations that do
not belong in a Rust crate.

| File | Responsibility |
| --- | --- |
| `install-windows.ps1` | Build or install only `yunxi-next.exe` to an explicit directory; never edits PATH, registry, services, or legacy files |
| `acceptance-audit.ps1` | Run the release gate: legacy SHA-256 protection, Rust quality, safe Next-only install, CLI/Web/ConPTY smoke, and focused plugin lifecycle tests |
| `test-powershell.ps1` | Parse every PowerShell script and statically reject PATH/registry/service/checkout/process-tree operations |

`acceptance-audit.ps1` builds only the named `yunxi-next` release binary, uses
`install-windows.ps1 -InstallBin <private-temp-bin> -SkipBuild` for a safe
Next-only installation check, and runs provider-free CLI/Web/ConPTY smoke in
isolated temporary homes. It reads and compares the SHA-256 and a read-only
tree fingerprint for `D:\YunXi Agent`; the legacy executable must exist and
match the protected baseline. No legacy executable is started. Generated
temporary directories are removed only when they are under the script's own
unique temp prefix.

```powershell
.\scripts\acceptance-audit.ps1
```

Use `-DryRun` for scope-only validation, or `-SkipClippy`, `-SkipTests`,
`-SkipWeb`, and `-SkipConPty` only for diagnosis; skipped gates are reported and
are not a release pass. `-KeepArtifacts` preserves private generated artifacts.
Voice and Weixin real-device/account checks are manual only. The complete gate
matrix and known production gaps are documented in
[`docs/acceptance-audit.md`](../docs/acceptance-audit.md).
