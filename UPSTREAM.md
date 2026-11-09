# Maintaining this fork

Canonical upstream: https://codeberg.org/Lonami/grammers

Review intake (2026-11-09): fetched `refs/heads/master` from the canonical Git endpoint at
`e61cf8585e1e9969ab81eda9fb4725a1500e2d86` and reviewed all 10 commits after the previous
review tip `7b414b1`, which is its ancestor. The range holds no handwritten feature and no
bug fix; its one functional change is the layer 233 schema. Nothing was merged or
cherry-picked: three ideas were written for this fork's own crates, each in its own commit.

| Upstream | Subject | Kind | Disposition |
|---|---|---|---|
| `174bd26` | Switch parser definition and type to store full name | Parser refactor, breaking | Not adopted: build-time only, and it would break field access on `Definition` and `Type` for no runtime effect |
| `3f94fd9` | Separate NormalParameterType from the enum | Parser refactor, breaking | Not adopted, for the same reason |
| `b4184af` | Address clippy lints | Chore | Not applicable: it rewrites acknowledgement code in upstream's `push` that this fork replaced; the fork passes strict Clippy |
| `8fb8135` | Update to layer 233 | Schema | **Deferred**, see below |
| `d16333b` | Derive Clone for all parser types | Additive | Adopted in our form, `ccd7a1a`: the existing parser types keep their fields and gain `Clone`, and so do both error enums, which upstream left out |
| `81fcef4` | Upgrade dependencies | Chore | Not adopted: manifest minimums only, and the fork has a committed lock. `aes` and `flate2` are on measured paths; moving them needs the byte-identity tests and benchmarks |
| `b8b4be1` | Delete unused bytes dependency from mtproto | Chore | Adopted, `ecf4d86`: no source file of `grammers-mtproto` used it here either |
| `a852bec` | Gate tokio as optional behind sqlite-storage session | Feature gating | Adopted in our form, `139e6e2`: enabled by `sqlite-storage` and also by `session-import-files`, which upstream does not have. The memory-only library builds without an async runtime |
| `9d5de24` | Use imported alias instead of full crate name | Cosmetic | Not adopted: no behaviour |
| `e61cf85` | Bump to v0.12.0 | Release label | Not applicable: the fork's versions are its own |

Layer 233 is not taken in this intake, and layer 230 stays. It is not a safe drop-in:

- `Update` would outgrow its size guard. `wallet.nftItem` gains an inline `StarGift`, and
  it sits inline in `updateSentWalletTransaction` and, through `wallet.card`, in three new
  updates. Modelled under the fork's boxing rules (not compiled), `Update` goes from 752 B
  to about 1.2 to 1.3 KiB against the 1,024 B guard in `depth_tests.rs`, which the
  2026-10-31 and 2026-11-09 measurements depend on. Restoring it needs a new boxing rule:
  `BOXED_FOR_SIZE` only accepts names without a namespace.
- `updateMessageWidgetData` carries `pts` and `pts_count`. `PtsInfo` ends in a wildcard, so
  without an explicit arm every one would look like a gap and cost a difference request.
  Which key it advances must be confirmed against Telegram's documentation first.
- Nine handwritten sites gain fields or arms, and 20 method pages change.

From 230 the schema adds 52 constructors and changes 11; none is removed or renamed, no
`BOXED_FOR_SIZE` name changes, and `message`, `messageService` and every `Updates`
constructor are byte-identical, so nothing here blocks it. It is a task of its own, with a
build, the guards and the bot workload re-measured, on a host that can build
`grammers-tl-types` without the memory limits this one ran under.

Review intake (2026-11-04): verified the default branch as `refs/heads/master`
through the canonical Git endpoint and fetched tip
`7b414b1d28537f4c354629c3ceeabe771a86f3b4`. Reviewed all 20 commits after
integrated ancestor `633d7fe`, including three new commits after the previous
review tip `1e31e73`. Cherry-picked `c8d6979` (`MsgId::into_inner`) and adapted
the public `Client::upload_media` helper from `58f7bd1`, keeping explicit album
errors and result cardinality. The new DH validation fix is already covered by
the fork's stricter validation. A focused continuation adapts `3122892`'s local
stack-buffer cursor optimization within the existing serialization trait, and
`6ce4c54`'s overflow-safety purpose as optional fallible TTL setters. Existing
serialization/extension APIs, TTL signatures and wire meanings stay intact;
the upstream trait replacement/deletions are not needed. See the
[compatible adaptations and measurements](benchmarks/2026-11-04-upstream/compatible-ideas.md)
and [complete dispositions, source hashes,
local probe and validation](benchmarks/2026-11-04-upstream/README.md).
This is selective intake: the integration ancestor and layer 230 remain unchanged.

Review intake (2026-10-30): inspected canonical upstream through
`1e31e73cd9797790905bbea82cd75adcbe6f1242`. The current implementation adapts
encrypted-packet accounting from `ad7b4725f7ad62a93ddd251035405b628e1bed1e`
and direct sender validation from `1e31e73`, preserving deferred ACKs and
independent inbound limits. This is not an upstream merge: layer 230 and the
integration ancestor below remain unchanged. See
[the implementation/review ledger](OPTIMIZATION-2026-10.md) for scope,
compatibility, measurements and publication gates.

The [2026-10-30 Git source verification](benchmarks/2026-10-30/final/upstream/README.md)
records the canonical remote tip and all 17 newer commit dispositions, with
saved accounting, request-validation, buffer-relocation and layer-232 patches.
Web-page access failed; the verification used a successful fetch from Codeberg's
official Git endpoint. The remote names below describe the maintenance setup;
the isolated review clone currently has only `origin`, so this audit fetched the
canonical URL directly without adding or changing a remote.

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
that Telegram will accept the key. The separate `session-import-files` feature
adds explicitly typed Pyrogram/Telethon SQLite and gotd file readers; tdata
remains unsupported.

No consuming application's manifest, lockfile, source, sessions or deployment
was changed. In the read-only downstream check, no use of `media_ttl` was found;
adopting the importer still requires enabling its feature and updating the
consumer's conversion call sites. Do that as a separate change.

See [PERFORMANCE.md](PERFORMANCE.md) for local measurements, tradeoffs and
validation scope. No authenticated RPC or application deployment ran.

Initial Codeberg baseline: `82eba650098b54e4c2cb9de83fdab7ea2d21a957`.

The `main` branch retains Codeberg's full history and licenses. This is an independently maintained Git fork hosted on
GitHub; GitHub does not support native fork relationships to Codeberg repositories.
Codeberg remains a source of reviewed fixes and updates, using the commands below rather than
GitHub's **Sync fork** button. The archived GitHub repository is not used.

## Remotes and branches

- `origin`: https://github.com/xD-Mohamed/grammers.git
- `upstream`: https://codeberg.org/Lonami/grammers.git
- `main`: verified fork integration branch, tracking `origin/main`.
- `codex/*`: focused implementation and upstream review branches.
- `upstream/master`: canonical upstream development.

## Bring in upstream changes

Start with a clean working tree. Inspect upstream before choosing a merge or
cherry-picking a self-contained fix. Perform integration on a review branch:

```sh
git switch main
git fetch upstream --tags
git log --oneline main..upstream/master
git switch -c codex/upstream-review
git merge upstream/master
cargo fmt --all -- --check
cargo test --locked --release --workspace --all-features
cargo check --locked --workspace --all-targets --all-features
cargo clippy --locked --workspace --all-targets --all-features -- -D warnings
cargo test --locked --release -p grammers-session --no-default-features --features session-import -- --test-threads=1
cargo test --locked --release -p grammers-session --no-default-features -- --test-threads=1
git push -u origin codex/upstream-review
```

Resolve conflicts, review the complete diff, and pass the CI checks before
integrating the review branch into `main`. Never resolve conflicts by blindly
taking all upstream or all fork files. Keep fork changes in focused commits.
Review dependency changes in the committed workspace `Cargo.lock` explicitly.
Performance changes need release benchmarks and integration checks; successful
compilation alone does not establish lower CPU use or better latency.

## Compatibility and release policy

- Treat upstream as a source, not an automatic deployment. A merge does not update consumers.
- Review schema changes separately from handwritten APIs. Compare constructors,
  flags, request/response shapes and the advertised layer; regenerate through
  the existing build pipeline and compile all examples. Never copy a newer
  layer number onto an older schema.
- `tl-gen` boxes `message`, `messageService` and `ephemeralMessage` for size, which upstream
  does not. Merge schema changes through it, and check the size guards in
  `grammers-tl-types/src/depth_tests.rs` when one of those definitions is renamed.
- Preserve the contracts listed in `AGENTS.md`: raw request compatibility,
  update suppression, session-import isolation, lifetime tracking, cancellation,
  wire validation, routing and no replay after an uncertain response.
- Run Windows and Linux CI and the memory-only feature checks. Build warnings
  fail the release gate; runtime errors remain visible and actionable.
- Record new APIs, changed behavior and remaining limitations in `CHANGELOG.md`.
  Use fork-specific tags for reproducible consumer revisions. A fork tag does
  not imply publication under the upstream crates.io package names.
- Keep docs.rs links identified as upstream release documentation. Generate
  this checkout's complete reference with `cargo doc --workspace --all-features
  --no-deps`; new fork APIs may not exist on docs.rs.
- Do not claim live authorization, deployment, performance, or panic freedom
  from offline tests. Keep integrity checks and errors even when they reject a
  server response; hiding a diagnostic is not a stability improvement.

The original Apache-2.0/MIT licensing and attribution are preserved. See
[PERFORMANCE.md](PERFORMANCE.md) for the fork's implementation changes, measured
microbenchmarks and validation limits.
