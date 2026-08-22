# Model plugin binaries

- `yunxi-model-openai.rs` is a standalone process entry point for hosts that
  want to launch the model plugin by path. The `yunxi-next` CLI also exposes the
  same plugin loop through a private child-process mode.
