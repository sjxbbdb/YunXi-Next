# yunxi-tool-patch

Process-isolated `tool.patch@1` provider. It parses the bounded YunXi patch
format, validates every path below the host-granted workspace, prepares all
file results before writing, and rolls earlier writes back if a later write
fails.

The plugin only runs with an explicit host approval and workspace-write grant.
It does not execute shell commands, follow patch paths outside the workspace,
or persist patch text in YunXi state.
