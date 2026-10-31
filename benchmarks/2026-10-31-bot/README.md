# Bot workload measurements (2026-10-31)

Local model of a busy bot: one long-lived session, concurrent small RPCs and live updates,
through the real `Client` API. Half of the calls are `send_message`, to a private chat
(short reply) or a large group (full updates); the rest are `messages.setTyping` and
`users.getUsers`. The mock pushes one incoming message per two replies. It runs in a
**separate process**, so CPU, heap and RSS below are the client's alone.

[`bot_workload.rs`](../../grammers-client/tests/bot_workload.rs) is the harness and
[`compare-bot-workload.py`](../../scripts/compare-bot-workload.py) the driver. The same
harness was built into each revision. `final` is this tree. Changes are medians of
paired percentage changes with unadjusted paired bootstrap 95% intervals; **bold** means
the interval excludes zero, and anything else is reported as no measured change. Rust
1.99.1, 4-core shared x86_64 host, client on CPU 0 (or 0-1), mock on CPUs 2-3.

This is all local mock traffic. Nothing here contacted Telegram.

## What the client library can and cannot change

Telegram's rate limits are enforced by its servers and no client can raise them. Per the
[Bot FAQ](https://core.telegram.org/bots/faq): "In a single chat, avoid sending more than
one message per second", "In a group, bots are not be able to send more than 20 messages
per minute", and "bots are not able to broadcast more than about 30 messages per second,
unless they enable paid broadcasts", which allows up to 1000 per second. Nothing here makes a client send faster than it
asks to, and [the audit](../../COMPLIANCE-2026-10-31.md) covers flood wait handling. What
these numbers mean is that the same work costs the host less, and that bursts no longer
lose updates. Those losses used to cost recovery requests to Telegram.

## Against all three baselines

CPU is user + system time per completed RPC, over the whole client process.

### Closed loop, 64 concurrent callers (capacity on one core), 12 rounds

| Metric | `7774ed6` | `2ffd991` | `79c1c0a` | final | vs `7774ed6` | vs `2ffd991` | vs `79c1c0a` |
|---|---:|---:|---:|---:|---:|---:|---:|
| RPC/s | 89,948 | 85,852 | 107,473 | 203,246 | **+120.05%** [+116.03, +130.80] | **+132.12%** [+124.86, +143.37] | **+87.72%** [+78.82, +95.67] |
| CPU µs/RPC | 11.08 | 11.61 | 8.73 | 4.92 | **-54.50%** [-56.57, -53.67] | **-56.85%** [-58.86, -55.51] | **-43.02%** [-45.28, -40.28] |
| user µs/RPC | 7.87 | 8.38 | 7.72 | 4.36 | **-43.69%** [-44.95, -42.43] | **-47.52%** [-49.97, -44.04] | **-42.65%** [-45.37, -41.16] |
| system µs/RPC | 3.28 | 3.25 | 1.04 | 0.60 | **-81.40%** [-83.49, -79.87] | **-81.78%** [-83.35, -79.09] | **-42.12%** [-47.28, -37.58] |
| Allocations/RPC | 27.25 | 27.20 | 27.25 | 21.44 | **-21.32%** [-21.32, -21.32] | **-21.18%** [-21.21, -21.18] | **-21.32%** [-21.32, -21.32] |
| Bytes allocated/RPC | 38,360 | 37,578 | 38,428 | 16,752 | **-56.35%** [-56.44, -56.25] | **-55.44%** [-55.58, -55.31] | **-56.41%** [-56.53, -56.34] |
| p50 µs | 684.0 | 720.9 | 589.8 | 303.1 | **-54.57%** [-56.45, -53.31] | **-56.80%** [-58.94, -55.90] | **-48.22%** [-49.99, -46.03] |
| p95 µs | 901.1 | 933.9 | 770.0 | 397.3 | **-54.95%** [-57.62, -53.49] | **-57.45%** [-59.54, -54.03] | **-47.39%** [-50.74, -45.34] |
| p99 µs | 1,122.3 | 1,146.9 | 905.2 | 444.5 | **-60.44%** [-63.34, -57.91] | **-61.60%** [-64.10, -56.79] | **-50.46%** [-54.49, -47.92] |
| Steady RSS KiB | 8,048 | 8,746 | 8,844 | 6,338 | **-21.31%** [-22.50, -20.05] | **-27.52%** [-29.18, -26.35] | **-29.00%** [-29.94, -28.22] |
| Live heap B | 2,272,920 | 2,861,496 | 2,700,152 | 492,584 | **-78.31%** [-78.41, -78.21] | **-82.78%** [-82.83, -82.71] | **-81.73%** [-81.79, -81.66] |

### Open loop, 5,000 RPC/s, 12 rounds

| Metric | `7774ed6` | `2ffd991` | `79c1c0a` | final | vs `7774ed6` | vs `2ffd991` | vs `79c1c0a` |
|---|---:|---:|---:|---:|---:|---:|---:|
| CPU µs/RPC | 42.34 | 47.70 | 44.64 | 33.75 | **-20.16%** [-26.41, -19.18] | **-30.27%** [-34.24, -25.57] | **-23.77%** [-26.66, -21.58] |
| Core share | 0.21 | 0.24 | 0.22 | 0.17 | **-20.13%** [-26.51, -19.11] | **-30.32%** [-34.28, -25.44] | **-23.67%** [-26.51, -21.47] |
| Allocations/RPC | 29.23 | 29.22 | 29.23 | 22.41 | **-23.32%** [-23.40, -23.28] | **-23.32%** [-23.38, -23.27] | **-23.33%** [-23.37, -23.28] |
| Bytes allocated/RPC | 58,433 | 64,866 | 64,966 | 16,794 | **-71.25%** [-71.38, -71.15] | **-74.14%** [-74.22, -74.03] | **-74.18%** [-74.23, -74.07] |
| p50 µs | 1,130.5 | 1,163.3 | 1,146.9 | 1,114.1 | -1.35% [-7.97, +4.46] | -2.80% [-10.13, +1.48] | -2.73% [-9.95, +2.96] |
| p99 µs | 2,097.2 | 2,113.6 | 2,072.6 | 2,064.4 | -3.06% [-6.40, +0.81] | -4.62% [-7.89, +0.77] | -1.15% [-5.40, +3.27] |
| Steady RSS KiB | 6,760 | 6,856 | 6,860 | 5,778 | **-14.40%** [-15.87, -13.06] | **-15.67%** [-18.02, -14.44] | **-16.01%** [-18.36, -15.27] |
| Peak RSS KiB | 6,834 | 7,206 | 7,170 | 5,778 | **-15.24%** [-18.77, -14.50] | **-20.33%** [-28.41, -18.88] | **-18.97%** [-21.58, -17.32] |
| Live heap B | 765,136 | 791,128 | 792,792 | 311,200 | **-59.70%** [-61.18, -58.47] | **-60.93%** [-62.83, -58.55] | **-60.91%** [-62.92, -58.32] |

Open-loop latency is measured from the scheduled arrival, whose timer has 1 ms resolution,
so it shows no measured change at this load and says little about the client itself.

### Open loop, 500 RPC/s, 8 rounds

| Metric | `7774ed6` | `2ffd991` | `79c1c0a` | final | vs `7774ed6` | vs `2ffd991` | vs `79c1c0a` |
|---|---:|---:|---:|---:|---:|---:|---:|
| CPU µs/RPC | 103.86 | 106.41 | 107.15 | 97.31 | -4.72% [-12.42, +6.06] | **-8.48%** [-19.11, -0.60] | **-8.07%** [-14.80, -5.27] |
| Allocations/RPC | 29.27 | 29.27 | 29.27 | 22.26 | **-23.95%** [-24.05, -23.92] | **-23.95%** [-23.95, -23.92] | **-23.95%** [-24.00, -23.95] |
| Bytes allocated/RPC | 56,972 | 63,353 | 63,353 | 15,950 | **-72.00%** [-72.04, -72.00] | **-74.82%** [-74.82, -74.82] | **-74.82%** [-74.83, -74.82] |
| p99 µs | 1,499.1 | 1,515.6 | 1,671.2 | 1,589.3 | +7.27% [-34.37, +35.53] | +0.00% [-11.20, +23.17] | -8.32% [-15.38, +8.07] |
| Steady RSS KiB | 5,696 | 5,958 | 5,976 | 4,054 | **-29.21%** [-41.68, -4.62] | **-32.38%** [-43.41, -8.71] | **-28.56%** [-44.62, -6.71] |
| Live heap B | 535,744 | 556,424 | 552,640 | 251,224 | **-52.97%** [-53.59, -51.16] | **-54.85%** [-54.97, -54.25] | **-54.53%** [-54.64, -52.44] |

At this rate each request pays for its own wake-ups and system calls, which is most of
the cost and is not the library's to remove. The CPU interval against `7774ed6` crosses
zero: no measured change.

### Session in a real SQLite file, open loop, 8 rounds

| Scenario, metric | `7774ed6` | `2ffd991` | `79c1c0a` | final | vs `7774ed6` | vs `79c1c0a` |
|---|---:|---:|---:|---:|---:|---:|
| 500 RPC/s, CPU µs/RPC | 451.55 | 469.65 | 449.81 | 95.96 | **-78.78%** [-79.44, -78.14] | **-78.70%** [-79.76, -78.05] |
| 500 RPC/s, allocations/RPC | 96.52 | 96.54 | 96.52 | 22.26 | **-76.94%** [-77.00, -76.89] | **-76.94%** [-76.99, -76.91] |
| 500 RPC/s, p99 µs | 5,308 | 5,243 | 5,243 | 1,597 | **-69.11%** [-74.33, -64.64] | **-68.39%** [-73.63, -64.20] |
| 500 RPC/s, a new sender every update, CPU µs/RPC | 453.69 | not run | 454.45 | 287.88 | **-36.11%** [-42.26, -30.37] | **-36.87%** [-41.10, -31.46] |
| 2,000 RPC/s, CPU µs/RPC | 184.52 | not run | 188.56 | 65.12 | **-64.68%** [-66.43, -63.49] | **-65.33%** [-66.86, -64.54] |
| 2,000 RPC/s, updates delivered/s of 2,500 | 126 | not run | 174 | 2,500 | | |
| 2,000 RPC/s, updates lost in the whole run | 16,113 | not run | 15,729 | 0 | | |
| 2,000 RPC/s, steady RSS KiB | 12,552 | not run | 13,150 | 6,524 | **-48.75%** [-54.11, -34.56] | **-49.90%** [-54.54, -47.55] |

The earlier revisions wrote every update's peers to storage again, one transaction per
update batch, even when nothing about them had changed. This host's disk is what limits
that, so the absolute figures vary with storage. A bot hearing from new senders all the
time still pays for those writes: see the second row.

### Bursts on one thread, 256 concurrent callers

The earlier revisions dropped updates when a burst filled the queue before its consumer
ran, then spent their time and memory recovering. Six rounds; one `7774ed6` run did not finish
within two minutes and its pair is left out. [Logs](final/closed256).

| Metric | `7774ed6` | `2ffd991` | `79c1c0a` | final | vs `7774ed6` | vs `2ffd991` | vs `79c1c0a` |
|---|---:|---:|---:|---:|---:|---:|---:|
| RPC/s | 16,155 | 7,434 | 7,657 | 205,362 | **+1185.04%** [+1123.40, +1229.25] | **+2600.09%** [+2544.04, +2867.82] | **+2571.68%** [+2361.92, +2784.10] |
| Updates lost in the whole run | 51,617 | 3,567 | 7,413 | 0 | | | |
| CPU µs/RPC | 61.90 | 134.50 | 130.63 | 4.87 | **-92.23%** [-92.48, -91.84] | **-96.30%** [-96.63, -96.22] | **-96.26%** [-96.54, -95.94] |
| Allocations/RPC | 23.35 | 26.55 | 26.52 | 21.41 | **-8.31%** [-8.70, -8.23] | **-19.36%** [-19.56, -19.15] | **-19.25%** [-19.65, -18.99] |
| Bytes allocated/RPC | 1,401,790 | 3,541,842 | 3,263,656 | 17,055 | **-98.78%** [-98.97, -98.70] | **-99.52%** [-99.52, -99.48] | **-99.48%** [-99.51, -99.45] |
| p99 µs | 55,575 | 128,975 | 125,305 | 1,802 | **-96.88%** [-97.76, -96.37] | **-98.61%** [-98.78, -98.39] | **-98.53%** [-98.85, -98.24] |
| Steady RSS KiB | 38,048 | 46,218 | 43,160 | 7,612 | **-80.70%** [-81.00, -79.86] | **-83.56%** [-84.80, -82.21] | **-82.51%** [-83.37, -81.85] |
| Peak RSS KiB | 42,060 | 47,848 | 47,552 | 7,612 | **-81.72%** [-82.47, -81.67] | **-84.11%** [-84.79, -83.57] | **-83.90%** [-84.73, -83.36] |

These extreme ratios come from the collapse, not from ordinary running: the closed loop with 64
callers above never overflowed in any revision. They apply when a burst outruns the consumer
on one thread, at rates far beyond what Telegram would send to a bot.

## Hot spots, closed loop with 64 callers

From [CPU samples](profile) at about 250 Hz for 30 s per tree, about 7,000 samples each.
Shares are of the client's CPU, so a fixed cost grows as the total shrinks. Allocator,
copies, crypto, kernel and clock are by the leaf function and add to the whole with
"other"; the rest are what the whole stack was doing and overlap them. Each µs/RPC figure is
its share of those runs' own CPU, 9.79 and 5.58 µs/RPC with the sampler attached, not the
table above.

| Where | `79c1c0a` share | final share | `79c1c0a` µs/RPC | final µs/RPC |
|---|---:|---:|---:|---:|
| Allocator (`malloc`, `free`, `realloc`) | 17.07% | 18.38% | 1.67 | 1.03 |
| Copies of large values (`ptr::read`/`write`, `Result` moves) | 15.91% | 12.84% | 1.56 | 0.72 |
| AES-IGE and SHA-256 | 8.46% | 14.92% | 0.83 | 0.83 |
| Kernel (`send`, `recv`, `epoll_wait`, futex, `getrandom`) | 9.77% | 11.88% | 0.96 | 0.66 |
| Clock reads | 1.49% | 2.91% | 0.15 | 0.16 |
| Stack in TL decoding | 30.19% | 39.17% | 2.96 | 2.19 |
| Stack in `Sender::on_net_read` | 29.87% | 38.16% | 2.92 | 2.13 |
| Stack in the message box | 8.78% | 5.84% | 0.86 | 0.33 |
| Stack in `Update::from_raw` | 5.75% | 9.50% | 0.56 | 0.53 |
| Stack building the peer map | 2.98% | 5.26% | 0.29 | 0.29 |
| Stack in `try_fill_write` (framing, encrypting) | 3.21% | 5.36% | 0.31 | 0.30 |
| `messages.rs:28`, moving replies through `send_message` | 5.88% | gone | 0.58 | |

At 5,000 RPC/s most of the cost is the kernel and the runtime waking tasks across threads:
[kernel 51.60%, allocator 4.46%, copies 5.82%, crypto 3.49%](profile/open5000-final.txt).
These samples come from a signal timer, and with so few of them in syscall-heavy code
their shares are rough; `sys_us` above is the better figure.

Largest allocation sites by bytes per RPC ([79c1c0a](profile/alloc-sites-79c1c0a.txt),
[final](profile/alloc-sites-final.txt)), sampled at 1 in 211, so these are estimates that
also carry the slowdown of sampling:

| Site | `79c1c0a` | final |
|---|---:|---:|
| `MessageBoxes::process_updates`: `Vec<(Update, State)>` and the stable sort's scratch | 24,289 B | 3,589 B |
| `Vec<Update>` decoding (`deserialize.rs:402`) | 19,206 B | 2,607 B |
| `Vec<Update>` growth | 12,463 B | gone |
| `Vec<UpdatesLike>` per read (`sender.rs` `process_update`) | 10,010 B | 1,248 B |
| `HashMap` of messages per send reply | 9,329 B | below the table |
| Peer map per update batch (`peer_map_from_raw`) | 6,704 B | 7,974 B |
| `Box<types::Message>`, new with the change below | | 5,294 B + 3,661 B |

## What each change gave

Measured in order, each against the build before it, in rounds of five, with an earlier
version of the harness in which every send got a short reply, and on the host's disk. [Summaries](steps),
where kept: the output behind figures marked † was not kept.

| Change | Result |
|---|---|
| `send_message` future 28,232 B to 1,712 B; no clone of updates per reply | Closed 64 CPU **-7.95%** [-11.49, -6.12], RSS **-21.95%** [-24.99, -20.47], live heap **-68.59%** [-68.60, -68.58]; open 5,000 bytes/RPC **-51.00%** [-51.26, -50.67] |
| Boxed `Message`, `Service`, `EphemeralMessage` variants: `Update` 5,240 B to 752 B, the client's `Update` 7,928 B to 880 B | Closed 64 CPU **-21.34%** [-22.15, -14.77], bytes/RPC -28.47%, RSS -7.51% |
| Yield once before dropping an update | Closed 256: 9,642 to about 200,000 RPC/s, 0 lost updates, RSS 51 to 7 MiB † (see the table above for the final build) |
| Reserved request buffer, reused update vector, read buffer growing when filled | Closed 64 CPU **-11.05%** [-14.89, -8.85], allocations -15.56%; closed 256 CPU **-13.47%** [-17.95, -10.40] |
| Peers written only when changed | SQLite, 500 RPC/s: 245 to about 95 µs/RPC † (see the table above for the final build) |
| Small stable sort in place of the standard one | 110 KiB of scratch per RPC on average, gone † |

Tried and not kept, on the same scale:

| Candidate | Result | Decision |
|---|---|---|
| Acknowledge in batches of 8 (`ack_batch`, already a setting) | CPU +1.94% closed, +2.78% open, medians of six pairs † | No gain: left at its default |
| `mimalloc` as the application's allocator | [Closed 64 CPU -6.99% [-8.74, -1.86]; open 5,000 -3.37% [-7.19, +12.66]; RSS +143%, +206%](steps/exp-mimalloc/summary.json) | Not recommended |
| Replay history and pending-request scans | Below 1% of samples even with 256 in flight | Left alone |
| Buffering `getrandom` for padding | At most 1.59% of samples | Not worth touching packet randomness |

## Where this stopped

What remains is required or not the library's: AES-IGE and SHA-256 are the protocol, one
`send` per request at low rates and cross-thread wake-ups belong to the kernel and the
runtime. The candidates left in the profile, none done:

- `update::Message` clones the message because both its public `raw` field and its
  `Message` hold one: about 3% of CPU and 3 allocations per update. Needs an API change.
- A send's reply is decoded twice, for the update stream and for the caller: up to 9%.
  Sharing it means changing how results reach callers in every request.
- A short sent message decodes its own request again to build the update: about 3%,
  because `UpdatesLike::ShortSentMessage` is public.
- Allocator time is now 21 allocations per RPC, most of them the decoded values themselves.

## Fairness across many pools on one thread

The overload scenario (64 pools on one thread, 120,000 RPC/s offered) measures something
else: how a busy pool shares a thread with quiet ones. 12 pairs.

| Metric | vs `7774ed6` | vs `2ffd991` | vs `79c1c0a` |
|---|---:|---:|---:|
| Completed | **+13.14%** [+9.32, +17.96] | **+9.57%** [+4.06, +17.00] | **+8.18%** [+2.76, +16.47] |
| Busy p99 | **+28.11%** [+8.78, +31.73] | **-38.51%** [-51.28, -33.15] | **-35.47%** [-39.72, -31.24] |
| Quiet p99 | **-22.49%** [-31.94, -18.10] | **+59.39%** [+25.84, +93.15] | **+37.74%** [+19.02, +44.08] |
| Process RSS | **+15.23%** [+14.54, +15.73] | **+13.67%** [+12.49, +15.42] | **+13.43%** [+10.31, +16.98] |

Busy p99 against `7774ed6` went from +96.45% to +28.11%, and quiet p99 from -43.87% to
-22.49%. **That is a real trade, and both could not be had.** Larger reads decode more per
poll, and how much of the scheduler's budget they cost decides who waits:

| Budget per | Completed | Busy p99 | Quiet p99 |
|---|---:|---:|---:|
| 1 KiB read | +8.16% | +78.58% [+67.01, +102.87] | -37.44% [-43.19, -27.17] |
| **2 KiB read, kept** | **+13.14%** | **+28.11%** | **-22.49%** |
| 4 KiB read | +15.95% | -12.82% [-41.82, +26.52] | -20.72% [-48.41, +22.03] |
| 8 KiB read | +12.59% | -17.41% [-38.48, +204.20] | -19.18% [-40.44, +189.43] |
| Not charged | +17.36% [+12.73, +19.53] | -17.08% [-19.88, -8.43] | -19.00% [-22.64, -11.74] |

Rows other than the kept one come from [five or six pairs](steps), and all are against
`7774ed6`. The RSS increase is each busy pool's 16 KiB read buffer, included in this
scenario's process figure along with the mock responder, which shares that process.

## Per-session heap

One connection per pool with updates disabled and one call in flight, 400 pools, one run
per binary (deterministic census), [logs](final/heap).

| Stage | `7774ed6` | `2ffd991` | `79c1c0a` | final |
|---|---:|---:|---:|---:|
| Pool created, not connected | 2,103 | 2,143 | 2,143 | 2,143 |
| Connected after one RPC | 7,911 | 7,919 | 7,911 | 7,951 |
| After 1100 replies in a burst | 7,911 | 16,079 | 14,031 | 14,071 |

**No further gain here.** The difference from `7774ed6` is the replay history that
[Telegram's guidelines](https://core.telegram.org/mtproto/security_guidelines) require,
at 6 bytes per ID. TDLib keeps a thousand 8-byte IDs twice over, about 16 KiB. The 40 B is
connection pacing. The gains above are in what a running bot allocates, not in this idle
figure. A session receiving updates also holds a channel block, 32 slots of `UpdatesLike`,
which went from about 168 KiB to about 41 KiB (5,272 to 1,272 B per slot).

## Measured builds

Scenarios above ran before a last change limited to connection pacing and a replay warning. A
[confirmation](steps/final-confirm/summary.json) of the final tree against that build shows no
measured change: closed 64 CPU +0.61% [-1.04, +3.22], open 5,000 CPU +0.14% [-7.76, +3.93], the
same allocations. The closed 256 table is from the final tree. Executable hashes are in each
scenario's `configuration.json`.

## Reproduction

```sh
# For each revision: export it, copy this tree's bot_workload.rs over it, add the sha1, sha2
# and libc dev-dependencies, then:
cargo test --release -p grammers-client --features session-file --test bot_workload --no-run
python3 scripts/compare-bot-workload.py --binary 7774ed6=<path> --binary 2ffd991=<path> \
    --binary 79c1c0a=<path> --binary final=<path> --rounds 12 --scenario closed64 --output <dir>
```

Scenarios are in the script. CPU samples come from [these](profile/mkprof.py)
[patches](profile/prof_patch.py), which add `pprof` to a scratch copy and are not part of
the library. `BOT_SITE_EVERY=211` gives the allocation sites.

## Validation

On this tree, Rust/Cargo 1.99.1, Linux x86_64, locked offline dependencies, all local:

| Check | Result |
|---|---|
| Format, strict all-target/all-feature Clippy, warning-free Rustdoc | Passed |
| Release workspace tests and doctests, all features | 748 passed, 0 failed, 48 ignored |
| Minimal client / with dispatcher and QR login | 125 / 72 passed, 3 / 0 ignored |
| Session memory / import / string / file import | 74 / 96 / 81 / 116 passed, 3 ignored each |
| Evidence checker (Python) | 32 passed |

Ignored tests are benchmarks and explicit network probes, including the optional
[test against Telegram](../../grammers-client/tests/live_telegram_bot.rs), which was not run.
Four of the new regressions were checked by removing their fix: the update burst, flood wait
during recovery, connection pacing, and a final difference short of a held update.
