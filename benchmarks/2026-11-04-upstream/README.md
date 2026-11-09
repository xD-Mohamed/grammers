# Codeberg upstream intake, 2026-11-04

Canonical source: <https://codeberg.org/Lonami/grammers.git>, default branch
`refs/heads/master`, checked tip `7b414b1d28537f4c354629c3ceeabe771a86f3b4`.
The source was fetched with ordinary public Git; no web search snippets were used.

Both local `main` and the actual remote `origin/main` initially pointed at
`14ffda1a9a622514232494082083fac57c4c049f`, with a clean working tree. Work was
prepared on `codex/upstream-review-20261104`. The merge base with upstream remains
`633d7fe4ecc829beec492d745bbbfe5911010623`. There are **20 upstream commits**
after that ancestor, **three new since the 2026-10-30 review** at
`1e31e73cd9797790905bbea82cd75adcbe6f1242`. These are upstream commits, not
fork-only work. [source.json](source.json) inventories all 20 with full hashes,
parents, trees, changed paths and hashes of the attributed Git patches.

The owner clarified acceptance after that intake: judge the idea and implement
useful behavior within this fork's APIs. The [focused continuation](compatible-ideas.md)
supersedes the original TTL/serialization dispositions. It adapts the private
stack cursor optimization and adds fallible TTL setters; the original full-suite
receipts below remain the receipts for `8ddcfab`, not a rerun of this continuation.

## Applied changes

- **`c8d6979`** is cherry-picked with `-x` as `83f0cb5`. Its stable patch ID,
  `c73827874a01104dda0bf55bbf54dd861a0edd10`, matches upstream exactly. The
  additive `MsgId::into_inner` accessor makes opaque protocol identifiers usable
  outside the crate. An external integration test correlates two returned IDs
  with their serialized request headers and checks increasing order.
- **`58f7bd1`** is adapted: expose `Client::upload_media`, and use that helper for
  album upload responses. Preserve the existing rejection of incomplete inputs
  and unsupported responses instead of upstream's silent filtering. Preserve the
  existing clone and upload ordering; this intake makes no performance claim.
  Local mocked-RPC tests cover returned photo identity/file reference/TTL, empty
  and unsupported results, transport/deserialization errors, album cardinality,
  and rejection before any send. The `.invalid` fixture URL is never fetched.

Original authorship is retained for the cherry-pick. The adaptation credits
HBcao and Lonami Exo in its commit message. Apache-2.0/MIT licenses are unchanged.

## Disposition of every commit after the integration ancestor

| Upstream commit | Current disposition and reason |
|---|---|
| `6ce4c54` | **Adapt overflow safety:** add `try_media_ttl`/`try_ttl` returning the existing conversion error. Keep old setters and their panic contract, optional TTLs, `TryFrom`, Unlimited=0 and exact duration mapping. Avoid upstream's silent clamping. |
| `5b80079` | Skip a blanket dependency refresh. The fork already has its tested dependency graph, including stable glass_pumpkin and newer Tokio; upstream also changes libsql's minor version. No dependency change is needed for the selected additions. |
| `15b7afa` | Skip upstream v0.11 package labels and inter-crate version changes; this fork is not that crates.io release. |
| `dfffb71` | Defer layer 232: nine changed existing constructors and 25 added constructors/methods require generated public API migration and regenerated method documentation. A layer-number-only change would be invalid. See the [earlier concrete schema analysis](../2026-10-30/final/upstream/README.md). |
| `a984cac` | Keep current separation: its purpose is reusable I/O-independent framing/testability. The fork already exposes generic MTP/Transport and tests synthetic/local I/O; moving the hardened reactor adds no identified protocol capability or measured gain here. Its parsing helpers already have equivalent constructor checks/update decoding in `sender.rs`. A future alternate-I/O consumer could justify a separate design. |
| `e564eef` | Defer explicit `DequeBuffer::extend_back`: current generic extension already delegates to Vec, and the padding call differs in this fork. No production replacement is justified by this intake's probe. |
| `4101f5e` | Keep static high-level routing: useful purpose is runtime custom framing. Generic low-level `connect`/`connect_with_auth` already accept custom transports; high-level built-in/proxy selection covers our current protocols. An opt-in custom pool could be additive, but no additional framing protocol is required here. Do not impose boxing/indirection on ordinary pools or bypass secret-bound MTProxy construction. |
| `c8d6979` | **Import exactly:** additive raw `MsgId` accessor; no connection or wire-format change. Previously deferred to an API intake; included now. |
| `07b51ab` | Skip standalone log-spelling churn; no behavioral fix. |
| `98b7b88` | Skip derived Debug on body-bearing responses/updates; would expose payloads in diagnostics. |
| `dad5581` | Inapplicable: the fork's sender still directly uses crypto for authentication, framing and proxy support. Removing that dependency breaks the build. |
| `84cfb99` | Keep crypto ownership: upstream separates padding/encryption from buffer prepend operations for modularity. Our AES backend already encrypts the existing contiguous buffer in place, without a staging copy, and uses valid minimal padding. Moving padding/prepends to callers adds no missing behavior or measured benefit; a duplicate slice API is not needed. |
| `d654b04` | Defer buffer relocation with `84cfb99`; moving it alone introduces a crypto/MTProto dependency cycle and removes a public crypto export. The move itself has no measured runtime benefit. |
| `4ca2263` | Skip documentation for the unadopted buffer method alongside its deferred introduction. |
| `58f7bd1` | **Adapt:** public upload helper and shared conversion; retain explicit album failure/cardinality behavior. Do not import filtering or claim clone/performance improvements. |
| `ad7b472` | Already adapted in the previous intake: packet ceilings include actual padding/update-wrapper overhead; retain deferred ACKs and independent inbound/expanded limits instead of the upstream assertions. |
| `1e31e73` | Already adapted: early request validation uses the fork's existing constructor/alignment checks, shared/tracked APIs, and typed errors. |
| `d2c6e84` (new) | Already covered more strictly: `do_step3` calls `check_p_and_g` before unsigned conversion/arithmetic, validates exact prime size, generator range/residues, and safe primality. The published prime skips only primality; subgroup checks still run. Entropy failure rejects validation, whereas upstream calls `unwrap()`. Existing authenticated DH fixtures independently exercise invalid generators/composite primes and pass. Do not duplicate the public crypto helper or add error variants. |
| `3122892` (new) | **Adapt the useful header optimization:** keeping the stack cursor local lets LLVM combine byte stores, through the unchanged `Extend<u8>` API. Paired release header measurements are in the continuation. Bulk byte-string output already delegates to Vec; the original full sink probe has mixed results. No trait replacement or second serializer is needed. |
| `7b414b1` (new) | No independent behavior to add: deletes traits made redundant only by upstream's sink redesign. Both public `DequeBuffer` extension impls and the private stack-buffer extension remain useful here. Preserve arbitrary/borrowed iterators; the local cursor obtains the measured header benefit without deleting them. |

The upstream DH changes are covered by `published_prime_still_checks_every_generator`,
`authenticated_dh_fixture_reaches_each_validation_gate`,
`hashed_invalid_dh_parameters_are_checked_before_arithmetic`, and the successful
handshake fixture. The sender's bounded blocking DH jobs remain in place. No
new DH implementation or speculative protocol change is needed.

## Isolated serialization probe

[slice-probe.rs](slice-probe.rs) compares the actual fork's serializers with the
upstream slice-append algorithm for a 24-byte container header and reused Vec
byte-string serialization. It checks exact wire equality at empty, padding and
253/254-length boundaries before timing. Rust 1.99.1, `rustc -O`, x86-64 Linux,
CPU 0 affinity, one warmup per variant, nine alternating-order pairs. No build
or other benchmark was launched during its run. [Raw samples](slice-probe.csv)
are ns per operation, including black-box/function-call overhead.

| Component | Legacy median ns | Slice median ns |
|---|---:|---:|
| 24-byte container header | 1.455 | 1.329 |
| 16-byte payload | 8.612 | 8.373 |
| 256-byte payload | 10.576 | 8.254 |
| 4096-byte payload | 78.851 | 95.169 |
| 65536-byte payload | 1313.541 | 1375.995 |

This probe is limited to components on one host, without generated-object,
allocation-heavy, encryption or network workloads. It is not an end-to-end
library comparison or a confidence interval. Compiler code layout can affect
such small measurements. No sink-based serializer is retained, and no universal
speedup or regression is inferred. The continuation separately measures and
retains the compatible cursor optimization using the existing serializers.

```sh
rustc --edition=2024 -O benchmarks/2026-11-04-upstream/slice-probe.rs -o /tmp/grammers-slice-probe
taskset -c 0 /tmp/grammers-slice-probe
```

## Validation and limits

Focused response-mapping tests: **5 passed**, including the two new media tests.
Final checks and exact commands are recorded in [validation.json](validation.json).
The release all-feature workspace run passed **781 tests/doctests**, with 48
ignored, including the external message-ID test, four DH validation/handshake
checks and 29 local networking tests (HTTP/HTTPS, SOCKS4/5 and MTProxy among them).
Formatting, all-target/all-feature check, strict Clippy and warning-free Rustdoc
passed. No-default-feature session-import/session-memory configurations passed
96/74 tests and doctests (three ignored each); the minimal client passed 59 tests.
Full local logs are under [validation/](validation/).
All traffic is local/synthetic; ignored live Telegram tests are not executed.
No real account session, authenticated Telegram call, consumer deployment,
bot restart, schema bump or force push is part of this intake.

Re-fetch the exact source for reproduction:

```sh
git fetch --no-tags --no-write-fetch-head https://codeberg.org/Lonami/grammers.git 7b414b1d28537f4c354629c3ceeabe771a86f3b4
git log --reverse --format='%H %s' 633d7fe..7b414b1
git show --format=fuller --binary --no-ext-diff d2c6e84
git show --format=fuller --binary --no-ext-diff 3122892
git show --format=fuller --binary --no-ext-diff 7b414b1
```
