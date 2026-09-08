# Browser Adapter

`web-api-client.ts` replaces the upstream browser-only WebSocket carrier during
the dsh client build. It keeps the upstream `AbstractApiClient`, envelope
validation, and unary RPC implementation, then repeatedly opens the bounded
YunXi SSE endpoints with a cancellation-aware idle delay. Each mux/host stream
keeps its own monotone cursor and sends both `afterSeq` and `Last-Event-ID`, so
a finite response or a dropped connection can resume from the last accepted
frame. The server's explicit replay-gap diagnostic is left visible to the dsh
connection controller: it uses the public `internal` stream-error shape plus
bounded `X-Yunxi-Event-*` headers. Retained event ids advance the reconnect
cursor only across usable frames; the gap control frame has no id. The
controller can then reconnect and let session history repair the evicted
portion.

The browser decoder enforces the same 512 KiB response ceiling, drops malformed
frames while consuming a valid transport id to avoid retrying a poison frame,
and keeps mux/host cursors isolated. `web-api-client.spec.ts`
is copied into the pinned upstream checkout by the import script and runs in
that checkout's Vitest environment; it is adapter coverage, not a production
Host or multi-session fixture.

No credential, model call, filesystem operation, or Rust plugin lifecycle is
implemented here.

`plugin-inventory/` is the second bounded overlay. It keeps the upstream
Plugins tab structure but binds each YunXi capability row to the
`yunxi-capabilities` settings namespace. The model row remains read-only and
the 15 optional capability changes are composition-scoped: Web writes rebuild
the current Host, while a standalone CLI process reads them at its next start.
