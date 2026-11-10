# Ignored-test inventory

At `59069f3`: 38 explicit Rust ignored tests, including the child role of the bot
mock. All local tests run only against synthetic data or loopback servers.
The outcome column is completed by the lead as commands run.

| Crate | Test | Needs | Outcome |
|---|---|---|---|
| crypto | `benchmark_ige_backend_closure` | Local release CPU benchmark | pending |
| crypto | `benchmark_decrypt_hashed` | Local, buffers up to 1 MiB | pending |
| crypto | `benchmark_factorization_budget` | Local bounded factorization | pending |
| crypto | `benchmark_message_crypto_components` | Local crypto CPU | pending |
| crypto | `benchmark_obfuscated_cipher_setup` | Local entropy, 100,000 iterations | pending |
| crypto | `benchmark_published_prime_validation` | Local safe-prime CPU | pending |
| mtproto | `benchmark_coarse_clock_cost` | Local, two million clock readings | pending |
| mtproto | `benchmark_gzip_resource_limit` | Local bounded gzip allocation | pending |
| mtproto | `benchmark_borrowed_response_envelope` | Local synthetic replies | pending |
| mtproto | `benchmark_reused_gzip_decoder` | Local bounded gzip | pending |
| mtproto | `benchmark_rpc_result_reuse` | Local reply buffers | pending |
| mtproto | `benchmark_ack_serialization` | Local synthetic ACKs | pending |
| mtproto | `benchmark_obfuscated_handshake_setup` | Local entropy | pending |
| mtsender | `benchmark_invocation_tracking_cost` | Local tracking | pending |
| mtsender | `benchmark_queue_handoff` | Local queue/runtime | pending |
| mtsender | `benchmark_own_full_update_decoding` | Local synthetic own updates | pending |
| mtsender | `benchmark_ping_timer_reuse` | Local timer polls | pending |
| mtsender | `benchmark_initial_receive_allocation` | Local allocation | pending |
| mtsender | `benchmark_cold_waiter_scan` | Local, up to 20,000 waiters | pending |
| mtsender | `benchmark_warm_connection_route` | Local mock, up to four threads | pending |
| mtsender | `benchmark_prepared_request_clones` | Local shared/owned buffers | pending |
| mtsender | `loopback_large_reply_retention` | 32 sessions, 1 MiB replies, 61 s idle | pending |
| mtsender | `loopback_session_footprint` | 400 sessions, 1,100 RPC/session | pending |
| mtsender | `loopback_rpc_cost` | 64 sessions, 16 callers, bounded window | pending |
| mtsender | `loopback_blocking_control` | 64 sessions, 16 worker threads | pending |
| mtsender | `loopback_open_loop` | Local two-DC mock, bounded rate/window | pending |
| session | `benchmark_missing_hash_retry_pass` | Local, up to 10,000 channels | pending |
| session | `benchmark_pending_channel_deadlines` | Local, up to 10,000 channels | pending |
| session | `benchmark_memory_and_erased_cache_peers` | Local memory storage | pending |
| session | `benchmark_sqlite_cache_peers_transaction` | Local in-memory SQLite | pending |
| tl-types | `benchmark_vector_deserialization` | Local vectors | pending |
| client | `benchmark_sqlite_self_lookup_and_live_receiver` | Local in-memory SQLite, session-file | pending |
| client | `bot_workload` | Separate local mock process | pending |
| client | `bot_mock_server` | Child role launched by bot_workload | pending |
| mtsender | `test_no_updates_initialization_on_public_test_dc` | Test-DC handshake, literal api_id=1, no account | pending |
| mtsender | `test_invoke_encrypted_method` | Test-DC handshake/read-only query, literal api_id=1 | pending |
| mtsender | `test_connection_through_proxy` | Local SOCKS5 proxy, test-DC handshake | pending |
| client | `read_only_bot_session_against_telegram` | TG_ID, TG_HASH, TG_TEST_BOT_TOKEN | needs test credentials from the owner |

No credentials, application sessions or consumer directories are searched. The three
existing network probes make a handful of unauthenticated test-DC connections. A
production-DC key exchange needs a separate explicit fixture; none exists in this
inventory. No probe runs in a retry loop.

Twelve `ignore` Rustdoc fragments in TL generator implementation comments are
schematic output with placeholders, rather than executable examples. The ignored
snippet in PERFORMANCE.md is a prose illustration. Archived patches are historical
evidence and are not counted as executable tests.
