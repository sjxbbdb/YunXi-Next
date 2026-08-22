# Project Structure

This is the canonical ownership map for the tracked repository. Generated
directories such as `.git/` and `target/` are intentionally excluded.

```text
YunXi Next/
|-- .gitattributes                 text normalization policy
|-- .gitignore                     generated and local-only file policy
|-- AGENTS.md                      contributor and agent constraints
|-- Cargo.lock                     reproducible workspace dependency lock
|-- Cargo.toml                     Rust workspace members and shared policy
|-- README.md                      project purpose and entry points
|-- docs/                          repository-wide architecture documents
|   |-- README.md                  documentation index
|   |-- kernel-architecture.md     kernel trust and lifecycle design
|   `-- project-structure.md       this ownership map
`-- crates/                        production Rust packages
    |-- README.md                  workspace crate index
    `-- yunxi-kernel/              minimal trusted plugin kernel
        |-- Cargo.toml             kernel crate manifest
        |-- README.md              crate scope and exclusions
        |-- src/                   production Rust source
        |   |-- README.md          source dependency map
        |   |-- error.rs           public operation errors
        |   |-- lib.rs             stable public facade
        |   |-- bin/               executable entry points
        |   |   |-- README.md      binary ownership rules
        |   |   `-- yunxi-kernel.rs empty-kernel smoke executable
        |   |-- plugin/            plugin-domain value types
        |   |   |-- README.md      plugin file index
        |   |   |-- command.rs     process launch description
        |   |   |-- id.rs          identifier and validation
        |   |   |-- mod.rs         plugin module facade
        |   |   |-- spec.rs        registration description
        |   |   `-- state.rs       lifecycle state and snapshots
        |   |-- runtime/           registry and coordination
        |   |   |-- README.md      runtime file index
        |   |   |-- kernel.rs      lifecycle coordinator
        |   |   |-- mod.rs         runtime module facade
        |   |   `-- snapshot.rs    aggregate kernel snapshots
        |   `-- supervision/       private OS process boundary
        |       |-- README.md      supervision file index
        |       |-- mod.rs         private supervisor facade
        |       `-- process.rs     child process supervisor
        `-- tests/                 public-boundary integration tests
            |-- README.md          integration test index
            `-- process_isolation.rs real-process crash isolation
```

## Placement Rules

1. Put stable data definitions in the domain that owns their meaning.
2. Put orchestration in `runtime`, never in domain value modules.
3. Keep direct operating-system process ownership inside `supervision`.
4. Keep executable entry points thin and move reusable behavior into the library.
5. Update this map in the same change that adds, removes, or moves a tracked file.
