# grammers-session

This library contains the `Session` trait and several session storages.

Sessions are used to remember the authorization key and home server address,
to prevent having to create a new key and login every single time (which is
an expensive process).

## Importing existing sessions

Enable the optional `session-import` feature to use
`grammers_session::import::ImportedSession`. It supports Pyrogram exported
session strings (current and both legacy layouts), Telethon version 1
`StringSession` strings, and version 1 JSON written by gotd's `session.Loader`.
SQLite session files, Telegram Desktop `tdata`, and custom JSON dialects are not
supported. The feature does not enable the separate `serde` feature or change
default storage behavior.

```toml
grammers-session = { version = "0.10", default-features = false, features = ["session-import"] }
```

Use the imported data directly with `MemorySession`, a sender pool, and a client
with automatic peer caching disabled:

```rust
use std::sync::Arc;
use grammers_client::Client;
use grammers_client::client::ClientConfiguration;
use grammers_mtsender::{ConnectionParams, SenderPool};
use grammers_session::import::{ImportError, ImportedSession};

fn imported_client(exported_session: &str, api_id: i32)
    -> Result<(Client, SenderPool), ImportError>
{
    let imported = ImportedSession::parse(exported_session)?;
    let params = ConnectionParams {
        use_ipv6: imported.prefer_ipv6(),
        ..ConnectionParams::default()
    };
    let session = Arc::new(imported.into_memory_session());
    let pool = SenderPool::with_configuration(session, api_id, params);
    let client = Client::with_configuration(
        pool.handle.clone(),
        ClientConfiguration { auto_cache_peers: false },
    );
    Ok((client, pool))
}
```

The caller supplies its own application API ID. Only current Pyrogram strings
have an API ID field; a zero value means unavailable source metadata and is
returned as `None` by `ImportedSession::api_id()`. Positive values must fit in
`i32`. Legacy Pyrogram, Telethon and gotd do not contain this field. This optional
metadata does not replace the caller's configured API ID. Run the returned pool's
runner as usual when ready to connect. The existing home-DC key is reused; no
new authorization is performed by the importer. Normal connection and protocol
initialization are still required, and the key must remain accepted by Telegram.
Parsing itself performs no file, network, login, or persistence operations.
Authentication keys are secrets; do not log or share the
input or the returned `SessionData`. `ImportedSession` debug output and
`ImportError` diagnostics redact credentials and do not echo malformed input.

The explicit constructors are `from_pyrogram`, `from_telethon`, and
`from_gotd_json`. Pyrogram routes follow its test/production flag. Telethon and
gotd preserve the serialized endpoint, including its port. IPv6-only endpoints
require `ConnectionParams::use_ipv6 = true`, as shown above. Telethon has no test
flag: known endpoint addresses determine the environment; an unrecognized
address reports `test_mode() == None` and imports only that route. Test sessions
never receive production fallback routes. The key is attached only to the home
datacenter; peer caches and update state start empty.

gotd input must include `Version`, `Data.DC`, `Data.Addr`, `Data.AuthKey`, and
`Data.Config.TestMode`. `Addr` must contain an IP address and nonzero port, not a
hostname. `AuthKeyID`, when present, must match the key. Old hand-created JSON
that omits `Addr` is not a gotd Loader export and must be re-exported or supplied
with its actual endpoint. Unrelated gotd configuration fields and the saved salt
are not imported. Imported keys must contain exactly 256 bytes and cannot be all
zero; no parser can prove that an otherwise well-formed key is authentic.

`session_data()` borrows the resulting state; `into_session_data()` transfers it
for other storage integrations. `SessionData::import_to` adds data to an existing
storage and does not clear old routes, so use a fresh destination when importing
a test session. `MemorySession` changes are not persisted automatically.
An in-memory storage alone does not disable peer caching: the high-level client
enables it by default. Set `auto_cache_peers: false` as above to prevent normal
response peer caching. Explicit cache calls and sign-in's own-user cache are
separate operations; the importer calls neither.

A complete runnable example is [import_session.rs](../grammers-client/examples/import_session.rs).
It reads the session from the environment, disables automatic peer caching,
checks authorization, and shuts down the pool. It never creates a session file.
The example is compiled by all-feature checks; it is not run by offline tests.

Format references: [Pyrogram storage layouts](https://github.com/pyrogram/pyrogram/blob/master/pyrogram/storage/storage.py),
[Telethon StringSession](https://github.com/LonamiWebs/Telethon/blob/v1/telethon/sessions/string.py),
[gotd Loader](https://github.com/gotd/td/blob/main/session/session.go), and
[gotd key fingerprint](https://github.com/gotd/td/blob/main/crypto/key.go).
