# yunxi-storage

Process-isolated `storage.sessions@1` provider. It writes YunXi Next session
snapshots beneath a workspace's `.yunxi-next/sessions` directory and can
project legacy `.yunxi/sessions` records without modifying them.

The crate owns session validation, bounded reads, crash-recoverable replacement,
legacy import, list/load, archive, pin, and fork behavior. Terminal commands and
model orchestration remain in `yunxi-cli`.

## Legacy Event Logs

`LegacyEventLog` reads an existing newline-delimited JSON (`.jsonl`) session
event log without opening it for writing. Replay uses the physical source-line
cursor, so malformed lines are isolated and cannot cause a client to repeat a
page. The normalizer recognizes legacy session/user metadata, assistant and
reasoning messages, tool/function/MCP/command events, and turn/session
lifecycle events. It keeps only bounded, allowlisted fields; unknown fields are
listed in `dropped_fields`, and credential-like fields are listed in
`redacted_fields` without retaining their values.

`LegacyEventLogMigration` provides a read-only `plan`, a grant-checked `apply`,
and fingerprint-checked `rollback`. Normalized logs are written only under
`.yunxi-next/migrations/events`; targets are created with no-overwrite
semantics, and rollback preserves a target changed after import. The source
legacy file is never changed. The source fingerprint is a stable FNV-1a 64-bit
change detector, not a cryptographic signature.

Supported input is UTF-8 JSONL with one JSON object per line and a `type` field
(including nested `response_item.item.type` message/function records). Blank
lines and malformed/non-object lines are skipped with warnings. Binary logs,
non-JSONL files, compressed logs, and undocumented envelope formats remain
unsupported and are reported explicitly; no fixture format is treated as a
production adapter.
