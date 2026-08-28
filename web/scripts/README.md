# Web Import Scripts

`import-dsh-web.ps1` reproduces the pinned dsh client build in an isolated
temporary worktree, overlays the YunXi browser transport and capability tab, captures the real
Host-generated boot graph, and atomically replaces `web/dist`.

The capture runs with a temporary `DSH_HOME`, so user-installed dsh plugins
cannot enter the embedded module graph. The current pinned import contains 42
upstream client bundles and records that count in `yunxi-web-build.json`.

The script requires Git, Node.js, and pnpm. Generated source maps are omitted
from the embedded distribution; source remains available from the pinned
upstream commit and the local adapters.
