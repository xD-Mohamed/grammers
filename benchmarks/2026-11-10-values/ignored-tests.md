# Ignored-test inventory

At `59069f3`: 38 explicit Rust ignored tests, including the child role of the bot
mock, and a 39th added in this round: one key exchange and `ping` with a production
DC. All local tests run only against synthetic data or loopback servers. Every test
that needs no credentials was run once on 2026-11-11 at `3f6dbd8` plus that probe, in
release builds, one crate and one process at a time.

| Crate | Test | Needs | Outcome |
|---|---|---|---|
| crypto | `benchmark_ige_backend_closure` | Local release CPU benchmark | passed |
| crypto | `benchmark_decrypt_hashed` | Local, buffers up to 1 MiB | passed |
| crypto | `benchmark_factorization_budget` | Local bounded factorization | passed |
| crypto | `benchmark_message_crypto_components` | Local crypto CPU | passed |
| crypto | `benchmark_obfuscated_cipher_setup` | Local entropy, 100,000 iterations | passed |
| crypto | `benchmark_published_prime_validation` | Local safe-prime CPU | passed |
| mtproto | `benchmark_coarse_clock_cost` | Local, two million clock readings | passed: precise 275.1 ns, coarse 13.9 ns; system time 277.8 ns, coarse 5.6 ns |
| mtproto | `benchmark_gzip_resource_limit` | Local bounded gzip allocation | passed |
| mtproto | `benchmark_borrowed_response_envelope` | Local synthetic replies | passed |
| mtproto | `benchmark_reused_gzip_decoder` | Local bounded gzip | passed |
| mtproto | `benchmark_rpc_result_reuse` | Local reply buffers | passed |
| mtproto | `benchmark_ack_serialization` | Local synthetic ACKs | passed |
| mtproto | `benchmark_obfuscated_handshake_setup` | Local entropy | passed |
| mtsender | `benchmark_invocation_tracking_cost` | Local tracking | passed |
| mtsender | `benchmark_queue_handoff` | Local queue/runtime | passed |
| mtsender | `benchmark_own_full_update_decoding` | Local synthetic own updates | passed |
| mtsender | `benchmark_ping_timer_reuse` | Local timer polls | passed |
| mtsender | `benchmark_initial_receive_allocation` | Local allocation | passed |
| mtsender | `benchmark_cold_waiter_scan` | Local, up to 20,000 waiters | passed |
| mtsender | `benchmark_warm_connection_route` | Local mock, up to four threads | passed |
| mtsender | `benchmark_prepared_request_clones` | Local shared/owned buffers | passed |
| mtsender | `loopback_large_reply_retention` | 32 sessions, 1 MiB replies, 61 s idle | passed, 62 s |
| mtsender | `loopback_session_footprint` | 400 sessions, 1,100 RPC/session | passed at every step: 2,143 / 5,959 / 6,031 / 12,079 B per session |
| mtsender | `loopback_rpc_cost` | 64 sessions, 16 callers, bounded window | passed, client on CPU 0, server on CPU 1 |
| mtsender | `loopback_blocking_control` | 64 sessions, 16 worker threads | passed |
| mtsender | `loopback_open_loop` | Local two-DC mock, bounded rate/window | passed at 500 RPC/s, 1 s window |
| session | `benchmark_missing_hash_retry_pass` | Local, up to 10,000 channels | passed |
| session | `benchmark_pending_channel_deadlines` | Local, up to 10,000 channels | passed |
| session | `benchmark_memory_and_erased_cache_peers` | Local memory storage | passed |
| session | `benchmark_sqlite_cache_peers_transaction` | Local in-memory SQLite | passed |
| tl-types | `benchmark_vector_deserialization` | Local vectors | passed |
| client | `benchmark_sqlite_self_lookup_and_live_receiver` | Local in-memory SQLite, session-file | passed |
| client | `bot_workload` | Separate local mock process | passed in every paired run of this round |
| client | `bot_mock_server` | Child role launched by bot_workload | exercised by those runs |
| mtsender | `test_no_updates_initialization_on_public_test_dc` | Test-DC handshake, literal api_id=1, no account | passed once, 1.1 s |
| mtsender | `test_invoke_encrypted_method` | Test-DC handshake/read-only query, literal api_id=1 | passed once, 1.1 s |
| mtsender | `test_connection_through_proxy` | Local SOCKS5 proxy, test-DC handshake | passed once: the test starts its own SOCKS5 server and completes the first key-exchange step through it |
| mtsender | `test_production_key_exchange_and_ping` | Production DC 2 key exchange and one encrypted `ping`; no `api_id`, no account | passed once, 1.0 s |
| client | `read_only_bot_session_against_telegram` | TG_ID, TG_HASH, TG_TEST_BOT_TOKEN | needs test credentials from the owner |

No credentials, application sessions or consumer directories were searched. The four
network probes made four connections in all: three to the public test DC and one to
production DC 2 (`149.154.167.41:443`, from this library's built-in DC options), each
run once, five seconds apart, none retried. The two with `api_id=1` use that literal
on the test DC only; the production probe calls no API method.

Only `read_only_bot_session_against_telegram` was not run: it **needs test
credentials from the owner** (`TG_ID`, `TG_HASH` and a token for a bot made for
testing in `TG_TEST_BOT_TOKEN`).

Twelve `ignore` Rustdoc fragments in TL generator implementation comments are
schematic output with placeholders, rather than executable examples. The ignored
snippet in PERFORMANCE.md is a prose illustration. Archived patches are historical
evidence and are not counted as executable tests.
