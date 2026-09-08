//! User-facing, explicit migration commands.
//!
//! This module only translates a CLI action into the storage migration
//! facade. It never opens the legacy namespace for writing and never performs
//! an implicit migration during normal chat startup.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use yunxi_protocol::WorkspaceGrant;
use yunxi_storage::{
    LegacyEventLog, LegacyEventLogMigration, MigrationItemState, SessionMigration,
};

use crate::args::{EventMigrationCommand, MigrationCommand};

pub(crate) fn execute(command: &MigrationCommand, cwd: &Path) -> Result<Value, String> {
    if let MigrationCommand::Events(command) = command {
        return execute_events(command, cwd);
    }
    let grant = match command {
        MigrationCommand::Status | MigrationCommand::Plan => WorkspaceGrant::read_only(cwd),
        MigrationCommand::Apply | MigrationCommand::Rollback(_) => WorkspaceGrant::read_write(cwd),
        MigrationCommand::Events(_) => unreachable!("event migrations are dispatched above"),
    };
    let migration = SessionMigration::from_grant(&grant).map_err(|error| error.to_string())?;
    match command {
        MigrationCommand::Status => status(&migration, cwd),
        MigrationCommand::Plan => plan(&migration, cwd),
        MigrationCommand::Apply => apply(&migration, cwd),
        MigrationCommand::Rollback(migration_id) => rollback(&migration, cwd, migration_id),
        MigrationCommand::Events(_) => unreachable!("event migrations are dispatched above"),
    }
}

fn execute_events(command: &EventMigrationCommand, cwd: &Path) -> Result<Value, String> {
    let source = event_source(cwd, command);
    let write = matches!(
        command,
        EventMigrationCommand::Apply { .. } | EventMigrationCommand::Rollback { .. }
    );
    let grant = if write {
        WorkspaceGrant::read_write(cwd)
    } else {
        WorkspaceGrant::read_only(cwd)
    };
    let migration =
        LegacyEventLogMigration::from_grant(&grant, &source).map_err(|error| error.to_string())?;

    match command {
        EventMigrationCommand::Status { .. } => {
            let log =
                LegacyEventLog::open(migration.source()).map_err(|error| error.to_string())?;
            let summary = log.summary().map_err(|error| error.to_string())?;
            Ok(json!({
                "schemaVersion": 1,
                "ok": true,
                "command": "migrate events status",
                "workspace": cwd,
                "source": migration.source(),
                "sourceFingerprint": log.source_fingerprint(),
                "readOnly": true,
                "summary": summary,
                "legacyUnchanged": true,
            }))
        }
        EventMigrationCommand::Replay {
            after_cursor,
            limit,
            ..
        } => {
            let log =
                LegacyEventLog::open(migration.source()).map_err(|error| error.to_string())?;
            let page = log
                .replay(*after_cursor, *limit)
                .map_err(|error| error.to_string())?;
            Ok(json!({
                "schemaVersion": 1,
                "ok": true,
                "command": "migrate events replay",
                "workspace": cwd,
                "source": migration.source(),
                "sourceFingerprint": log.source_fingerprint(),
                "readOnly": true,
                "afterCursor": after_cursor,
                "requestedLimit": limit,
                "page": page,
                "legacyUnchanged": true,
            }))
        }
        EventMigrationCommand::Plan { .. } => {
            let plan = migration.plan().map_err(|error| error.to_string())?;
            Ok(json!({
                "schemaVersion": 1,
                "ok": true,
                "command": "migrate events plan",
                "workspace": cwd,
                "source": migration.source(),
                "readOnly": true,
                "plan": plan,
                "legacyUnchanged": true,
            }))
        }
        EventMigrationCommand::Apply { .. } => {
            let plan = migration.plan().map_err(|error| error.to_string())?;
            let migration_id = plan.migration_id().to_string();
            let report = migration.apply(&plan).map_err(|error| error.to_string())?;
            Ok(json!({
                "schemaVersion": 1,
                "ok": true,
                "command": "migrate events apply",
                "workspace": cwd,
                "source": migration.source(),
                "readOnly": false,
                "migrationId": migration_id,
                "report": report,
                "rollbackArgs": ["migrate", "events", "rollback", migration.source(), migration_id],
                "legacyUnchanged": true,
            }))
        }
        EventMigrationCommand::Rollback { migration_id, .. } => {
            let report = migration
                .rollback(migration_id)
                .map_err(|error| error.to_string())?;
            Ok(json!({
                "schemaVersion": 1,
                "ok": true,
                "command": "migrate events rollback",
                "workspace": cwd,
                "source": migration.source(),
                "readOnly": false,
                "migrationId": migration_id,
                "report": report,
                "legacyUnchanged": true,
            }))
        }
    }
}

fn event_source(cwd: &Path, command: &EventMigrationCommand) -> PathBuf {
    let source = match command {
        EventMigrationCommand::Status { source }
        | EventMigrationCommand::Replay { source, .. }
        | EventMigrationCommand::Plan { source }
        | EventMigrationCommand::Apply { source }
        | EventMigrationCommand::Rollback { source, .. } => source,
    };
    if source.is_absolute() {
        source.clone()
    } else {
        cwd.join(source)
    }
}

fn status(migration: &SessionMigration, cwd: &Path) -> Result<Value, String> {
    let plan = migration.plan().map_err(|error| error.to_string())?;
    let counts = plan_counts(&plan);
    Ok(json!({
        "schemaVersion": 1,
        "ok": true,
        "command": "migrate status",
        "scope": "workspace_and_optional_legacy_user_home",
        "workspace": cwd,
        "readOnly": true,
        "legacySource": plan.source_root(),
        "nextTarget": plan.target_root(),
        "legacyUserHome": plan.legacy_user_home(),
        "nextUserHome": plan.next_user_home(),
        "capabilities": plan.capabilities(),
        "manifestPath": plan.manifest_path(),
        "migrationId": plan.migration_id(),
        "counts": counts,
        "warnings": plan.warnings(),
    }))
}

fn plan(migration: &SessionMigration, cwd: &Path) -> Result<Value, String> {
    let plan = migration.plan().map_err(|error| error.to_string())?;
    Ok(json!({
        "schemaVersion": 1,
        "ok": true,
        "command": "migrate plan",
        "scope": "workspace_and_optional_legacy_user_home",
        "workspace": cwd,
        "readOnly": true,
        "legacyUserHome": plan.legacy_user_home(),
        "nextUserHome": plan.next_user_home(),
        "plan": plan,
    }))
}

fn apply(migration: &SessionMigration, cwd: &Path) -> Result<Value, String> {
    let plan = migration.plan().map_err(|error| error.to_string())?;
    let migration_id = plan.migration_id().to_string();
    let report = migration.apply(&plan).map_err(|error| error.to_string())?;
    Ok(json!({
        "schemaVersion": 1,
        "ok": true,
        "command": "migrate apply",
        "scope": "workspace_and_optional_legacy_user_home",
        "workspace": cwd,
        "readOnly": false,
        "migrationId": migration_id,
        "legacyUserHome": plan.legacy_user_home(),
        "nextUserHome": plan.next_user_home(),
        "capabilities": plan.capabilities(),
        "report": report,
        "rollback": format!("yunxi-next migrate rollback {migration_id}"),
        "legacyUnchanged": true,
    }))
}

fn rollback(migration: &SessionMigration, cwd: &Path, migration_id: &str) -> Result<Value, String> {
    let report = migration
        .rollback(migration_id)
        .map_err(|error| error.to_string())?;
    Ok(json!({
        "schemaVersion": 1,
        "ok": true,
        "command": "migrate rollback",
        "scope": "workspace_and_optional_legacy_user_home",
        "workspace": cwd,
        "readOnly": false,
        "migrationId": migration_id,
        "report": report,
        "legacyUnchanged": true,
    }))
}

fn plan_counts(plan: &yunxi_storage::MigrationPlan) -> Value {
    let mut ready = 0;
    let mut target_exists = 0;
    let mut invalid = 0;
    let mut applied = 0;
    let mut rolled_back = 0;
    for item in plan.items() {
        match item.state() {
            MigrationItemState::Ready => ready += 1,
            MigrationItemState::TargetExists => target_exists += 1,
            MigrationItemState::Invalid { .. } => invalid += 1,
            MigrationItemState::Applied { .. } => applied += 1,
            MigrationItemState::RolledBack => rolled_back += 1,
        }
    }
    json!({
        "total": plan.items().len(),
        "ready": ready,
        "targetExists": target_exists,
        "invalid": invalid,
        "applied": applied,
        "rolledBack": rolled_back,
    })
}
