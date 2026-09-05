# yunxi-settings

Bounded persistence for YunXi Next user-editable runtime composition settings.
The crate owns capability defaults, environment-variable precedence, simple
plugin on/off overrides, schema validation, revision fencing, backup recovery,
and same-directory atomic file replacement.

The settings document keeps the fifteen built-in capability booleans and may
also contain a bounded `plugins` object keyed by plugin id. Plugin entries are
only user intent: an unknown but valid id can be retained for a later install,
but this crate does not load code or make an unknown id active. It never stores
provider credentials, plugin executable paths, arbitrary JSON configuration, or
legacy YunXi state. Writes take effect when the next `yunxi-next` Host starts.

## State file

The document is stored at `settings.json` beneath `YUNXI_NEXT_HOME`, or beneath
the normal `~/.yunxi-next` state root when that variable is absent. The format
is versioned and rejects unknown fields. `settings.update`,
`settings.replace`, and `settings.mutate` use the stored revision as an
optional optimistic-concurrency fence. Explicit `YUNXI_NEXT_*_ENABLED`
variables override stored values for the current process, providing a CI and
recovery path without rewriting the document.

`set_plugin`, `unset_plugin`, `update_plugins`, `replace_plugins`, and
`mutate_plugins` use the same revision fence and atomic replacement path. A
plugin switch is intentionally coarse: it is an enable/disable preference,
not a fine-grained permission tree or an OS sandbox.

The built-in `voice` and `weixin` switches are disabled by default because they
represent external capabilities. They can be changed through the same
capability `get`/`set`/`merge` APIs or with `YUNXI_NEXT_VOICE_ENABLED` and
`YUNXI_NEXT_WEIXIN_ENABLED` for the current process. Omitting either field from
an older settings document keeps the default disabled state.

## Layout

- `src/capabilities.rs` owns capability names, defaults, and environment
  overrides.
- `src/store.rs` owns bounded loading, revisioned capability/plugin edits,
  backup recovery, and atomic persistence.
- `src/lib.rs` is the public facade.
