# grammers-client examples

These examples use the same Apache-2.0/MIT licensing as the libraries. They are
compiled during checks, but are never automatically run against Telegram.

Set `TG_ID` to your positive Telegram API ID at runtime. Interactive user-login
examples also need `TG_HASH` when authorization is required. The echo and inline-pagination examples use
`TG_HASH` and `TG_BOT_TOKEN`, both from the runtime environment. Do not put
credentials in source code, command-line arguments, logs, or version control.
Session databases and exported strings grant account access: keep them private.
The managed-client and QR examples document any additional inputs in their source.

```powershell
$env:TG_ID = "YOUR_API_ID"
$env:TG_HASH = "YOUR_API_HASH"
cargo run -p grammers-client --example dialogs --features session-file
```

Use your own values locally. Setting environment variables is not account
authorization. Examples which sign in or request data make real Telegram calls
when you deliberately run them. User login prompts run on a blocking worker;
login codes and two-factor passwords are hidden rather than echoed.

| Example | Purpose | Required client features | Arguments / inputs |
| --- | --- | --- | --- |
| [ping](ping.rs) | Raw MTProto Ping through the managed client; connectivity only | none | `TG_ID`; no account or file |
| [managed_client](managed_client.rs) | Builder settings, native string session, connectivity and Ctrl-C lifecycle | `session-string` | `TG_API_ID`, `TG_SESSION` (native secret) |
| [dialogs](dialogs.rs) | Interactive user login and paginated dialog iteration | `session-file` | `TG_ID`, `TG_HASH` |
| [echo](echo.rs) | Bot echo with at most 16 concurrent handlers and graceful draining | `session-file` | `TG_ID`, `TG_HASH`, `TG_BOT_TOKEN` |
| [inline-pagination](inline-pagination.rs) | Ordered bot callbacks, bounded payloads and checked arithmetic | `session-file` | Bot environment; send `/start` |
| [downloader](downloader.rs) | Iterate messages and download their media | `session-file`, `fs` | `PEER OUTPUT_DIR`; user environment |
| [send_album](send_album.rs) | Reuse two photo/document messages as a spoiler album in Saved Messages | `session-file` | `PEER MESSAGE_ID MESSAGE_ID`; user environment |
| [import_session](import_session.rs) | Explicit foreign/native session input, memory-only authorization check | `session-string`, `session-import-files`, `fs` | `FORMAT INPUT_FILE [--ipv6]` |
| [export_session](export_session.rs) | Local native SQLite-to-string export; never starts networking | `session-file`, `session-string` | `INPUT_SESSION --output NEW_FILE` or deliberate `--stdout` |
| [send_options](send_options.rs) | Common optional message flags with `..Default::default()` | `session-string`, `fs` | `TG_ID` and `NATIVE_STRING_FILE`; sends to Saved Messages |
| [error_handling](error_handling.rs) | Error categories and explicit server wait hints without automatic retries | none | `TG_ID`; unauthenticated Ping |
| [dispatcher](dispatcher.rs) | Opt-in dispatcher and filters | `dispatcher`, `session-file` | `TELEGRAM_API_ID`, `TELEGRAM_API_HASH`, `TELEGRAM_BOT_TOKEN` |
| [qr_login](qr_login.rs) | Opt-in QR authorization | `qr-login`, `session-file` | `TELEGRAM_API_ID`, `TELEGRAM_API_HASH` |

`fs` is a default feature; include it explicitly with `--no-default-features`.
Use `--features session-file` for SQLite persistence. The builder defaults to
memory storage; a memory session still caches peers unless
`ClientConfiguration { auto_cache_peers: false }` is selected.

For explicit import, choose `native`, `pyrogram-string`, `telethon-string`,
`gotd-file`, `pyrogram-file`, or `telethon-file`. String formats are read from the
input file, not argv. File formats are selected explicitly and imported read-only
into memory; there is no automatic format guessing or conversion back to the
foreign file. `--ipv6` supplies the required preference for IPv6 imports; native
strings retain their saved preference unless an override is explicitly supplied.
The application API ID always comes from `TG_ID`, not imported source metadata.
Import does not prompt for login or save a session. The authorization check is a
real server request when run.

Export requires an existing native SQLite session and an explicit destination.
`--output` creates a new file and refuses to overwrite an existing file; Unix
creation uses owner-only permissions. On Windows, choose a private directory with
appropriate access controls. `--stdout` deliberately exposes the secret to the
terminal or redirection target. Never paste that output into an issue or chat.

```powershell
cargo run -p grammers-client --example import_session --features session-string,session-import-files -- telethon-string private-string.txt
cargo run -p grammers-client --example export_session --features session-file,session-string -- dialogs.session --output private-native.txt
cargo check -p grammers-client --all-features --examples
cargo check -p grammers-client --no-default-features --example ping --example error_handling
```

Ordinary examples use `Client::builder`, `start`, and awaited `stop`, including
operation failures. `build` loads local session data without a runner or network;
`start` enables the runner lazily; `connect` performs an MTProto connectivity
check. `run_until` is suitable when the managed client owns the whole lifecycle.
Update examples explicitly synchronize their owned stream state before stopping.
The echo example drains its bounded task set first; inline pagination handles
updates in order. Slow processing can still overflow a finite upstream update
queue; production applications should monitor pool status/events and choose an
appropriate handler strategy. No example promises exactly-once side effects.

Low-level integrations may still use `SenderPool` and `Client::new`; those callers
must supervise their own runner and shutdown. The raw Ping example demonstrates
`Client::invoke` without requiring manual sender-pool ownership. Error categories
are diagnostic hints, not proof that replaying a request is safe. Reconcile
ambiguous network failures before retrying operations which change server state.

Common message flags can use a struct without changing the simple send API:

```rust
use grammers_client::SendOptions;

// Inside an async function with an authorized client and a peer:
client.send_message(peer, "Hello").await?;
client.send_message_with_options(peer, "Quiet hello", SendOptions {
    silent: true,
    ..Default::default()
}).await?;
```

`InputMessage::with_options` applies the same options to an existing rich message.
The six listed options replace earlier values, even when `false` or `None`;
text, media, entities, markup, rich content, and scheduling remain intact.
Defaults match the existing message builder, including `link_preview: false`.
