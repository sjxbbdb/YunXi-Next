# CLI integration tests

Tests in this directory launch the compiled `yunxi-next` executable, which then
launches isolated context, memory, persona, and model children according to
their switches. A loopback mock HTTP server verifies context composition,
legacy memory recall, disabled-plugin routing, and API error recovery without
using a real API key or network service.

`web_host.rs` starts `yunxi-next web` as a separate process and verifies the
same Host path through HTTP: session creation, chat, history, mux events, and
the approval response boundary. It also covers completion-before-final stream
events, active-turn cancellation, independent sessions, and bounded
continuable child-agent prompt/interrupt behavior. It uses an isolated
`YUNXI_NEXT_HOME` to
verify settings describe/update/replace/mutate, revision conflicts, Host
events, disk persistence, restart-scoped disabled inventory, route removal,
and continued model chat. The fixture closes standard input to exercise
explicit Web shutdown and plugin cleanup.

`cli_surface.rs` verifies management parsing and output, explicit migration
plan/apply/rollback with unchanged legacy files, and loopback Voice/Weixin
facades. External devices, accounts, and provider services are deliberately not
part of these fixtures.
