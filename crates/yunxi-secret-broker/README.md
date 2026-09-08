# YunXi Secret Broker

This crate defines a small, host-controlled secret boundary for YunXi Next.

- `SecretRef` is an opaque, in-memory, single-use authorization reference.
- `SecretValue` cannot be serialized or formatted with its contents. Callers may
  only borrow its bytes through a scoped callback.
- `SecretStore` has explicit missing, unavailable, and invalid-encoding errors.
- `MemorySecretStore` and `EnvironmentSecretStore` are provided for tests and
  local deployments. `FileSecretStore` provides durable storage for a host that
  explicitly supplies a 32-byte master key.
- `FileSecretStore` stores one versioned ChaCha20-Poly1305 authenticated
  snapshot. It uses a same-directory lock and an atomic replacement; failed
  validation, encryption, or writes leave the previous snapshot untouched.
  Wrong keys, malformed bytes, and tampering return the same `Corrupt` error.
- Plugin access is enabled by default, and the host can disable a plugin to
  revoke newly issued access and immediately discard all pending references.
  Re-enabling the plugin never revives those references.
- Every resolve or injection attempt creates bounded metadata-only audit data.
- Reference, plugin identifier, environment-name, value, and audit sizes are
  bounded.
- Callback panics are recorded without secret data, and the scoped value is
  dropped before the panic resumes. Secret values are cleared on drop and on
  replacement in the in-memory store.

This is a host-controlled API boundary, not an operating-system sandbox. It
does not claim to implement an OS keychain, process isolation, or a hardware
secret store. Integration with those systems can implement `SecretStore`
later without changing the authorization and auditing contract. The Host must
own and provision the master key; this crate never derives one from an
environment variable, path, or persisted data. No real credential integration
is implied by this crate alone.
