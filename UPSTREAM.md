# Maintaining this fork

Canonical upstream: https://codeberg.org/Lonami/grammers

Current upstream integration (2026-10-13): merged all nine pending commits through
`49019a579121c9025c728231bdd9bb9b7113c0b5` from `upstream/master` (layer 230).
This includes structured tracing, diagnostic dropped-request reasons, HTML date
formatting without intermediate date allocations, and peer-cache fixes for bulk
SQLite inserts and fresh metadata replacing older known values.

Compatibility changes inherited from upstream:
- `InvocationError::Dropped` is now `Dropped(&'static str)`. Match `Dropped(_)`
  by variant; the reason is diagnostic text, never a retry/replay decision.
- Generated `Username` gains `deletable` and `expired` flag booleans. Applications
  constructing struct literals must initialize them; handwritten TL scanners
  must revalidate the schema and full-consumption behavior.
- SDK diagnostics use `tracing`; existing subscribers should filter SDK targets
  and retain an appropriate global level hint for disabled hot-path diagnostics.

The merge retains shared raw bodies, synchronous tracked calls, warm routing,
pre-queue raw validation, opt-in update suppression/ACK batching, TCP_NODELAY,
abridged framing, bounded growable buffers, minimal valid padding, reused gzip
state, and permit release before result/error/drop wakeups. It does not add
consumer-specific behavior to the library. Previously sent work retains its
reservation until completion or teardown; a missing reply never permits replay.

Verification: 342 release all-feature workspace tests/doctests passed (24 ignored),
all-target/all-feature check and strict Clippy passed. All nine opt-in local
component benchmarks also ran successfully; none is a live-network speed claim.
Additional peer-merge regressions cover new values, retained missing values and
mismatched identities. Isolated downstream validation is kept outside this public
checkout. No authenticated RPC, application deployment or live speed claim.

Initial Codeberg baseline: `82eba650098b54e4c2cb9de83fdab7ea2d21a957`.

The working `main` branch comes directly from Codeberg, with its full history
and licenses intact. This is an independently maintained Git fork hosted on
GitHub; GitHub does not support native fork relationships to Codeberg repositories.
Codeberg is the source for future updates, using the commands below rather than
GitHub's **Sync fork** button. The archived GitHub repository is not used.

## Remotes and branches

- `origin`: https://github.com/xD-Mohamed/grammers.git
- `upstream`: https://codeberg.org/Lonami/grammers.git
- `main`: our working branch, tracking `origin/main`.
- `upstream/master`: canonical upstream development.

## Bring in upstream changes

Start with a clean working tree. From this checkout:

```sh
git switch main
git fetch upstream --tags
git log --oneline main..upstream/master
git merge upstream/master
cargo test --workspace
cargo check --workspace --all-targets
git push origin main
```

Resolve any merge conflicts and finish validation before pushing. Keep fork
changes in small, focused commits so upstream updates remain easy to review.
Performance changes need release benchmarks and integration checks; successful
compilation alone does not establish lower CPU use or better latency.

The original Apache-2.0/MIT licensing and attribution are preserved. See
[PERFORMANCE.md](PERFORMANCE.md) for the fork's implementation changes, measured
microbenchmarks and validation limits.
