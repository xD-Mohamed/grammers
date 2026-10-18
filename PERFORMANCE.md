# Library efficiency and fork changes

## General library audit (2026-10-18)

Merged canonical Codeberg through `633d7fe` before applying general library
changes. Reviewed sender/routing/transports, MTProto/crypto, TL types/parser/gen,
client/session storage, and the CLI integration. Existing crypto, gzip integrity,
constructor/bounds validation, request lifetime and no-replay rules remain.

Retained changes:
- TL vectors decode their first item before reserving. Initial capacity is bounded
  to roughly 4 KiB, rounded down to a power of two; valid larger vectors still
  grow. Empty vectors allocate nothing; singleton capacity is one rather than
  the old collector's minimum four. Malformed counts cannot request unbounded
  initial storage. Custom zero-byte/ZST elements remain supported.
- Full own-update responses no longer search pending requests and deserialize a
  discarded SendMessage. Short-sent and affected-channel responses still decode
  the original request when required, before its retirement. This has no benefit
  for callers that already disable update delivery.
- Obfuscation initialization uses two fixed 64-byte array scratch buffers rather
  than temporary Vecs. Keys, wire header, and cipher position are unchanged.
  Proxy success paths no longer eagerly format errors. Both concern setup only.
- Erased sessions forward bulk caching to the actual backend and borrow their Arc
  instead of incrementing it for each call. Memory batches acquire one lock;
  SQLite batches reuse SELECT/INSERT statements in one transaction. Reads and
  writes stay ordered, including duplicate IDs and failed intermediate writes.
  Setup errors return normally; accepted rows still commit after per-row errors.
- MemorySession resolves the self-user sentinel from actual cached self metadata.
  Normal ID lookup stays a direct map lookup. Disabling the client's automatic
  peer cache is tested independently and does not discard peers in returned data.
- Schema comment removal now supports Unicode without slicing at invalid UTF-8
  boundaries. This is a build-time correctness fix, not faster response decoding.

Windows x86-64, rustc 1.94.1, release `-C target-cpu=native`, locked dependencies;
local synthetic medians with alternating execution order (eight samples unless
noted). No concurrent build was started during the benchmark run.

| Component fixture | Previous equivalent | Current |
|---|---:|---:|
| Decode vector of 8 i32 | 56.7 ns | 30.3 ns |
| Decode vector of 32 i64 | 113.9 ns | 47.4 ns |
| Decode vector of 32 generated Peer | 140.9 ns | 74.7 ns |
| Decode vector of 128 strings | 3.811 us | 3.526 us |
| Full own update, 32-byte sent message, one pending request | 390.5 ns | 318.5 ns |
| Full own update, 4 KiB sent message, one pending request | 1.219 us | 0.318 us |
| Memory batch, 8 peers (seven samples) | 337.4 ns | 126.4 ns |
| Erased memory batch, 100 peers (seven samples) | 6.211 us | 1.107 us |
| SQLite batch, 100 existing peers (five samples) | 380.85 us | 129.78 us |
| Obfuscation cipher setup | 62.0 ns | 53.6 ns |
| Obfuscation header setup (same current cipher in both) | 232.3 ns | 217.6 ns |

Tradeoffs: empty vector fixtures cost about 3-7 ns more; singleton i64/string/Peer
fixtures cost about 2-6 ns more, while i32 was slightly faster. Bulk forwarding
has fixed overhead: an empty erased batch measured 62.3 ns versus 30.6 ns, and
one-peer SQLite measured 4.53 us versus 3.98 us. These paths preserve backend
semantics, including empty bulk overrides. The retained improvements target
nontrivial vectors/batches and reduced copying; this is not a claim that every
input becomes faster. A first reservation prototype retained 1,360 slots for
1,024 strings; rounding the bound down removes that regression (1,024 slots).

Vector tests cover constructors, truncation, huge short counts, zero-sized and
large elements, and drops on errors. Obfuscation fixtures compare exact ciphertext
and stream advancement. Storage tests cover ordered merges, identities, poisoned
empty memory storage, transaction/preparation errors, row failures and duplicate
IDs after a rejected write. Import fixtures use synthetic keys only.

The vector baseline reproduces the former Result-based collector. Own-update
figures include input cloning and real decoding. Memory baselines reproduce
sequential calls; the erased baseline is a local legacy-equivalent helper, not
an old dynamic trait object. SQLite uses an in-memory database with fixtures
prepared outside timing. These are component measurements, not whole-process
allocation counts, RSS, CPU savings, network latency or production throughput.

Reproduce (first generate the ignored local lockfile on a fresh clone):

```sh
cargo test --locked --release --workspace --all-features -- --test-threads=1
cargo test --locked --release -p grammers-tl-types -p grammers-session -p grammers-mtsender -p grammers-mtproto -p grammers-crypto --all-features benchmark_ -- --ignored --nocapture --test-threads=1
cargo check --locked --release --workspace --all-targets --all-features
cargo clippy --locked --release --workspace --all-targets --all-features -- -D warnings
cargo test --locked --release -p grammers-session --no-default-features --features session-import -- --test-threads=1
```

Validation: 402 release all-feature tests/doctests passed (30 ignored), all-target/
all-feature check and strict Clippy passed. Without SQLite, importer tests passed
51 tests/doctests (one benchmark ignored); the minimal memory library passed 28
tests (one ignored). All 15 selected local benchmarks passed, including the nine
existing preservation checks.
No authenticated Telegram test, consumer change, VPS deployment or live speed
measurement was performed. Session import is optional and has no request-path
cost when disabled; see [session documentation](grammers-session/README.md).

## Upstream integration (2026-10-13)

Merged Codeberg through 49019a5 (layer 230). The existing shared-body, borrowed
reply, gzip, ACK, keepalive and tracked-routing optimizations remain. Conflict
resolution retains retirement-before-wakeup and does not restore eager error
formatting when retry is rejected. The schema/logging changes do not establish
an end-to-end latency improvement.

342 release all-feature tests/doctests passed (24 ignored), strict workspace
Clippy passed, and the nine existing local `benchmark_` cases passed separately.
These exercises include synthetic buffer allocation, result/gzip/ACK/timer reuse,
request clones, invocation tracking and routing. They are preservation checks,
not a controlled before/after production benchmark; concurrent builds can affect
absolute timings. No further send-path optimization was justified by this audit.


Baseline: Codeberg `82eba650098b54e4c2cb9de83fdab7ea2d21a957`.
These changes reduce specific allocations and fix failure paths. They do not
establish a whole-application CPU reduction or eliminate Telegram rate limits.

## Raw-input robustness audit (2026-10-11)

The owned/shared and tracked/untracked raw invocation paths now reject unaligned
or oversized request bodies before allocating a response channel or routing them.
Previously these reached the encrypted framer's assertions and could panic the
shared sender task. Validation shares the framer's size limit and automatic
`invokeWithoutUpdates` classification through `Encrypted::request_fits`; service
requests and already wrapped requests retain their full capacity. The existing
limit applies before compression, so compressible input support is unchanged.
The error is `InvocationError::Io` with `InvalidInput`; bodies shorter than a
constructor retain their existing error.

The framer also now accepts an exact-limit uncompressed request. Its previous
`>=` capacity check returned `None` indefinitely although its preceding assertion
permitted equality. Regressions cover exact limits and four bytes over them with
updates on/off, API/service/already-wrapped bodies, compressible maximum input,
cold/cached routes, all four entry points, empty queues after rejection, and
release of rejected tracked permits. Sent-request ownership, keepalives, crypto
validation and retries are unchanged.

The established routing, framing, crypto, gzip and delivery paths were also
reviewed. An in-place TLS gzip-decoder prototype was rejected after a native-CPU
Windows release comparison with eight samples and rotating execution order: 512-byte replies took
621.9 ns versus the current 620.0 ns, repeated-pattern 2 KiB replies 1243.1 versus
1242.6 ns, and pseudorandom 2 KiB replies regressed to 278.6 from 261.2 ns.
The prototype and its benchmark edits were removed. Existing decoder reuse,
shared request bodies and result storage remain. This audit claims no reduction
in whole-process CPU, RSS, or end-to-end RPC latency.

See UPSTREAM.md for the canonical upstream fetch and explicitly deferred changes.

Validation on the final source: 334 release all-feature workspace tests/doctests
passed (24 ignored), all-target/all-feature `cargo check` and strict Clippy
passed. Commands used `--offline --locked` and the native CPU release flag for
tests. No authenticated RPC, deployment or public push ran.

## Gzip reuse and request retirement (2026-10-04)

The shared/raw RPC path was inspected through warm routing, socket reads/writes,
framing, crypto, gzip, result delivery and invocation tracking. Two changes were
retained after focused validation:

- Gzip decompression reuses flate2's resettable bufread decoder per OS thread.
  This removes repeated inflate-state construction without allocating a decoder
  for every connection. Input scratch owns its bytes and retains at most 64 KiB
  of compressed-input capacity; larger scratch is discarded. Output is separately
  owned by the caller. The footer provides a capped capacity hint, never proof of
  validity or a maximum output size. Normal gzip header/CRC/length validation and
  rejection of trailing bytes/extra members remain. Failed decodes cannot poison
  later streams. No unsafe borrow extension, new dependency or wire change is used.
- Request retirement now precedes receiver notification on RPC errors, decode
  failures, connection failures and terminal protocol rejections, matching the
  successful-result path. Cold-setup errors explicitly release the reservation.
  Queued request fields drop the permit before the response sender. A deterministic
  receiver-waker test reproduced the previous behavior: the callback observed
  `Sent` instead of `Idle` while being told the request had completed. Tests now
  cover both explicit completion and queue/connection drops. Sent cancellation
  ownership and rules against replaying accepted requests are unchanged.

A redundant `Ok(...await?)` wrapper in guest-chat editing was also removed to
restore warning-free workspace Clippy with the current compiler.

Windows x86-64, rustc 1.94.1, flate2 1.1.10 with zlib-rs/runtime detection,
release with `-C target-cpu=native`; medians of eight alternating samples using
deterministic synthetic payloads:

| Plain bytes | Packed bytes | Data | Fresh decoder | Reused decoder |
|---:|---:|---|---:|---:|
| 512 | 289 | Repeated byte pattern | 1.356 us | 0.664 us |
| 2,048 | 302 | Repeated byte pattern | 2.019 us | 1.311 us |
| 8,192 | 359 | Repeated byte pattern | 5.091 us | 4.464 us |
| 65,536 | 591 | Repeated byte pattern | 36.478 us | 34.168 us |
| 2,048 | 2,071 | Pseudorandom bytes | 0.918 us | 0.279 us |

These measure complete gzip-to-owned-Vec decoding for these fixtures, including
the reused path's input copy and output allocation. They omit socket scheduling,
framing, crypto and server work, and apply only to gzip-packed replies. They do
not establish a whole-application CPU saving or an end-to-end latency improvement.
Reuse exchanges small retained per-thread working storage for less allocation
churn; this is not evidence of lower process RSS.

The tracker benchmark measured 39.6 ns for reserve/write-stamp/release versus
11.6 ns for a comparison loop omitting the timestamp. Timestamp behavior remains
unchanged: this small component cost does not justify losing write diagnostics.
Warm routing, shared request bodies, borrowed envelopes, reusable result vectors,
keepalive timers, hardware crypto and acknowledgement batching were already in
place. No runtime/allocator/crypto replacement was supported by these measurements.

Reproduce without account credentials or remote traffic. On a fresh checkout,
create the local ignored lockfile first with `cargo generate-lockfile`, then use:

```sh
cargo test --locked --release -p grammers-mtproto benchmark_reused_gzip_decoder -- --ignored --nocapture --test-threads=1
cargo test --locked --release -p grammers-mtsender benchmark_invocation_tracking_cost -- --ignored --nocapture --test-threads=1
cargo test --locked --release --workspace --all-features -- --test-threads=1
cargo check --locked --workspace --all-targets --all-features
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
```

Use the native CPU flag above when comparing with the table. Default/ignored test
separation remains: the selected benchmarks are local, and live network probes
remain ignored. Verification passed 330 release all-feature workspace tests and
doctests (24 ignored), workspace all-target/all-feature checks and strict Clippy.
An isolated consuming application passed 219 release tests (9 ignored) and strict
Windows/Linux-target Clippy with all eight SDK packages overridden to this source;
its production manifest and lockfile were not changed by that compatibility check.

## Request and reply ownership

- `SenderPoolHandle::raw_invoke_shared_in_dc` accepts a reusable `RequestBody`
  (`bytes::Bytes`). Queueing and retrying shared bodies do not clone the payload.
- The existing `raw_invoke_in_dc(Vec<u8>)` API remains available and transfers
  its buffer into `Bytes` without copying. Its caller may still be making copies;
  use the shared API for repeated prepared requests.
- Typed invocation also shares its serialized body across retries. Retry-policy
  semantics are preserved, including explicit `NoRetries` configurations.
- Decoding an owned RPC envelope reuses the envelope allocation for the result.
  Removing the header still moves payload bytes within that allocation.
- TL strings and compressed envelopes borrow their input while decoding. Owned
  return values remain owned; no borrowed receive buffer escapes its lifetime.
- Gzip uses the pure Rust zlib-rs backend and retains CRC/length validation.
  Applications which already selected this backend should not expect a second
  backend-related improvement.

For a serialized request prepared once:

```rust,ignore
use grammers_mtsender::RequestBody;
use grammers_tl_types::Serializable;

let prepared = RequestBody::from(request.to_bytes());
let response = handle
    .raw_invoke_shared_in_dc(home_dc, prepared.clone())
    .await?;
```

Raw calls still bypass the SDK retry wrapper. Applications must preserve their
own deadlines and respect actual flood waits.

## Connections and cancellation

Initial receive/write allocations now start at 4 KiB rather than roughly 1 MiB
each. Receive capacity doubles when a partial packet fills the buffer, retaining
the original 1 MiB + 8 KiB hard receive limit. Larger packets remain supported;
the first large response on a connection may need extra growth allocations and
reads. Small-RPC workloads benefit most. Capacity figures are not RSS measurements:
allocator behavior, lazy pages and socket buffers also affect resident memory.

Cancelled callers are rejected before routing/connection creation where possible,
and unsent, unframed requests are removed before serialization. Requests already
serialized or sent are retained for framing and update correctness; dropping a
future cannot undo a remote operation. Retryable protocol rejections do not resend
a request whose caller is gone. Protocol-owned keepalives remain independent of
caller cancellation and no longer allocate an unused response channel.

A zero-byte write with pending output returns `WriteZero` instead of repeatedly
attempting the same write. Invalid message lengths return decoding errors instead
of panicking. Raw request bodies shorter than a constructor fail before queueing.
Encryption, authentication, packet checksums and acknowledgement handling remain.

## Local release microbenchmarks

Measured on Windows x86-64, rustc 1.94.1, using synthetic data. Payload/reply clone
comparisons alternate execution order and report the median of ten samples.

| Operation | Previous path | Fork path |
|---|---:|---:|
| Clone 152-byte request body | 19.3 ns | 11.7 ns |
| Clone 1,024-byte request body | 28.4 ns | 11.7 ns |
| Clone 8,192-byte request body | 64.6 ns | 14.0 ns |
| Extract 512-byte RPC result, including input allocation | 54.4 ns | 35.1 ns |
| Extract 2,048-byte RPC result, including input allocation | 70.5 ns | 45.5 ns |
| Extract 8,192-byte RPC result, including input allocation | 134.5 ns | 98.0 ns |
| Allocate initial zeroed receive buffer | 5,449 ns / 1,056,768 B | 53.3 ns / 4,096 B |

The last row is an isolated construction loop, not network connection throughput.
These are not full RPC timings or a forecast of VPS CPU savings. Shared-body use
requires adopting the new API; other internal changes apply through existing APIs.

Reproduce without contacting Telegram:

```sh
cargo test --workspace
cargo check --workspace --all-targets --all-features
cargo test --release -p grammers-mtsender -p grammers-mtproto benchmark_ -- --ignored --nocapture --test-threads=1
```

Tests cover borrowed TL lengths/padding/truncation, lossy UTF-8 compatibility,
reused RPC result storage, corrupt/truncated gzip, cancellation versus keepalives,
partial receive growth over localhost TCP, request-buffer ownership, typed retries,
and invalid inputs. An existing application also passed its 168 release tests with
temporary local dependency overrides; its active dependencies were not changed.

Final validation: 272 passing workspace tests/doctests with default features;
289 passing with all features. The respective 16/17 ignored tests include explicit
microbenchmarks, network probes and upstream documentation examples. All-targets,
all-features compilation passed. The three release microbenchmarks above passed.

The upstream unauthenticated public-test-DC integration test passed once during
validation. Tests contacting Telegram are now opt-in (`--ignored`); ordinary test
runs use no account credentials. Compiler/lint checks use the current Rust toolchain;
upstream Clippy style warnings remain and strict `-D warnings` is not clean.

Before deployment, compare against upstream on one server at matched throughput:
CPU per completed request, memory, tail latency, timeouts and flood waits. Faster
throughput alone can increase total CPU even when per-request work decreases.

## ACK, reply-list and routing follow-up (2026-09-19)

- ACK serialization writes directly into the outgoing message buffer. Its message-ID
  vector is cleared and retained instead of discarded, with no temporary serialized
  ACK Vec. Constructor, IDs, sequence number and message length stay identical.
- Sender returns consumed deserialization-list storage to MTP for reuse. Encrypted
  MTP retains capacity up to 64 entries; larger one-off lists are released. This is
  a storage-retention bound, not a response-count limit. Result bodies still move
  into their callers. The new trait hook has a default no-op for compatibility.
- Established DC connections receive shared requests directly instead of making
  the pool task forward each call to the connection task. The routing cache is per
  pool, uses a short RwLock read and a sender-handle clone, and is written only on
  connection/control changes. No lock is held while sending or awaiting replies.
  It adds small retained per-pool storage in exchange for one fewer queue handoff.
- A rejected channel send returns a request which was not queued and may use the
  normal pool path. A request accepted by a connection is NEVER automatically
  resent here if its reply disappears. Typed retry policy still decides typed
  retries; raw invocation still bypasses that policy.
- Disconnect invalidates the route immediately. Pending disconnect counts prevent
  a concurrent connection initialization from publishing a stale route; different
  DCs remain independent. Quit blocks publication and clears cached senders so they
  cannot keep connection tasks alive after pool shutdown.

All changes apply to existing shared/raw and typed APIs; no consumer API change is
needed. No new runtime dependency, per-request task, timer or protocol relaxation
was added. Multi-thread Tokio is enabled only as a dev dependency for benchmarking.

Windows release medians (eight alternating samples):

| Component | Previous path | Follow-up |
|---|---:|---:|
| One-ID ACK serialization, including message header | 121.9 ns | 27.0 ns |
| Synthetic request routing, single-thread runtime | 1,105.9 ns | 1,060.1 ns |
| Synthetic request routing, four-thread runtime | 2,200.9 ns | 1,904.6 ns |

Routing includes a shared body, response channel, trivial four-byte response and
task scheduling, but no sockets/encryption/Telegram. The baseline keeps routes
uncached and forwards through the pool task. These component timings do not predict
complete RPC latency, Linux CPU reduction or claim wins. Response-channel and owned
reply allocations still exist; the complete request is not allocation-free.

Validation: 297 all-feature workspace tests/doctests passed, 19 ignored;
all-target/all-feature compilation passed. Workspace Clippy completed with existing
upstream warnings. A consuming application's 169 release tests and strict release
all-target Clippy passed with isolated local overrides. New tests cover ACK bytes
and capacity, result-list reuse/retention, direct routing, failed-send fallback,
lost replies without replay, cancellation, repeated disconnects, late publication,
pool shutdown and cached-handle release. No live Telegram/account operation ran.


## Timer, reply decoding and optional update delivery

Each sender now retains one pinned keepalive Sleep and resets it only when its
keepalive fires. Cancelling a step does not postpone the deadline. This adds one
small retained allocation per connection (112 bytes for Sleep in the measured
Windows build) instead of repeatedly registering and discarding pending timers.

Incoming message envelopes and container children borrow from decrypted input.
Containers validate every child envelope before processing any child. Final RPC
results still own their bytes; gzip-produced storage is reused where possible.
Length, constructor, checksum and protocol validation are retained. This does not
make a complete RPC allocation-free.

ConnectionParams.receive_updates defaults to true. Setting it false before pool
creation wraps API calls with Telegram's invokeWithoutUpdates, acknowledges but
skips unsolicited API update delivery, and suppresses duplicate own-update copies.
Explicit RPC return values, errors and required MTProto service messages remain
available. The update channel is closed and update-stream creation returns an error.
Already wrapped calls are not wrapped twice; service requests are left unchanged.
A compatible explicit struct literal must initialize the new fields, or use defaults.

Optional InvocationTracker reservations follow a request inside the SDK. Dropping
a waiting caller does not release a serialized/sent request's reservation. It is
released when the request completes, is discarded before transmission, or its
connection is torn down. PendingInvocation is a movable response future without
an additional task or timer. The old untracked APIs remain available. The Sent
stage means a transport packet was fully written, not that the server processed it.
No automatic retry or consumer-specific scheduling policy is added.

ConnectionParams.connection_timeout optionally bounds connection/key exchange/API
initialization. Its default None preserves existing unlimited establishment. It
does not impose a deadline on ordinary RPCs or persistent session-storage calls.

Windows release medians from eight alternating synthetic samples:

| Component | Previous path | New path |
|---|---:|---:|
| Pending keepalive timer creation/poll/drop versus reuse | 339.9 ns | 18.4 ns |
| 512-byte reply envelope to owned result | 40.7 ns | 32.6 ns |
| 2048-byte reply envelope to owned result | 69.3 ns | 48.5 ns |
| 8192-byte reply envelope to owned result | 110.1 ns | 79.3 ns |

Reproduce with the ignored benchmark_ping_timer_reuse and
benchmark_borrowed_response_envelope release tests. These omit sockets, encryption
and server work; they do not establish total CPU or end-to-end latency savings.

Validation: 310 all-feature workspace tests/doctests passed, 22 ignored, plus
all-target/all-feature compilation. Subsequent warning cleanup passed strict
all-target/all-feature Clippy with -D warnings. Tests cover cancelled callers, retained reservations, sent/unsent
lifetime, initialization expiry, borrowed container validation, compressed replies,
update suppression, RPC errors and required control responses. The opt-in
unauthenticated public-test-DC initialization/GetNearestDc test passed; it uses no
account credentials. Authenticated account/bot update suppression and production
performance remain deployment checks.


## Send/receive robustness audit (2026-09-20)

Reviewed the raw/typed invoke path, warm routes, connection initialization,
request cancellation/retirement, socket framing, reply dispatch, gzip handling,
and temporary buffer ownership. This is a source and offline-regression audit,
not a production CPU profile or a claim that every possible issue is eliminated.

Reproduced and fixed:

- RPC payloads inside gzip now use normal constructor validation and error
  classification. A compressed rpc_error previously reached callers as a success
  payload; an empty/short decoded payload could reach a sender assertion. Both
  now produce the appropriate RPC/deserialization error. Gzip validation remains.
- rpc_drop_answer acknowledgements complete the cancellation RPC itself. They
  previously disappeared during dispatch, leaving its caller pending. This does
  not retire the separate original request or imply remote work was undone.
- A cancelled caller now interrupts its uncommitted cold initialization. Quit
  and matching-DC disconnect also interrupt setup, including with no configured
  connection deadline. Unrelated DC control changes do not cancel it. A per-pool
  Notify is registered only during cold setup, with registration before checking
  control state; established RPCs gain no task, timer or notification subscription.
  Already-framed/sent request tracking and the no-replay policy remain intact.
- Intermediate transport validates header plus entire advertised body before
  returning offsets or reading a status. Fragmented input previously could panic
  or produce an out-of-bounds range. Abridged extended lengths are decoded as
  unsigned 24-bit values, avoiding huge wrapped offsets. Negative status conversion
  handles the minimum signed integer without a debug-build overflow.
- Typed retry handling no longer eagerly allocates/formats an error String before
  knowing whether a retry is permitted. Retry logging formats the borrowed error
  only when retry is approved and the log level is enabled. Returned errors and
  policy decisions remain unchanged; raw calls already bypass this typed path.

The new regressions reproduced failures before fixes. Current validation:
319 all-feature workspace tests/doctests passed, 22 ignored, and strict
all-target/all-feature Clippy passed. Tests use local fixtures/loopback sockets;
no credentials or production RPCs are required.

A separate native-CPU Windows release experiment preallocated gzip output using
its trailer size, capped at 32 KiB. Eight alternating medians improved only
0.3-1.4% over 256-16384-byte synthetic outputs, so it was rejected. Production
allocation policy is unchanged. Prior AES experiments were also rejected.
These fixes target failure handling and avoidable typed-error work, not a measured
reduction in steady-state whole-process CPU or network latency.
