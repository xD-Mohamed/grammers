# Layer 233, the update path's clock, and what `io_uring` could save (2026-11-10)

Baseline `df8e786` (layer 233 taken), result `bddcd5e`. Everything here is local mock
traffic on this host: nothing contacted Telegram and no credential was used. Rust 1.99.1,
4-core KVM guest (Ryzen 9 7950X3D), Linux 5.15, 5.9 GB RAM, shared with other services.
Method and statistics are those of [the 2026-11-09 round](../2026-11-09/README.md): changes
are medians of paired percentage changes with unadjusted paired bootstrap 95% intervals,
and a change whose interval includes zero is reported as no measured change.

Three things were measured:

1. Whether taking TL layer 233 costs anything on the update path:
   [no measured change](layer233/README.md).
2. The typed `Client` path with updates, where the clock turned out to be read four times
   for every batch of updates: fixed, **+14% requests per second at saturation**.
3. `io_uring` for the socket path, as an upper bound before building anything: **not
   built**, the numbers are below.

## The update path read the clock four times per batch

The 2026-11-09 round removed clock reads from the request path, because on this host
`clock_gettime` is a real system call of about 270 ns (the guest's `kvm-clock` is not
served from the vDSO). It measured the many-session raw path, which has updates off. The
typed path with updates was not counted then.

Counted now on the client thread of the bot workload with
[`sysc.c`](../2026-11-09/tools/sysc.c), and attributed by sampling the callers of
`clock_gettime` one call in 251 with [`clockbt.c`](tools/clockbt.c)
([before](update-clock/clock-reads-base.txt), [after](update-clock/clock-reads-final.txt)).
The harness reads the clock twice per request for its own latency figure, which gives the
scale: its share of the samples was 30.8% before and 57.7% after.

| Monotonic reads per request | `df8e786` | `bddcd5e` |
|---|---:|---:|
| The harness's own | 2.0 | 2.0 |
| `MessageBoxes::check_deadlines`, each turn of `UpdatesReceiver::next` | 1.0 | |
| `recovery_paused_until` compared with the clock, no pause being set | 1.0 | |
| The deadline `MessageBoxes::process_updates` gives the sequences it applies to | 1.0 | |
| `StoredPeers::forget_if_old` from the update stream's `cache_peer_map` | 1.0 | |
| One reading for the receiver's turn, shared by the first and third above | | 1.0 |
| `forget_if_old` from `send_message` (a full reply, one request in four) | 0.25 | 0.25 |
| The sender's write and the runtime's timer | 0.2 | 0.2 |
| All | 6.5 | 3.5 |

One batch of updates per request is this workload's shape (it delivers 1.25 updates per
request); a bot that receives more batches than it sends requests pays per batch.

What changed, in `bddcd5e`:

- The receiver reads the clock once for a turn of its loop and uses that reading for the
  deadline check and for the batch it applies. The reading is dropped at the start of a
  turn and after everything in it that can wait (a request, a session call, the wait for
  the next batch), so no step sees a time from before a wait.
- A recovery pause is an `Option`: it is compared with the clock only while one is set.
- The update stream ages the table of stored peers with the receiver's reading. A peer's
  lifetime still starts at the clock, and other callers of `cache_peer_map` read it.

### One busy bot connection with updates

| Scenario | Metric | `df8e786` | `bddcd5e` | Change |
|---|---|---:|---:|---:|
| Closed loop, 64 callers, 10 rounds | RPC/s | 160,315 | 183,427 | **+14.09%** [+12.26, +16.00] |
| | CPU µs/RPC | 6.24 | 5.45 | **-12.35%** [-13.80, -11.04] |
| | user / kernel µs/RPC | 4.29 / 1.97 | 4.04 / 1.42 | **-4.11%** / **-28.71%** |
| | p50 / p95 / p99 µs | 387 / 463 / 520 | 338 / 410 / 463 | **-13.00%** / **-10.26%** / **-9.45%** |
| | Allocations per RPC | 21.44 | 21.44 | none |
| | Steady RSS KiB | 6,336 | 6,450 | **+1.23%** [+0.69, +3.22] |
| | Live heap B | 496,176 | 499,936 | **+0.66%** [+0.36, +1.32] |
| Open loop, 5,000 RPC/s, 8 rounds | CPU µs/RPC | 36.42 | 35.55 | -1.76% [-4.96, +2.02] |
| | p99 µs | 2,032 | 2,089 | -0.02% [-3.91, +5.83] |
| | Steady RSS KiB | 5,812 | 6,066 | **+4.25%** [+2.42, +7.65] |
| | Live heap B | 333,488 | 320,528 | -4.13% [-6.65, +1.92] |
| Open loop, 500 RPC/s, 12 rounds | CPU µs/RPC | 85.69 | 85.59 | +0.39% [-3.85, +1.05] |
| | p99 µs | 1,557 | 1,712 | +9.07% [-11.19, +31.76] |
| | Steady RSS KiB | 4,964 | 4,918 | -2.00% [-7.28, +4.41] |
| | Live heap B | 259,656 | 259,600 | -0.06% [-0.39, +0.17] |

Logs: [closed](update-clock/bot-closed64), [5,000](update-clock/bot-open5000),
[500](update-clock/bot-open500).

Stated as they are:

- **The gain is at saturation.** The open loops show no measured change in CPU or latency.
  Three reads are 0.8 µs, about 2% of a request at those rates, which is under their noise;
  what costs there is the runtime waking tasks across two workers, as before.
- **It belongs to this class of host.** Where the kernel serves the clock from the vDSO a
  read costs a few nanoseconds and this change is not measurable. Check
  `clocksource0/available_clocksource` for `tsc` with the provider: that would remove the
  remaining reads for every process on the machine.
- **Memory.** The change adds two `Option<Instant>` to a receiver, 32 B. Live heap is
  +0.66% in the closed loop (3.8 KiB, with 14% more requests in flight per second) and
  unchanged in the open loops. RSS moved +1.2%, +4.3% and -2.0% across the three scenarios
  and -1.9% in the layer 233 check on identical code paths: it is flagged where the
  interval excludes zero, and nothing in the change accounts for a quarter of a megabyte.
- The measured build and `bddcd5e` differ by a documentation comment and tests.

The many-session raw path has updates off, runs none of this code, and was not re-measured.

### Left on this path

| Candidate | Finding | Decision |
|---|---|---|
| The receiver's one reading per batch | Needed to notice a 0.5 s gap deadline while batches keep coming. Skipping it when only 15-minute deadlines are armed would save 0.27 µs per batch here | Not done: it makes deadline detection depend on which deadlines are armed |
| `forget_if_old` from `send_message` | 0.25 reads per request. No caller there has a recent reading, and reusing the update stream's would never forget for a client that stopped streaming | Left exact |
| `update::Message` cloning its message (3,661 B and 1.42 allocations per request in the 2026-10-31 profile) | `update::Message::raw` and `message::Message::raw` are both public owned fields holding the same message | Needs a public type change: not done |
| A send's reply decoded twice, once for the caller and once for the update stream (`process_own_update` 8.96% inclusive on 2026-10-31) | The two consumers each own a decoded value; `Deserialization::OwnUpdate` and `UpdatesLike` are public and own theirs | Needs a public type change: not done |
| The request decoded again for `updateShortSentMessage` | `UpdatesLike::ShortSentMessage` carries the typed request | Needs a public type change: not done |
| `home_dc_id` session lock and the two allocations per typed call | Route and enqueue were 1.3% of client CPU on 2026-11-09 | Left alone |

The three that need a public type change are the same three the 2026-10-31 and 2026-11-09
rounds left. They stay the owner's decision.

## `io_uring` for the socket path: the ceiling, measured before building

`send` and `recv` were 46% of the raw path's client CPU on 2026-11-09, one of each per
request. Most of that is the kernel's TCP work, which `io_uring` does not remove: what it
can remove is the entry into the kernel, by submitting many operations in one call. Before
writing a second I/O backend for the sender, [`uring_ceiling.c`](tools/uring_ceiling.c)
measures the most that could save on this kernel: a C loop with no runtime, no wakers and no
library around it, so that only the kernel interface differs.

One process: a server thread answers loopback TCP connections, the client thread keeps
`inflight` requests in flight over `conns` connections, a 270 B request and a 1592 B
reply, as in the many-session scenario. `--work-iters` spends the same user CPU per reply
in every mode, standing in for decryption and the library. Client pinned to CPU 0, server
to CPU 2, 3 s windows, pairs in alternating order
([`compare-uring-ceiling.py`](tools/compare-uring-ceiling.py)).

- **epoll**: the library's pattern today. One `send`, one `recv` on readiness, all events
  of one `epoll_wait` handled before the next.
- **ring**: `IORING_OP_SEND` and `IORING_OP_RECV` queued for every request, one
  `io_uring_enter` submitting everything queued and waiting.
- **ring, recv only**: `send` called directly, only the `recv` on the ring.
- **ring, waiting past sends**: as the first, waiting for one completion more than the
  sends submitted, so that a call does not return holding only those.

With 6 µs of work per reply (`--work-iters 4700`) the epoll side is at 12.4 µs and 80,700
requests per second, 0.068 `epoll_wait` per request: the library's own figures on
2026-11-09 were 11.55 µs, 86,054 and about 0.1. That is the row to read.

| Shape | Variant, against epoll | Pairs | RPC/s | Client CPU per request | p99 | Calls into the kernel per request |
|---|---|---:|---:|---:|---:|---:|
| 64 connections, 16 in flight, 6 µs work | ring | 12 | +0.18% [-1.14, +4.05] | -0.11% [-3.91, +1.27] | -2.01% [-3.03, +1.80] | 2.07 to 0.063 |
| | ring, recv only | 12 | +3.78% [-0.01, +7.63] | -3.62% [-7.09, +0.03] | **-5.47%** [-7.64, -1.06] | 2.07 to 1.063 |
| | ring, recv only | 24 | **+4.64%** [+2.99, +6.92] | **-4.49%** [-6.39, -2.97] | -2.77% [-7.13, +0.24] | 2.07 to 1.063 |
| | ring, waiting past sends | 12 | +1.06% [-4.25, +3.98] | -1.10% [-3.84, +4.39] | -3.14% [-6.51, +5.49] | 2.07 to 0.063 |
| 256 connections, 64 in flight, 6 µs work | ring | 12 | **+9.21%** [+4.87, +13.69] | **-8.45%** [-11.95, -4.74] | **-8.86%** [-12.88, -2.84] | 2.02 to 0.016 |
| | ring, recv only | 12 | **+5.78%** [+2.21, +10.46] | **-4.31%** [-9.38, -1.58] | -4.43% [-15.21, +2.92] | 2.02 to 1.016 |
| 64 connections, 16 in flight, no work | ring | 12 | -14.25% [-18.66, +8.51] | +18.05% [-6.74, +24.04] | **+29.30%** [+12.46, +34.96] | 2.27 to 0.22 |
| | ring, recv only | 12 | **-14.70%** [-21.77, -4.34] | **+18.14%** [+5.47, +28.80] | **+12.53%** [+9.05, +23.43] | 2.25 to 1.06 |
| | ring, waiting past sends | 12 | **-16.63%** [-21.85, -2.99] | **+20.62%** [+4.53, +28.76] | **+35.37%** [+23.82, +53.71] | 2.26 to 0.22 |

[Runs and summaries](uring-ceiling).

What that says:

- **At the library's own shape the ceiling is 4.5% of client CPU**, and only for the
  variant that leaves `send` alone. Putting both on the ring makes a thirtieth of the
  calls and saves nothing measurable.
- **Four times the connections per thread raises it to 8.5%**, with both on the ring.
- **A client that does nothing but I/O loses 18 to 21%.** On 5.15 a completion for a
  running thread is delivered as task work signalled with an interrupt from the server's
  CPU, and a `recv` that has to wait is attempted twice. With both operations on the ring
  that is 0.23 to 0.38 rescheduling interrupts per request, where epoll has none. The
  calls saved are cheaper than that.
- Later kernels changed exactly this (`IORING_SETUP_COOP_TASKRUN` and buffer rings in
  5.19, multishot `recv` in 6.0, `DEFER_TASKRUN` in 6.1). **This is a ceiling for 5.15
  and says nothing about them.**

Why nothing was built on it:

- The ceiling is what a loop with nothing around it gets. In the library a ring would sit
  under a runtime it does not own: completions wake a driver task that wakes the
  connection's task, and submissions have to be gathered without a hook before the thread
  parks. Each of those spends part of 4.5%.
- A `recv` on the ring holds its buffer while it waits. Today a connection holds a socket
  buffer only while a packet is in it (5,959 B per connected session, 2026-11-09); a
  committed 4 KiB per waiting connection would be +69%. The kernel's older provided
  buffers avoid that at the cost of another operation per reply.
- It would be a second I/O backend in the sender, with the ring mapped and its entries
  written through raw pointers, in a workspace whose only `unsafe` is the call into AES
  after runtime detection; with cancellation that has to outlive a dropped read; and with
  TLS and proxied streams staying on the first.
- Container runtimes commonly forbid `io_uring` in their default seccomp profile, so the
  first backend has to stay and be the fallback whatever is built.

So: `io_uring` is not adopted, on a measurement and not on a guess, and nothing in the
library changed for it. The tool stays, so that the same question can be asked again in a
minute on another kernel, or for another shape:

```sh
gcc -O2 -Wall -Wextra -pthread -o uring_ceiling benchmarks/2026-11-10/tools/uring_ceiling.c
./uring_ceiling --calibrate --pin 0,2        # nanoseconds per work iteration
python3 benchmarks/2026-11-10/tools/compare-uring-ceiling.py ./uring_ceiling \
    --variant uring-send-inline --pairs 24 --seconds 3 --pin 0,2 --work-iters 4700 --out <dir>
```

What the benchmark cannot tell is written at the top of the C file: it is loopback, a
closed loop at full speed, one kernel, and no library code runs in it.

## What is left, by expected gain

1. **The host's clock.** After this round the typed path still reads it about 1.5 times
   per request and the raw path about 2.5 times, 270 ns each. A host with `tsc` as its
   clock source removes that for everything on it, without code.
2. **The three typed-path items that need a public type change**, above.
3. **`io_uring`, on a newer kernel only**, and only after the ceiling there says more than
   it does here.
4. **More than one request per packet per session**, the application's choice, as on
   2026-11-09.

## API changes and consumers

Nothing that compiled before stops compiling because of this round's performance work.
The layer has its own [compatibility note](../../CHANGELOG.md).

| Change | Kind |
|---|---|
| `MessageBoxes::check_deadlines_at`, `MessageBoxes::process_updates_at` | New methods; the ones without a time read the clock and call them |
| `UpdatesReceiver::last_clock_read` | New method |

## Reproduction

Build one step at a time with bounded parallelism on a small host.

```sh
# For the old revision: `git archive <rev> | tar -x -C <dir>` and build there.
nice cargo test --release --locked -j 2 -p grammers-client --features session-file --test bot_workload --no-run
python3 scripts/compare-bot-workload.py --binary base=<old> --binary final=<new> --rounds 10 --scenario closed64 --output <dir>
python3 scripts/compare-bot-workload.py --binary base=<old> --binary final=<new> --rounds 8 --scenario open5000 --output <dir>
python3 scripts/compare-bot-workload.py --binary base=<old> --binary final=<new> --rounds 12 --scenario open500 --output <dir>

# Who reads the clock: one call in 251, per thread.
gcc -O2 -shared -fPIC benchmarks/2026-11-10/tools/clockbt.c -o clockbt.so -ldl
CLOCKBT_OUT=$PWD/bt.out LD_PRELOAD=$PWD/clockbt.so BOT_CONCURRENCY=64 BOT_CLIENT_CPUS=0 \
    <bot_workload binary> --exact bot_workload --ignored --nocapture --test-threads=1
python3 benchmarks/2026-11-10/tools/clockbt-summary.py bt.out.<pid of the client> <bot_workload binary>
```

## Validation

On `bddcd5e`, one crate at a time, `-j 2`, each command under a 4 GB address-space limit:

- `cargo fmt --all -- --check`.
- Release tests, with every feature: `grammers-session` 109 passed (4 ignored), plus its
  30 integration tests and 3 doctests; `grammers-mtsender` 184 passed (8 ignored) in the
  library, its integration tests and doctests having passed before the last test was
  strengthened; `grammers-client` 125 passed, and 69 doctests.
- Strict Clippy (`--all-targets --all-features -D warnings`) and warning-free Rustdoc for
  those three crates.

On `df8e786`, for the layer: the same for `grammers-tl-parser` (39), `grammers-tl-gen`
(28 and 8), `grammers-tl-types` (13, with the size guards, and 24 doctests),
`grammers-session`, `grammers-mtproto` (121), `grammers-mtsender` and `grammers-client`;
strict Clippy on those and on `grammers`; Rustdoc for `grammers-tl-gen`,
`grammers-session` and `grammers-client`.

Not run:

- `grammers-tl-types` with `impl-serde` (every feature): its build passes the 4 GB limit
  on this host, aborting at 3.1 GB resident even as a type-check. No crate of the workspace
  enables it. The shape the generator emits for the boxed field was compiled on its own.
- Rustdoc for `grammers-tl-types`, and the documentation generator's offline tests, which
  need a Python module this host does not have.
- The whole workspace in one command, by the host's rule.
- Anything against Telegram, and any consumer of the fork.

The layer and the clock change were each read by a fresh reviewer before they were pushed.
It found no defect in either; its findings were test corrections, missing test cases and
wording, and are in the commits.
