# Changelog

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

### Compatibility and migration

Exhaustive matches must handle new `InvocationError::{Read, Shared, BadMessage}`
and `ReadError::Shared` variants. `Read` includes `may_have_been_sent`; retry only
when application semantics permit it. Use `root_cause`, `category`, `transport_error`
and `std::error::Error::source` instead of assuming every connection error is `Io`.
`DeserializeError::{InvalidEnvelope, ResourceLimit}` and authentication
`Error::{InvalidPq, InvalidDhParameters}` also require exhaustive-match updates.
`DhGenRetry` is now reported distinctly from `DhGenFail`.

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
