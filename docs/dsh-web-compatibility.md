# dsh Web Compatibility

YunXi Next uses DeepSeek Harness (dsh) as an architectural and Web contract
reference. The reference checkout is `D:\deepseek-harness-reference`; it is
read-only and is not a path dependency of this repository.

## Upstream Record

- Repository: `https://github.com/deepseek-ai/deepseek-harness`
- Reviewed commit: `b150a551b8d465e31e418e1b2eaf5e79bbb7d28e`
- License: MIT
- Copyright notice: `Copyright (c) 2026 DeepSeek`

The dsh source and any reused frontend package must retain the upstream MIT
notice and be tracked separately from YunXi-owned Rust code. This record pins
the behavior reviewed by the current compatibility work; future upstream
updates require a new review and commit entry.

## Reuse Boundary

The dsh Web client is a browser-side composition of modules, remotes, session
projection, settings, and React UI. YunXi can reuse that client because the
Rust side can provide the same wire contract. The first compatibility surface
is deliberately small:

- unary RPCs use `POST /api/<method>`;
- requests use `{type: "client-request", rpcId, method, payload}`;
- responses use `{type: "server-response", rpcId, result}`;
- event streams use `/api/events.mux` and `/api/events.host`;
- plugin settings can consume a `pluginInventory.list()` result shaped as
  `{entries: [{entryId, moduleName, enabled, fiberPhase}]}`;
- browser module boot uses the dsh `__DSH_BOOT__` graph and `/plugins` bundle
  routes, which remain a later gateway concern.

`yunxi-composition` owns the Rust data model for ordered bundle layers and
overlays. `yunxi-web-contract` owns the bounded RPC and event envelope types;
it does not open sockets or start a server. Neither crate loads frontend code,
spawns processes, or makes network calls. `yunxi-kernel` and
`yunxi-plugin-host` remain the owners of lifecycle, process isolation,
readiness, and capability routing. A dsh-style settings toggle therefore
changes composition state before launch; disabled entries do not execute
plugin code.

## Migration Order

1. Keep dsh's Web/client packages in a separately tracked compatibility input.
2. Implement a Rust Web Gateway for the stable RPC envelopes and event
   carriers, with bounded JSON and no credentials in messages or logs.
3. Map YunXi session and plugin inventory snapshots to dsh client contracts.
4. Add a thin TypeScript adapter only for contract differences that cannot be
   represented by the Rust gateway directly.
5. Replace dsh host services incrementally while retaining its browser UI.

The frontend is not copied into the Rust kernel. This keeps the old
`D:\YunXi Agent` fallback untouched and keeps a failed optional Web or plugin
surface from changing kernel health.
