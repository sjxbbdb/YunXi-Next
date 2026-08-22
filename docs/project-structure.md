# Project Structure

This is the canonical ownership map for the tracked repository. Generated
directories such as `.git/` and `target/` are intentionally excluded.

```text
YunXi Next/
|-- .gitattributes                     text normalization policy
|-- .gitignore                         generated and local-only file policy
|-- AGENTS.md                          contributor and agent constraints
|-- Cargo.lock                         reproducible workspace dependency lock
|-- Cargo.toml                         members, versions, dependencies, and lints
|-- README.md                          project purpose, setup, and entry points
|-- scripts/                           explicit build and installation operations
|   |-- README.md                      script index and ownership
|   `-- install-windows.ps1            `yunxi-next` Cargo command-bin installer
|-- docs/                              repository-wide design documents
|   |-- README.md                      documentation index
|   |-- capability-migration.md        legacy capability ledger and acceptance gates
|   |-- chat-runtime.md                end-to-end chat process and failure flow
|   |-- kernel-architecture.md         kernel trust and lifecycle design
|   |-- project-structure.md           this ownership map
|   `-- provider-configuration.md      provider environment resolution
`-- crates/                            production Rust packages
    |-- README.md                      workspace crate and dependency index
    |-- yunxi-cli/                     user-facing terminal chat host
    |   |-- Cargo.toml                 CLI package and `yunxi-next` binary
    |   |-- README.md                  crate scope and layout
    |   |-- src/                       terminal host implementation
    |   |   |-- README.md              source file index
    |   |   |-- args.rs                command-line parser
    |   |   |-- lib.rs                 CLI coordinator and public entry point
    |   |   |-- main.rs                executable and private child mode switch
    |   |   |-- repl.rs                commands and bounded chat history
    |   |   |-- session.rs             kernel, catalog, and model-plugin host session
    |   |   `-- ui.rs                  ANSI-aware compact presentation
    |   `-- tests/                     complete process-path tests
    |       |-- README.md              integration test purpose
    |       `-- chat_stack.rs          CLI-to-plugin-to-HTTP verification
    |-- yunxi-kernel/                  minimal trusted plugin kernel
    |   |-- Cargo.toml                 dependency-free kernel package
    |   |-- README.md                  crate scope and exclusions
    |   |-- src/                       production kernel source
    |   |   |-- README.md              source dependency map
    |   |   |-- error.rs               public operation errors
    |   |   |-- lib.rs                 stable public facade
    |   |   |-- bin/                   executable entry points
    |   |   |   |-- README.md          binary ownership rules
    |   |   |   `-- yunxi-kernel.rs    empty-kernel smoke executable
    |   |   |-- plugin/                plugin-domain value types
    |   |   |   |-- README.md          plugin file index
    |   |   |   |-- command.rs         process launch description
    |   |   |   |-- id.rs              identifier and validation
    |   |   |   |-- mod.rs             plugin module facade
    |   |   |   |-- spec.rs            registration description
    |   |   |   `-- state.rs           lifecycle state and snapshots
    |   |   |-- runtime/               registry and coordination
    |   |   |   |-- README.md          runtime file index
    |   |   |   |-- kernel.rs          lifecycle coordinator
    |   |   |   |-- mod.rs             runtime module facade
    |   |   |   `-- snapshot.rs        aggregate kernel snapshots
    |   |   `-- supervision/           private OS process boundary
    |   |       |-- README.md           supervision file index
    |   |       |-- mod.rs              private supervisor facade
    |   |       `-- process.rs          child process supervisor
    |   `-- tests/                      kernel boundary integration tests
    |       |-- README.md               integration test index
    |       `-- process_isolation.rs    real-process crash isolation
    |-- yunxi-model-openai/             first model capability plugin
    |   |-- Cargo.toml                  HTTP and protocol dependencies
    |   |-- README.md                   crate scope and layout
    |   `-- src/                        provider plugin implementation
    |       |-- README.md               source file index
    |       |-- client.rs               bounded Chat Completions HTTP client
    |       |-- config.rs               provider and credential resolution
    |       |-- lib.rs                  stable model-plugin facade
    |       |-- plugin.rs               request loop and API error containment
    |       `-- bin/                    standalone plugin entry points
    |           |-- README.md           binary purpose
    |           `-- yunxi-model-openai.rs standalone plugin executable
    |-- yunxi-plugin-host/              capability provider catalog
    |   |-- Cargo.toml                  kernel and protocol dependencies
    |   |-- README.md                   crate scope and exclusions
    |   `-- src/                        host catalog implementation
    |       |-- README.md               source file index
    |       |-- catalog.rs              provider indexing and route resolution
    |       `-- lib.rs                  stable plugin-host facade
    `-- yunxi-protocol/                 local host/plugin wire contract
        |-- Cargo.toml                  serialization-only dependencies
        |-- README.md                   crate scope and layout
        `-- src/                        protocol implementation
            |-- README.md               source file index
            |-- capability.rs           capability ids, versions, and built-in names
            |-- handshake.rs            loopback setup and readiness exchange
            |-- invocation.rs           generic typed-payload call envelopes
            |-- lib.rs                  stable protocol facade
            |-- message.rs              versioned host/plugin messages
            `-- transport.rs            bounded JSONL TCP transport
```

## Placement Rules

1. Put stable data definitions in the domain that owns their meaning.
2. Put process lifecycle in `yunxi-kernel`, not in capability crates.
3. Put wire compatibility in `yunxi-protocol`, not in the CLI or providers.
4. Keep provider HTTP details inside the model capability plugin.
5. Keep executable entry points thin and reusable behavior in libraries.
6. Add a concise `README.md` to every new non-generated directory.
7. Update this map with every tracked file or directory move.
