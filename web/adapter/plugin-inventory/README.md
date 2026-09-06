# Plugin Inventory Overlay

These files replace the pinned dsh plugin-inventory client package during the
reproducible Web import. The overlay preserves the upstream inventory search,
cards, expansion details, localization, and slot registration while adding a
YunXi capability settings scope and accessible composition-scoped switches. The
built-in capability fields include the original 13 YunXi capabilities plus
`voice` and `weixin`; their inventory entries are mapped to
`yunxi.voice.fixture` and `yunxi.channel.weixin` respectively.

No plugin process is started or stopped by browser code. A switch performs a
bounded `settings.mutate` write; WebHost rebuilds the current Rust Host, while
a standalone CLI process applies the setting on its next start.

The Gateway keeps the upstream inventory response strict and therefore omits
local manifest fields. User-installed packages are identified on that wire by
the reserved `yunxi.dynamic.` module prefix; core and built-in entries remain
read-only in this tab.
