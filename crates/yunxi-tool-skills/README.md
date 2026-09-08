# yunxi-tool-skills

This crate is the isolated, read-only Skills capability for YunXi Next.

It discovers immediate child directories containing `SKILL.md`, validates
bounded metadata, returns instruction bodies on explicit Host requests, and
projects metadata-only tool declarations. `tools.json` never contains an
executable command. An explicitly separate `actions.json` can declare a
fixed relative program, fixed arguments, timeout, output limit, and optional
workspace-write requirement, but actions are disabled by default.

For example, an action allowlist entry is shaped like:
`{"tool_name":"check","program":"bin/check.exe","arguments":["--strict"],"timeout_millis":5000,"max_output_bytes":65536}`.
There is no caller-controlled command string or caller-controlled process
argument list.

`SkillActionExecutor` is an opt-in Host-side boundary. It requires an
approved `ActionGrant`, keeps the Skill root and working directory below that
grant, rejects network and secret authority, starts the program directly with
an empty environment, and exchanges one versioned JSONL request/response.
The effective timeout and output budget are the smaller of the manifest and
grant limits. Timeout, Host cancellation, bad frame, malformed response,
crash, or output overflow affects only that action. No shell, MCP, secret, or
recursive-agent channel is provided to the child.

This is a Host admission and cooperative process boundary, not a claim of an
OS-level sandbox. A deployment that needs kernel-enforced filesystem, CPU, or
network isolation must add that platform-specific policy before enabling
actions.

Enablement is explicit with `SkillsConfig::with_actions_enabled(true)` or
`YUNXI_NEXT_SKILLS_ACTIONS_ENABLED=true`; the environment default is false.
When enabled, the CLI Host projects only matching `tools.json` and
`actions.json` entries as executable model tools. Every invocation still goes
through the normal model-tool approval flow, receives a ticketed minimal
workspace grant, observes turn cancellation, and returns a bounded audit id.
Metadata-only tools remain visible as declarations but are rejected rather
than executed.
