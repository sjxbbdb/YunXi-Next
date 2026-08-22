# yunxi-companion-mailbox

Process-isolated `companion.mailbox@1` provider. It stores idempotent companion
messages in a granted workspace, keeps message bodies encrypted at rest, and
supports bounded list/get/read-state operations.

YunXi Next uses `.yunxi-next/mailbox`; the legacy `.yunxi` mailbox remains
untouched. Mailbox metadata is visible to list operations, while content is
decrypted only for an explicit `get` call.
