# Acceptance Audit

This document is the security and release acceptance gate for YunXi Next. It is
deliberately separate from the runtime implementation. The audit may write
Cargo output and unique private temporary directories only. It does not edit
`D:\YunXi Agent`, the old `yunxi.exe`, PATH, the registry, Windows services, or
the current checkout.

## Automated Entry Point

Run from a PowerShell terminal in this repository:

```powershell
.\scripts\acceptance-audit.ps1
```

The default run executes PowerShell parser/safety checks, builds only
`target\release\yunxi-next.exe`, installs only that file into a unique private
temporary bin, then runs provider-free CLI, Web loopback, and Windows ConPTY
smoke. It also runs focused plugin bad-frame, crash, timeout, disable, bounded
restart, replacement, unload/ownership, and migration rollback tests. Child
processes receive an isolated temporary `YUNXI_NEXT_HOME` with provider
credentials removed. The script reads `D:\Apps\YunXi Agent\bin\yunxi.exe` and
the `D:\YunXi Agent` tree before and after the run and fails if either changes.
It never starts the legacy executable and never calls PATH, registry, service,
reset, checkout, or process-tree kill commands.

`-DryRun` performs only scope and legacy SHA-256 checks. `-SkipClippy`,
`-SkipTests`, `-SkipWeb`, and `-SkipConPty` are diagnostic escape hatches and
are reported as excluded items; they should not be used for a release gate.
Use `-KeepArtifacts` when investigating private temporary output.

The standalone CLI exposes `status`, `diagnostics`, `enable`, `disable`, and
`reload`. The first two inspect a detached/configuration view; `enable` and
`disable` persist a revision-fenced plugin choice and take effect on the next
Host launch. `reload` validates the current inventory and reports that a
restart is required. Live Web changes rebuild the attached Host and are covered
by the WebHost tests.

## Gate Matrix

| Gate | Automated evidence | Pass condition |
| --- | --- | --- |
| Build and scope | Script builds the named release target and safely installs one new binary in a private temp bin | `yunxi-next.exe` exists under `target\release`; installed hash matches; no legacy write |
| Rust quality | `cargo fmt`, strict Clippy, workspace tests | All commands exit zero |
| Plugin defaults | Child sets all optional switches false; inventory RPC is queried | Required model remains available; optional entries are disabled and not exposed as active routes |
| Permission policy | Settings/provider docs plus disabled inventory | Enabling is the only v1 user permission control; disabled plugins receive no route/grant; no hidden default elevation |
| Secret handling | Child credential variables are cleared; inventory response is scanned; source tests cover redaction | No credential value in persisted settings, wire inventory, normal diagnostics, or audit output |
| Audio handling | Voice fixture and contract tests; source audit | Raw audio is not persisted or logged by default; device access requires a Host-issued Device grant |
| Resource bounds | Protocol, HTTP, SSE, settings, voice, Weixin and tool constants/tests | Oversized frame/body/response, timeout, buffer, transcript and retry cases fail closed |
| Fault isolation | Process host tests and CLI stack tests | Bad frame, crash, timeout and retry exhaustion remove only the failed route; kernel and sibling remain usable |
| Dynamic package lifecycle | Discovery and manager integration tests | Invalid packages are skipped, dependencies are ordered, enabled packages launch, changed packages are replaced, and removed directories unload their routes/processes |
| Old-version isolation | SHA256 before/after; explicit path checks | `D:\Apps\YunXi Agent\bin\yunxi.exe` hash is identical; no old source or binary write occurs |
| CLI smoke | `--version`, `--help`, `web --help`, `status --json`, `diagnostics --json`, `controls status --json` | Every command exits zero and emits the expected surface/JSON marker |
| Web smoke | Loopback listener, health, inventory, malformed RPC, SSE content type | HTTP/Web carrier checks exit cleanly; optional entries remain disabled and no obvious secret/audio field is projected |
| ConPTY smoke | Windows ConPTY launches the installed binary and captures `--version` | Pseudo-terminal process exits within the bound and returns `yunxi-next 0.1.0` |
| Browser layout | Manual browser run | 1440px and 390px have no horizontal overflow, blocking overlay, console error, or unreachable plugin switch |

## Permission and Default Policy Findings

The current v1 policy is intentionally coarse: a user switches a plugin on or
off; an enabled plugin receives its declared Host grants, while a disabled
plugin is not launched and has no catalog route. Model and the Agent spine are
required and are not user-disableable. Context, Persona, Memory, Companion, and
Storage are safe default services.
Mailbox, Scheduler, Shell, Patch, Files, MCP, Skills, Multi-agent, Voice, and
Weixin are opt-in because they execute actions, reach external systems, or
request broader authority. This policy is acceptable only while the following
remain true:

1. A manifest declaration never grants itself authority.
2. Settings cannot store provider credentials, executable paths, raw audio, or
   arbitrary command configuration.
3. Environment overrides are treated as operator/CI controls and are not
   confused with a user-facing permission grant.
4. A successful Web settings update applies the new state without making the
   current session disappear. The current implementation rebuilds the Host;
   it is not an in-place hot-unmount.

## Sensitive Data Review

The current code has good local redaction tests for MCP configuration and
persisted multi-agent state, and the model/voice/weixin fixtures do not need
real credentials or devices. This is not yet a complete production guarantee:

- the Host broker is in-memory by default and optionally uses an explicitly
  keyed authenticated file store; this is an authorization/persistence boundary,
  not an OS keychain, and master keys still need an operator-managed channel;
- normal process stderr and provider error text need a release-level redaction
  policy, especially when a provider includes request headers or response
  bodies in an error;
- the Voice/Weixin Host routes select deterministic loopback unless their
  external sidecar/iLink configuration is explicit. Those same process routes
  require real device/account testing; device identifiers, raw audio retention,
  media policy, and account tokens have not passed a production test;
- `audioData` or arbitrary binary payloads must not be added to normal session,
  event, diagnostic, or error projections without an explicit retention policy.

These are audit risks, not evidence that the current fixture leaks a secret.

## Resource and Failure Review

The repository contains bounds for protocol frames, cumulative plugin invocation
output, HTTP headers/bodies,
responses, event queues, settings, model responses, tool output, voice chunks,
transcripts, Weixin messages, and retry counts. The acceptance suite must still
exercise the process-level operating-system limits separately: CPU, resident
memory, child count, open handles, filesystem quota, and network egress are
not enforced by the current Rust process boundary. Process isolation contains
crashes; it is not an OS sandbox.

The release script maps the local fault scenarios to exact test filters so the
console output is attachable evidence, rather than relying only on the aggregate
workspace test count:

1. malformed handshake/frame;
2. plugin crash during startup and during invocation;
3. plugin timeout and transport disconnect;
4. automatic restart exactly three times, then disabled until explicit enable;
5. disable removes routes and future calls;
6. model API failure leaves the kernel and sibling plugin healthy;
7. voice cancellation/backpressure and malformed audio;
8. Weixin malformed signature/payload, retry, duplicate delivery, ACK and
   cancellation;
9. Web malformed JSON, oversized body, wrong route/method, and SSE drain;
10. legacy binary hash unchanged after every release/install/Web/ConPTY run.

The focused filters are in `Invoke-FocusedAcceptanceTests` in
`scripts/acceptance-audit.ps1`. They cover malformed frames, invocation crash,
timeout, three-restart exhaustion, disable/re-enable, dynamic replacement,
failed replacement recovery, unload ownership, and migration apply/rollback.

## Remaining External/Platform Queue

Ordered by acceptance impact:

1. **External: validate real Voice hardware/provider.** The Host Device grant,
   sidecar process boundary, WAV/PCM adapter, cancellation, replacement, and
   text fallback are automated. A real microphone, speaker, codec, and provider
   credential still require operator evidence.
2. **External: validate a real Weixin account and media path.** The Host iLink
   process, encrypted SecretStore, QR/status/serve/send/reply/session controls,
   non-blocking polling, and idempotent Agent bridge are automated. Real QR
   confirmation, account policy, reconnect, and media bytes remain manual.
3. **Platform: decide OS sandbox policy.** Host limits bound plugin count,
   frames, requests, output, duration, and per-plugin concurrency. CPU, resident
   memory, handles, filesystem mediation, and network egress are not OS-enforced;
   do not describe the runtime as an OS sandbox without a platform layer.
4. **Platform: integrate an OS keychain/HSM if required.** The scoped Host broker
   supports one-shot references and optional authenticated durable storage, but
   this is not Windows Credential Manager, DPAPI, TPM, or an HSM.
5. **Product extension: remote multi-host scheduling and authenticated remote
   Web.** Local concurrent workers, exact child grants, model selection,
   interruption, persisted recovery, and sibling isolation are covered; remote
   execution and non-loopback multi-user service are outside this local release.

## External Manual Integration Checklist

The following checks require operator-owned credentials, devices, accounts, or
network policy. Fixture output, a static inventory entry, a library contract,
or a `production_ready` field does not satisfy them.

1. **Model and streaming:** configure a test provider credential outside
   `settings.json`; run `run --once`, `run --jsonl`, and the loopback Web host;
   verify text deltas, tool progress, terminal result, provider error, active
   cancellation, reconnect/cursor behavior, and absence of the credential in
   settings, protocol frames, logs, and diagnostics.
2. **Voice sidecar:** provide a real JSONL sidecar implementing sidecar protocol
   version 1; set `YUNXI_VOICE_SIDECAR_PROGRAM` and, when needed, its args,
   frame-size and timeout variables; run `voice doctor`, device enumeration,
   transcribe, synthesize, speak, and talk. Verify real microphone/speaker
   permissions, codec/playable audio, backpressure, cancellation, timeout,
   crash recovery, and text fallback. The fixture's synthetic audio does not
   count.
3. **Weixin management path:** set `YUNXI_WEIXIN_MODE=production`, a valid
   32-byte master key, account, secret-store path, and token reference; run
   login/QR polling, status/doctor, serve/send, pairing/session, ACK/cancel,
   reconnect, and logout against a test account. Verify duplicate delivery,
   retry/rate limits, signature/encryption/media requirements, and redaction.
   This validates the CLI and Host iLink control-plane boundary; local loopback
   or Host inventory evidence still does not count as an external channel
   integration.
4. **Multi-agent:** exercise CLI spawn/message/interrupt approval and failure
   behavior, then Web `subagent.list/history/prompt/interrupt` with two bounded
   child workers, streamed child events, active interruption, history, and
   restart. Confirm that child grants expose only the exact file/patch catalog
   allowed by the parent sandbox and that no provider credential enters child
   protocol or persisted graph state.
5. **Migration:** run `migrate sessions plan`, `apply`, and `rollback`; compare
   the legacy tree before/after, test source-change conflict and pre-existing
   target files, and exercise optional legacy user-home/Next-home imports. Confirm
   that unsupported opaque mailbox formats are not silently imported and that
   `D:\YunXi Agent` remains unchanged.
6. **Secret broker and sandbox boundary:** verify model secret issue/use/revoke
   and redacted failure paths. Separately document that the current in-memory
   Host broker and process supervision are not an OS keychain or OS sandbox;
   CPU/memory/handle/child-count/filesystem/network-egress controls require
   platform-specific tests or must remain outside the production claim.
7. **Web carrier:** verify loopback-only binding, malformed/oversized requests,
   finite SSE polling with cursor recovery, multi-session isolation, approval
   correlation, and the absence of authentication/non-loopback guarantees.

## Completion Boundary

The repository may claim a **local baseline** when the automated Rust/process
tests pass and fixture paths prove protocol bounds, grants, failure isolation,
streaming, migration semantics, and Web child-worker behavior. It may claim
**external integration** only after the checklist above has recorded real
provider, sidecar/device, Weixin account/network, and platform resource results.
It may claim **production-ready** only after external integration, secret
redaction review, authenticated Web policy, and OS resource controls are all
accepted by the release owner. No fixture, static inventory, or contract test
may advance a capability across those boundaries.

## Gate Record

Do not pre-fill a release result in this document. Attach the output from the
same worktree and record the commit, timestamp, installed `yunxi-next.exe`
SHA256, old `yunxi.exe` SHA256 before/after, and each focused test result. A
passing local gate is still a local baseline; it does not close the external
Voice/Weixin or OS-resource items below.

## Evidence to Attach Before Release

Record the following with the release commit:

- the audit script output and installed release SHA256;
- `cargo fmt --all -- --check`, strict Clippy, and workspace test summaries;
- desktop 1440px and mobile 390px browser results, recorded manually;
- ConPTY launch/capture result;
- fault-test results for crash, timeout, bad frame and three-restart exhaustion;
- dynamic package replacement, disable, rollback/ownership, and unload results;
- old `yunxi.exe` SHA256 before and after;
- a statement that real Voice devices/sidecar and Weixin account/network
  integration are manual and are not supplied by the repository.
