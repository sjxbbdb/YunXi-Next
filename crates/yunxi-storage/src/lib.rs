#![doc = "Workspace-scoped persistent session capability for YunXi Next."]
#![forbid(unsafe_code)]

mod event_log;
mod migration;
mod plugin;
mod record;
mod store;

pub use event_log::{
    EventCursor, EventImportError, EventImportPlan, EventImportReport, EventImportState,
    EventLogError, EventLogPage, EventLogSummary, EventLogWarning, EventRollbackReport,
    LegacyEventKind, LegacyEventLog, LegacyEventLogMigration, NormalizedLegacyEvent,
};
pub use migration::{
    MigrationCapability, MigrationItem, MigrationItemState, MigrationManifest, MigrationPlan,
    MigrationReport, MigrationScope, MigrationState, RollbackReport, SessionMigration,
};
pub use plugin::{STORAGE_PLUGIN_ID, StoragePluginError, run_storage_plugin};
pub use store::{SessionStore, StorageError};
