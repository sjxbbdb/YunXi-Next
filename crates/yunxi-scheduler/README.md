# yunxi-scheduler

Process-isolated `scheduler.proactive@1` policy provider. It evaluates explicit
signals against quiet hours and bounded per-session/per-day limits, then emits
message plans. It does not send messages or execute tools itself.
