# `yunxi-launcher`

This zero-dependency executable owns the global `yunxi` command route. It
forwards `yunxi next ...` to YunXi Next and sends every other argument sequence
to the existing legacy executable.

Arguments remain operating-system strings and are passed directly to the child
process without a command shell. This preserves prompts containing `%`, quotes,
or other shell-sensitive text.

The installer writes UTF-8 route files next to the launcher:

- `yunxi-next.path` is required and identifies the YunXi Next executable.
- `yunxi-legacy.path` is optional and identifies the preserved legacy command.

See [`src/`](src/README.md) for source ownership.
