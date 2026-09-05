# yunxi-settings

Bounded persistence for YunXi Next user-editable runtime composition settings.
The crate owns capability defaults, environment-variable precedence, schema
validation, revision fencing, backup recovery, and same-directory atomic file
replacement.

The settings document contains thirteen built-in capability booleans only. It never stores provider
credentials, plugin executable paths, arbitrary JSON configuration, or legacy
YunXi state. Writes take effect when the next `yunxi-next` Host starts.

## State file

The document is stored at `settings.json` beneath `YUNXI_NEXT_HOME`, or beneath
the normal `~/.yunxi-next` state root when that variable is absent. The format
is versioned and rejects unknown fields. `settings.update`,
`settings.replace`, and `settings.mutate` use the stored revision as an
optional optimistic-concurrency fence. Explicit `YUNXI_NEXT_*_ENABLED`
variables override stored values for the current process, providing a CI and
recovery path without rewriting the document.

## Layout

- `src/capabilities.rs` owns capability names, defaults, and environment
  overrides.
- `src/store.rs` owns bounded loading, revisioned edits, backup recovery, and
  atomic persistence.
- `src/lib.rs` is the public facade.
