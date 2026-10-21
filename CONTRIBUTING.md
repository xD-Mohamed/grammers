# Contributing to this fork

This repository maintains its own changes and releases on top of
[Lonami's grammers](https://codeberg.org/Lonami/grammers). Open fork-specific
issues and pull requests here. Follow upstream's own contribution policy when
contributing directly to upstream.

Keep each change focused and describe the behavior before and after it. Include
a small regression test for a bug, and explain which checks were actually run.
Preserve existing public APIs unless an intentional migration is documented.

## Local checks

Use a current stable Rust toolchain with rustfmt and Clippy. The committed
workspace lockfile makes dependency resolution reproducible; changes to it
must be reviewed alongside manifest changes.

```sh
cargo fmt --all -- --check
cargo test --locked --release --workspace --all-features
cargo check --locked --workspace --all-targets --all-features
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --release -p grammers-session --no-default-features --features session-import -- --test-threads=1
cargo test --locked --release -p grammers-session --no-default-features -- --test-threads=1
cargo test --locked --release -p grammers-client --no-default-features
```

The default checks are offline. Tests that connect to Telegram's public test
servers or run local benchmarks are explicitly ignored; run them only for the
specific task and report their scope. Do not use real account credentials for
ordinary verification.

## Compatibility and evidence

- Read `AGENTS.md` before touching request lifetimes, cancellation, routing,
  updates, session imports, serialization, or crypto.
- Keep sent-request reservations until protocol completion or connection
  teardown. A caller timeout does not prove a request was never sent.
- Return errors for recoverable conditions. Keep integrity and protocol checks;
  do not hide diagnostics or add blanket warning suppression.
- Document optional feature requirements and keep examples compiling with the
  features they advertise.
- Measure performance changes in release mode and report small-input tradeoffs.
  Offline tests or benchmarks do not prove live Telegram or VPS improvements.
- Update `CHANGELOG.md` for user-visible behavior and `UPSTREAM.md` when an
  upstream change affects compatibility.

Never attach session strings, auth keys, API hashes, bot tokens, account
inventories, or unredacted network captures to public issues or commits. A bug
report should contain the fork revision, OS, Rust version, enabled features,
redacted error, and a minimal reproduction.

The original MIT/Apache-2.0 notices remain in place. Preserve attribution for
any permitted port or copied code, and check its license before including it.
