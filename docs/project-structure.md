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
|   |-- development-roadmap.md         ordered implementation phases and exit gates
|   |-- dsh-web-compatibility.md       dsh source record and Web wire boundary
|   |-- kernel-architecture.md         kernel trust and lifecycle design
|   |-- project-structure.md           this ownership map
|   `-- provider-configuration.md      provider environment resolution
|-- web/                               pinned dsh browser compatibility fork
|   |-- README.md                      Web ownership and rebuild entry point
|   |-- UPSTREAM.md                    reviewed repository, commit, and version
|   |-- LICENSE.deepseek-harness       retained upstream MIT license
|   |-- THIRD_PARTY_NOTICES.deepseek-harness.md upstream dependency notices
|   |-- adapter/                       YunXi-owned browser compatibility overlays
|   |   |-- README.md                  adapter scope and exclusions
|   |   |-- web-api-client.ts          bounded SSE polling carrier
|   |   `-- plugin-inventory/          capability-switch Plugins tab overlay
|   |       |-- README.md              settings binding and restart semantics
|   |       |-- index.ts               plugin-inventory client entry point
|   |       |-- locales.ts             overlay locale strings
|   |       |-- PluginInventorySettingsTab.module.css switch layout and states
|   |       `-- PluginInventorySettingsTab.tsx inventory list and capability switches
|   |-- scripts/                       reproducible upstream import operation
|   |   |-- README.md                  import requirements and isolation policy
|   |   `-- import-dsh-web.ps1         isolated build and atomic dist replacement
|   `-- dist/                          generated embedded shell and 42 client bundles
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
    |-- yunxi-composition/             pure profile layers and Web inventory projection
    |   |-- Cargo.toml                 composition crate metadata and dependencies
    |   |-- README.md                  composition scope and ownership
    |   `-- src/                       composition implementation
    |       |-- README.md              source file ownership index
    |       |-- entry.rs               validated entry ids, modules, and JSON config
    |       |-- error.rs               composition validation errors
    |       |-- inventory.rs           dsh-compatible plugin inventory projection
    |       |-- layer.rs               serializable layer operations
    |       |-- profile.rs              ordered bundle/profile/overlay composition
    |       `-- lib.rs                 public composition facade
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
    |   |   |-- web.rs                 reusable Web Host facade over ChatSession
    |   |   |-- session/               stateful orchestration split from model routing
    |   |   |   |-- README.md          session module ownership
    |   |   |   |-- multi_agent.rs     coordinator calls and isolated child-model turns
    |   |   |   |-- stateful.rs        storage, memory-write, scheduler, and mailbox calls
    |   |   |   `-- tool_loop.rs       bounded model-tool catalog and argument decoding
    |   |   `-- ui.rs                  ANSI-aware compact presentation
    |   `-- tests/                     complete process-path tests
    |       |-- README.md              integration test purpose
    |       |-- chat_stack.rs          CLI-to-plugin-to-HTTP verification
    |       `-- web_host.rs            Web session, settings, restart, and approval verification
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
    |-- yunxi-multi-agent/              isolated agent coordination plugin
    |   |-- Cargo.toml                  serde and protocol dependencies
    |   |-- README.md                   coordination boundary and exclusions
    |   |-- src/                        graph and plugin implementation
    |   |   |-- README.md               source file index
    |   |   |-- lib.rs                  stable coordinator-plugin facade
    |   |   |-- plugin.rs               handshake and typed operation dispatch
    |   |   |-- store.rs                bounded graph, transcript, budget, and persistence
    |   |   `-- bin/                    standalone plugin entry point
    |   |       |-- README.md           binary purpose
    |   |       `-- yunxi-multi-agent.rs coordinator plugin executable
    |   `-- tests/                      process-boundary coordinator tests
    |       |-- README.md               integration test ownership
    |       `-- process.rs              typed calls, persistence, restart, and secret fixture
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
    |-- yunxi-settings/                 bounded restart-scoped capability settings
    |   |-- Cargo.toml                  serde-only settings dependencies
    |   |-- README.md                   persistence, precedence, and exclusions
    |   `-- src/                        settings implementation
    |       |-- README.md               source file ownership index
    |       |-- capabilities.rs         known keys, defaults, and environment overrides
    |       |-- lib.rs                  stable settings facade
    |       `-- store.rs                versioned loading and atomic revisioned writes
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
    |-- yunxi-tool-files/              read-only workspace file tools plugin
    |   |-- Cargo.toml                  protocol-only file tool dependencies
    |   |-- README.md                   read-only scope and grant boundary
    |   `-- src/                        file tool implementation
    |       |-- README.md               source file index
    |       |-- executor.rs             bounded path traversal and UTF-8 reads
    |       |-- lib.rs                  stable file-tool plugin facade
    |       |-- plugin.rs               manifest handshake and operation dispatch
    |       `-- bin/                    standalone plugin entry point
    |           |-- README.md           binary purpose
    |           `-- yunxi-tool-files.rs file tool plugin executable
    |-- yunxi-tool-mcp/                isolated stdio/HTTP MCP bridge plugin
    |   |-- Cargo.toml                 reqwest, serde, JSON, and protocol dependencies
    |   |-- README.md                  MCP process, HTTP, and authority boundary
    |   `-- src/                        MCP bridge implementation
    |       |-- README.md              source file index
    |       |-- client.rs              bounded MCP JSON-RPC client and cancellation
    |       |-- config.rs              explicit command and environment config
    |       |-- fixture.rs              deterministic integration-test server
    |       |-- http.rs                 bounded HTTP/HTTPS JSON/SSE transport
    |       |-- lib.rs                 stable MCP-plugin facade
    |       |-- plugin.rs              YunXi handshake and typed dispatch
    |       `-- bin/                   plugin and fixture entry points
    |   `-- tests/                     external-process MCP client tests
    |       |-- README.md              integration test ownership
    |       |-- http.rs                HTTP/SSE, session, grant, and timeout fixtures
    |       `-- stdio.rs               normal, malformed, and crash fixtures
    |-- yunxi-tool-skills/             isolated read-only Skills capability
    |   |-- Cargo.toml                 serde and protocol dependencies
    |   |-- README.md                  discovery and execution boundary
    |   |-- src/                       Skills implementation
    |   |   |-- README.md              source file index
    |   |   |-- config.rs              root and disabled-id configuration
    |   |   |-- discovery.rs           bounded SKILL.md and tools.json loading
    |   |   |-- lib.rs                 stable Skills-plugin facade
    |   |   |-- plugin.rs              typed list/context/status request loop
    |   |   `-- bin/                   standalone plugin entry point
    |   |       |-- README.md           binary purpose
    |   |       `-- yunxi-tool-skills.rs Skills plugin executable
    |   `-- tests/                     process-boundary Skills tests
    |       |-- README.md              integration test ownership
    |       `-- process.rs             minimal Skill discovery fixture
    |-- yunxi-tool-patch/              Host-approved workspace patch plugin
    |   |-- Cargo.toml                  protocol and patch dependencies
    |   |-- README.md                   patch scope and approval boundary
    |   `-- src/                        patch implementation
    |       |-- README.md               source file index
    |       |-- applier.rs              bounded patch validation and application
    |       |-- lib.rs                  stable patch-plugin facade
    |       |-- plugin.rs               manifest handshake and operation dispatch
    |       `-- bin/                    standalone plugin entry point
    |           |-- README.md           binary purpose
    |           `-- yunxi-tool-patch.rs patch plugin executable
    |-- yunxi-tool-shell/              Host-approved shell plugin
    |   |-- Cargo.toml                  protocol and process-execution dependencies
    |   |-- README.md                   shell scope and approval boundary
    |   `-- src/                        shell implementation
    |       |-- README.md               source file index
    |       |-- executor.rs             bounded child-process execution
    |       |-- lib.rs                  stable shell-plugin facade
    |       |-- plugin.rs               manifest handshake and operation dispatch
    |       `-- bin/                    standalone plugin entry point
    |           |-- README.md           binary purpose
    |           `-- yunxi-tool-shell.rs shell plugin executable
    |-- yunxi-protocol/                 local host/plugin wire contract
    |   |-- Cargo.toml                  serialization-only dependencies
    |   |-- README.md                   crate scope and layout
    |   `-- src/                        protocol implementation
    |       |-- README.md               source file index
    |       |-- authority.rs             exact network and Secret grant contracts
    |       |-- capability.rs           capability ids, versions, and built-in names
    |       |-- companion.rs            companion policy payload contracts
    |       |-- files.rs                bounded file search and viewing payloads
    |       |-- grant.rs                explicit workspace-access contract
    |       |-- handshake.rs            loopback setup and readiness exchange
    |       |-- identity.rs             context, memory, and persona payload contracts
    |       |-- invocation.rs           generic typed-payload call envelopes
    |       |-- manifest.rs             plugin identity, capability, and grant declarations
    |       |-- lib.rs                  stable protocol facade
    |       |-- mailbox.rs              encrypted mailbox payload contracts
    |       |-- memory_write.rs         memory extraction and review contracts
    |       |-- message.rs              versioned host/plugin messages
    |       |-- mcp.rs                  bounded MCP list/call/status contracts
    |       |-- multi_agent.rs          bounded delegation, graph, turn, and event contracts
    |       |-- scheduler.rs            proactive scheduling payload contracts
    |       |-- sessions.rs             persistent-session payload contracts
    |       |-- skills.rs               bounded Skill metadata/context contracts
    |       |-- tool_calls.rs           versioned model tool orchestration contracts
    |       `-- transport.rs            bounded JSONL TCP transport
    |-- yunxi-web-gateway/              dsh-compatible Web dispatcher and HTTP/SSE carrier
    |   |-- Cargo.toml                  gateway metadata and inward dependencies
    |   |-- build.rs                    bounded `web/dist` static table generator
    |   |-- README.md                   gateway scope and transport boundary
    |   |-- src/                        gateway implementation
    |   |   |-- README.md               source file ownership index
    |   |   |-- assets.rs               exact embedded asset lookup and cache policy
    |   |   |-- dispatch.rs             unary method registry and dispatch
    |   |   |-- error.rs                gateway and event-buffer errors
    |   |   |-- events.rs               bounded mux/host event queues
    |   |   |-- http.rs                 bounded HTTP/1.1 parser and `/api` routes
    |   |   |-- lib.rs                  public gateway facade
    |   |   |-- projection.rs           health, inventory, and session projections
    |   |   `-- sse.rs                  bounded mux/host SSE framing
    |   `-- tests/                      bounded gateway and carrier integration tests
    |       |-- README.md              integration test purpose
    |       |-- gateway.rs             dispatcher, projection, and event tests
    |       `-- http.rs                HTTP framing, settings routes, SSE, and TCP tests
    `-- yunxi-web-contract/             dsh-compatible browser wire contract
        |-- Cargo.toml                  bounded serde contract dependencies
        |-- README.md                   Web contract scope and transport boundary
        `-- src/                        Web contract implementation
            |-- README.md               source file ownership index
            |-- bounds.rs               frame and payload limits
            |-- error.rs                Web contract validation errors
            |-- events.rs               mux and host event channels
            |-- lib.rs                  public Web contract facade
            `-- rpc.rs                  four RPC message quadrants
```

## Placement Rules

1. Put stable data definitions in the domain that owns their meaning.
2. Put process lifecycle in `yunxi-kernel`, not in capability crates.
3. Put local plugin wire compatibility in `yunxi-protocol`; put browser wire
   compatibility in `yunxi-web-contract`, not in the CLI or providers.
4. Keep provider HTTP details inside the model capability plugin.
5. Keep executable entry points thin and reusable behavior in libraries.
6. Add a concise `README.md` to every new non-generated directory.
7. Update this map with every tracked file or directory move.
