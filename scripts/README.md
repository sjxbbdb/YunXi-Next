# Repository scripts

This directory contains explicit developer and installation operations that do
not belong in a Rust crate.

| File | Responsibility |
| --- | --- |
| `install-windows.ps1` | Build `yunxi-next`, install it beside Cargo, migrate prior PATH entries, and preserve legacy `yunxi` |
| `acceptance-audit.ps1` | Run no-system-change release, Web loopback, default-policy, secret-scan, and legacy-isolation checks |

`acceptance-audit.ps1` runs the repository acceptance checks without changing
system PATH, the registry, Windows services, or the legacy `D:\YunXi Agent`
project. It starts the installed release binary on an ephemeral loopback port,
uses a temporary `YUNXI_NEXT_HOME`, clears provider credentials from the audit
child, and removes only its own temporary process tree and files.

```powershell
.\scripts\acceptance-audit.ps1 -RunCargo
```

Use `-SkipWeb` to omit the release Web check and `-KeepArtifacts` to preserve
the temporary child state for diagnosis. The complete gate matrix and known
production gaps are documented in [`docs/acceptance-audit.md`](../docs/acceptance-audit.md).
