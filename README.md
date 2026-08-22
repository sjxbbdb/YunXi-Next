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

The repository now has two explicit layers.

The L0 kernel owns only the minimum lifecycle needed to run plugins safely:

- register a process-isolated plugin;
- start and observe it;
- contain an unexpected process exit;
- keep the kernel and sibling plugins running;
- stop supervised processes during shutdown.

The L1 chat surface adds:

- a versioned, size-bounded loopback plugin protocol;
- an isolated OpenAI-compatible Chat Completions plugin;
- inherited YunXi/DeepSeek/OpenAI environment configuration;
- an interactive CLI with rolling in-memory history;
- `/help`, `/status`, `/clear`, and `/quit` commands;
- `--once` mode for scripts and health checks.

API failures are returned per request and do not terminate the model plugin.
Plugin process failures remain visible until an explicit restart; the kernel
does not automatically restart a crashing plugin.

Process isolation protects the kernel from plugin crashes. It is not yet a
filesystem, network, or resource-usage sandbox.

## Run

Install the global route without replacing an existing legacy YunXi binary:

```powershell
.\scripts\install-windows.ps1
```

Open a new terminal, set a provider credential, then start the CLI:

```powershell
$env:DEEPSEEK_API_KEY = "your-key"
yunxi next
```

An already-open terminal may still hold the old PATH. Refresh that process once
before running `yunxi next`:

```powershell
$router = Join-Path $env:LOCALAPPDATA 'YunXi\bin'
$env:Path = "$router;$env:Path"
```

Send one prompt without entering interactive mode:

```powershell
yunxi next --once "你好"
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
