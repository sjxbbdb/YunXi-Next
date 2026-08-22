//! Private operating-system process supervision boundary.
//!
//! Runtime code talks to this module through commands and events. No public
//! kernel API exposes the supervisor implementation.

mod process;

pub(crate) use process::{SupervisorCommand, SupervisorEvent, SupervisorHandle, spawn_supervisor};
