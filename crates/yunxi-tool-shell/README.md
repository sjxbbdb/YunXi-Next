# yunxi-tool-shell

Process-isolated `tool.shell@1` provider. The plugin executes an explicitly
approved command in a host-granted working directory, with bounded output and
an enforced wall-clock timeout.

The current implementation is a permission and failure-containment baseline,
not an operating-system sandbox. It does not claim to prevent a shell command
from reaching outside the workspace or using the network; those controls need
platform-specific sandbox providers in a later hardening pass.
