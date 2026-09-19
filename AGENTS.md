# grammers fork

This is a public library fork. Canonical upstream is Codeberg; see UPSTREAM.md
for remotes, branch roles and the update procedure. Keep upstream history and
Apache-2.0/MIT licenses intact. This is a cross-host Git fork with no native GitHub
fork relationship. The archived GitHub repository is not the update source.

The Cargo workspace includes client, session storage, sender, MTProto, crypto,
TL parser/generator/types and the grammers command-line crate. Follow the upstream
README and each crate's guide when changing that component.

Keep changes focused, preserve unrelated work, and verify them with relevant
tests plus `cargo check --workspace --all-targets`. Performance work requires
release benchmarks and clear distinction between local and live measurements.
Never add credentials, application session files, user inventories or unrelated
application source to this public repository. Push only when authorized by the
user's task. This checkout's publication policy is separate from sibling projects.

## Polling-oriented changes

See PERFORMANCE.md for changes and reproducible benchmarks. Preserve the legacy
Vec-based raw invocation API alongside the shared RequestBody API. Never mutate
prepared request bytes after sharing them. Ordinary typed retry policies remain
configurable; applications may explicitly choose NoRetries.

Cancellation only discards work before framing, or after a definitive retryable
rejection. Do not remove serialized/sent metadata blindly: it supports own updates
and partially written packets. Keepalives have no user response channel and must
survive cancellation cleanup. A zero-byte nonempty write must terminate with error.

Buffers grow from4KiB to the existing receive cap; preserve complete-packet and
fragmented-packet behavior. Borrowed TL/gzip paths retain constructor, length,
padding and checksum validation. Do not remove cryptographic or protocol checks.
Large-buffer consumers may have different allocation/latency tradeoffs.

Network integration tests using Telegram's public test DC are explicitly ignored;
default workspace tests do not need live account credentials. Known upstream Clippy
style warnings remain; do not describe strict Clippy as clean until addressed.
The library's initial implementation changes are not deployed to an application
automatically. Keep consumer compatibility checks isolated from public source.

Verified baseline for this patch:272 default-feature and289 all-feature workspace
tests/doctests passed; all-target/all-feature check passed;168 consumer release
tests passed with local overrides. Three isolated release microbenchmarks passed.
No consumer deployment or account authorization was performed. These measurements
do not establish VPS CPU savings or a cause for prior connection-timeout bursts.

## Reused ACK/results and warm connection routing

ACK bytes serialize directly into the outgoing buffer; pending_ack retains its
capacity after clearing. Preserve the typed encoder's exact wire format, sequence
semantics and send timing. Mtp::recycle_deserialization is a backward-compatible
default hook; Encrypted keeps ordinary response-list capacity (up to 64 entries)
after Sender drains it, without retaining large one-off allocations or RPC bodies.

SenderPoolHandle shares a per-pool routing cache. Healthy connections bypass the
pool queue; cold/closed connections still use the pool. Hold no routing lock across
a send or await. Only a failed channel send which returns ownership of an unqueued
RPC permits fallback; accepted requests with lost replies must never be replayed.
Pending disconnect counts suppress stale route publication until each control
request is processed. Quit clears routes and suppresses new publication. Extra
sender handles must not keep the connection tasks alive after shutdown.

Follow-up validation: 297 all-feature workspace tests/doctests passed (19 ignored),
all-target/all-feature check passed, and 169 consumer release tests plus strict
consumer Clippy passed. Library Clippy retains upstream style warnings. See
PERFORMANCE.md for local ACK/routing benchmarks and their measurement limits.
