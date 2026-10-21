# grammers fork

## Managed client and portable sessions (2026-10-21)

The ordinary client now supports `Client::builder(api_id)`, grouped
`DeviceConfig`, explicit `SessionSource`, and `start/connect/run/run_until/stop`.
Build performs local preparation only; start is lazy and connect performs a
fresh home-DC Ping, not an account-authorization check. Concurrent run waiters
share startup and one terminal outcome. Stop is terminal; build a new client to
restart. Never reopen a stopped request channel or replay work from an old
lifetime. Keep the legacy externally managed constructors available.

Managed defaults are 1024 SDK-owned pending requests, 30-second initialization,
and 10-second shutdown. Low-level ConnectionParams retains its prior unlimited
default. Capacity travels with the invocation permit and is released before
response wakeup; cancelling a caller must not release sent capacity. Keep
original errors, typed categories, cumulative counters and bounded events.
Checked runners contain unwinds and still tear down children; the process panic
hook and non-unwinding aborts remain outside this guarantee. On connection
failure determine the current home DC, including after QR migration.

`session-string` independently provides the native `grammers:1:` format: 454
ASCII characters, fixed bounded binary payload, exact home DC/endpoints/IP
preference/key, and a SHA-256 corruption checksum. It is unencrypted and is not
a complete SQLite backup. No peer cache, update state or other-DC keys are
exported. Keep diagnostics redacted and credential extraction explicit.
`Client::export_session_string()` returns `String` directly from any supported
source; `export_session_secret()` retains the optional redacted wrapper.

`session-import-files` adds explicit read-only Pyrogram v3/Kurigram v7,
Telethon SQLite v7/v8 and gotd Loader v1 file readers. Never auto-detect arbitrary
strings, migrate source databases, or overwrite existing sessions. Require
closed, stable local sources; reject nonempty WAL/SHM/journals. Use immutable
read-only SQLite, bound TEXT/BLOB selection before materialization, and preserve
Unix literal backslashes when encoding URI paths. Existing string decoders
remain file/network-free. Keep all format dependencies optional and preserve
the existing API-ID-zero, home-key-only and exact endpoint rules below.
Managed native file sources use `SqliteSession::open_native` to validate the
schema read-only before opening it for writes. Require a closed, stable file;
reject foreign schemas and active sidecars without modifying them. Legacy
`SqliteSession::open` remains available for application-controlled recovery.

Parallel downloads own bounded chunk futures; cancellation must not detach
workers. Preserve learned file DCs and finite rejection recovery. Partial files
can remain after cancellation, and issued OS writes cannot be rolled back.
CDN redirects are explicit Unsupported errors, not full CDN support.

`SendOptions::default()` matches existing InputMessage defaults (including
link_preview=false). Applying options changes only its six documented fields
and preserves rich payloads. Dispatcher handlers are sequential with explicit
errors; QR steps have bounded migration, explicit token display, redacted Debug,
and no hidden polling. Examples use TG_* runtime variables and awaited cleanup.

Documentation is built by `scripts/build-docs.py` for GitHub Pages. Rustdoc is
the API source; generated method pages use local signatures/types and a cached
official Telegram availability snapshot. Unknown or mismatched constructors
must remain explicit. `refresh-telegram-docs.py` refreshes missing/new schema
methods by default; full refresh is explicit. Do not infer permissions from
namespaces or replace local types with a different online schema version.

Verified locally on Rust 1.94.1: 511 release all-feature workspace tests/doctests
passed (30 ignored), all-target/all-feature check, strict Clippy and warning-free
Rustdoc. Optional session builds passed: importer 54, native strings 38, file
imports 73, memory-only 32; minimal client 107 and optional-helper library 54.
Documentation generation has 17 offline tests and validates links across 923
method pages. Windows/Linux CI is configured; do not claim a hosted run passed
without observing it. No live authorization or consumer deployment was used.
Follow-up regressions passed: 25 managed-runtime, 7 native-codec and 5 native
file-guard tests, plus strict client/session all-target/all-feature Clippy.

The dependency lock now uses Tokio 1.54.0 and stable glass_pumpkin 2.0.0. Highest
declared dependency Rust requirement is 1.89; that is metadata, not a tested
MSRV. The historical 1.87-compatible syntax notes below are not a claim that
current dependencies build on 1.87. PERFORMANCE.md records fresh offline
benchmarks, including measurable route overhead; do not claim universal gains.

Canonical upstream was refreshed and remains `633d7fe`, layer 230. Upstream
intake goes through a review branch and compatibility checks, not an automatic
merge into main. Keep this fork's package links and generated docs distinct from
the upstream crates.io/docs.rs release.

Current integration (2026-10-18): canonical Codeberg through `633d7fe` is
merged (layer 230 unchanged). Keep new InputAudio/InputVideo/TimeToLive support,
including the upstream `media_ttl(Option<TimeToLive>)` API; UPSTREAM.md records
compatibility and retained fork behavior. Public media fields remain inline.
No consuming application is modified or deployed by updating this checkout.
Verified locally:402 release all-feature tests/doctests,15 explicit local
benchmarks, all-target/all-feature check and strict Clippy. Memory-only importer
tests also pass without SQLite. No live authorization or deployment was tested.

General efficiency: vector initial reservation follows one valid item and is
bounded to about 4 KiB with a power-of-two growth base. Preserve generic ZST and
zero-byte decoders, constructors, error cleanup and normal growth. Full own
updates skip unneeded request decoding; short-sent/channel outcomes still decode
needed context before retirement. Obfuscation scratch is stack-backed but wire,
keys and stream position are unchanged. PERFORMANCE.md includes measured gains
and small-input tradeoffs; do not claim every input becomes faster.

Session bulk overrides must survive type erasure. Memory batches acquire one
lock, and self-user lookup follows actual cached self flags. SQLite reads/merges/
writes each peer in order under the same lock and transaction; duplicate IDs
must not inherit fields from a rejected earlier write. Preserve per-row error
reporting/continued writes and return transaction/preparation errors, never panic.

`grammers-session/session-import` is opt-in: exported Pyrogram current/legacy
strings, Telethon v1 IPv4/IPv6, and gotd Loader v1 JSON. Keep strict bounded format
validation, home-DC-only keys, test routing, exact saved endpoints, redacted
errors/Debug, and optional dependency isolation. No files/network/authorization
are performed while importing. Unknown Telethon endpoints keep only their own
route; IPv6 imports require the supplied preference. Do not add speculative
support for SQLite sessions, tdata or custom JSON without explicit format tests.

Current Pyrogram strings may carry zero in their API ID field: import it as
missing source metadata (`api_id() == None`), never as the sender pool's API ID.
Keep positive IDs within the signed `i32` range and retain key, DC, boolean,
user-ID and payload-length validation when this optional metadata is absent.

For memory-only use: `ImportedSession::into_memory_session()` and
`ClientConfiguration { auto_cache_peers: false }`; MemorySession itself is not a
cache-disable switch. Self login/explicit cache calls are distinct. The compiled
`grammers-client/examples/import_session.rs` demonstrates setup and teardown;
do not run it with real credentials as part of ordinary verification.
Check optional builds with `cargo test --locked --release -p grammers-session
--no-default-features --features session-import -- --test-threads=1` and the
same command without `--features session-import` for the memory-only library.

Raw-RPC audit (2026-10-11): raw request bodies must have a constructor,
word-aligned length and fit the existing uncompressed MTProto limit, including
any automatic update-suppression wrapper. Reject malformed lengths before
queueing; a bad caller must not panic a shared sender or retain an invocation
reservation. An exact-limit request must be frameable, not indefinitely deferred.
Tests cover boundary lengths, compression, wrapper modes, and owned/shared plus
synchronous/asynchronous tracked entry points on cold/cached routes.
At that earlier audit, upstream was fetched through layer 230 but not merged;
UPSTREAM.md records the compatibility scope. No new hot-path optimization was
retained without a measured gain; PERFORMANCE.md records the rejected experiment.
Verified locally: 334 release all-feature workspace tests/doctests passed,
24 ignored; all-target/all-feature check and strict Clippy passed. No live RPC,
deployment or public push ran.

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
default workspace tests do not need live account credentials. Strict all-target/all-feature Clippy must pass before publishing.
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


## Optional update delivery and invocation lifetime

ConnectionParams.receive_updates defaults true; disabled mode wraps API calls,
skips unsolicited update delivery and rejects update-stream creation while keeping
RPC/service handling. Set it before initialization, including fresh-key fallback.
Borrowed message/container views must not escape decrypted-input lifetime; returned
RPC buffers remain owned. Prevalidate all container envelopes before side effects.
Keep the sender's pinned keepalive timer across step cancellation.

InvocationTracker is optional and its permit belongs to the SDK request, not the
caller future. Never release a sent reservation solely because a caller cancelled.
PendingInvocation does not create an extra task. Optional connection_timeout bounds
network establishment/init, not ordinary RPCs or session-storage calls. Existing
untracked APIs and default update-enabled behavior remain supported. See
PERFORMANCE.md for verification and component benchmark limitations.


## Warning-clean builds

Workspace all-target/all-feature Clippy passes with -D warnings. Cleanup uses
behavior-preserving helpers and Rust 1.87-compatible syntax. Example runner tasks
are explicitly detached. Private module aliases avoid repeated module names.
The public Media enum retains its inline variants with one documented
large_enum_variant expectation: boxing would break constructors and introduce
allocations. There is no blanket warning suppression. Stable rustfmt no longer
requests the nightly-only group_imports option. Recheck after upstream merges.


## Send/receive audit follow-up

Compressed RPC results must pass the same constructor/error handling as plain
results. A gzip wrapper does not establish success; preserve CRC validation and
per-request deserialization failures. Return rpc_drop_answer acknowledgements to
their own caller without retiring another request's sent metadata.

Cold initialization is cancellable by its caller, Quit or matching-DC disconnect.
Register the per-pool control Notify before checking state to avoid missed wakes.
Other DCs are independent, and ordinary routed calls do not subscribe. Cancelling
setup may discard an uncommitted connection; never apply this shortcut to already
serialized/sent RPCs. Keep original typed errors without eager string formatting
when retry is rejected. Transport offsets must cover only complete input, and
length/status conversions must not wrap into out-of-bounds ranges.

Verification: 319 all-feature tests/doctests passed (22 ignored), strict
all-target/all-feature Clippy passed. See PERFORMANCE.md for scope and rejected
micro-optimizations. No production performance benefit has been measured.

## Reused gzip state and retirement ordering (2026-10-04)

Gzip replies reuse one decoder per runtime thread, not one per connection.
Compressed input is copied into owned scratch storage; no transport borrow may
escape the call. Cache only scratch whose input capacity is at most 64 KiB and
discard larger scratch. Return independently owned output. The gzip footer is
only a capped allocation hint: preserve CRC, length, header and trailing-data
validation, including recovery after malformed streams and nested gzip envelopes.

Release invocation permits before notifying receivers on terminal success/error
paths and failed connection setup. Queued Rpc/Request and sender Request fields
deliberately place the permit before the response sender, so drop also retires
the reservation before waking a closed-channel receiver. Preserve that declaration
order. Cancellation must still retain serialized/sent requests until protocol
completion, definitive rejection or connection teardown; no replay policy changed.

Verification: 330 release all-feature workspace tests/doctests passed, 24 ignored;
all-target/all-feature check and strict Clippy passed. An isolated consumer passed
219 release tests, 9 ignored, plus strict Windows and Linux-target Clippy. See
PERFORMANCE.md for component benchmarks and their limits. No live deployment or
authenticated API call was used for this verification.
