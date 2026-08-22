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
    |-- yunxi-companion/               deterministic response-policy plugin
    |   |-- Cargo.toml                 protocol-only companion package
    |   |-- README.md                  capability scope and exclusions
    |   `-- src/                        companion implementation
    |       |-- README.md               source file index
    |       |-- decision.rs             emotion, tone, and follow-up policy
    |       |-- lib.rs                  stable companion-plugin facade
    |       |-- plugin.rs               companion capability request loop
    |       `-- bin/                    standalone plugin entry point
    |           |-- README.md           binary purpose
    |           `-- yunxi-companion.rs  companion plugin executable
    |-- yunxi-companion-mailbox/        encrypted companion mailbox plugin
    |   |-- Cargo.toml                 crypto, serde, and protocol dependencies
    |   |-- README.md                  storage and encryption boundary
    |   `-- src/                        mailbox implementation
    |       |-- README.md               source file index
    |       |-- lib.rs                  stable mailbox-plugin facade
    |       |-- plugin.rs               mailbox capability request loop
    |       |-- store.rs                encrypted records, keys, and idempotency
    |       `-- bin/                    standalone plugin entry point
    |           |-- README.md           binary purpose
    |           `-- yunxi-companion-mailbox.rs mailbox plugin executable
    |-- yunxi-cli/                     user-facing multi-plugin terminal host
    |   |-- Cargo.toml                 CLI package and `yunxi-next` binary
    |   |-- README.md                  crate scope and layout
    |   |-- src/                       terminal host implementation
    |   |   |-- README.md              source file index
    |   |   |-- args.rs                command-line parser
    |   |   |-- lib.rs                 CLI coordinator and public entry point
    |   |   |-- management.rs          session, memory, and mailbox command types
    |   |   |-- main.rs                executable and built-in child mode switch
    |   |   |-- repl.rs                commands and bounded chat history
    |   |   |-- session.rs             capability launch, composition, and model routing
    |   |   |-- session/               stateful orchestration split from model routing
    |   |   |   |-- README.md          session module ownership
    |   |   |   `-- stateful.rs        storage, memory-write, scheduler, and mailbox calls
    |   |   `-- ui.rs                  ANSI-aware compact presentation
    |   `-- tests/                     complete process-path tests
    |       |-- README.md              integration test purpose
    |       `-- chat_stack.rs          CLI-to-plugin-to-HTTP verification
    |-- yunxi-context/                 AGENTS.md context capability plugin
    |   |-- Cargo.toml                 protocol-only context package
    |   |-- README.md                  capability scope, limits, and layout
    |   `-- src/                        context implementation
    |       |-- README.md               source file index
    |       |-- compose.rs              bounded root-to-cwd instruction loading
    |       |-- lib.rs                  stable context-plugin facade
    |       |-- plugin.rs               context capability request loop
    |       `-- bin/                    standalone plugin entry point
    |           |-- README.md           binary purpose
    |           `-- yunxi-context.rs    context plugin executable
    |-- yunxi-kernel/                   minimal trusted plugin kernel
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
    |-- yunxi-memory/                   recall and write capability plugin
    |   |-- Cargo.toml                 JSON, serde, and protocol dependencies
    |   |-- README.md                  legacy compatibility and exclusions
    |   `-- src/                        memory implementation
    |       |-- README.md               source file index
    |       |-- lib.rs                  stable memory-plugin facade
    |       |-- plugin.rs               memory capability request loop
    |       |-- recall.rs               boot and dynamic recall policy
    |       |-- record.rs               legacy-compatible stored record schema
    |       |-- store.rs                bounded legacy/Next reads and Next writes
    |       |-- write.rs                extraction, policy, dedup, and review
    |       `-- bin/                    standalone plugin entry point
    |           |-- README.md           binary purpose
    |           `-- yunxi-memory.rs     memory plugin executable
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
    |-- yunxi-persona/                  persona context capability plugin
    |   |-- Cargo.toml                 JSON, serde, and protocol dependencies
    |   |-- README.md                  persona trust boundary and layout
    |   `-- src/                        persona implementation
    |       |-- README.md               source file index
    |       |-- compiler.rs             bounded escaped context rendering
    |       |-- lib.rs                  stable persona-plugin facade
    |       |-- plugin.rs               persona capability request loop
    |       |-- profile.rs              built-in/custom profile and soul loading
    |       |-- settings.rs             legacy settings compatibility
    |       `-- bin/                    standalone plugin entry point
    |           |-- README.md           binary purpose
    |           `-- yunxi-persona.rs    persona plugin executable
    |-- yunxi-plugin-host/              multi-process capability host
    |   |-- Cargo.toml                  serde, kernel, and protocol dependencies
    |   |-- README.md                   crate scope and exclusions
    |   |-- src/                        catalog and process runtime
    |   |   |-- README.md               source file index
    |   |   |-- catalog.rs              provider indexing and route resolution
    |   |   |-- lib.rs                  stable plugin-host facade
    |   |   `-- runtime.rs              launch, invoke, failure removal, and shutdown
    |   `-- tests/                      process/protocol isolation tests
    |       |-- README.md               integration test purpose
    |       `-- process_runtime.rs      crashed-route and healthy-sibling verification
    |-- yunxi-scheduler/                proactive scheduling policy plugin
    |   |-- Cargo.toml                 protocol-only scheduler package
    |   |-- README.md                  scheduling boundary and exclusions
    |   `-- src/                        scheduler implementation
    |       |-- README.md               source file index
    |       |-- lib.rs                  stable scheduler-plugin facade
    |       |-- plugin.rs               scheduler capability request loop
    |       |-- policy.rs               quiet-hour, signal, and limit policy
    |       `-- bin/                    standalone plugin entry point
    |           |-- README.md           binary purpose
    |           `-- yunxi-scheduler.rs  scheduler plugin executable
    |-- yunxi-storage/                  persistent session capability plugin
    |   |-- Cargo.toml                 serde, JSON, and protocol dependencies
    |   |-- README.md                  session ownership and legacy boundary
    |   `-- src/                        session storage implementation
    |       |-- README.md               source file index
    |       |-- lib.rs                  stable storage-plugin facade
    |       |-- plugin.rs               session capability request loop
    |       |-- record.rs               Next schema and legacy projection
    |       |-- store.rs                bounded persistence and mutations
    |       `-- bin/                    standalone plugin entry point
    |           |-- README.md           binary purpose
    |           `-- yunxi-storage.rs    storage plugin executable
    `-- yunxi-protocol/                 local host/plugin wire contract
        |-- Cargo.toml                  serialization-only dependencies
        |-- README.md                   crate scope and layout
        `-- src/                        protocol implementation
            |-- README.md               source file index
            |-- capability.rs           capability ids, versions, and built-in names
            |-- companion.rs            companion policy payload contracts
            |-- grant.rs                explicit workspace-access contract
            |-- handshake.rs            loopback setup and readiness exchange
            |-- identity.rs             context, memory, and persona payload contracts
            |-- invocation.rs           generic typed-payload call envelopes
            |-- lib.rs                  stable protocol facade
            |-- mailbox.rs              encrypted mailbox payload contracts
            |-- memory_write.rs         memory extraction and review contracts
            |-- message.rs              versioned host/plugin messages
            |-- scheduler.rs            proactive scheduling payload contracts
            |-- sessions.rs             persistent-session payload contracts
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
