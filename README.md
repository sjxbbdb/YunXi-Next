# YunXi Next

YunXi Next is a Rust rebuild of YunXi around a small trusted kernel and
process-isolated capabilities. The current baseline is deliberately narrow but
usable: it can call an OpenAI-compatible chat API from an isolated plugin and
present a persistent terminal conversation.

## Repository Boundary

This repository is intentionally independent from the legacy project at
`D:\YunXi Agent`. The legacy project remains a read-only reference and a
runnable fallback while the new architecture is developed and verified.

## Current Baseline

The repository now has three explicit layers.

The L0 kernel owns only the minimum lifecycle needed to run plugins safely:

- register a process-isolated plugin;
- start and observe it;
- contain an unexpected process exit;
- keep the kernel and sibling plugins running;
- stop supervised processes during shutdown.

The L1 capability platform adds:

- validated, versioned capability declarations;
- generic typed invocations over a size-bounded loopback protocol;
- a provider catalog outside the trusted kernel;
- deterministic rejection of missing or ambiguous capability routes.

The first connected capability and CLI surface add:

- an isolated OpenAI-compatible Chat Completions plugin using `model.chat@1`;
- inherited YunXi/DeepSeek/OpenAI environment configuration;
- an interactive CLI with rolling in-memory history;
- `/help`, `/status`, `/clear`, and `/quit` commands;
- `--once` mode for scripts and health checks.

API failures are returned per request and do not terminate the model plugin.
Plugin process failures remain visible until an explicit restart; the kernel
does not automatically restart a crashing plugin.

Process isolation protects the kernel from plugin crashes. It is not yet a
filesystem, network, or resource-usage sandbox.

The inheritance plan for persona, memory, companion behavior, tools, Weixin,
voice, and storage is tracked in
[`docs/capability-migration.md`](docs/capability-migration.md). A capability is
counted as migrated only after it has a real process boundary, versioned
contract, explicit grants, and failure-containment tests.

## Run

Install the independent `yunxi-next` command beside Cargo without replacing an
existing legacy YunXi binary:

```powershell
.\scripts\install-windows.ps1
```

Open a new terminal, set a provider credential, then start the CLI:

```powershell
$env:DEEPSEEK_API_KEY = "your-key"
yunxi-next
```

The installer uses the command directory that already contains `cargo.exe`, so
terminals with Cargo available can resolve `yunxi-next` immediately. If needed,
refresh that directory in the current process once:

```powershell
$commandBin = Split-Path (Get-Command cargo.exe).Source
$env:Path = "$commandBin;$env:Path"
```

Send one prompt without entering interactive mode:

```powershell
yunxi-next --once "你好"
```

See [`docs/provider-configuration.md`](docs/provider-configuration.md) for
custom OpenAI-compatible endpoints and the complete resolution order. The
runtime does not load `.env` files automatically and never writes credentials
to repository files or local protocol messages.

## Repository Map

- [`crates/`](crates/README.md) contains production Rust packages.
- [`docs/`](docs/README.md) contains architecture and repository maps.
- [`AGENTS.md`](AGENTS.md) defines repository boundaries and verification rules.
- [`docs/project-structure.md`](docs/project-structure.md) explains every tracked
  directory and source file.
- [`docs/chat-runtime.md`](docs/chat-runtime.md) explains the end-to-end process
  and failure boundaries.

## Verify

```text
cargo fmt --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
cargo build --workspace --release
```
