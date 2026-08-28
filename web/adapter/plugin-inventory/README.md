# Plugin Inventory Overlay

These files replace the pinned dsh plugin-inventory client package during the
reproducible Web import. The overlay preserves the upstream inventory search,
cards, expansion details, localization, and slot registration while adding a
YunXi capability settings scope and accessible restart-scoped switches.

No plugin process is started or stopped by browser code. A switch performs a
bounded `settings.mutate` write; the Rust Host applies it only on its next
start.
