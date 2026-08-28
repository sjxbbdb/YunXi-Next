# DeepSeek Harness Upstream Record

- Repository: `https://github.com/deepseek-ai/deepseek-harness`
- Commit: `b150a551b8d465e31e418e1b2eaf5e79bbb7d28e`
- Version: `0.1.1-rc.2`
- License: MIT
- Copyright: `Copyright (c) 2026 DeepSeek`

The imported Web output comprises the Vite shell from `apps/web`, the browser
module loader, and the client bundles selected by the upstream `base` and
`web-app` compositions. The generated boot manifest is captured from the real
upstream Web host rather than reconstructed by string templates.

YunXi does not import the dsh Host runtime into its trusted Rust kernel. The
adapter changes only the browser event carrier from WebSocket downlinks to
bounded SSE polling, matching `yunxi-web-gateway`. All unary payloads continue
through dsh's own schema validation before reaching the YunXi HTTP contract.

Updating this commit requires rebuilding, reviewing the module roster and
license notices, rerunning browser screenshots, and passing the Rust gateway
tests before replacing the embedded output.
