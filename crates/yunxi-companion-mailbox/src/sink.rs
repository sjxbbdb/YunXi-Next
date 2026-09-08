//! Scheduler sink that writes through the encrypted mailbox store.

use yunxi_protocol::{MailboxEnqueueRequest, WorkspaceGrant};
use yunxi_scheduler::{EnqueueDisposition, ProactiveSink, ScheduledMessage, SchedulerSinkError};

use crate::{MailboxError, MailboxStore};

#[derive(Clone, Debug)]
pub struct MailboxProactiveSink {
    store: MailboxStore,
}

impl MailboxProactiveSink {
    pub fn from_grant(grant: &WorkspaceGrant) -> Result<Self, MailboxError> {
        Ok(Self {
            store: MailboxStore::from_grant(grant)?,
        })
    }

    pub fn new(store: MailboxStore) -> Self {
        Self { store }
    }
}

impl ProactiveSink for MailboxProactiveSink {
    fn enqueue(
        &self,
        message: &ScheduledMessage,
    ) -> Result<EnqueueDisposition, SchedulerSinkError> {
        let request = MailboxEnqueueRequest::new(
            WorkspaceGrant::read_write(self.store.workspace_root()),
            message.kind(),
            message.subject(),
            message.content(),
            message.reason(),
            message.idempotency_key(),
        );
        let result = self
            .store
            .enqueue(&request)
            .map_err(|error| SchedulerSinkError::new(error.to_string()))?;
        Ok(if result.created() {
            EnqueueDisposition::Created
        } else {
            EnqueueDisposition::AlreadyPresent
        })
    }
}

impl MailboxProactiveSink {
    pub fn store(&self) -> &MailboxStore {
        &self.store
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::time::{SystemTime, UNIX_EPOCH};

    use yunxi_protocol::{
        MailboxItemKind, MailboxListRequest, ProactiveSchedulerRequest, WorkspaceGrant,
    };
    use yunxi_scheduler::{SchedulerConfig, SchedulerFacade, SchedulerTick};

    use super::*;

    #[test]
    fn facade_writes_a_real_encrypted_mailbox_item() {
        let root = std::env::temp_dir().join(format!(
            "yunxi-scheduler-mailbox-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        fs::create_dir_all(&root).expect("workspace");
        let grant = WorkspaceGrant::read_write(&root);
        let sink = MailboxProactiveSink::from_grant(&grant).expect("sink");
        let facade = SchedulerFacade::new(SchedulerConfig::new().enabled(true), sink);
        let outcome = facade.process_tick(SchedulerTick::new(
            ProactiveSchedulerRequest::new(12 * 60).with_reminder_due(true),
            "mailbox-test",
        ));
        assert_eq!(outcome.enqueued(), 1);
        let reader = MailboxStore::from_grant(&WorkspaceGrant::read_only(&root)).expect("reader");
        let listed = reader
            .list(&MailboxListRequest::new(WorkspaceGrant::read_only(&root)))
            .expect("list");
        assert_eq!(listed.items().len(), 1);
        assert_eq!(listed.items()[0].kind(), MailboxItemKind::ProactiveMessage);
        let _ = fs::remove_dir_all(root);
    }
}
