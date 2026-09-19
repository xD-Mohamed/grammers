# Polling-oriented fork changes

Baseline: Codeberg `82eba650098b54e4c2cb9de83fdab7ea2d21a957`.
These changes reduce specific allocations and fix failure paths. They do not
establish a whole-application CPU reduction or eliminate Telegram rate limits.

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
