# Maintaining this fork

Canonical upstream: https://codeberg.org/Lonami/grammers

Current upstream integration (2026-10-18): merged all three pending commits
through `633d7fe` from `upstream/master`; the TL schema remains layer 230.
This adds `InputAudio`, `InputVideo`, typed media lifetime controls, and upstream
Clippy cleanup. Fork history and licenses are preserved.

Compatibility changes inherited from this update:
- `InputMedia::media_ttl` now accepts `Option<TimeToLive>`, replacing `i32`.
  Use `None` instead of a negative unset value, `Some(TimeToLive::Unlimited)`
  for zero, or `Some(TimeToLive::Duration(Duration::from_secs(n)))` for seconds.
- New audio/video builders and `TimeToLive` are exported from `media`.
- `UpdateAndPeers` is now a public alias in `grammers_session::updates`.
- The previous integration's `InvocationError::Dropped(_)`, layer-230 Username
  flags and structured tracing still apply.

Merge resolutions retain public inline `Poll.raw_results` and `WebPage.raw`
fields and the targeted Media lint expectation instead of adding public boxing.
The upstream private `PasswordToken` boxing is retained. Existing module aliases,
raw-input validation and update-suppression-aware compression are preserved;
new Clippy syntax was adapted where needed rather than removing fork safeguards.

The merge retains shared raw bodies, synchronous tracked calls, warm routing,
pre-queue raw validation, opt-in update suppression/ACK batching, TCP_NODELAY,
abridged framing, bounded growable buffers, minimal valid padding, reused gzip
state, and permit release before result/error/drop wakeups. Sent work retains
its reservation until completion or teardown; missing replies never allow replay.

The optional `grammers-session/session-import` feature adds library-level imports
for exported Pyrogram/Telethon strings and standard gotd Loader JSON. See
[session usage](grammers-session/README.md) and the compiled
[in-memory example](grammers-client/examples/import_session.rs).
Applications may adopt it instead of manual conversion; nothing changes their
session or peer-cache configuration automatically. Importing does not prove
that Telegram will accept the key. SQLite files and tdata are outside this API.

No consuming application's manifest, lockfile, source, sessions or deployment
was changed. In the read-only downstream check, no use of `media_ttl` was found;
adopting the importer still requires enabling its feature and updating the
consumer's conversion call sites. Do that as a separate change.

See [PERFORMANCE.md](PERFORMANCE.md) for local measurements, tradeoffs and
validation scope. No authenticated RPC or application deployment ran.

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
