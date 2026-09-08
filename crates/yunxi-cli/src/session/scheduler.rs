//! Host-owned lifecycle for the optional proactive scheduler worker.

use std::env;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use yunxi_companion_mailbox::MailboxProactiveSink;
use yunxi_protocol::{ProactiveSchedulerRequest, QuietHours, WorkspaceGrant};
use yunxi_scheduler::{
    SchedulerConfig, SchedulerFacade, SchedulerHandle, SchedulerStatus, SchedulerTick,
    TemplateContext,
};

const INTERVAL_ENV: &str = "YUNXI_NEXT_SCHEDULER_INTERVAL_MILLIS";
const IDLE_ENV: &str = "YUNXI_NEXT_SCHEDULER_IDLE_MINUTES";
const QUIET_HOURS_ENV: &str = "YUNXI_NEXT_SCHEDULER_QUIET_HOURS";
const TEMPLATE_ENV: &str = "YUNXI_NEXT_SCHEDULER_TEMPLATE_FALLBACK";
const DEFAULT_IDLE: Duration = Duration::from_secs(120 * 60);
const MAX_IDLE: Duration = Duration::from_secs(7 * 24 * 60 * 60);
const MAX_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

pub(super) struct HostScheduler {
    state: Arc<Mutex<TickState>>,
    handle: SchedulerHandle,
}

struct TickState {
    session_id: String,
    focus: String,
    last_activity: Instant,
}

#[derive(Clone, Copy)]
struct HostSchedulerConfig {
    interval: Duration,
    idle_after: Duration,
    quiet_hours: Option<QuietHours>,
    template_enabled: bool,
}

impl HostSchedulerConfig {
    fn from_environment() -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let interval = duration_from_environment(
            INTERVAL_ENV,
            yunxi_scheduler::DEFAULT_WORKER_INTERVAL,
            Duration::from_millis(10),
            MAX_INTERVAL,
            &mut warnings,
        );
        let idle_after = duration_from_environment(
            IDLE_ENV,
            DEFAULT_IDLE,
            Duration::from_secs(60),
            MAX_IDLE,
            &mut warnings,
        );
        let quiet_hours = env::var(QUIET_HOURS_ENV)
            .ok()
            .and_then(|value| match parse_quiet_hours(&value) {
                Some(value) => Some(value),
                None => {
                    warnings.push(format!(
                        "ignored invalid {QUIET_HOURS_ENV}; expected HH:MM-HH:MM"
                    ));
                    None
                }
            });
        let template_enabled = match env::var(TEMPLATE_ENV) {
            Ok(value) => match parse_bool(&value) {
                Some(value) => value,
                None => {
                    warnings.push(format!(
                        "ignored invalid {TEMPLATE_ENV}; expected true or false"
                    ));
                    false
                }
            },
            Err(_) => false,
        };
        (
            Self {
                interval,
                idle_after,
                quiet_hours,
                template_enabled,
            },
            warnings,
        )
    }
}

impl HostScheduler {
    pub(super) fn start(
        workspace: &Path,
        session_id: impl Into<String>,
    ) -> Result<(Self, Vec<String>), String> {
        let (config, warnings) = HostSchedulerConfig::from_environment();
        Self::start_with_config(workspace, session_id.into(), config)
            .map(|scheduler| (scheduler, warnings))
    }

    fn start_with_config(
        workspace: &Path,
        session_id: String,
        config: HostSchedulerConfig,
    ) -> Result<Self, String> {
        let sink = MailboxProactiveSink::from_grant(&WorkspaceGrant::read_write(workspace))
            .map_err(|error| format!("scheduler mailbox sink unavailable: {error}"))?;
        let state = Arc::new(Mutex::new(TickState {
            session_id,
            focus: String::new(),
            last_activity: Instant::now(),
        }));
        let source_state = Arc::clone(&state);
        let facade = SchedulerFacade::new(
            SchedulerConfig::new()
                .enabled(true)
                .with_interval(config.interval)
                .with_template_fallback(config.template_enabled),
            sink,
        );
        let handle = facade.start(move || scheduler_tick(&source_state, config).map(Some));
        Ok(Self { state, handle })
    }

    pub(super) fn observe_activity(&self, prompt: &str) {
        let mut state = lock(&self.state);
        state.last_activity = Instant::now();
        state.focus = compact(prompt, 160);
    }

    pub(super) fn reset_session(&self, session_id: &str) {
        let mut state = lock(&self.state);
        state.session_id = compact(session_id, 160);
        state.focus.clear();
        state.last_activity = Instant::now();
    }

    #[allow(dead_code)]
    pub(super) fn status(&self) -> SchedulerStatus {
        self.handle.status()
    }
}

fn scheduler_tick(
    state: &Arc<Mutex<TickState>>,
    config: HostSchedulerConfig,
) -> Result<SchedulerTick, String> {
    let state = lock(state);
    let idle = state.last_activity.elapsed();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| "system clock is before the Unix epoch".to_string())?;
    let minute = ((now.as_secs() / 60) % 1440) as u16;
    let day = now.as_secs() / (24 * 60 * 60);
    let mut request = ProactiveSchedulerRequest::new(minute);
    if let Some(quiet_hours) = config.quiet_hours {
        request = request.with_quiet_hours(quiet_hours);
    }
    let scope = format!("{}:idle:{day}", state.session_id);
    let mut tick = SchedulerTick::new(request, scope);
    if idle < config.idle_after {
        return Ok(tick);
    }
    if config.template_enabled {
        tick = tick.with_template(TemplateContext::new("你", state.focus.clone()));
    } else {
        // The policy's idle threshold is fixed at 120 minutes. A lower Host
        // threshold is useful for tests but must not weaken that policy input.
        let idle_minutes = (idle.as_secs() / 60).max(120);
        request = ProactiveSchedulerRequest::new(minute).with_idle_minutes(idle_minutes);
        if let Some(quiet_hours) = config.quiet_hours {
            request = request.with_quiet_hours(quiet_hours);
        }
        tick = SchedulerTick::new(request, format!("{}:idle:{day}", state.session_id));
    }
    Ok(tick)
}

fn duration_from_environment(
    name: &'static str,
    default: Duration,
    minimum: Duration,
    maximum: Duration,
    warnings: &mut Vec<String>,
) -> Duration {
    let Ok(value) = env::var(name) else {
        return default;
    };
    let parsed = value.trim().parse::<u64>().ok().map(|number| {
        if name == IDLE_ENV {
            Duration::from_secs(number.saturating_mul(60))
        } else {
            Duration::from_millis(number)
        }
    });
    match parsed {
        Some(value) => value.max(minimum).min(maximum),
        None => {
            warnings.push(format!(
                "ignored invalid {name}; expected an unsigned integer"
            ));
            default
        }
    }
}

fn parse_quiet_hours(value: &str) -> Option<QuietHours> {
    let (start, end) = value.trim().split_once('-')?;
    QuietHours::new(parse_minute(start)?, parse_minute(end)?)
}

fn parse_minute(value: &str) -> Option<u16> {
    let (hour, minute) = value.trim().split_once(':')?;
    let hour = hour.parse::<u16>().ok()?;
    let minute = minute.parse::<u16>().ok()?;
    (hour < 24 && minute < 60).then_some(hour * 60 + minute)
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

fn compact(value: &str, maximum: usize) -> String {
    let mut output = value.trim().chars().take(maximum).collect::<String>();
    if output.is_empty() {
        output = "session".to_string();
    }
    output
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use yunxi_companion_mailbox::MailboxStore;
    use yunxi_protocol::MailboxListRequest;
    use yunxi_scheduler::WorkerLifecycle;

    use super::*;

    #[test]
    fn enabled_worker_writes_once_and_stops_with_the_session() {
        let root = std::env::temp_dir().join(format!(
            "yunxi-host-scheduler-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("workspace");
        let scheduler = HostScheduler::start_with_config(
            &root,
            "session-test".to_string(),
            HostSchedulerConfig {
                interval: Duration::from_millis(10),
                idle_after: Duration::ZERO,
                quiet_hours: None,
                template_enabled: false,
            },
        )
        .expect("scheduler");
        let deadline = Instant::now() + Duration::from_secs(1);
        while scheduler.status().enqueued() == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(scheduler.status().enqueued(), 1);
        scheduler.handle.cancel();
        assert_eq!(scheduler.status().lifecycle(), WorkerLifecycle::Stopped);

        let grant = WorkspaceGrant::read_only(&root);
        let store = MailboxStore::from_grant(&grant).expect("mailbox store");
        let items = store
            .list(&MailboxListRequest::new(grant))
            .expect("mailbox list");
        assert_eq!(items.items().len(), 1);
        fs::remove_dir_all(root).expect("cleanup");
    }

    #[test]
    fn quiet_hours_parser_rejects_invalid_ranges() {
        assert_eq!(parse_quiet_hours("22:00-07:00"), QuietHours::new(1320, 420));
        assert_eq!(parse_quiet_hours("25:00-07:00"), None);
        assert_eq!(parse_quiet_hours("invalid"), None);
    }
}
