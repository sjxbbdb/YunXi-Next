# Browser Adapter

`web-api-client.ts` replaces the upstream browser-only WebSocket carrier during
the dsh client build. It keeps the upstream `AbstractApiClient`, envelope
validation, unary RPC implementation, and SSE decoder, then repeatedly opens
the bounded YunXi SSE endpoints with a cancellation-aware idle delay.

No credential, model call, filesystem operation, or Rust plugin lifecycle is
implemented here.

`plugin-inventory/` is the second bounded overlay. It keeps the upstream
Plugins tab structure but binds each YunXi capability row to the
`yunxi-capabilities` settings namespace. The model row remains read-only and
the 15 optional capability changes are composition-scoped: Web writes rebuild
the current Host, while a standalone CLI process reads them at its next start.
