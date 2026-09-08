# YunXi Next CLI

`yunxi-next` keeps the legacy prompt entry point and adds explicit control and
management commands. The default invocation is still a human-readable REPL:

```text
yunxi-next
yunxi-next "summarize this workspace"
yunxi-next run --once "summarize this workspace"
```

The management surface is provider-free and can emit JSON:

```text
yunxi-next status [--json]
yunxi-next diagnostics [--json]
yunxi-next sessions list|show <id>|archive <id>|unarchive <id>|pin <id>|unpin <id>|fork <id>
yunxi-next persona status|profile [id]|list|import <file>|set <id>|on|off
yunxi-next memory status|list|show <id>|search <text ...>|pending|approve <id>|reject <id>|delete <id>|clear --workspace --confirm|on|off
yunxi-next companion status|check <text ...>|on|off
yunxi-next controls status|enable <name>|disable <name>
yunxi-next voice status|doctor|devices|transcribe <text>|speak <text>|chat <text>|talk <text>
yunxi-next weixin status|doctor|login|poll-login|serve|pair|session|logout
```

The `voice` and `weixin` forms are management facades with explicit modes. Voice
uses a deterministic loopback provider unless `YUNXI_VOICE_SIDECAR_PROGRAM`
selects an external JSONL sidecar; the sidecar is still responsible for real
devices, codecs, and provider authentication. Weixin uses loopback by default;
`YUNXI_WEIXIN_MODE=production` selects the HTTPS iLink control plane and
encrypted local SecretStore, but QR confirmation, account state, and network
operation remain operator-controlled. When Voice is enabled, the Host route
uses the same sidecar selection and requires a `Device` declaration; without a
sidecar it remains deterministic loopback. The Weixin Host entry uses the same
loopback or explicitly configured production iLink control plane as the
management commands; real account and media acceptance remains external.

Session options apply to `run`, `tui`, and `web` without changing the parent
process environment:

```text
--cwd <PATH>
--provider <NAME>
--model <NAME>
--approval never|on-request|on-failure|untrusted
--sandbox read-only|workspace-write|danger-full-access
```

The Rust TUI is started with `yunxi-next tui`. Its management input is backed
by the same Host facade and supports the legacy operator surface:
`/status`, `/cwd`, `/session`, `/plugins`, `/tools`, `/mcp`, `/model [name]`,
`/provider [name]`, `/sessions`, `/memory`, `/persona`, `/companion`,
`/controls`, `/voice`, `/migrate`, `/new`, `/resume <id>`, `/approve`,
`/deny`, `/cancel`, `/clear`, and `/quit`. Backend replacement through
`/model` or `/provider` is explicit and keeps the current transcript in memory;
the active turn remains cancellable with `Ctrl-C`.

`run --json` emits one structured result. `run --jsonl` emits stream events,
warnings, and a final result record. `status`, `doctor`, `diagnostics`, and the
management command families are provider-free paths; they accept `--json`.
Metadata commands reject `--jsonl` so their output format cannot be mistaken
for a turn event stream. `memory clear` is workspace-scoped and requires both
`--workspace` and `--confirm`; without them it performs no write.

Voice and Weixin commands call the existing crate APIs through bounded
transports. Voice reports `productionReady: false` even for a configured
sidecar until its external readiness is established. Weixin production mode
reports the control-plane state, but a `productionReady` value is not evidence
that the account has completed an external QR confirmation or that the full
YunXi channel/session integration has passed.

Legacy files are read for compatibility only. Next writes go to the Next home
and never overwrite `D:\YunXi Agent`. `migrate sessions plan|apply|rollback` is
an explicit workspace/user-home migration path; normal session resume can also
perform a bounded import-on-first-write into a new Next record.

Legacy JSONL event logs have a separate, explicit read-only/reversible path:
`migrate events status|replay|plan|apply <.yunxi/sessions/file.jsonl>` and
`migrate events rollback <source> <migration-id>`. Imports normalize bounded
records into `.yunxi-next/migrations/events`, redact secret-bearing fields, and
never replace the source or an existing Next target.
