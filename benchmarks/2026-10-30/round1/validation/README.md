# Final correction validation

All commands completed successfully after the final Rust source/harness change.
The benchmark source manifest records that same frozen input. Paths in logs are
sanitized; elapsed times and test results are unchanged. The empty fmt.log is a
successful formatter check, not missing test output.

```sh
cargo test --locked --release --workspace --all-features
cargo fmt --all -- --check
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 \
  cargo check --locked --workspace --all-targets --all-features
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 \
  cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 RUSTDOCFLAGS='-D warnings' \
  cargo doc --locked --workspace --all-features --no-deps
cargo test --locked --release -p grammers-client --no-default-features --lib
cargo test --locked --release -p grammers-session --no-default-features -- --test-threads=1
cargo test --locked --release -p grammers-session --no-default-features --features session-import -- --test-threads=1
cargo test --locked --release -p grammers-session --no-default-features --features session-string -- --test-threads=1
cargo test --locked --release -p grammers-session --no-default-features --features session-import-files -- --test-threads=1
```

Workspace tests/doctests: **590 passed, 39 ignored, zero failed**. Minimal client:
39 passed. Session memory/import/string/file-import builds: 39/61/46/81 passed,
one ignored in each. `counts.json` is calculated from the complete corresponding
logs. Ignored tests include live integration tests that were not run; only named
offline benchmarks are selected separately. These commands do not assert a
Windows build, MSRV check, hosted CI result or authenticated Telegram compatibility.
