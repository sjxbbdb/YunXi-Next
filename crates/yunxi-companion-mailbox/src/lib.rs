#![doc = "Encrypted companion mailbox capability for YunXi Next."]
#![forbid(unsafe_code)]

mod plugin;
mod store;

pub use plugin::{MAILBOX_PLUGIN_ID, MailboxPluginError, run_mailbox_plugin};
pub use store::{MailboxError, MailboxStore};
