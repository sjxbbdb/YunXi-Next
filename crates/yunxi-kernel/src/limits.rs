//! Portable admission limits for plugin registrations.
//!
//! These limits protect the kernel from accidentally accepting unbounded
//! metadata. They are deliberately independent from operating-system CPU or
//! memory sandboxes, which require platform-specific policy and are outside
//! this crate's portable process boundary.

use crate::{PluginCommand, PluginSpec};

/// Bounds applied before a plugin is inserted into the kernel registry.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct KernelLimits {
    max_plugins: usize,
    max_arguments: usize,
    max_argument_bytes: usize,
    max_environment_entries: usize,
    max_environment_bytes: usize,
    max_path_bytes: usize,
    max_display_name_bytes: usize,
}

impl KernelLimits {
    /// Creates an explicit set of admission limits.
    pub const fn new(
        max_plugins: usize,
        max_arguments: usize,
        max_argument_bytes: usize,
        max_environment_entries: usize,
        max_environment_bytes: usize,
        max_path_bytes: usize,
        max_display_name_bytes: usize,
    ) -> Self {
        Self {
            max_plugins,
            max_arguments,
            max_argument_bytes,
            max_environment_entries,
            max_environment_bytes,
            max_path_bytes,
            max_display_name_bytes,
        }
    }

    pub const fn max_plugins(self) -> usize {
        self.max_plugins
    }

    pub const fn max_arguments(self) -> usize {
        self.max_arguments
    }

    pub const fn max_argument_bytes(self) -> usize {
        self.max_argument_bytes
    }

    pub const fn max_environment_entries(self) -> usize {
        self.max_environment_entries
    }

    pub const fn max_environment_bytes(self) -> usize {
        self.max_environment_bytes
    }

    pub const fn max_path_bytes(self) -> usize {
        self.max_path_bytes
    }

    pub const fn max_display_name_bytes(self) -> usize {
        self.max_display_name_bytes
    }

    pub const fn with_max_plugins(mut self, value: usize) -> Self {
        self.max_plugins = value;
        self
    }

    pub const fn with_max_arguments(mut self, value: usize) -> Self {
        self.max_arguments = value;
        self
    }

    pub const fn with_max_argument_bytes(mut self, value: usize) -> Self {
        self.max_argument_bytes = value;
        self
    }

    pub const fn with_max_environment_entries(mut self, value: usize) -> Self {
        self.max_environment_entries = value;
        self
    }

    pub const fn with_max_environment_bytes(mut self, value: usize) -> Self {
        self.max_environment_bytes = value;
        self
    }

    pub const fn with_max_path_bytes(mut self, value: usize) -> Self {
        self.max_path_bytes = value;
        self
    }

    pub const fn with_max_display_name_bytes(mut self, value: usize) -> Self {
        self.max_display_name_bytes = value;
        self
    }

    pub(crate) fn validate_spec(&self, spec: &PluginSpec) -> Result<(), LimitViolation> {
        if spec.display_name().len() > self.max_display_name_bytes {
            return Err(LimitViolation::new(
                "plugin display name bytes",
                self.max_display_name_bytes,
                spec.display_name().len(),
            ));
        }
        self.validate_command(spec.command())
    }

    pub(crate) fn validate_command(&self, command: &PluginCommand) -> Result<(), LimitViolation> {
        let program_bytes = path_bytes(command.program());
        if program_bytes == 0 {
            return Err(LimitViolation::new("program path bytes", 1, 0));
        }
        if program_bytes > self.max_path_bytes {
            return Err(LimitViolation::new(
                "program path bytes",
                self.max_path_bytes,
                program_bytes,
            ));
        }

        if let Some(current_dir) = command.configured_current_dir() {
            let bytes = path_bytes(current_dir);
            if bytes > self.max_path_bytes {
                return Err(LimitViolation::new(
                    "working directory path bytes",
                    self.max_path_bytes,
                    bytes,
                ));
            }
        }

        if command.arguments().len() > self.max_arguments {
            return Err(LimitViolation::new(
                "argument count",
                self.max_arguments,
                command.arguments().len(),
            ));
        }
        let argument_bytes = command
            .arguments()
            .iter()
            .map(os_string_bytes)
            .try_fold(0usize, usize::checked_add)
            .unwrap_or(usize::MAX);
        if argument_bytes > self.max_argument_bytes {
            return Err(LimitViolation::new(
                "argument bytes",
                self.max_argument_bytes,
                argument_bytes,
            ));
        }

        if command.environment().len() > self.max_environment_entries {
            return Err(LimitViolation::new(
                "environment entry count",
                self.max_environment_entries,
                command.environment().len(),
            ));
        }
        let environment_bytes = command
            .environment()
            .iter()
            .map(|(key, value)| os_string_bytes(key).saturating_add(os_string_bytes(value)))
            .try_fold(0usize, usize::checked_add)
            .unwrap_or(usize::MAX);
        if environment_bytes > self.max_environment_bytes {
            return Err(LimitViolation::new(
                "environment bytes",
                self.max_environment_bytes,
                environment_bytes,
            ));
        }

        Ok(())
    }
}

impl Default for KernelLimits {
    fn default() -> Self {
        Self {
            max_plugins: 256,
            max_arguments: 128,
            max_argument_bytes: 64 * 1024,
            max_environment_entries: 128,
            max_environment_bytes: 64 * 1024,
            max_path_bytes: 4 * 1024,
            max_display_name_bytes: 256,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LimitViolation {
    pub(crate) resource: &'static str,
    pub(crate) limit: usize,
    pub(crate) actual: usize,
}

impl LimitViolation {
    const fn new(resource: &'static str, limit: usize, actual: usize) -> Self {
        Self {
            resource,
            limit,
            actual,
        }
    }
}

fn path_bytes(path: &std::path::Path) -> usize {
    path.to_string_lossy().len()
}

fn os_string_bytes(value: &std::ffi::OsString) -> usize {
    value.to_string_lossy().len()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{PluginId, PluginSpec};

    fn spec(command: PluginCommand) -> PluginSpec {
        PluginSpec::new(PluginId::new("yunxi.test").expect("valid id"), command)
    }

    #[test]
    fn default_limits_accept_a_normal_command() {
        let limits = KernelLimits::default();
        let command = PluginCommand::new("yunxi-plugin").arg("--serve");
        assert!(limits.validate_spec(&spec(command)).is_ok());
    }

    #[test]
    fn argument_limits_are_checked_before_spawn() {
        let limits = KernelLimits::default().with_max_argument_bytes(2);
        let command = PluginCommand::new("yunxi-plugin").arg("123");
        let violation = limits
            .validate_spec(&spec(command))
            .expect_err("oversized argument");
        assert_eq!(violation.resource, "argument bytes");
    }
}
