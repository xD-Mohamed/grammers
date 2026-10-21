# Changelog

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
