# Final correction validation

Commands ran after the final Rust source change. Paths are sanitized; numerical
results and test output are unchanged. The empty fmt.log records successful
`cargo fmt --all -- --check` (exit 0).

```sh
cargo test --locked --release --workspace --all-features
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 \
  cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 \
  cargo check --locked --workspace --all-targets --all-features
CARGO_BUILD_JOBS=1 CARGO_PROFILE_DEV_DEBUG=0 CARGO_INCREMENTAL=0 RUSTDOCFLAGS='-D warnings' \
  cargo doc --locked --workspace --all-features --no-deps
cargo fmt --all -- --check
cargo test --locked --release -p grammers-client --no-default-features --lib
cargo test --locked --release -p grammers-session --no-default-features -- --test-threads=1
cargo test --locked --release -p grammers-session --no-default-features --features session-import -- --test-threads=1
cargo test --locked --release -p grammers-session --no-default-features --features session-string -- --test-threads=1
cargo test --locked --release -p grammers-session --no-default-features --features session-import-files -- --test-threads=1
```

**599 workspace tests/doctests passed, 41 ignored, zero failed.** Minimal client:
39 passed. Session memory/import/string/file-import: 40/62/47/82 passed, one
ignored each. Counts are calculated from the complete logs in counts.json.
Clippy, check, rustdoc and formatting passed. Ignored live tests are excluded
from these counts; the two explicitly requested public test-DC probes are
recorded separately in live-tests.json. No Windows/MSRV/hosted CI claim.

`evidence-check.log` records a fresh `git checkout-index --all --prefix=.../`
export: all 18 round-one and 12 round-two JSON files were present and identical
to the working artifacts, and 54 local Markdown links resolved. All 171 source
hashes per round matched (round one against checkpoint 5b16ac4, round two against
the exported index). Binary/source/receipt hashes, both sets of 12 complete pairs
and the test totals above were checked against their complete logs. The check
uses Git's index, so an ignored checkout-only file cannot satisfy a linked path.
`git diff --check HEAD -- . ':(exclude)**/*.patch'` checks whitespace outside
literal patch artifacts; their empty context lines require a leading space.
Trailing empty log lines were trimmed without changing test output or numbers.
