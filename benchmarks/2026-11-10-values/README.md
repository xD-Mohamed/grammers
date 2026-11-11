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
      (36 representative checks passed on the round's first source; the matrix at the final head is open)
- [x] 1.4 Ignored tests: every one listed with what it needs, in [the inventory](ignored-tests.md)
- [x] 1.5 Ignored tests that need no credentials: all 38 run and pass, the four network
      probes included (one of them new, against a production DC); one test needs
      test credentials from the owner
- [ ] 1.6 The coarse clock's open loops (5,000 and 500 RPC/s) measured again at the head
- [!] 1.7 The peers-step RSS rise persists in non-anonymous pages; exact mapping/function cause unresolved after the 20-minute time box (see peers-rss)

Part 2, the decisions the owner delegated:

- [x] 2.1 The coarse clock: kept, on `libc`; its `unsafe` block reviewed, its failure path tested
- [ ] 2.2 Large TL values moved by value (`Update`, `Updates`, `types::Message`), on `main`
      (steps 1, 2, 3 and 5 kept; step 4 measured and reverted; shared ownership of a message is open)
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
The sub-1% heap difference is not interpreted as retention. Eight more saturated
rounds with tcache disabled measure 420,040 to 417,736 B, -0.53% [-0.56, +0.52].
Neither reading establishes retained growth. The `old VmRSS` row above is the counter
the next paragraph replaces: it is kept as measured, and is not this step's RSS result.

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

## Step 2: rare update payloads are boxed

At `acaf759`, twelve variants used the existing definition-boxing rule;
common message updates and payload struct literals stay inline/unchanged. Populated
rare variants add one allocation. `Update` falls from 760 to 240 B, `Updates` from
768 to 712 B. Independent raw-wire fixtures, owned conversions, every truncated
prefix, the ordinary inline control and existing depth/RAII tests pass.

Both binaries use the fixed RSS harness and identical release/features. Local
mocks on the same host where the precise clock is a syscall; both already use
its coarse-clock optimization. Paired median-ratio bootstrap, as before.

| Scenario | CPU/RPC change, 95% interval | Other result |
|---|---:|---|
| Saturated 64 callers, 30 pairs | -4.75% [-6.03, -3.56] | rate +5.20% [+3.59, +6.68] |
| 5,000 RPC/s, 8 pairs | -0.19% [-3.94, +2.50] | no resolved change |
| 500 RPC/s, 12 pairs | +0.12% [-3.70, +4.03] | no resolved change |
| Raw plain, 14 pairs | -0.38% [-3.42, +2.36] | no resolved change |
| Raw gzip, 14 pairs | +1.28% [-0.70, +2.60] | no resolved change |

Allocated bytes/RPC fall 14.03% at saturation (11,152 to 9,577 B), 13.79% at
5,000/s and 14.58% at 500/s. Allocation counts stay 17.69 at saturation. Page-table
steady RSS is -0.26% [-0.84, +0.76], unresolved; the old approximate VmHWM is not a
reliable peak. With tcache disabled, eight additional closed pairs show mallinfo
heap -0.51% [-0.52, -0.49]; this small process result is not a per-session claim.
The 400-session census remains exactly 2,143 / 5,959 / 6,031 / 12,079 B per session
before connecting / after one / nine / 1,100 replies, matching step 1.

Decision: keep. The saturated gain is resolved, allocation traffic falls, and
open loops, raw CPU and the session census show no resolved regression.
[Measurements](update-variants), [validation](validation/update-variants).
Generator tests and TL tests (15 unit, 1 dependency, 24 doctests) pass; strict
all-target Clippy passes without serde; the whole API serde unit passes check and
Clippy without defaults. All-feature Rustdoc passes with warnings denied. It first
hit the 4,000,000 KiB virtual cap, then passed as a proven single heavy unit under
the owner's exception, peaking at 2,750 MiB reported RSS with at least 1,011 MiB
available and 26 MiB swap movement. Full all-feature check/Clippy remains distinct
from these narrower passing scopes and watchdog-limited as recorded above.

## Step 3: box the large webpage and game leaves

Full `WebPage::Page` and `Game::Game` payloads now use the same definition rule.
Empty and pending webpages stay inline. The temporary outer boxes on webpage
update wrappers from step 2 are removed to avoid allocating twice. CHANGELOG
records both migrations, including consumers of the intermediate commit.

| Type | Before, B | After, B |
|---|---:|---:|
| MessageMedia | 656 | 304 |
| Message | 1,960 | 1,608 |
| MessageService | 1,248 | 1,128 |
| Updates | 712 | 624 |
| WebPage | 648 | 40 |
| Game | 344 | 8 |

The paired binaries differ only in this library step and its regression tests;
the release profile, features and fixed RSS harness match. Both use the coarse
clock on this host, where the precise clock is a syscall. All measurements are
local mocks, with paired median-ratio bootstrap intervals as in prior steps.

| Scenario | CPU/RPC change, 95% interval | Other result |
|---|---:|---|
| Saturated 64 callers, 30 pairs | -3.03% [-4.23, -1.81] | rate +2.90% [+1.65, +4.43] |
| 5,000 RPC/s, 8 pairs | +0.76% [-0.68, +1.36] | no resolved change |
| 500 RPC/s, 12 pairs | +0.34% [-5.30, +5.78] | no resolved change |
| Raw plain, 14 pairs | +0.36% [-0.83, +3.31] | no resolved change |
| Raw gzip, 14 pairs | +1.09% [-2.16, +2.00] | no resolved change |

Saturated allocations/RPC fall from 17.69 to 17.63, allocated bytes from median
9,574.5 to 8,231 (-14.13% paired); ordinary photos/documents add no allocation.
Page-table steady RSS is -0.49% [-1.04, +0.24] at saturation, +0.17% [-1.44, +2.40]
at 5,000/s and +0.60% [-0.55, +0.98] at 500/s, all unresolved. With tcache off,
eight more saturated pairs show heap -8.31% [-8.33, -8.30], from process medians
415,672 to 381,120 B. This busy-process heap result is not a per-session census.

Generator tests (10 plus dependency test), TL tests (16 plus dependency test and
24 doctests) and strict all-target Clippy pass with TL default features plus
`tl-mtproto,deserializable-functions`, without serde. Generator Clippy and
Rustdoc cover all its features; TL all-feature Rustdoc passes separately.
The complete API serde unit passes check and Clippy again: 3,360/3,376 MiB peak
reported RSS, minimum available 446/435 MiB, swap movement 33/1 MiB. All-feature
Rustdoc passes as the previously established indivisible unit under the memory
exception: 2,908 MiB reported peak, minimum available 881 MiB, swap movement 4 MiB.
These passing scopes do not establish all-feature serde check/Clippy success.

The raw 400-session census remains exactly 2,143 / 5,959 / 6,031 / 12,079 B per
session before connecting / after one / nine / 1,100 replies. Raw peak RSS changes
+0.07% [-0.07, +0.14], unresolved. Decision: keep the resolved CPU and allocation
gain, with no resolved open-loop/raw/RSS increase and an unchanged session census.
[Measurements](media-leaves), [validation](validation/media-leaves).

## Step 4, tested and reverted: optional reply-forward metadata

Candidate `c792112` boxed `MessageReplyHeader.reply_from` using the existing
optional-field rule. Its wire/depth/conversion tests passed; Header fell from
336 to 168 B, Message from 1,608 to 1,440 B, Service from 1,128 to 960 B, and
Updates from 624 to 456 B. Populated forward metadata added one allocation.
The change was reverted normally by `19b852f`; the final API retains this field's
original inline shape. No branch or rewritten history was used.

| Scenario | CPU/RPC change, 95% interval | Other result |
|---|---:|---|
| Saturated, first 30 pairs | -1.29% [-2.75, +0.20] | unresolved |
| Saturated, all 60 pairs | -0.49% [-1.59, +0.60] | rate +0.40% [-0.50, +1.41] |
| 5,000 RPC/s, 8 pairs | -0.25% [-1.74, +1.60] | unresolved |
| 500 RPC/s, 12 pairs | -0.58% [-1.91, +2.34] | unresolved |
| Raw plain, 14 pairs | +2.01% [-3.22, +3.49] | unresolved |
| Raw gzip, 14 pairs | -2.60% [-3.62, +1.43] | unresolved |

One additional phase of 30 pairs was chosen to resolve the small initial CPU
estimate; both phases and all 60 paired observations are retained. The combiner
validates binary identities, disjoint logs, exact log/sample agreement and each
original summary before joining them, using the original unadjusted paired
bootstrap method. Its membership file identifies every source log. Both phases
used the same task controller without allocator overrides; original harness
configurations do not themselves record `GLIBC_TUNABLES`. Base and candidate
within each pair inherited the same environment. Both binaries use the coarse
clock on this host, where the precise clock is a syscall.

Allocated-byte traffic **does improve**, by 2.99% [-3.17, -2.88] over all 60 pairs
(process medians 8,224 to 7,971.5 B/RPC). Allocation count stays 17.63/RPC.
The normal heap estimate is unresolved (-0.33% [-0.88, +0.19]); with tcache off,
eight additional pairs show a tiny increase, +0.03% [+0.01, +0.04], from process
medians 381,176 to 381,264 B. This provides no demonstrated retained-memory cut.
Page-table steady RSS at saturation is +0.62% [-0.14, +1.61], unresolved; the
open-loop RSS intervals also cross zero. The 400-session census remains exactly
2,143 / 5,959 / 6,031 / 12,079 B before connecting / after one / nine / 1,100 replies.

Decision: insufficient demonstrated net benefit. The byte-traffic saving is real,
but CPU, allocation count and retained/session memory show no useful improvement,
while populated fields pay another allocation. This is not a restored 5% bar:
the smaller traffic saving alone does not establish that this trade helps the
target workload. Further speculative cold-field boxing stops here; shared message
ownership and reply decoding remain separately required work.

Candidate generator/TL tests, strict Clippy, Rustdoc and whole-API serde check and
Clippy passed in the same scopes as step 3. After the revert, the four changed
files match `54731c2` byte for byte; fresh validation of that restored code is
recorded with the candidate receipts. [Measurements](reply-forward),
[candidate patch and validation](validation/reply-forward).

## Correctness: concurrent first write timestamps

`InvocationTracker` encoded a write relative to the first **reported** write.
If thread A captured time `t1`, paused, and thread B reported later `t2` first,
the origin became `t2`; A's negative offset saturated to zero and read back as
`t2`. A fresh-child regression reproduced this with about 2 ms between writes.

Successful tracked admission now initializes the shared origin before the permit
escapes. Thus it precedes every real write, whichever report arrives first.
`mark_sent` retains its first-write CAS and precise timestamp. There is one
initial clock read per process and a OnceLock fast-path check per successful
tracked acquisition; untracked and capacity-only requests are unchanged.

The regression failed on the old code and passed after the fix. Its child must
report exactly one passing test, preventing a stale test filter from passing
without executing it. The full all-feature sender run passes 189 unit tests,
one dependency test, two loopback integration tests and five doctests; the eight
unit benchmarks and eight integration/network tests remain ignored in that run.
Sender-only strict all-target Clippy (`--no-deps`) and all-feature Rustdoc pass.
The ordinary Clippy command exposed a dependency's `UpdatesLike` size warning
after the preceding TL shrink: short-sent payload 920 B versus 648 B. The timestamp
fix was committed separately as `b912fa9` and held back until step 5 below removed
that warning; the two were pushed together. [Receipts](validation/tracker-origin).

The active compiler is Rust/Cargo 1.99.1. Executable `.comment` sections verify
that the fixed-RSS baseline, webpage/game candidate and reply-forward candidate
also used Rust 1.99.1; older toolchain mentions in historical documentation are
not this round's compiler identity.

## Step 5: rich-message inputs are boxed at their leaves

After step 3 the session crate's `UpdatesLike` failed strict Clippy
(`large_enum_variant`): its short-sent variant holds the `messages.sendMessage`
request, 920 B, beside 648 B for the next variant. Boxing that request would add an
allocation to every sent message. The request is large because its optional
`rich_message` reserves the largest of three `InputRichMessage` bodies inline, in
every send. Those three leaves now use the definition rule: `InputRichMessage` falls
to 16 B and `SendMessage` from 920 to 472 B. A request without a rich message
allocates nothing more; one with a rich message allocates once. The unfulfilled
`large_enum_variant` expectation on `grammers_client::media::Media` is removed with
it: since step 3 that enum no longer has a large variant.

The base is `b912fa9`, the candidate differs in this rule and its tests. Same
release profile, features (`session-file`) and fixed RSS harness; local mocks; both
use the coarse clock on this host, where the precise clock is a system call. Paired
median-ratio bootstrap as before.

| Scenario | CPU/RPC change, 95% interval | Other result |
|---|---:|---|
| Saturated 64 callers, 30 pairs | -1.92% [-2.59, -0.33] | rate +1.83% [+0.39, +2.70] |
| 5,000 RPC/s, 8 pairs | +0.48% [-2.93, +3.23] | no resolved change |
| 500 RPC/s, first 12 pairs | +1.87% [-0.00, +3.68] | on the edge: repeated |
| 500 RPC/s, all 24 pairs | +1.77% [-1.28, +3.96] | no resolved change |
| Raw plain, 14 pairs | -5.71% [-7.13, -2.50] | not claimed, see below |
| Raw gzip, 14 pairs | +1.41% [-4.26, +3.22] | no resolved change |

Saturated: 4.68 to 4.585 us/RPC, 213,475.5 to 217,879 RPC/s. Allocations stay
17.63/RPC; allocated bytes fall 2.02% [-2.10, -1.94] (8,237 to 8,070.5 B/RPC), 1.92%
at 5,000/s and 1.49% at 500/s. `mallinfo` heap is -3.32% [-4.39, -1.89], and with
tcache disabled -4.27% [-4.30, -3.18] over eight more pairs (393,568 to 376,696 B).
Page-table steady RSS is -0.74% [-1.15, +0.17] saturated, -0.40% [-2.74, +0.70] at
5,000/s and -1.21% [-2.04, +0.05] at 500/s: no resolved change. The 400-session raw
census is exactly 2,143 / 5,959 / 6,031 / 12,079 B per session, as in every step.

The first 500 RPC/s phase ended with its interval touching zero from above, so a
second phase of twelve pairs was run and both are kept: the second alone is +0.22%
[-4.20, +5.84], and all 24 pairs do not resolve. `combine-bot-pairs.py` gained a
`--scenario` option for this; it still reproduces step 4's combined summary byte for
byte. The raw path never builds a `SendMessage`, so its plain-reply result is read
as layout noise between two binaries, not as a gain of this step.

**Decision: keep.** The saturated gain resolves, allocated bytes and heap fall, no
scenario resolves as a loss, and the workspace is free of the Clippy error without
an allocation on the send path. [Measurements](input-rich),
[validation](validation/input-rich).

Generator tests (28 unit, 11 integration and the dependency test) and TL tests (17, one ignored, plus the
dependency test and 24 doctests) pass. Strict Clippy passes for the generator, the TL
types, the sender and the client (all targets, all features, which lints the session
and MTProto libraries as path dependencies). Client all-feature tests pass, 69
doctests included. Rustdoc with warnings denied passes for the generator, the client
and the TL types (all features). Two of those needed care: under the 4,000,000 KiB
address-space cap, rustdoc aborts while merging the shared search index in
`target/doc` (an allocation failure, not a diagnostic). The generator is therefore
documented into its own directory under the cap, and the client and TL types run as
single units under the watchdog: 1,750 MiB and 3,541 MiB peak (time -v). The TL
types' Rustdoc left 179 MiB available at its lowest sample, for less than the
watchdog's four seconds: it is now the heaviest command of the round. The whole-API
serde unit passes check and Clippy again at 3,357 / 3,378 MiB peak, 447 / 453 MiB
available at the lowest, swap moving 67 / 42 MiB.

## Ignored tests

[The inventory](ignored-tests.md) lists 39 ignored tests with what each needs and how
it ended. 38 need no credentials and all were run once and pass: 32 local benchmarks
and loopback tests, the bot workload and its mock, and four probes of Telegram's
servers. The probes are a key exchange and an encrypted `ping` with production DC 2
(new: `test_production_key_exchange_and_ping`, no `api_id`, no API method), two
read-only queries on the public test DC, and the first key-exchange step through a
SOCKS5 server the test starts itself. Four connections, once each, never retried.
`read_only_bot_session_against_telegram` needs test credentials from the owner and
was not run. Nothing on this server was searched for credentials or sessions.

## The coarse clock: kept, and its `unsafe` block

**Decision: keep `grammers_mtproto::coarse_clock` (`7e79532`), on `libc`.** It is the
largest single gain of these rounds where the clock is a system call, as on this host
(275 ns for a precise reading against 14 ns, measured again above), and costs nothing
where it is not. `rustix` would replace one `unsafe` block by a safe call, at the
price of adding it and its three dependencies to every build of the protocol crate,
and of reading the clock through its own vDSO lookup instead of the C library's. The
gain depends on that reading staying out of the kernel, which `sysc.c` has counted
for the C library's path only. `libc` is already in every build through Tokio. No
concrete reason for `rustix` was found.

The block was read again:

- It is one call, `clock_gettime`, in one function, `read`. The clock identifier is
  passed by value and an unknown one is reported through the status. The only memory
  it touches is the `timespec` behind the pointer, which the function owns for the
  call. That value is zeroed first and all zeroes is a `timespec`, so reading it is
  sound whether or not the call wrote. The SAFETY comment now says each of these.
- The status is checked before the value is believed, and a negative or oversized
  field is refused rather than converted.
- New tests: a clock the kernel refuses gives no reading
  (`a_clock_the_kernel_refuses_gives_no_reading`, the failure path of the block), and
  the raw readings are ordered like their clocks. The existing ones cover the public
  readings, the conversion on both sides of the anchor and the plausibility check.
- The fallback is three separate things. Other systems compile a module with no
  `unsafe` that reads the precise clock: that is what the Windows and macOS checks of
  line 1.3 compile. A kernel without the clock, or a process whose readings are found
  over 50 ms from the precise clock twice in a row, falls back for good.
- The documentation claimed a reading is "at most one tick earlier" than the precise
  clock. For `instant` that bound is not strict: the conversion adds the delay of one
  pairing of two clocks. It now says so, and names the 50 ms check as the strict bound.
  "Never later" was and is exact.

MTProto tests pass (129 unit with the two new ones, its three other test targets and
its doctests); its seven ignored benchmarks pass. Strict all-target, all-feature Clippy passes for the
MTProto and sender crates, and Rustdoc with warnings denied for the MTProto crate.
