# What was left unverified, and the large TL values (2026-11-10, evening)

Baseline `4cc655d`. The owner answered [the typed-path round](../2026-11-10-typed/README.md):
close every item it listed as not done or not verified, as long as it harms nothing, and
decide the two open questions here instead of asking. A later instruction (20:23 EET) put
the large-value work on `main` in small steps, each kept only if it measures as a gain and
taken back out with an ordinary revert otherwise.

## Plan and status

One line per item, updated as each ends. `[ ]` open, `[x]` done, `[-]` measured and
rejected (numbers below), `[!]` blocked (reason beside it). A resumed task continues from
the first open line.

Part 1, what the last report listed as not done or not verified:

- [x] 1.1 `grammers-tl-types` with `impl-serde` as one API unit: check passes, 3.28 GiB peak; all-features stopped by watchdog
- [x] 1.2 The same API unit under Clippy: clean, 3.30 GiB peak; all-target/all-features stopped by watchdog
- [ ] 1.3 Windows and macOS: `cargo check --target` on the real targets, crate by crate
- [x] 1.4 Ignored tests: every one listed with what it needs, in [the inventory](ignored-tests.md)
- [ ] 1.5 Ignored tests that need no credentials: run (benchmarks, and probes of Telegram's
      servers without an account)
- [ ] 1.6 The coarse clock's open loops (5,000 and 500 RPC/s) measured again at the head
- [ ] 1.7 The 1.53% rise of RSS in the peers step: its cause, or shown to be noise

Part 2, the decisions the owner delegated:

- [ ] 2.1 The coarse clock: decision recorded, its `unsafe` block reviewed once more
- [ ] 2.2 Large TL values moved by value (`Update`, `Updates`, `types::Message`), on `main`
- [ ] 2.3 A send's reply decoded once for its caller and for the update stream
- [ ] 2.4 A fresh profile at the head, and the rest of "what is left", largest first
- [ ] 2.5 The allocator's share
- [ ] 2.6 The peers of a batch
- [ ] 2.7 The connection's precise clock reading at each write

Closing:

- [ ] 3.1 CHANGELOG, AGENTS.md and PERFORMANCE.md follow what was kept
- [ ] 3.2 Validation at the final head, per crate
- [ ] 3.3 Scratch build directories removed

Not on this list because the owner excluded them: any consumer of the fork (MONITOR
included), `io_uring`, a kernel upgrade, the host's clock source, anything deployed, and
any traffic to Telegram that needs an account.

## Resumed state and evidence

The first session committed the checklist as `59069f3`. The source at that point is
identical to `4cc655d`. Its scratch logs survived under `/tmp/claude-0/de27`.
All 36 cross-target `cargo check` commands passed with `-D warnings`, for Windows
MSVC, Intel macOS and Apple Silicon macOS. This compiles the actual fallback rather
than an inverted `cfg`; it does not execute code on those operating systems.
The matrix and final-head rechecks are recorded below as they complete.

The resumed Windows Clippy matrix passed eleven commands. Its TL-types all-targets
command was stopped by the watchdog at 537 MB available, 987 MB maximum resident
for one compiler and 2,011 MB across processes; swap grew 5 MB. It will be retried
with one compiler. This stop is a host resource limit, not a diagnostic from Clippy.

The two earlier single-unit serde attempts left only their compiler-start logs;
their watchdog summaries were not saved in the workspace. No peak or outcome is
inferred from those incomplete logs. The final source will get a guarded check and
Clippy attempt with summaries retained.

## Single-unit serde check and Clippy

The owner authorized more memory for one compiler unit. Each command ran alone,
`nice -n 19 ionice -c3`, `-j 1`, no address-space cap, no incremental state or debug
info, with the watchdog. It kills the process group if available memory stays under
300 MiB for eight half-second samples, or swap free falls under 1,200 MiB. A forced
threshold self-test exercised the stop path before these commands.

| Command | Features | Result | Peak one-process RSS MiB | Minimum available MiB | Swap growth MiB |
|---|---|---|---:|---:|---:|
| `cargo check --lib` | all | watchdog stopped at 54 s | 3,532 sampled | 128 | 54 |
| `cargo check --lib` | no defaults, `impl-serde,tl-api` | pass, 52 s | 3,359 (time -v) | 404 | 7 |
| `cargo clippy --all-targets -- -D warnings` | all | watchdog stopped at 59 s | 3,554 sampled | 179 | 33 |
| `cargo clippy --lib -- -D warnings` | no defaults, `impl-serde,tl-api` | pass, 50 s | 3,377 (time -v) | 461 | 1 |

The lighter attempt checks the whole API schema in a single compiler unit, rather
than stubbing parts. It omits the MTProto schema and optional Debug/From/function-
deserialization implementations. It is not an all-feature result. All-feature
Rustdoc passes separately, peaking at 2,458 MiB sampled. Full serde expansion with
every feature remains a host-resource limit; the watchdog was never disabled.

## Step 1: reserve reply-preview media only when present

`MessageReplyHeader.reply_media` was a 656 B optional media enum inside a 984 B
reply header. It is now `Option<Box<MessageMedia>>`, through the generator's existing
optional-field rule. It adds an allocation only to a reply with media in its preview,
not to ordinary text replies. Common message struct literals and enum variants keep
their shape; the migration for populated preview media is in CHANGELOG.md.

Both binaries use the ordinary release profile, default client features plus
`session-file`, the same Rust toolchain and harness. The baseline executable was
retained from the previous round; its library source equals `4cc655d` (and `59069f3`).
The candidate differs in this boxing rule; new regression tests do not enter the
benchmark executable. Binary SHA-256 values are in each configuration.json.
A preliminary comparison with mismatched feature sets was discarded, not included
in any result here.

Local mocks, paired rotating execution order and the existing bootstrap of paired
median ratios. This host's precise clock is a system call; both binaries already
use the same coarse-clock optimization. These deltas measure the new field layout.

| Scenario | Metric | Before | After | Paired change, 95% interval |
|---|---|---:|---:|---:|
| Saturated bot, 64 callers, 30 rounds | RPC/s | 166,210.5 | 171,757 | +2.70% [+1.67, +4.83] |
| | CPU us/RPC | 5.99 | 5.81 | -2.55% [-4.28, -1.83] |
| | allocations/RPC | 17.69 | 17.69 | unchanged |
| | allocated B/RPC | 12,171 | 11,145 | -8.50% [-8.58, -8.38] |
| | steady RSS KiB | 6,456 | 6,428 | -0.31% [-1.34, +0.19] |
| | mallinfo live heap B | 479,432 | 482,504 | +0.55% [-0.02, +0.97] |
| 5,000 RPC/s, 8 rounds | CPU us/RPC | 42.88 | 41.87 | -2.04% [-3.83, -0.64] |
| | allocated B/RPC | 12,723.5 | 11,716.5 | -8.13% [-8.35, -7.60] |
| | p99 us | 2,146.3 | 2,129.9 | -1.13% [-3.79, +3.08] |
| 500 RPC/s, 12 rounds | CPU us/RPC | 111.44 | 113.41 | +0.43% [-6.47, +7.25] |
| | allocated B/RPC | 11,673 | 10,701 | -8.33% [-8.46, -8.28] |
| | old VmRSS KiB | 3,360 | 4,622 | +20.46% [+1.69, +43.86] |

Raw logs: [saturated](reply-media/closed64), [5,000 RPC/s](reply-media/open5000).
The sub-1% heap difference is not interpreted as retention: a tcache-disabled repeat
has been repeated with tcache disabled: 420,040 to 417,736 B, -0.53%
[-0.56, +0.52], eight rounds. Neither establishes retained growth.

The low-rate VmRSS increase was unresolved after the first comparison. The unchanged
allocator heap is not used to dismiss it. [The RSS accounting correction](rss-accounting.md)
was then applied identically to both binaries. Twelve fresh 500 RPC/s rounds measure
page-table RSS at 8,352 to 8,324 KiB, +0.07% [-1.95, +1.04]: **no resolved increase**.
CPU is -3.40% [-5.29, +1.62], allocation count unchanged and allocated bytes -8.36%
[-8.48, -8.23]. [Corrected logs](reply-media/rss-open500). These are a new comparison,
not a direct subtraction of the old counter and new page-table values.
The raw path also passes fourteen paired rounds (1.5 s windows per reply format):
plain CPU -3.35% [-4.38, +3.36], gzip CPU -0.48% [-1.88, +2.88]. Neither resolves
as a gain or loss. Peak process RSS is +0.07% [-0.04, +0.11], also unresolved.
[Raw logs](reply-media/raw).

The heap census, 400 sessions and 1,100 replies per session, is exactly unchanged:
2,143 B before connecting, 5,959 B after one reply, 6,031 B after nine and 12,079 B
after saturating replay history. This is a raw-session census, not the typed bot's
whole-process `mallinfo`. [Census logs](validation/reply-media).

**Decision: keep this step.** The saturated CPU gain is resolved, allocated bytes
fall in every bot scenario, the raw path has no resolved regression, and the
corrected RSS and session census show no resolved increase. A populated reply
preview now costs one extra allocation; absent media remains allocation-free.
Wire/depth/partial-drop tests, per-crate tests, strict Clippy and Rustdoc pass in
the scopes recorded below. Full all-feature serde checking remains watchdog-limited.
