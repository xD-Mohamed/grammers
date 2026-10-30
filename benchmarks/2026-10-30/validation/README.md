# Local validation receipts

All commands used Rust 1.99.1 with `/root/.cargo/bin` on PATH. No live Telegram
tests, account authorization, deployment or hosted CI ran.

| Log | Command/result |
|---|---|
| `final-tests5.log` | `cargo test --locked --release --workspace --all-features`: 565 distinct tests/doctests passed, 36 ignored. The process-deadline regression also prints one successful child rerun; summing every result line gives 566. |
| `final-check2.log` | `CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 cargo check --locked --workspace --all-targets --all-features`: passed. |
| `final-clippy4.log` | `CARGO_BUILD_JOBS=1 cargo clippy --locked --workspace --all-targets --all-features -- -D warnings`: passed. |
| `final-docs.log` | `CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 RUSTDOCFLAGS='-D warnings' cargo doc --locked --workspace --all-features --no-deps`: passed. |
| `final-fmt.log` | `cargo fmt --all -- --check`: passed, no output. |
| `session-memory.log` | `cargo test --locked --release -p grammers-session --no-default-features -- --test-threads=1`: 37 passed, 1 ignored. |
| `session-import.log` | Same session command with `--features session-import`: 59 passed, 1 ignored. |
| `session-string.log` | Same session command with `--features session-string`: 44 passed, 1 ignored. |
| `session-files.log` | Same session command with `--features session-import-files`: 79 passed, 1 ignored. |
| `client-minimal.log` | `cargo test --locked --release -p grammers-client --no-default-features --lib`: 39 passed. |
| `fresh-key.log` | Release sender test `rejected_cached_key_starts_a_fresh_plain_handshake` with all sender features: passed; also included in final workspace tests. |

`final-check.log` retains the unsuccessful first check: rustc received SIGKILL
while checking generated TL types with normal debug settings and parallelism.
It did not report a Rust diagnostic. The lower-memory check above subsequently
passed; this is an environment limitation, not an omitted check.

Optional session/client feature checks preceded the final transport-404 accessor
fix. The final all-feature workspace tests/check/Clippy include that fix; no
feature gates or optional dependencies changed in it. Rustdoc includes the new
transport inspection API. Independent Duo review and any correction/review
rounds are still required before publication.
