# grammers fork

## Large TL values (2026-11-10, evening)

- Ten rare, large update payloads are boxed by the existing full-name rule.
  `Update` measured 240 B instead of 760 B; normal message updates stay inline.
  Keep the owned-conversion/wire/partial-drop tests and the 256 B size guard.
- Full `webPage` and `game` payloads are boxed at their leaves, shrinking
  `MessageMedia` to 304 B and `Message` to 1,608 B. Their update and media wrappers
  stay inline; do not reintroduce a redundant outer box on webpage updates.
- Enum deserializers charge TL constructor depth independently of Rust heap
  boxing. A storage change must preserve the cursor's RAII depth scope.
- `messageReplyHeader.reply_media` is an optional boxed field, using the existing
  generator rule. It keeps absent reply previews from reserving the full media
  enum in every message and short update. Keep ordinary replies allocation-free.
  The wire tests cover absent/present media and every truncated prefix, including
  failure after owned media fields have decoded. Preserve those cleanup checks.
- This round's live checklist and measurements are in
  [benchmarks/2026-11-10-values/README.md](benchmarks/2026-11-10-values/README.md).
  The owner approved API changes on `main`; retain only measured gains and document
  each migration. The previous 5% acceptance bar no longer applies.

## Typed client path and the coarse clock (2026-11-10)

[Measurements, the profile, what was rejected and what was verified](benchmarks/2026-11-10-typed/README.md).

- `update::Message` holds its message once. `Update::from_raw` moves it out of the raw
  update: do not add a field that keeps that update, and do not clone the message there.
  The update is put back together from the message, its `Kind` and its position, by
  `Message::raw()` on first use, by `into_raw()` without a clone, and for `Update::Raw`
  when the message names no peer. A fifth update carrying a message needs its `Kind`.
- The bot workload's "live heap" is `mallinfo`, which counts chunks parked in the
  allocator's thread cache. Before reading a shift under 1% as retained memory, repeat
  the comparison with `GLIBC_TUNABLES=glibc.malloc.tcache_count=0`.
- The whole API schema now passes single-unit `impl-serde` check and Clippy at
  3.28/3.30 GiB resident, without defaults (`impl-serde,tl-api`). This uses the owner's
  heavy-unit exception: one compiler, nice/ionice, no address-space cap, memory and
  swap watchdogs. Full all-feature expansion still hits the watchdog, at 3,554 MiB
  sampled; check that graph in parts with
  `benchmarks/2026-11-10-typed/tools/serde_parts.py` when the host cannot fit it.
  Never disable the watchdog. The ordinary command cap is unchanged.
- A closed-loop change of 5% needed 30 rounds to resolve on a noisy day; 10 did not.
- `grammers_mtproto::coarse_clock` is the kernel's last tick, read without a system call:
  never later than the precise clock, up to a tick (1 to 10 ms) earlier. Message
  identifiers, `UpdatesReceiver::turn_now` and `StoredPeers::forget_if_old` read it. Use
  it only for what is compared with deadlines far coarser than a tick, and never to
  stamp something a caller reads as a time (`InvocationTracker::sent_at` stays precise).
- **After a timer fires, read the precise clock.** A coarse reading can be a tick short
  of the deadline that woke the task: checking it again would find nothing due and spin
  until the tick. The receiver sets `deadline_fired` when its wait times out, and
  `a_deadline_that_fired_is_found_due_without_waiting_for_the_tick` fails without it.
- The offset correction and the identifiers read the same clock. A precise reading taken
  first can be later than a coarse one taken after it: do not subtract one from the other.
- That module has the workspace's second `unsafe` block, the call to `clock_gettime`,
  which replaces "the only `unsafe`" in the 2026-11-09 section below. Keep it at that
  one function. Its conversion to `Instant` and its check against the precise clock are
  plain functions with their own tests.
- Its gain exists where the clock is a system call, as here. Say so beside every number
  that includes it, and count reads per request with `sysc.c` before adding one.
- `PeerMap` keeps up to sixteen peers as a list (`Peers::Few`) and more in a map. Both
  keep the later of two peers with one identifier. Responses without peers share
  `Peers::empty()`: do not allocate an empty map for a message.
- One sent message is sorted out by `map_random_id_to_message`, albums and forwards by
  `map_random_ids_to_messages`. They must choose alike:
  `one_sent_message_is_chosen_as_among_several` runs both on the same replies.
- The connections this library makes build `Encrypted` with `own_updates_in_results`: an
  `OwnUpdate` then arrives empty, and `process_mtp_buffer` reads the update from the
  result that follows, before that result retires its request. A new place that builds
  an `Encrypted` for a connection needs the option, or it copies every such reply twice.
- The last three were kept for allocations, with no measurable CPU gain: the profile at
  `f1dd333` has no single cost left on the typed path that does not need generated
  types to change shape. Read "What is left" in the measurements before looking again.
- A request's reply is decoded by its connection and again by its caller. Handing the
  caller a copy was built two ways and measured at -3.8% and 0%: a copy of a decoded
  reply costs what decoding it costs. Do not build it a third time without shared
  ownership of the decoded message, which is a break of `message::Message::raw`.

## Update path clock, and `io_uring` assessed (2026-11-10)

[Measurements, the tools and what is left](benchmarks/2026-11-10/README.md).

- The rule below about the clock covers the update path too. `UpdatesReceiver::next` reads
  it once for a turn of its loop (`turn_now`), for the deadline check and for the batch it
  applies, through `MessageBoxes::check_deadlines_at` and `process_updates_at`. That
  reading is dropped at the start of a turn and after everything in it that can wait: a
  step added after an `await` must not see a time from before it.
- A recovery pause is an `Option` and is compared with the clock only while one is set.
- The update stream ages the table of stored peers with `UpdatesReceiver::last_clock_read`.
  A peer's lifetime still starts at the clock, and callers without a recent reading read
  it: do not reuse an old reading there, or a client that stopped streaming never forgets.
- Find who reads the clock with `benchmarks/2026-11-10/tools/clockbt.c` after touching
  `UpdatesReceiver::next`, `MessageBoxes::process_updates` or `Client::cache_peer_map`.
- `io_uring` is not used. Its ceiling on this kernel (5.15), in a loop with no library
  around it, is 4.5% of client CPU at 64 connections per thread and a loss of 18 to 21%
  for a client that only does I/O. Measure the ceiling again with
  `benchmarks/2026-11-10/tools/uring_ceiling.c` on a newer kernel before building on it.
- The typed path's remaining measured costs (`update::Message` cloning its message, a
  send's reply decoded for the caller and again for the update stream) need public type
  changes. They are the owner's decision, not an optimization to slip in.

## TL layer 233 (2026-11-10)

[Measured sizes, the `pts` evidence and every handwritten site](UPSTREAM.md).

- `Update` is 760 B (752 B at layer 230; 1,264 B with this schema and no new rule).
  `tl-gen` boxes `wallet.card` as a definition and `wallet.nftItem`'s `gift` as a field:
  `BOXED_FOR_SIZE` matches full names, and `BOXED_FIELDS_FOR_SIZE` holds optional fields
  in a `Box`. Box what a layer adds, as a field when its definition already existed, so
  that generated types callers already use keep their shape. The guard stays at 1,024 B.
- For a new layer, build `grammers-tl-types` alone and read the sizes first: the guard in
  `depth_tests.rs` prints them.
- `updateMessageWidgetData` is on the common `pts` sequence and rejected for a channel
  `peer`, like `updatePinnedMessages`. TDLib, Telegram Desktop and Telegram for Android
  agree; Telegram's page for it was not written when this was decided.
- `PtsInfo::raw_from_update` has no wildcard: a new update must be given its sequence to
  compile (and an arm in `valid_update` if it names a peer). One with `pts` or `qts`
  treated as having none is delivered, and the next update of that sequence then looks
  like a gap, costs a difference request and is delivered a second time.
- Every update carrying `qts` advances the secondary sequence by one, however many items
  it names: `qts` events are never grouped. Do not derive a count from a vector's length.

## Request path, crypto and socket buffers (2026-11-09)

[Measurements, the path of one request and what is left](benchmarks/2026-11-09/README.md).

- On a host this size never run `cargo test --release --workspace` in one command: it
  exhausted memory and froze the machine. One crate at a time, `-j 2`, nothing else heavy
  alongside, and no measurement while anything builds.
- The clock is a system call on this host. Nothing on the request path reads it without a
  measured reason: one wall-clock read per packet (`get_new_msg_id`), and the write's one
  monotonic read serves tracked requests and `Mtp::on_packet_sent`. Reads and writes only
  set flags, which `Sender::idle_tick` ages at keepalive ticks. Count per request with
  `benchmarks/2026-11-09/tools/sysc.c` after touching `Sender::step`, `Encrypted::push`,
  `finalize` or `queue_ack`.
- `Encrypted` stops checking the acknowledgement delay itself once a driver has read
  `service_deadline`: a driver that reads it must call `on_service_timeout`. An
  acknowledgement's delay may start before its arrival, at the last reported write, never after.
- A connection holds a socket buffer only while a packet is in it; between packets they
  are spares of the thread. A read on a plain socket waits for readiness first, polled
  directly so that the connection task does not grow. TLS streams keep their own buffer.
  The spare is 4 KiB: 16 KiB cost a one-connection process 34 KiB. Busy connections grow
  their own to 16 KiB as before. Re-measure with `compare-overload.py` and the heap census.
- AES-IGE has an AES-NI path and a portable one that must stay byte-identical, and incoming
  packets are decrypted and hashed in one pass (`ige_decrypt_hashed`). The only `unsafe`
  is the call into those functions after runtime detection. Encryption cannot be joined
  with its hash: the key depends on it.
- Padding bytes come from the system generator 512 at a time, each used once and cleared.
- TL layer 233 was deferred in this round and taken on 2026-11-10: see the section above.

## Bot workload and compliance follow-up (2026-10-31)

[Measurements](benchmarks/2026-10-31-bot/README.md) and [audit with sources](COMPLIANCE-2026-10-31.md).
Keep `grammers-client/tests/bot_workload.rs`'s mock in its own process, and heap accounting
out of its CPU window. Mock-only: never point it at Telegram. Live checks go through the
ignored `live_telegram_bot` test, with a credential the owner provided for it.

Keep these, each measured or required by the audit:

- `send_message` and anything like it must not hold `tl::enums::Updates` across an await:
  destructure it first in a plain function. Long-lived futures stay small, so box a value
  held across a yield in `run_sender`. Size guards are in `depth_tests.rs`.
- `tl-gen`'s `BOXED_FOR_SIZE` keeps `Update` near 752 B. A schema rename silently unboxes,
  which those guards catch. This diverges from upstream: see UPSTREAM.md.
- Do not sort `Vec<Update>` with the standard stable sort on a path taken per batch.
- A full update queue gets one yield per batch, then drops. Dropping costs a recovery
  request to Telegram; more yields stall RPCs on a stalled consumer.
- Written peers are only remembered after storage succeeds, and forgotten every ten minutes,
  because a `Session` may forget them. Never write fewer than a peer's changes.
- Every recovery failure honours `retry_after` and backs off to 60 s; unreadable replies keep a
  short pause so that their limited attempts end. Per-channel delays apply to any cause of a
  difference. Updates beyond a position stay in the gap, capped, never advancing it.
- Connection attempts are paced per DC. `Dropped` and `InvalidDc` never reached a server.
- A read-buffer change also changes fairness: reads cost budget per 2 KiB, a measured trade
  against quiet pools. Re-measure with `compare-overload.py`, not only the bot workload.

Limits Telegram's servers enforce are not this library's to raise. Report them as such.

## Audit follow-up (2026-10-31)

[REVIEW-FOLLOWUP.md](REVIEW-FOLLOWUP.md) records the status of every remaining audit
item against `2ffd991`, current tests and measurements, residual limits and the owner's
direction replacing second-model review with a fresh-eyes Claude subagent. Historical
acceptance of deferrals below does not waive what it lists as unresolved.

Preserve the generated-TL depth budget and RAII restoration: partial fields and decoded
siblings must stay safe to drop on failure. It counts boxed values only, because every
schema recursion passes through one; recheck that if bare recursive types ever appear.
The limit of 64 is a resource policy sized from measured stack use, and an overly small
limit wedges updates on real data, so re-measure before lowering it.

A failed recovery RPC, or failing to store its result, pauses retries, from a second up to a
minute as failures repeat (see the follow-up above), while live updates keep flowing; GetState
then replaces any provisional account position formed meanwhile. Releasing held
channel updates after a temporary failure must not re-queue updates beyond a gap: they wait in
that gap, or recovery would restart with no new traffic. Replay eviction by age must keep raising the floor
to the highest ID that actually left. Batched requests stay charged per request, or a
ready queue can do many times the work per poll. Grown socket buffers are released only
when empty and idle, which replaces the older note below that grown capacity is kept.

## Acceptance review corrections (2026-10-30)

The current source/tests/evidence disposition is in
[the acceptance ledger](benchmarks/2026-10-30/final/acceptance-review.md).
Acceptance/publication receipts are external under [PUBLICATION.md](PUBLICATION.md).
Preserve lifetime missing-hash exhaustion across entry removal, ready-only recovery
queues, confirmed login proof, and existing local channel PTS. Account updates
provide provisional proof; pre-login service pushes do not. Initial GetState 401
rejects provisional proof until paced self-metadata discovery confirms login.
Cached-self or successful GetState proof survives reconnects. Never query self
metadata on ordinary live delivery. Resolve unknown account kind once on first
channel recovery, retaining the result only after the lookup completes.
Retain update batches and interrupted peer-cache work across next_raw cancellation.
Resolve self-target send response identities without sentinel conversion or unwrap.
Handshake RNG and EncryptedBuilder setup use fallible entropy; obfuscation and
packet-padding RNG still panic on entropy failure. Started SRP work retains its
bounded admission.
The gate must print and optionally constrain the staged tree, require real fresh
analysis logs, and hash every final artifact except the manifest itself.

## Historical acceptance evidence continuation (2026-10-30)

That historical Rust/Cargo manifest was the second-Opus correction. Validation
again passed 651 workspace tests/doctests (42 ignored), the existing optional
feature counts and fmt/check/Clippy/Rustdoc. The evidence checker at that
checkpoint had 22 passing Python regressions: benchmark logs require exactly one successful named
test and complete execution membership, including current heap/allocation and
deadline-component runs. See final/acceptance-review.md under benchmarks/2026-10-30.
Acceptance is recorded outside this tree; see PUBLICATION.md.

## Corrections after second Opus review (2026-10-30)

Missing channel hashes may defer recovery at most 30 times (one-second receiver
retry delays). After grace exhaustion, warn and discard that channel's backlog
position so live updates resume, including with peer caching disabled. Exhaustion
must survive entry removal for that receiver lifetime; only usable access or a
successful difference clears it. The lost backlog cannot be reconstructed without
access; the 2026-10-31 follow-up retains live updates received during the grace. Track pending membership and
ready counts directly; deadline checks must not repeatedly scan the pending queue.

Replay floor means the highest actually evicted ID. Keep replay decisions stable
throughout a container. Deliver delayed RPC results/errors below the floor so
pending callers complete, suppress their own updates, and ACK/drop other stale
leaves except correlated Pong/FutureSalts and bad-message rejections. Correlated
service responses retire or retry callers without stale salt/clock/sequence
mutation. Empty future-salt replies retain the current salt and consume a bounded
prefetch attempt. Continuous ACK demand must not starve near-limit requests.

Opt-in liveness expiry gives the reactor one retained timer wake before its final
bounded read; cancellation cannot restart this grace. Return nonempty decoded
update batches. Rearm after both config and key storage. Negative RPC codes are
conservatively uncertain delivery, without changing default retry safety.

Generated TL recursion was then an accepted deferral, separate from MTProto/gzip
depth budgets. The 2026-10-31 follow-up above bounds it; historical detail is in the
scope ledger and REVIEW-OPUS-ROUND2.md.

Historical second-Opus correction validation: 651 release workspace tests/doctests
passed, 42 ignored; fmt/check/strict Clippy/warning-free Rustdoc passed. Optional
session configurations passed 50/72/57/92; minimal client 39, debug mentions 2,
evidence Python 19. All execution is parent-run; acceptance is recorded outside this tree under PUBLICATION.md.

## Update-state validation continuation (2026-10-30)

Validate GetState counters before persistence and keep recovery armed on rejection.
Checked peer constructors must validate ranges before arithmetic. Validate channel
IDs before filtering zero-PTS updates, and reject negative adapted date/sequence
fields before changing message-box state. Preserve valid peerless messages.
REVIEW-OPUS-ROUND2.md records that historical correction and evidence. Current
disposition is in the acceptance ledger; acceptance receipts are external. A
completed local validation run does not establish publication approval.

## Protocol hardening and review corrections (2026-10-30)

`OPTIMIZATION-2026-10.md` and `REVIEW-ROUND1.md` contain the scope, measured
limits and review dispositions. Keep outbound packet accounting independent of
inbound and expanded gzip sizes. Preserve shared nesting/work/expansion budgets,
exact-limit checksum validation, deferred ACK IDs and the original ACK deadline.
Invalid pushes cannot consume service state. Salt-prefetch packets remain alone.

Only difference results may bypass pts progression during catch-up. Socket pts
updates for a pending key must be deferred/dropped while non-pts updates remain
usable without advancing saved backlog state. Peerless messageEmpty channel
updates are valid. A secondary-only saved state needs fresh primary state.

Cold callers use a flag waker: poll new waiters once and rescan on closure wakes,
preserving cancellation across Tokio's cooperative budget. Unframed callers
retain definite-not-sent errors even if connection initialization wrote bytes.
Keep count/byte permits owned by SDK work until framing/delivery retires them.
The actual spawned `run_sender` future returns `(dc_id, result)` directly.
On connection failure, close and drain its RPC queue with the original shared
error and definite-not-sent certainty, retiring permits before waking callers.

Telegram's published DH prime skips only primality; subgroup checks still run.
PQ factorization and DH validation/exponentiation run in blocking jobs bounded
to four concurrently.
A started CPU job may finish after caller cancellation and retains its permit;
it cannot be aborted by an async timeout. Factorization uses one shared work
budget, bounded GCD batches, and a process-enforced regression deadline.

Keepalives replace a sent record even without a pong, preserving partially
written packets. Liveness below 60 seconds requires an earlier ping; huge
user timeouts must not overflow Instant. Preserve the message-ID floor across
clock jitter; reset only when more than three seconds ahead of corrected time. Exhaustive error enums gained variants: see CHANGELOG.md for migration.
GetState remains armed through caller cancellation and storage failure. Detect
missing account state independently of saved channels and check the self-user
cache for unproven restored state before rearming recovery. Account updates
provide provisional proof; pre-login service pushes do not. First channel recovery
also resolves unknown account kind once. Relearning account state preserves channels.
After four failed optional salt-prefetch attempts, keep the connection and use
bad_server_salt recovery. Container child sequence numbers above the parent
warn pending representative server evidence; other envelope checks remain strict.

Performance claims require identical release harnesses, paired measurements and
noise limits. Replay history is measured after more than 1024 replies; a nine-RPC
census does not represent steady-state heap. No runtime replacement follows from
microbenchmarks alone. New review artifacts retain native identities separately
from requested effort, and acceptance must come from the independent reviewer.

Round-one checkpoint validation: 590 release all-feature workspace tests/doctests passed,
39 ignored; all-target/all-feature check, strict Clippy, warning-free Rustdoc
and rustfmt passed. Minimal client: 39; session memory/import/string/file-import
configurations: 39/61/46/81 passed. Logs are under benchmarks/2026-10-30/round1.
These local checks do not constitute independent review acceptance or a hosted run.
Historical pre-Opus checkpoint: 599 release workspace tests/doctests passed, 41 ignored;
strict Clippy, all-target/all-feature check, warning-free Rustdoc and fmt passed.
Minimal client 39; session memory/import/string/file-import 40/62/47/82 passed.
Two explicit read-only public test-DC probes passed once each, without login.
See REVIEW-ROUND2.md for that historical scope. Current validation is recorded
in REVIEW-OPUS-ROUND2.md; historical test counts are not current receipts.

## Per-session footprint and AES-IGE (2026-10-23)

Applications may hold many pools, so idle per-session state matters.
Keep long-lived futures (`run_sender`, runner `run`/`run_checked`) as plain
functions returning `async move` blocks: `async fn` stores by-value arguments
twice. Keep connection setup boxed inside the runner. Socket buffers start at
1 KiB and keep grown capacity. Pool/connection queues use `queue.rs`, which
allocates on use; preserve cancel-safe receive, drain-after-close, dropping
queued items with the receiver and returning rejected items. The `PoolEvent`
buffer is created by the first subscriber. `MemorySession` stores DC options
compactly (boxed key) behind the unchanged public `DcOption`/`SessionData`.
AES-IGE runs inside the cipher backend closure and must stay byte-identical to
the reference test implementation. `tests/loopback.rs` measures heap/RPC cost
locally; see PERFORMANCE.md for figures and the AuthKey/DcOption review.

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

## Repeated raw invocations and buffers

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
do not establish server CPU savings or explain any connection-timeout behavior.

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
