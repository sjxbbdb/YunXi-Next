//! Kernel registry and lifecycle coordination.
//!
//! The runtime consumes plugin descriptions and supervisor events. It does not
//! spawn operating-system processes directly.

mod kernel;
mod snapshot;

pub use kernel::YunxiKernel;
pub use snapshot::{KernelSnapshot, KernelState};
