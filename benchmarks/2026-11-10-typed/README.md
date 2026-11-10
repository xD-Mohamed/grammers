# The typed client path, and what earlier rounds had not verified (2026-11-10)

Baseline `4458f8d`. Everything here is local mock traffic on this host: nothing contacted
Telegram and no credential was used. Rust 1.99.1, 4-core KVM guest (Ryzen 9 7950X3D),
Linux 5.15, 5.9 GB RAM, shared with other services. Method and statistics are those of
[the 2026-11-09 round](../2026-11-09/README.md): changes are medians of paired percentage
changes with unadjusted paired bootstrap 95% intervals, and a change whose interval
includes zero is reported as no measured change.

The owner approved breaking public types for the typed path in this round. Each break is
in [CHANGELOG.md](../../CHANGELOG.md) with what to write instead.

| | Result | Holds on other hosts |
|---|---|---|
| [An update holds its message once](#an-update-holds-its-message-once) | **+6.05% requests per second** at saturation, 2 allocations and 2.7 kB less per request | Yes |
| [A reply decoded once](#a-reply-decoded-once-measured-twice-and-dropped) | Built two ways, **no gain**, dropped | |
| [The request decoded again](#the-request-decoded-again-not-built) | 0.75% of CPU, not built | |
| [The kernel's coarse clock](#the-kernels-coarse-clock) | **+10.11% requests per second** at saturation with updates, 3% less CPU on the raw path | Only where the clock is a system call |
| [Peers as a list, one sent message without maps, one copy of a reply](#fewer-allocations-per-request) | 1.75 allocations and 1.8 kB less per request, **no measured change in CPU** | Yes |
| [What earlier rounds had not verified](#what-the-earlier-rounds-had-left-unverified) | All of it passes, `impl-serde` in four parts | |

## Where the typed path's CPU goes

One busy bot connection with updates, closed loop with 64 callers, sampled at about 250 Hz
for 30 s at `4458f8d` ([7,494 samples](profile/closed64-4458f8d.txt), with the sampler
attached: 6.40 µs per request). Shares are inclusive.

| Where | Share |
|---|---:|
| The connection's task | 40.8% |
| - decrypting what it read | 8.4% |
| - decoding updates that arrived on their own | 6.8% |
| - decoding a request's reply for the update stream (`process_own_update`) | 5.9% |
| - of which the request, decoded again for `updateShortSentMessage` | 0.75% |
| - copying a reply's bytes for the update stream, and for its caller | 0.66% and 1.37% |
| The caller decoding its reply (`invoke_in_dc`) | 4.3% |
| The caller of `send_message` turning a full reply into a message and its peers | 4.4% |
| The update stream's consumer (`next_update`) | 20.8% |
| - building each update's peers | 4.6% |
| - `Update::from_raw` cloning the message of an update | 2.5% |
| Reading the clock, all callers | 18.6% |
| - the harness, twice per request for its latency figure | about 10% |
| - the update stream, once per batch | 5.9% |
| - `send_message` ageing the table of written peers, on a full reply | 1.7% |

The clock is a system call on this host, as the earlier rounds found. On a host whose
kernel serves it from the vDSO those rows are close to zero and every other share is
larger by a fifth.

## An update holds its message once

`update::Message` kept the message twice: in the message it wraps and inside the raw
update in its public `raw` field. `Update::from_raw` cloned every new or edited message
it delivered. In `7bd960f` the message is moved out of the raw update, and the update is
put back together from the message, its kind and its position only when asked for.

| Scenario | Metric | `4458f8d` | `7bd960f` | Change |
|---|---|---:|---:|---:|
| Closed loop, 64 callers, 30 rounds | RPC/s | 164,499 | 173,385 | **+6.05%** [+4.43, +6.91] |
| | CPU µs/RPC | 6.07 | 5.75 | **-5.65%** [-6.10, -4.07] |
| | user / kernel µs/RPC | 4.61 / 1.53 | 4.31 / 1.49 | **-6.37%** / -1.30% [-5.68, +3.51] |
| | p50 / p95 / p99 µs | 379 / 465 / 524 | 360 / 447 / 502 | **-5.60%** / **-4.88%** / **-5.65%** |
| | Allocations per RPC | 21.44 | 19.44 | **-9.33%** |
| | Bytes allocated per RPC | 16,736 | 14,075 | **-15.89%** [-16.11, -15.75] |
| | Steady RSS KiB | 6,404 | 6,424 | +0.28% [-1.17, +1.40] |
| Open loop, 5,000 RPC/s, 8 rounds | CPU µs/RPC | 40.19 | 38.83 | -1.10% [-6.12, +3.10] |
| | p99 µs | 2,105 | 2,146 | +0.76% [-0.81, +4.62] |
| | Allocations per RPC | 22.47 | 20.47 | **-8.92%** |
| | Bytes allocated per RPC | 17,180 | 14,524 | **-15.56%** [-15.70, -15.31] |
| | Steady RSS KiB | 6,054 | 6,100 | +0.46% [-0.66, +1.39] |
| Open loop, 500 RPC/s, 12 rounds | CPU µs/RPC | 97.08 | 98.40 | +1.08% [-0.23, +6.00] |
| | p99 µs | 1,532 | 1,647 | +14.26% [-7.28, +31.09] |
| | Allocations per RPC | 22.26 | 20.26 | **-8.98%** |
| | Bytes allocated per RPC | 16,110 | 13,459 | **-16.46%** [-16.56, -16.46] |
| | Steady RSS KiB | 4,956 | 4,958 | +3.11% [-1.13, +6.22] |

Logs: [closed](update-message/bot-closed64), [5,000](update-message/bot-open5000),
[500](update-message/bot-open500).

Stated as they are:

- **The CPU gain is at saturation.** At fixed rates the allocations and bytes are saved
  and CPU and latency show no measured change, as in the earlier rounds: what costs there
  is the runtime waking tasks across two workers.
- **It took 30 rounds to resolve.** [The first 10](update-message/bot-closed64-first10)
  gave +2.44% [-0.63, +5.52] requests per second, which does not exclude zero. The host
  was noisier than on 2026-11-09. Allocations and bytes are exact in every run.
- **It does not depend on this host's clock.** The saving is a clone and its two
  allocations, on any hardware. The percentage is of a request that spends 18% reading
  the clock here, so it is larger elsewhere.
- **A bot that receives more than it sends gains more.** This workload delivers 1.25
  updates per request. The clone was per update.
- **"Live heap" moved +0.43% [+0.27, +0.76] in the closed loop and +0.69% [+0.31, +1.08]
  at 500 per second, about 2 KiB, and that is the allocator's thread cache.** The harness
  reads `mallinfo`, which counts freed chunks parked in that cache as in use. With the
  cache off (`GLIBC_TUNABLES=glibc.malloc.tcache_count=0`,
  [6 rounds](update-message/bot-open500-no-thread-cache)) the figures are 212,848 B and
  212,816 B, +0.00% [-2.96, +1.26]. The change itself holds one message where there
  were two.
- The measured build and `7bd960f` differ by tests, a test-only accessor and comments.

## A reply decoded once: measured twice, and dropped

A request whose reply holds updates has that reply decoded twice: by the connection, for
the stream of updates, and by its caller. The two shares in the profile are 3.8% and 4.3%.
Both consumers end up owning the value, so decoding once means handing one of them a
copy. Two ways of doing that were built and measured against `7bd960f`, closed loop with
64 callers, 30 rounds each.

| Variant | RPC/s | CPU per RPC | Allocations per RPC | Steady RSS |
|---|---:|---:|---:|---:|
| [The caller is sent a copy of every reply the connection decoded](reply-decoded-once/every-own-reply.diff) | **-3.80%** [-6.12, -2.38] | **+3.81%** [+2.38, +6.19] | 19.44 to 19.94 | **+7.57%** [+6.22, +8.52] |
| [Only of a full `updates` reply, as its 80-byte struct](reply-decoded-once/full-replies-only.diff) | -0.24% [-1.98, +1.16] | +0.17% [-1.03, +2.02] | 19.44 to 19.69 | **+4.58%** [+3.48, +6.39] |

Logs: [first](reply-decoded-once/every-own-reply-closed64),
[second](reply-decoded-once/full-replies-only-closed64).

What that says:

- **A copy of a decoded reply costs what decoding it costs.** Both allocate the same
  strings and vectors, and the parsing between them is not where the time goes.
- The first variant lost because `tl::enums::Updates` is over a kilobyte as a value: a
  box of it for every such reply, half of which are `updateShortSentMessage` and decode
  in a few fields, costs more than those decodes did.
- The second removes that and is left with nothing: the decode it saves on one request
  in four is paid for by the copy, plus a box.
- Neither needed a public type to change. Both went through a method added to
  `RemoteCall`, generated for the 144 calls that return `Updates`. The second diff is the
  library alone: its unit tests were not brought along once it measured nothing.

What would remove the work is one decoded message owned by both the message
`send_message` returns and the update the stream delivers, which means
`message::Message::raw` behind an `Arc`: the field most code using this library reads,
for at most 4% of one connection's CPU at saturation. Not done, and not proposed.

## The request decoded again: not built

For `updateShortSentMessage` the connection decodes the request it kept as bytes, to build
the message the stream delivers. That is 0.75% of CPU in the profile (56 samples). The
caller still needs its text and markup for the message it returns, so the alternative is
again a copy of the same strings in place of a decode of them, which the measurement
above puts at nothing. It was not built.

## The kernel's coarse clock

The precise clock is a system call on this host: the guest's `kvm-clock` is not served
from the vDSO. Linux also keeps the time of its last tick where a process reads it
without entering the kernel.

| Reading, ns each on this host | Precise | As of the last tick (4 ms here) |
|---|---:|---:|
| `clock_gettime` in C, monotonic / realtime | 281 / 273 | 4.3 / 4.2 |
| From Rust: `Instant::now` / `coarse_clock::instant` | 299 | 11.5 |
| `SystemTime::now` / `coarse_clock::unix_time` | 286 | 5.1 |

After the 2026-11-09 and 2026-11-10 rounds the library still read the precise clock for
every packet and every batch of updates, to compare it with deadlines of half a second
and more. `7e79532` reads the coarse clock in three places:

- the wall clock for the first message identifier of a packet (`get_new_msg_id`), and the
  offset correction beside it, so that both sides of the offset are one clock;
- the receiver's one reading per turn of its loop (`UpdatesReceiver::turn_now`), except
  in the turn after a deadline's timer fired, which reads the precise clock;
- the ageing of the table of written peers when its caller has no reading.

| Scenario | Metric | `7bd960f` | `7e79532` | Change |
|---|---|---:|---:|---:|
| One bot connection with updates, closed loop, 64 callers, 30 rounds | RPC/s | 166,781 | 182,968 | **+10.11%** [+8.15, +12.08] |
| | CPU µs/RPC | 5.98 | 5.45 | **-8.92%** [-10.71, -7.60] |
| | user / kernel µs/RPC | 4.44 / 1.54 | 4.27 / 1.17 | **-3.47%** / **-24.77%** |
| | p50 / p95 / p99 µs | 375 / 457 / 516 | 342 / 420 / 463 | **-9.32%** / **-8.19%** / **-9.49%** |
| | Allocations per RPC | 19.44 | 19.44 | none |
| | Steady RSS KiB | 6,418 | 6,306 | **-1.38%** [-2.43, -0.06] |
| The same, open loop, 5,000 RPC/s, 8 rounds | CPU µs/RPC | 39.22 | 38.80 | +0.44% [-4.30, +3.97] |
| | p99 µs | 2,081 | 2,146 | +2.39% [-4.48, +11.29] |
| The same, open loop, 500 RPC/s, 12 rounds | CPU µs/RPC | 102.68 | 100.90 | -1.94% [-6.40, +1.43] |
| | p99 µs | 1,606 | 1,655 | -1.17% [-16.95, +29.77] |
| 64 sessions on the raw path, 16 callers, plain replies, 14 pairs | RPC/s | 83,514 | 86,750 | +3.17% [-0.23, +6.77] |
| | CPU µs/RPC | 11.87 | 11.45 | **-3.16%** [-6.29, -0.62] |
| | p95 / p99 µs | 312 / 388 | 294 / 358 | **-5.70%** / **-8.40%** |
| The same, gzip replies | RPC/s | 82,046 | 84,471 | **+3.35%** [+1.20, +4.99] |
| | CPU µs/RPC | 12.14 | 11.79 | **-2.94%** [-4.51, -1.10] |

Logs: [closed](coarse-clock/bot-closed64), [5,000](coarse-clock/bot-open5000),
[500](coarse-clock/bot-open500), [raw path](coarse-clock/loopback).

Counted on the client thread of the closed loop with [`sysc.c`](../2026-11-09/tools/sysc.c),
over one whole run of each build ([before](coarse-clock/clock-reads-7bd960f.txt),
[after](coarse-clock/clock-reads-7e79532.txt)). A packet carries about a dozen requests
in this loop, and the harness reads the precise clock twice per request for its own
latency figure, which is most of what is left in the first row.

| Readings of the clock per packet sent | `7bd960f` | `7e79532` |
|---|---:|---:|
| Precise, monotonic, the harness's own included | 44.5 | 28.2 |
| Precise, wall clock | 1.00 | 0.00 |
| Coarse, which is not a system call | | 17.4 |

Stated as they are:

- **This gain exists because the clock is a system call here.** On a host whose kernel
  serves the precise clock from the vDSO, both readings cost a few nanoseconds and
  nothing measurable changes. Nothing was tuned on the server: `kvm-clock` is still its
  clock source.
- **It is at saturation.** The open loops show no measured change, as for every change to
  this path so far.
- **What a reading can no longer tell.** A message identifier's time, the receiver's
  deadlines and `UpdatesReceiver::last_clock_read` can be one tick old: 4 ms here, 10 ms
  at the longest. Deadlines are 0.5 s and 15 minutes, the table of peers ages by 10
  minutes, and Telegram accepts identifiers 300 s old.
- **The hazard, and its test.** A timer that fires at a deadline can find the coarse
  clock a tick short of it. A receiver reading only that would find nothing due, wait for
  the same deadline and spin until the tick. The turn after a timer therefore reads the
  precise clock. `a_deadline_that_fired_is_found_due_without_waiting_for_the_tick` gives
  a receiver a tick of 300 ms and counts its turns: with that reading removed it fails.
- **One `unsafe` block**, the call to `clock_gettime`: the standard library has no safe
  way to ask for these clocks. `libc` becomes a direct dependency of `grammers-mtproto`
  on Linux and Android. It was already in every build.
- **When it stops being used.** An `Instant` is paired with the raw monotonic clock once,
  and the pairing is checked against `Instant::now` once a second. If they are ever
  further apart than a tick explains, twice in a row, the process reads the precise clock
  from then on. So does one whose kernel refuses the coarse clock, and every other
  system.
- Not converted: the connection's reading at each write, which stamps when a tracked
  request was sent. That time is public (`InvocationTracker::sent_at`), and 4 ms of it is
  something a caller measuring latency would see. It is 0.2 readings per request here.
- The closed loop was measured once [before the review](coarse-clock/bot-closed64-before-review),
  +9.20% [+7.62, +10.26], and again with its corrections, which is the row above. The
  open loops were measured before them and not repeated: they change no reading on the
  path those scenarios take. The measured builds differ from `7e79532` by tests.

## Fewer allocations per request

[The profile after the two changes above](profile/closed64-f1dd333.txt) (7,471 samples) has
no single cost left on the typed path: 31% is decoding, 16% moving the large values it
produces, 15% the allocator, 12% the kernel, and of the 11% left for the clock nearly all
is the harness's own two readings per request. Three places allocated what they did not
need. The workload sends a message with half its requests, one to a private chat, which
is answered with `updateShortSentMessage`, and one to a group, answered with full updates.

| Change | Allocations per RPC | Bytes allocated per RPC |
|---|---:|---:|
| `7e79532`, before | 19.44 | 13,954 |
| `9fd5e44`: up to sixteen peers of a response are kept as a list and found by looking through it, and responses without peers share one empty map | 18.94 | 12,416 |
| `482a4ef`: the reply to one sent message is sorted out in one pass, without the two maps and the vector an album needs | 18.19 | 12,286 |
| `35a85cd`: a reply that holds updates is copied out of the read buffer once, and the stream's update is read from the caller's copy | 17.69 | 12,175 |

Those are exact counts from one run of each build. Paired, 30 rounds of the closed loop
and the open loops for the first two together:

| Scenario | Metric | Before | After | Change |
|---|---|---:|---:|---:|
| `7e79532` to `482a4ef`, closed loop, 30 rounds | RPC/s | 182,990 | 185,267 | +0.22% [-1.52, +3.96] |
| | CPU µs/RPC | 5.46 | 5.39 | -0.19% [-3.72, +1.50] |
| | p99 µs | 473 | 461 | -0.42% [-4.28, +2.65] |
| | Allocations per RPC | 19.44 | 18.19 | **-6.43%** |
| | Bytes allocated per RPC | 13,978 | 12,263 | **-12.18%** [-12.43, -12.05] |
| | Steady RSS KiB | 6,376 | 6,516 | **+1.53%** [+0.65, +3.02] |
| The same, open loop, 5,000 RPC/s, 8 rounds | CPU µs/RPC | 38.80 | 38.52 | -0.85% [-3.01, +2.98] |
| | Bytes allocated per RPC | 14,526 | 12,808 | **-11.80%** [-12.46, -11.46] |
| | Steady RSS KiB | 5,876 | 5,824 | -1.62% [-3.47, +0.97] |
| The same, open loop, 500 RPC/s, 12 rounds | CPU µs/RPC | 101.00 | 101.11 | +0.13% [-4.40, +4.29] |
| | Bytes allocated per RPC | 13,459 | 11,743 | **-12.75%** [-12.77, -12.75] |
| | Steady RSS KiB | 4,958 | 4,016 | -3.81% [-35.07, +2.57] |
| `482a4ef` to `35a85cd`, closed loop, 30 rounds | RPC/s | 183,586 | 186,757 | +0.95% [-1.03, +3.57] |
| | CPU µs/RPC | 5.44 | 5.35 | -0.93% [-3.42, +0.93] |
| | Allocations per RPC | 18.19 | 17.69 | **-2.75%** |
| | Bytes allocated per RPC | 12,275 | 12,175 | **-0.78%** [-1.06, -0.53] |
| | Steady RSS KiB | 6,496 | 6,498 | -0.22% [-0.78, +0.50] |

Logs: [closed](peers-and-replies/bot-closed64), [5,000](peers-and-replies/bot-open5000),
[500](peers-and-replies/bot-open500), [the copy](peers-and-replies/own-update-copy-closed64).

Stated as they are:

- **No measured change in CPU, throughput or latency, in any scenario.** These are kept
  for what they allocate: 1.75 allocations and 1.8 kB less per request, on any hardware.
  The medians lean the right way and the intervals include zero.
- **RSS rose 1.53% in the closed loop with the first two**, 140 KiB, and did not in the
  open loops, whose intervals include zero on the other side. Live heap is unchanged
  (+0.15% [-0.34, +0.31]). The peers of a batch are now one allocation of another size,
  which the allocator serves from other bins: not explained further than that.
- A bot whose sends go to groups gets the second change on every send, where this
  workload gets it on one request in four. One whose sends go to private chats gets the
  shared empty map on every send.
- `PeerMap::iter` and `iter_peers` go through a few peers in the order they arrived. The
  order was arbitrary before. Nothing else about them changes.
- The third is an option of the protocol layer, `own_updates_in_results`, off by default.
  The connections this library makes turn it on. A driver of its own built on
  `Encrypted` sees what it saw before.
- The measured builds differ from the commits by tests and comments.

## What is left, and why this round stops here

The last three changes were kept on allocations with no measurable CPU gain, which is the
stopping rule. What the profile still shows, largest first:

| Candidate | Share at `f1dd333` | Why it is not done |
|---|---|---|
| Large TL values moved by value: `Update` is 760 B, `Updates` over 1 kB, `types::Message` about 2.5 kB, each copied out of a `Result`, into its enum and into a box or a vector as it is decoded | 16% of CPU as copies | Smaller values mean boxed variants or fields in generated types every caller matches on. Layer 233 boxed only what it added, to leave those shapes alone |
| The reply to a send decoded by its connection and again by its caller | 5.4% and 4.4% | Measured above: a copy costs what the decode costs. One shared decoded message means `message::Message::raw` behind an `Arc` |
| The allocator: 17.69 allocations per request are left | 15% of CPU | What remains is the decoded data itself (vectors of updates, users and chats, strings, the boxed message) and one reply buffer and one channel per request |
| The peers of a batch converted and cached (`peer_map_from_raw`, `cache_peer_map`) | 4.8% and about 2%, before `9fd5e44` | The conversion wraps each raw user and chat, which the consumer may read. The cache check is a hash per peer under a lock, and nothing contends for it at one connection per thread |
| The connection's reading of the precise clock at each write | 0.2 readings per request, about 1% | It stamps `InvocationTracker::sent_at`, which callers read as a time. A coarse one would be 4 ms out |
| The harness's own two readings per request | about 9% | Not the library's. It makes every share above look a tenth smaller than it is |
| `send` and `recv` | 11% | One of each per packet. `io_uring` was measured on 2026-11-10 and is excluded by the owner on this kernel |

## What the earlier rounds had left unverified

Each command ran alone, one crate at a time, `-j 2` (`-j 1` where noted), `nice`, under a
4 GB address-space limit, with a watchdog that stops a command when available memory
falls under 700 MB. None was stopped except the first row. All at `4458f8d`.

| Left by | Item | Result |
|---|---|---|
| 2026-11-10 | `grammers-tl-types` with `impl-serde`, as one unit | **Does not fit this host.** `cargo check -j 1` with only `impl-serde,tl-api`, no debuginfo and no incremental state reached 2,834 MB resident in 40 s and was stopped at 692 MB available, before the limit. Swap grew 24 MB |
| | The same, [in four parts](tools/serde_parts.py) | **Passes.** All 3,351 derives with every feature, each part expanded and type-checked as generated against the whole type graph. 1.9 GB resident per part, 19 s each, swap untouched |
| | Rustdoc for `grammers-tl-types` | Passes with every feature including `impl-serde`, warnings denied: 2.2 GB resident, 23 s |
| | The documentation generator's tests | 17 pass, in a scratch virtual environment with `docs/requirements.txt` (`beautifulsoup4` 4.14.3, `Pygments` 2.21.0). The evidence checker's 32 pass |
| 2026-11-09 | Clippy and Rustdoc for the crates that round did not touch | Clean for all nine crates (below) |
| | The minimal-client feature combinations | Pass (below) |

Why four parts work: with `impl-serde` every generated type derives `Serialize` and
`Deserialize`, and one compiler process holds the expansion of all of them. The tool
makes a scratch copy of the crate whose build script keeps the derive on a quarter of the
items and gives the rest a stub implementation, so that a part is checked against every
type it refers to. It is a check of what the generator emits, in the memory this host
has. It is not the build a user of the feature runs, which needs about 3 GB or more.

Every crate, with warnings denied in Clippy (`--all-targets`) and Rustdoc:

| Crate | Features | Release tests | Clippy | Rustdoc |
|---|---|---|---|---|
| `grammers-tl-parser` | all | 39, 6, 1 doctest | clean | clean |
| `grammers-tl-gen` | all | 28, 8, 1 (12 ignored) | clean | clean |
| `grammers-crypto` | all | 39, 1 (6 ignored) | clean | clean |
| `grammers-tl-types` | default | 13, 1, 24 doctests (1 ignored) | | |
| | all but `impl-serde` | 13, 1, 24 doctests (1 ignored) | clean | clean |
| | all | in four parts, above | not run | clean |
| `grammers-mtproto` | all | 121, 3 (6 ignored) | clean | clean |
| `grammers-session` | all | 109, 22, 7, 1, 3 doctests (4 ignored) | clean | clean |
| | none | 76, 1, 3 doctests (3 ignored) | | |
| | `session-import` | 76, 22, 1, 3 doctests | | |
| | `session-string` | 76, 7, 1, 3 doctests | | |
| | `session-import-files` | 96, 22, 1, 3 doctests | | |
| `grammers-mtsender` | all | 184, 5, 2, 1 (16 ignored) | clean | clean |
| `grammers-client` | all | 125, 2, 1, 69 doctests (4 ignored) | clean | clean |
| | default | 82, 2, 1, 69 doctests | | |
| | none | 63, 2, 1, 67 doctests | | |
| | `dispatcher,qr-login`, library only | 78 | | |
| | all, debug profile, `malformed_mentions` | 2 | | |
| | examples: all features, and `ping` with `error_handling` without any | compile | | |
| `grammers` | none exist | 0 | clean | clean |

`cargo fmt --all -- --check` passes. Nothing these runs turned up needed a fix.

Not run: Clippy for `grammers-tl-types` with `impl-serde`, which needs the same memory as
the single check; the whole workspace in one command, by the host's rule; ignored tests,
which are benchmarks and network probes; anything against Telegram.

## API changes and consumers

| Change | Kind |
|---|---|
| `update::Message::raw` (field) | **Removed.** `update::Message::raw()` builds it on first use; `into_raw()` returns it owned |
| `update::Message::raw()`, `update::Message::into_raw()` | New methods |
| `Update::raw()` | Unchanged signature. For a message it builds the raw update on first use |
| `grammers_mtproto::coarse_clock::{instant, unix_time}`, re-exported as `grammers_mtsender::coarse_clock` | New |
| `UpdatesReceiver::last_clock_read` | Unchanged signature. The reading can be a kernel tick old |
| `grammers-mtproto` depends on `libc` on Linux and Android | New direct dependency, already in the build |
| `mtp::Encrypted` builder: `own_updates_in_results` | New option, off by default. With it `Deserialization::OwnUpdate::update` is empty |
| `PeerMap::iter`, `PeerMap::iter_peers` | Unchanged signatures. Up to sixteen peers come in the order they arrived |

## Reproduction

Build one step at a time with bounded parallelism on a small host.

```sh
# For the old revision: `git archive <rev> | tar -x -C <dir>` and build there.
nice cargo test --release --locked -j 2 -p grammers-client --features session-file --test bot_workload --no-run
python3 scripts/compare-bot-workload.py --binary base=<old> --binary final=<new> --rounds 30 --scenario closed64 --output <dir>
python3 scripts/compare-bot-workload.py --binary base=<old> --binary final=<new> --rounds 8 --scenario open5000 --output <dir>
python3 scripts/compare-bot-workload.py --binary base=<old> --binary final=<new> --rounds 12 --scenario open500 --output <dir>

# The raw path.
nice cargo test --release --locked -j 2 -p grammers-mtsender --test loopback --no-run
python3 scripts/compare-loopback-r1.py --baseline <old> --candidate <new> --runs 14 --window-ms 2000 --output <dir>

# What a reading of each clock costs on a host.
nice cargo test --release --locked -j 2 -p grammers-mtproto --lib -- --ignored --nocapture benchmark_coarse_clock_cost

# The profile: benchmarks/2026-10-31-bot/profile/mkprof.py and prof_patch.py on a scratch copy.

# impl-serde in parts.
python3 benchmarks/2026-11-10-typed/tools/serde_parts.py . <scratch dir>
```
