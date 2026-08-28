# yunxi-tool-skills

This crate is the isolated, read-only Skills capability for YunXi Next.

It discovers immediate child directories containing `SKILL.md`, validates
bounded metadata, returns instruction bodies on explicit Host requests, and
projects metadata-only tool declarations. It never executes a Skill command,
starts a network client, applies a patch, or receives Provider credentials.

The Host owns the enabled/disabled selection, the allowed root, context
injection, and model tool projection. A malformed Skill or a crashed process
removes only this capability route.
