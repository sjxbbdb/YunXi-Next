# YunXi Web

This directory owns the browser surface embedded by `yunxi-web-gateway`.
It is intentionally outside the Rust kernel and plugin processes.

The UI is a source-level compatibility fork of the DeepSeek Harness Web
client pinned in `UPSTREAM.md`. The upstream React shell, three-column layout,
CSS token system, module loader, and client-plugin graph remain structurally
intact. YunXi-owned differences are limited to the transport adapter and
the capability-switch Plugins tab overlay, plus import-time product-title
substitutions.

## Layout

- `adapter/` contains the small, reviewable YunXi transport and Plugins tab
  overrides.
- `scripts/` contains the reproducible import operation.
- `dist/` is generated browser output embedded into `yunxi-next`.
- `LICENSE.deepseek-harness` retains the upstream MIT license.
- `THIRD_PARTY_NOTICES.deepseek-harness.md` retains upstream dependency notices.
- `UPSTREAM.md` pins the reviewed source and explains the reuse boundary.

`dist/` contains no provider credentials and no plugin executables. Client
plugin JavaScript still runs only in the browser; Rust capability plugins stay
under `yunxi-plugin-host` process supervision.

## Rebuild

From the repository root:

```powershell
.\web\scripts\import-dsh-web.ps1
```

The script clones the pinned checkout into a temporary worktree, applies the
YunXi adapter, runs the frozen upstream build, starts the upstream Web host only
long enough to materialize its generated boot manifest, and imports the shell
plus every referenced client bundle. It never edits the reference checkout.
