# yunxi-storage

Process-isolated `storage.sessions@1` provider. It writes YunXi Next session
snapshots beneath a workspace's `.yunxi-next/sessions` directory and can
project legacy `.yunxi/sessions` records without modifying them.

The crate owns session validation, bounded reads, crash-recoverable replacement,
legacy import, list/load, archive, pin, and fork behavior. Terminal commands and
model orchestration remain in `yunxi-cli`.
