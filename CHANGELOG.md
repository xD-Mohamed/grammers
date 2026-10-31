# Changelog

## Unreleased audit follow-up

See the [follow-up ledger](REVIEW-FOLLOWUP.md) for the status of every remaining
audit item, and [its measurements](benchmarks/2026-10-31/README.md).

- Generated TL deserialization rejects values nested beyond 64 boxed levels with
  `deserialize::Error::DepthLimitExceeded`, replacing a process-aborting stack
  overflow. This is a library resource policy and can reject schema-valid values
  nested deeper. Exhaustive matches on that enum need a new arm; generated code now
  requires a cursor with `enter()`. Too-deep socket updates are treated as a gap.
- A failed update-recovery RPC, or failing to store its result, pauses retries for
  about one second. Errors still reach the caller, and live updates keep flowing.
- A difference reply that cannot be decoded no longer wedges its sequence: after 30
  consecutive failures its backlog is discarded with a warning. Those updates are
  lost. One channel's unreadable reply does not block its siblings.
- Socket updates received during a failed channel recovery are retained (at most 512
  across channels) and delivered when it ends, instead of being lost. Call the new
  `MessageBoxes::take_released_updates` if using message boxes directly.
- Bots try the zero-hash channel form before discarding a backlog with no cached
  access. This follows TDLib and has not been observed against Telegram.
- Copying authorization to a DC with no key creates the key first with one
  `help.getNearestDc`, so the import is attributed to a known key.
- Up to 8 already-queued requests now share a write, charging the cooperative budget
  per request. Local overload completion is +9.27% with busy p99 -19.07%; quiet p50 and
  p95 are about +5%. Closed-loop throughput shows no resolved change.
- Replay history stores 6 bytes per ID and also evicts IDs over 300 seconds of server
  time older than the newest, so quiet sessions keep far less. Socket buffers grown
  beyond 16 KiB are released after 60 seconds without a larger transfer, and read
  buffer growth stops exactly at the receive limit instead of reserving 2 MiB.
- The evidence checker refuses Python optimization and reports that no checks ran when
  privacy patterns are absent (status 2).

## 2026-10-30: protocol limits, initialization and recovery

- Bound handshake factorization and gzip expansion/work/nesting; validate DH inputs
  before arithmetic. Cache only the published safe prime and offload DH CPU work.
- Separate complete outbound packet limits from large inbound responses; preserve
  ACKs on deferral and keep salt-prefetch packets identifiable.
- Initialize each DC independently with shared failures, cancellation and byte
  admission. Correct clock skew and bound protocol rejection retries.
- Preserve delivery certainty, avoid ambiguous automatic replay, and keep original
  errors inspectable. Definitive local input errors are not retried by AutoSleep.
- Correct update catch-up duplication, peerless channel messages, orphan qts state,
  missing/inaccessible channels and home-connection failure recovery.
- GetState recovery errors now reach the update-stream caller, retaining state
  for an explicit subsequent next call instead of silently waiting or spinning.
- Empty reply albums fail locally; download errors remain errors on subsequent calls.
- Preserve the original connection failure for queued, unframed calls; close the
  route and release admission before notifying them. Avoid repeated full request
  scans on replies and full cold-waiter scans on every arrival.
- Keep GetState recovery armed across cancellation/storage errors, preserve saved
  channels when relearning primary state, and avoid logged-out recovery requests.
- Offload PQ factorization alongside DH work. Disable optional salt prefetch after
  repeated rejection without closing the connection. Diagnose unexpected container
  child sequence ordering with a warning while preserving other validation.
- Reject malformed GetState counters and live update metadata before changing
  recovery state. Repair invalid saved rows/counters with warnings at stream startup;
  strict `MessageBoxes::try_load` remains available for callers requiring rejection. Checked peer constructors reject extreme IDs
  without overflowing in debug builds.

### Post-review recovery corrections

- Preserve zero-QTS updates and valid siblings; skip malformed difference elements
  while applying validated response state. Recognize login after stream creation.
- Limit missing channel access-hash retries to 30 deferrals, then warn and release
  that saved backlog position so live delivery resumes without a peer cache.
  Remember exhaustion for that receiver lifetime so later recovery episodes do
  not repeat the grace; usable access or a successful difference resets it.
  The first grace suppresses live channel PTS updates and may lose them permanently
  when no hash becomes available, including with `auto_cache_peers: false`. (The
  follow-up above now retains those received during the grace.)
- Avoid repeated pending-queue scans during catch-up deadline checks.
- Deliver delayed RPC replies below the actual replay floor without replaying own
  updates or discarding valid siblings. Keep container replay decisions stable.
- Give the reactor a bounded opportunity to report buffered replies before opt-in
  liveness expiry, retaining the probe across cancellation.
- Preserve shared setup RPC errors, reject invalid mention IDs, retain working salts
  on empty replies, and prevent ACK traffic from starving near-limit requests.
- Classify unknown negative RPC codes as `UncertainDelivery`; default retry remains
  conservative.
- Retain an update batch before awaiting peer caching; cancellation no longer
  loses it. Preserve local channel PTS when receiving `updateChannelTooLong`,
  and let other channels progress after channel-scoped RPC rejection.
- Return unmapped send results through the existing fallback, preserve peerless
  channel edits as raw updates, and invalidate copied authorization after key rotation.
- Retire failed/wrapped optional salt replies, preserve sibling results on sequence
  correction, and offload SRP validation/proof work behind bounded admission.

- Preserve recovery for imported sessions without a cached self user: account
  updates establish provisional login proof; pre-login service pushes do not.
  Missing proof uses paced self-metadata checks, at most once per second.
  First channel recovery also resolves unknown account kind with one lookup.
- Preserve channel positions and propagate session-wide errors; unknown channel
  rejections defer recovery without erasing saved PTS or blocking other channels.
- Resolve missing self identity before sending to Saved Messages; `get_me` caches
  explicit self metadata even with automatic peer caching disabled.
- Refresh invalid SRP parameters once when B is outside the supplied prime, and
  preserve peerless guest-chat queries as raw updates.

### Known limitations and measured costs

Generated TL decoding is bounded to 64 boxed levels, so an authenticated deep value
is rejected instead of overflowing the stack. Values built by applications, serde or
third-party code are outside this, as are stacks well below 2 MiB, including
unoptimized builds on a 1 MiB Windows main thread.

Receive-arm-only budget placement was measured and rejected for its quiet-pool cost.
No stack profile was obtained because `perf` was unavailable; socket/runtime/thread
controls do not establish that Tokio is a bottleneck or justify replacing it.

Correctness costs against the historical baseline are still material, and no general
speedup is established. Against `7774ed6`, plain throughput is -4.23% [-8.23%, +4.81%],
overload completion +1.27% [+0.18%, +6.88%], busy p99 +96.45% and quiet p99 -43.87%.
Heap after a burst of 1100 replies is 7,911 to 14,031 B/session, and falls towards the
baseline as traffic slows. See the
[current measurements](benchmarks/2026-10-31/README.md).
The figures below describe the published `2ffd991` and earlier. At `2ffd991`, twelve
pairs measured plain throughput -3.15% [-7.23%, -1.53%], overload completion -0.91%
[-6.17%, +3.65%], busy p99 +124.27%, quiet p99 -45.47% and saturated heap 7,911 to
16,079 B/session, with dedicated heap-run process RSS +29.55%. Such figures vary between
sessions: a rerun with the current harness measured `2ffd991` at plain +2.16% and
overload -6.45%.
The historical post-Opus2 comparison measured plain throughput -5.80%,
CPU/RPC +5.94%, overload completion -3.58%, busy p99 +115.73% and quiet p99 -45.69%.
Saturated client heap rose from 7,911 to 16,079 B/session; heap-run whole-process
RSS rose 28.15%. Throughput/CPU losses remain unattributed. See
[measurement identities, intervals and scope](benchmarks/2026-10-30/final/README.md)
for the current build and historical comparisons; local/mock runs do not establish
production capacity.

The historical acceptance-round1 1 MiB reply scenarios used 90,504 KiB candidate
whole-process max RSS versus 15,052 KiB baseline at 100 offered calls/s, and
353,152 versus 71,652 KiB at 2,000/s. The baseline failed about half its calls because
it could not accept the valid large replies, so these are not equivalent-work
memory comparisons. The absolute candidate memory footprint matters for many
bots in one process; these figures include the local responder. Reruns at `2ffd991`
measured 92,388/15,056 KiB at 100 calls/s and 372,304/71,924 KiB at 2,000/s, with the
same baseline-failure caveat. Peak figures have not improved, because they are dominated
by traffic in flight. One 1 MiB reply no longer pins memory: a session holds about 1.06
MB right after it, and about 7.9 KB from the first transfer 60 s after it. Keepalive
replies provide that transfer, so an idle session releases within about two minutes.

Obfuscation and encrypted-packet padding still panic if operating-system entropy
fails. Fallible handshake and EncryptedBuilder setup do not cover those paths.

Pre-login service pushes, including QR login-token notifications, are delivered
without starting account recovery. Account updates provide provisional login
proof; a rejected initial GetState (401) warns with the RPC name and returns to
paced self-metadata discovery. It does not retry account recovery until confirmed
self metadata is cached by completing login or a successful explicit `get_me()`.
That call caches self metadata even with automatic peer caching disabled. A revoked
key will not recover merely by waiting; a failed `get_me()` surfaces its error.
Errors after cached-self or successful GetState confirmation still propagate.

Logout retains cached self metadata and existing stream state. Prefer a fresh
update stream after re-login. A stream used during QR/re-login can therefore return
account recovery 401 errors until login/2FA completes, at most about once per second,
and it delivers live updates between them. Tokens can also be checked explicitly on
your own schedule.
Ordinary live delivery does not read self metadata. If account kind is unknown,
the first channel recovery reads it once: cached bots retain limit 100,000;
missing metadata uses the valid conservative limit of 100 for that receiver.

Unknown channel-difference errors (400/403/406) preserve the channel's PTS and
allow sibling channels to recover. Contiguous live PTS updates resume after a failed
attempt, including those received during it. Updates beyond a remaining PTS gap stay
withheld until that gap is recovered: if Telegram keeps rejecting recovery they stay
withheld, rather than silently skipping data. The failure is logged as a warning.

Missing-hash grace postpones about 30 seconds of channel updates on the first recovery
episode, or longer under scheduling delays. Those received during it are retained, up
to 512 across channels, and delivered when it ends. Known bots then try the zero-hash
form. Otherwise, the backlog from before them is discarded and cannot be reconstructed
without access; user accounts have no fallback here. Exhaustion memory is per receiver,
grows with distinct exhausted channels, and is not persisted. New receivers can
therefore encounter a new grace.
Peer caching is required for reliable channel catch-up. Cancelled peer-cache work
is retained and retried on the next stream call before buffered updates are delivered.

Opt-in `liveness_timeout` measures time without a **complete valid packet**.
Partial transport progress does not reset it. Configure enough time for the
largest slow response, or keep the default `None`; trickled bytes cannot keep an
otherwise unresponsive connection alive indefinitely.

### Compatibility and migration

Exhaustive matches must handle new `InvocationError::{Read, Shared, BadMessage}`
and `ReadError::Shared` variants. `Read` includes `may_have_been_sent`; retry only
when application semantics permit it. Use `root_cause`, `category`, `transport_error`
and `std::error::Error::source` instead of assuming every connection error is `Io`.
`DeserializeError::{InvalidEnvelope, ResourceLimit}` and authentication
`Error::{InvalidPq, InvalidDhParameters, RandomnessUnavailable}` also require exhaustive-match updates.
`DhGenRetry` is now reported distinctly from `DhGenFail`.
The legacy tuple-returning `grammers_crypto::factorize` now panics on invalid
non-semiprimes such as `30` or exhausted work; use fallible `try_factorize` for
untrusted inputs. Session construction offers `Builder::try_finish`; the legacy
`finish` retains its entropy-failure panic while sender setup uses the fallible path.

New public configuration fields include `max_pending_bytes`, `liveness_timeout`
and `decode_limits`. Update explicit struct literals or use `..Default::default()`.
Low-level unlimited admission remains the default. Oversized bodies relative to
the entire byte budget return `InvalidInput`; temporary exhaustion is `WouldBlock`.
The conservative uncompressed request ceiling is now 1,044,372 bytes, less any
automatic wrapper. Inbound 1 MiB file data remains supported.

See [the scope ledger](OPTIMIZATION-2026-10.md) and [review corrections](REVIEW-ROUND1.md)
for resource policies, measured costs and remaining review gates.

## 2026-10-21: managed client and portable sessions

### Added

- `Client::builder`, `start`, `connect`, `run`, `run_until`, and awaited `stop`.
  Device settings are grouped in defaultable `DeviceConfig`.
- Explicit session sources for native SQLite, native strings, Pyrogram and
  Telethon strings/files, and gotd JSON text/files. Direct named parsers are
  available without constructing a client.
- Native `grammers:1:` session strings with bounded canonical parsing, exact
  home-DC routes, explicit secret extraction, and redacted diagnostics.
- `client.export_session_string()` returns a native `String` from every
  supported session source; `export_session_secret()` offers a redacted wrapper.
- Request admission limits, connection/status events, typed error categories,
  wait-duration helpers, and cumulative `RetryBudget`.
- Defaultable `SendOptions` and `send_message_with_options`, preserving the
  existing two-argument send API and rich message payloads.
- Optional sequential dispatcher/filter composition and explicit QR login,
  including existing 2FA handling.
- Updated examples and new lifecycle, error, session export, dispatcher,
  QR, send-options, and offline native-format demonstrations.
- GitHub Pages guides and automatically generated Rust API/method reference,
  with local parameter types, source-backed account availability, code colors,
  search, and copy controls.

### Fixed

- Expected/unsupported auth responses and malformed password state now return
  errors instead of panicking. Password-token diagnostics are redacted.
- Downloads no longer detach workers on cancellation. Work is bounded, learned
  file routes are retained, and chunk/write/flush failures propagate.
- Runner and handler unwinds have explicit outcomes. Concurrent startup,
  shutdown deadlines and terminal failures are shared consistently.
- QR home-DC migration no longer leaves a stale connection role that can hide
  update-recovery notifications.
- Foreign imports preserve source files, reject unsupported database versions,
  and bound large SQLite values before materializing them.
- Managed native file sources validate the schema before writing, so a foreign
  SQLite file cannot be accidentally migrated as a grammers session.
- IPv6-only imported sessions can be exported and restored with their exact
  endpoint metadata; unused IPv4 placeholders cannot become active routes.
- Feature-gated examples and doctests compile in supported minimal builds.

### Compatibility and limits

- Stop is terminal; construct a new client from the same session to restart.
  Existing manually managed sender pools keep their previous defaults.
- Sequential download errors now preserve their original InvocationError
  variants rather than being wrapped as generic I/O errors.
- Native strings are unencrypted portable authorization data, not complete
  database backups. They omit peer/update caches and additional DC keys.
- CDN redirects return an actionable unsupported error. Full CDN transfers
  are not implemented. Interrupted downloads may leave partial files.
- Telegram account permissions are not inferred where official metadata is
  absent or does not match the local constructor.
- The TL layer remains 230. The public upstream remains a reviewed update
  source, with history and licenses preserved.

### Verification and dependencies

Local release workspace verification: 511 passed, 30 intentionally ignored.
All-target/all-feature check, strict Clippy, Rustdoc and optional build tests
passed on Rust 1.94.1. No live account authorization or consumer deployment was
used. Hosted checks must be read from the associated GitHub Actions run.

The lockfile records 49 compatible dependency updates, including Tokio 1.54.0
and stable glass_pumpkin 2.0.0. See [PERFORMANCE.md](PERFORMANCE.md) for baseline
comparisons: no universal speedup is claimed, and one synthetic route benchmark
shows additional overhead. Dependency metadata's Rust floor is not a tested
minimum toolchain promise.
