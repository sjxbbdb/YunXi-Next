# Acceptance Audit

This document is the security and release acceptance gate for the
`codex/complete-plugin-runtime` work. It is deliberately separate from the
runtime implementation. The audit does not edit `D:\YunXi Agent`, the old
`yunxi.exe`, PATH, the registry, or Windows services.

## Automated Entry Point

Run from a PowerShell terminal in this repository:

```powershell
.\scripts\acceptance-audit.ps1 -RunCargo
```

The default run starts the installed release binary with an ephemeral
`127.0.0.1` port and an isolated temporary `YUNXI_NEXT_HOME`. It removes only
that temporary directory and the process tree it started. Provider credentials
are removed from the audit child environment. Use `-SkipWeb` when the release
binary is not installed, or `-KeepArtifacts` when investigating a failed start.
The WebHost startup check receives the literal marker
`acceptance-only-not-secret`; it is not a real credential and the script never
sends a chat request.

The standalone CLI exposes `status`, `diagnostics`, `enable`, `disable`, and
`reload`. The first two inspect a detached/configuration view; `enable` and
`disable` persist a revision-fenced plugin choice and take effect on the next
Host launch. `reload` validates the current inventory and reports that a
restart is required. Live Web changes rebuild the attached Host and are covered
by the WebHost tests.

## Gate Matrix

| Gate | Automated evidence | Pass condition |
| --- | --- | --- |
| Branch and scope | Script checks branch and release paths | `codex/complete-plugin-runtime`; only this repository is used |
| Rust quality | `cargo fmt`, strict Clippy, workspace tests | All commands exit zero |
| Plugin defaults | Child sets all optional switches false; inventory RPC is queried | Required model remains available; optional entries are disabled and not exposed as active routes |
| Permission policy | Settings/provider docs plus disabled inventory | Enabling is the only v1 user permission control; disabled plugins receive no route/grant; no hidden default elevation |
| Secret handling | Child credential variables are cleared; inventory response is scanned; source tests cover redaction | No credential value in persisted settings, wire inventory, normal diagnostics, or audit output |
| Audio handling | Voice fixture and contract tests; source audit | Raw audio is not persisted or logged by default; device access requires a Host-issued Device grant |
| Resource bounds | Protocol, HTTP, SSE, settings, voice, Weixin and tool constants/tests | Oversized frame/body/response, timeout, buffer, transcript and retry cases fail closed |
| Fault isolation | Process host tests and CLI stack tests | Bad frame, crash, timeout and retry exhaustion remove only the failed route; kernel and sibling remain usable |
| Dynamic package lifecycle | Discovery and manager integration tests | Invalid packages are skipped, dependencies are ordered, enabled packages launch, changed packages are replaced, and removed directories unload their routes/processes |
| Old-version isolation | SHA256 before/after; explicit path checks | `D:\Apps\YunXi Agent\bin\yunxi.exe` hash is identical; no old source or binary write occurs |
| Web loopback | Script starts `web --bind 127.0.0.1:<ephemeral>` | Root, health RPC, inventory RPC, malformed RPC rejection and SSE content type pass |
| Release install | Installed executable hash and `--help`/`web --help` | Release binary exists and exposes the documented command surface |
| Browser layout | Optional Node/Playwright check or manual browser run | 1440px and 390px have no horizontal overflow, blocking overlay, console error, or unreachable plugin switch |

## Permission and Default Policy Findings

The current v1 policy is intentionally coarse: a user switches a plugin on or
off; an enabled plugin receives its declared Host grants, while a disabled
plugin is not launched and has no catalog route. Context, Persona and Storage
are safe default services. Side-effecting or externally connected capabilities
such as Shell, Patch, MCP, Skills, Multi-agent, Voice and Weixin are off by
default. Model and the Agent spine are required and are not user-disableable.

This policy is acceptable only while the following remain true:

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

- the model plugin still receives provider credentials through its child
  environment rather than an external Secret broker;
- normal process stderr and provider error text need a release-level redaction
  policy, especially when a provider includes request headers or response
  bodies in an error;
- real Voice and Weixin adapters are not present, so device identifiers, raw
  audio retention, webhook signatures, and account tokens have not passed a
  production test;
- `audioData` or arbitrary binary payloads must not be added to normal session,
  event, diagnostic, or error projections without an explicit retention policy.

These are audit risks, not evidence that the current fixture leaks a secret.

## Resource and Failure Review

The repository contains bounds for protocol frames, HTTP headers/bodies,
responses, event queues, settings, model responses, tool output, voice chunks,
transcripts, Weixin messages, and retry counts. The acceptance suite must still
exercise the process-level operating-system limits separately: CPU, resident
memory, child count, open handles, filesystem quota, and network egress are
not enforced by the current Rust process boundary. Process isolation contains
crashes; it is not an OS sandbox.

Required fault scenarios:

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
10. legacy binary hash unchanged after every release/install/Web run.

## Main-Thread Repair Queue

Ordered by acceptance impact:

1. **P0: Complete production secret handling.** Replace model-child inherited
   credentials and future Weixin/Voice secret values with a Host-controlled
   broker/reference flow, and prove that provider failures and diagnostics do
   not echo values.
2. **P0: Enforce OS-level resource policy.** Add explicit process CPU/memory,
   child-count, handle, filesystem and network controls, or document and accept
   a narrower threat model before calling the runtime sandboxed.
3. **P1: Finish real Voice adapters.** Add Host Device grants, microphone and
   speaker/provider boundaries, playable chunk streaming, cancellation and
   fallback to text. Keep the current loopback fixture as a deterministic test
   backend.
4. **P1: Finish real Weixin adapter.** Add Host Secret references, signature
   verification, login/status/serve/session lifecycle, bounded network I/O,
   retry/idempotency/ACK and local HTTP mock coverage.
5. **P1: Finish streaming and background agents.** Add token/tool progress
   streaming, in-flight cancellation propagation, background parallel workers,
   delegated child tool grants and Web graph/event projection.
6. **P2: Add executable Skill lifecycle.** Metadata-only Skills are safe, but
   they are not parity with legacy executable capabilities until they have an
   explicit process/grant/approval contract.

## Latest Gate Run

On 2026-09-06, the current source passed `cargo fmt --all -- --check`, strict
Clippy, the complete workspace test suite, and `git diff --check`. The source
also passed the dynamic discovery/manager integration tests, including
invalid-package isolation, replacement, disable, unload-on-directory-loss, and
same-ID ownership isolation. The installed release was rebuilt from this
source and passed the release/Web audit with SHA256
`E09A338C19EE9F6FDD83ED0F1A597558B40A7C3708F109EC47A89F2426E93490`.
The installed Web was additionally checked in Edge at 1440x900 and 390x844:
both views exposed 16 entries and 15 capability switches, with no horizontal
overflow, card overlap, console error, or failed request.

## Evidence to Attach Before Release

Record the following with the release commit:

- the audit script output and installed release SHA256;
- `cargo fmt --all -- --check`, strict Clippy, and workspace test summaries;
- desktop 1440px and mobile 390px browser results;
- fault-test results for crash, timeout, bad frame and three-restart exhaustion;
- dynamic package discovery, replacement, disable, and unload results;
- Voice loopback and Weixin mock results;
- old `yunxi.exe` SHA256 before and after;
- a statement of external prerequisites: provider credentials, real audio
  devices, and Weixin account/network configuration are not supplied by the
  repository and must be configured by the operator.
