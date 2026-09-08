# yunxi-scheduler

Process-isolated `scheduler.proactive@1` policy provider. It evaluates explicit
signals against quiet hours and bounded per-session/per-day limits, then emits
message plans. It does not send messages or execute tools itself.

The SchedulerFacade adds a host-side proactive lifecycle without putting
provider credentials or tool execution in this crate. A host supplies a
TickSource and a ProactiveSink, then starts a bounded SchedulerHandle. The
handle runs one tick at a time, catches source/sink failures, keeps a bounded
idempotency window, exposes counters, and can be cancelled promptly.
SchedulerConfig is disabled by default, so user intent is required before
background ticks run. Quiet hours remain part of every typed request.

render_love_letter is an explicitly labelled LocalDeterministic template
fallback. It is useful when no model or credential is configured and must not
be described as LLM output. The minimal Host integration still needs to build a
tick source from session state and connect ProactiveSink to the enabled
mailbox capability.
