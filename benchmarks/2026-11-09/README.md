# Request and reply path review (2026-11-09)

Baseline `dfb8e25`, result `6cd992d`. Everything here is local mock traffic on this host:
nothing contacted Telegram and no credential was used. Rust 1.99.1, 4-core KVM guest
(Ryzen 9 7950X3D), 5.9 GB RAM, shared with other services. Changes are medians of paired
percentage changes with unadjusted paired bootstrap 95% intervals; a change whose interval
includes zero is reported as no measured change.

The workload measured first is the many-session pattern: updates off, raw prepared
requests through `start_raw_invoke_shared_in_dc_tracked`, one request in flight per session. That is
[`loopback.rs`](../../grammers-mtsender/tests/loopback.rs). The busy single bot connection
with updates is [`bot_workload.rs`](../../grammers-client/tests/bot_workload.rs). Neither
harness file changed, so both revisions ran the identical harness.

## Result

### Many sessions, raw path (64 sessions, 16 callers, one thread, 14 pairs)

Request about 270 B. [Logs](loopback).

| Metric | `dfb8e25` | `6cd992d` | Change |
|---|---:|---:|---:|
| Plain reply 1592 B: RPC/s | 65,759 | 86,054 | **+32.77%** [+29.15, +37.41] |
| CPU µs/RPC | 15.17 | 11.55 | **-24.82%** [-27.40, -22.96] |
| user / kernel µs/RPC | 6.79 / 8.36 | 4.39 / 7.04 | **-35.67%** / **-16.93%** |
| p50 / p95 / p99 µs | 214 / 399 / 449 | 181 / 296 / 353 | **-19.23%** / **-26.01%** / **-21.39%** |
| Gzip reply 528 B: RPC/s | 70,896 | 84,906 | **+20.64%** [+16.01, +24.86] |
| CPU µs/RPC | 14.10 | 11.70 | **-17.28%** [-19.87, -13.76] |
| p99 µs | 433 | 363 | **-14.93%** [-22.14, -12.81] |
| Process RSS KiB | 11,508 | 11,506 | -0.03% [-0.19, +0.12] |

### Heap per session (400 sessions, deterministic census)

| Stage | `dfb8e25` | `6cd992d` | Change |
|---|---:|---:|---:|
| Pool created, not connected | 2,143 B | 2,143 B | none |
| Connected after one RPC | 7,951 B | 5,959 B | **-25.1%** |
| After 1100 replies (replay history full) | 14,071 B | 12,079 B | **-14.2%** |

### Fairness under overload (64 pools, one thread, 120,000 RPC/s offered, 12 pairs)

Required by AGENTS.md for any read-buffer change. [Logs](overload).

| Metric | Change |
|---|---:|
| Completed | **+22.31%** [+19.36, +27.02] |
| Busy p99 | **-19.35%** [-25.08, -9.41] |
| Quiet p99 | **-13.81%** [-26.78, -6.81] |
| Process RSS | -1.66% [-2.93, +2.61] |

Both sides improved; no trade was made between them.

### One busy bot connection with updates

| Scenario | Metric | `dfb8e25` | `6cd992d` | Change |
|---|---|---:|---:|---:|
| Closed loop, 64 callers, 10 rounds | RPC/s | 134,293 | 149,831 | **+11.54%** [+7.79, +13.88] |
| | CPU µs/RPC | 7.45 | 6.67 | **-10.29%** [-12.13, -7.86] |
| | p99 µs | 627 | 573 | **-8.00%** [-15.19, -4.67] |
| | Steady RSS KiB | 6,374 | 6,482 | **+0.94%** [+0.03, +2.56] |
| | Live heap B | 490,336 | 490,784 | +0.67% [-0.76, +1.41] |
| Open loop, 5,000 RPC/s, 8 rounds | CPU µs/RPC | 39.64 | 39.35 | -0.25% [-3.70, +2.33] |
| | p99 µs | 2,048 | 2,032 | -2.33% [-3.86, +0.82] |
| | Live heap B | 301,960 | 316,688 | **+4.51%** [+3.65, +6.12] |
| Open loop, 500 RPC/s, 12 rounds | CPU µs/RPC | 94.69 | 91.11 | -2.53% [-4.69, +0.52] |
| | p99 µs | 1,843 | 1,720 | -8.83% [-15.94, +8.88] |
| | Live heap B | 248,464 | 258,584 | **+3.82%** [+3.64, +4.73] |

Logs: [closed](bot-closed64), [5,000](bot-open5000), [500](bot-open500).

Two things here are not gains and are stated as such:

- **Open loops show no measured CPU or latency change.** At these rates the cost is the
  runtime waking tasks across two workers, as the 2026-10-31 round found.
- **A process with one connection holds 10 to 15 KiB more heap** (and about 100 KiB more RSS
  in the closed loop). That is the per-thread spare buffers below: a fixed cost per runtime
  thread, repaid once a thread drives a handful of connections.

An earlier run of the 500 RPC/s scenario flagged p50 +35.64% [+7.54, +38.30] (six rounds,
with the 16 KiB spare; its output was not kept). It did not reproduce: in a [second run of three builds](steps/bisect-open500/summary.json) the baseline's
own p50 moved from 823 to 1,204 µs and the difference was +5.25% [-17.47, +28.22], and the
twelve rounds above give -10.29% [-22.24, +13.40]. That latency is quantized by the harness's
1 ms arrival timer and no change is established in either direction.

## The path of one request and its reply

Raw tracked call on a warm connection, as counted and profiled at `dfb8e25`.

| Stage | Where | Per request |
|---|---|---|
| Caller | `SenderPoolHandle::start_invoke_shared`, `try_route` | Size check, tracker CAS, one `oneshot` allocation, five lock operations on per-pool locks, one `Bytes` refcount. The typed `Client` path adds a session lock (`home_dc_id`), a `Vec` and a `Bytes` allocation. |
| Hand-off | `queue.rs` to `run_sender` | One task wake-up, one queue lock, up to eight queued requests drained per turn, each charged to the scheduler budget. |
| Framing | `Sender::try_fill_write`, `Encrypted::push`/`finalize` | Body copied once into the write buffer; acknowledgements and a container added. **Three wall-clock reads** (request, acknowledgement and container identifiers), and a monotonic one per request once future salts are held, which the mock does not send. |
| Encrypt | `encrypt_data_v2` | **One `getrandom` call**, SHA-256 of the plaintext, two SHA-256 for the key, AES-IGE in place. |
| Write | `Sender::step` | One `send`. **Two monotonic reads** (idle-buffer check, tracked send time). |
| Read | `Sender::step`, `on_net_read` | One `recv` into the connection's buffer, one `epoll_wait` per about twelve requests at this load. **One monotonic read** for the idle-buffer check. |
| Decrypt | `decrypt_data_v2` | Two SHA-256 for the key, AES-IGE in place, then SHA-256 of the plaintext as a second pass. |
| Dispatch | `Encrypted::process_bounded`, `Sender::process_result` | Replay check, **one monotonic read** to start the acknowledgement delay, reply copied into one `Vec` (the second allocation), linear match against pending requests. |
| Idle step | `Sender::step` again | **One monotonic read** re-checking the acknowledgement delay. |
| Reply | `Request::complete` | Permit released, `oneshot` send, one task wake-up. |
| Updates, storage | `run_sender`, `updates_receiver.rs`, message box, session | Not on this path: updates are off. Measured in the 2026-10-31 round and unchanged here. |

A pool with one connection is two tasks and one timer; nothing is shared between sessions
on this path except the runtime thread.

## What the profile showed

Client thread, [samples](profile) at the 250 Hz the kernel's timer allows, and
[system calls counted natively](tools/sysc.c) per thread.

| Where | `dfb8e25` share | µs/RPC | `6cd992d` share | µs/RPC |
|---|---:|---:|---:|---:|
| AES-IGE and SHA-256 | 33.4% | 5.1 | 22.8% | 2.6 |
| `send` | 20.3% | 3.1 | 34.7% | 4.0 |
| `clock_gettime` | 18.2% | 2.8 | 9.9% | 1.1 |
| Library and runtime code outside crypto | 14.6% | 2.2 | 17.2% | 2.0 |
| `recv` | 8.8% | 1.4 | 11.4% | 1.3 |
| `getrandom` | 2.3% | 0.35 | 1.1% | 0.1 |
| `epoll_wait` | 1.3% | 0.2 | 1.8% | 0.2 |
| Allocator | 1.2% | 0.2 | 1.0% | 0.1 |

| System calls per request | `dfb8e25` | `6cd992d` |
|---|---:|---:|
| `send` / `recv` | 1.00 / 1.00 | 1.00 / 1.00 |
| `clock_gettime(CLOCK_REALTIME)` | 3.00 | 1.00 |
| `clock_gettime(CLOCK_MONOTONIC)` | 6.35 | 2.44 |
| `getrandom` | 1.00 | 0.06 |
| `epoll_wait` | 0.08 | 0.10 |

About one of the monotonic reads is the harness's own loop and 0.2 the runtime's timer.

**The clock is a system call on this host.** `clock_gettime` costs about 270 ns here against
4 ns for a clock the kernel serves from the vDSO: the guest's `kvm-clock` falls back to a
real call, and `tsc` is not offered. Nine reads per request were 18% of the client's CPU. On
a host with a working vDSO clock the same reads cost about a tenth of that, so the
clock part of the gain is this class of host's. The crypto and memory parts are not.

`send` costs more per call in the result than in the baseline. The mock is on the same
machine and a loopback `send` does the receiver's work inline; with less else to do the
client finds the mock's thread asleep more often. That is the harness, not the library.

## What changed, in order

Each step was measured against the build before it with the many-session scenario, 12
pairs. [Summaries](steps).

### 1. AES-IGE on AES-NI directly (`4ba555e`)

IGE is one serial chain, so its speed is the latency of that chain. The cipher crate's
block interface moved every block between vector and general registers on it. A block now
stays in one vector register, with the XORs that do not depend on the previous block
folded into the first and last round keys. Runtime detection, portable fallback kept.

| Buffer | Before | After |
|---|---:|---:|
| 1600 B decrypt / encrypt | 2,931 / 2,822 ns | 1,226 / 1,218 ns |
| 16 KiB decrypt | 24.3 µs | 12.5 µs |
| 1 MiB decrypt / encrypt | 1,473 / 1,467 µs | 804 / 805 µs |

### 2. Padding randomness sixteen messages at a time (`b568278`)

One `getrandom` call (326 ns) per packet became one per sixteen. Same generator, each byte
used once and cleared, pool owned by the thread.

### 3. Clock reads (`375fbb7`)

One wall-clock read per packet instead of per message; idle-buffer release judged at
keepalive ticks from flags that reads and writes set; the acknowledgement delay left to
the timer the sender already arms; the next salt examined when the clock has moved 16 s;
one sent time per packet.

Steps 1 to 3 together: plain **+24.04%** [+22.77, +26.50] RPC/s, CPU **-19.82%**
[-21.06, -18.61], p99 **-21.08%** [-23.09, -16.17]; gzip **+13.15%** [+11.53, +18.49], CPU
**-11.63%** [-15.62, -10.51]. They were not measured apart at the workload level; the
component figures are in each commit.

### 4. Socket buffers only while a packet is in them (`ebe1550`, `6cd992d`)

A connection kept a 1 KiB read buffer and a 1.1 KiB write buffer for life, and a busy one a
16 KiB read buffer. They are now spares of the runtime thread: a read waits for the socket
without a buffer and takes a 4 KiB spare once there is data; a packet is built in a spare
and returns it once written. A partial packet stays with its connection, and a connection
whose reads fill the spare grows its own buffer to 16 KiB as before.

Heap per session as in the table at the top. Against the previous step: RPC/s +0.04%
[-2.00, +2.14], CPU +0.00% [-2.01, +2.32], plain p95 **+2.52%** [+1.08, +3.99], gzip p99
**+3.11%** [+0.07, +5.07], the other four latency intervals include zero. A small cost in
the tail may be real; the final figures above include it.

The spare was first 16 KiB. That cost a one-connection process on two workers 34 KiB
(**+13.43%** [+12.48, +14.15] live heap), so it became 4 KiB (`6cd992d`).

### 5. Decrypt and authenticate in one pass (`bc59f61`)

Decryption and the SHA-256 that authenticates the plaintext are two serial chains that do
not need each other's result for the same 64 bytes. Issued together, with the hash one
block behind, the processor works on both at once.

| Buffer | `dfb8e25` | After step 1 | Together |
|---|---:|---:|---:|
| 1600 B decrypt and hash | 3,027 ns | 1,899 ns | 1,441 ns |
| 16 KiB | 29.9 µs | 19.4 µs | 14.4 µs |
| 1 MiB | 1,919 µs | 1,257 µs | 934 µs |

A complete outgoing packet of 448 B went from 1,384 to 772 ns and of 1024 B from 2,477 to
1,469 ns. Encryption cannot be joined the same way: its key depends on the finished hash.

### 6. Acknowledgements timed from the write (`aeb4341`)

The read of the clock when a packet is written now also starts the delay of the
acknowledgements its replies need, through the new optional `Mtp::on_packet_sent`. That time
is never later than the true one, so no acknowledgement waits longer than ten seconds.

Steps 5 and 6 together: plain CPU **-6.77%** [-12.87, -1.54], RPC/s **+6.71%**
[+1.53, +14.75]; gzip CPU **-2.64%** [-5.33, -0.41], RPC/s **+2.81%** [+0.43, +5.50].

## Assessed and not changed

| Candidate | Finding | Decision |
|---|---|---|
| Five lock operations in `try_route`, `home_dc_id` lock, second allocation on the typed path | Route and enqueue are 1.3% of the client's CPU; all library code outside crypto is about 10% | Left alone |
| Another allocator | The allocator is 1% here; `mimalloc` measured +143% RSS on 2026-10-31 | System allocator stays the advice |
| Vectored writes | One packet is already one contiguous buffer and one `send` | Nothing to coalesce |
| `TCP_NODELAY`, socket buffer sizes | `TCP_NODELAY` is set; requests and replies are far below the kernel's defaults | Left alone |
| A cheaper result channel than `oneshot` | Two allocations per request in total, 1% of CPU | Left alone |
| SHA-1 | Only in key exchange and login | Not on this path |
| A tick for the acknowledgement delay, with no clock read at all | About 2% on this host; changes the "original ACK deadline" invariant | Not done |
| Message identifiers from the monotonic clock | About 2% on this host; a suspended machine would send stale identifiers until re-anchored | Not done |
| Writing before the `select` in `Sender::step` | Under 2%, and it changes which of read and write goes first | Not done |
| AES for aarch64 | No ARM host to measure on | Portable path stays there |
| TL schema layer 233 | See [UPSTREAM.md](../../UPSTREAM.md) | Deferred |

## What is left, by expected gain for this workload

1. **Batched socket I/O (`io_uring`).** `send` and `recv` are 46% of what remains, one of
   each per request per session. Submitting many in one system call is the only thing that
   reduces them. It means a different reactor under the sender: not attempted.
2. **The host's clock.** About two and a half reads per request are left in the library at
   270 ns each, 6% here.
   A host whose clock the kernel serves from the vDSO removes that for every process on
   it. Check `clocksource0/available_clocksource` for `tsc` with the provider.
3. **More than one request per packet per session.** Requests queued together already
   share one packet, one `send` and one encryption. One in flight per session cannot use
   that. It is the application's choice, and Telegram's limits apply.
4. **The typed `Client` path.** The 2026-10-31 round's open items (a send's reply decoded
   twice, `update::Message` cloning its message) still need API changes.
5. **Heap per session.** Of 12 KiB, 6 KiB is the replay history the protocol requires and
   about 1.4 KiB the connection task.

## API changes and consumers

Nothing that compiled before stops compiling.

| Change | Kind |
|---|---|
| `grammers_crypto::aes::ige_decrypt_hashed` | New function |
| `Mtp::on_packet_sent` | New trait method with a default |
| `Mtp::service_deadline` | Contract documented: a driver that reads it must call `on_service_timeout`. A custom driver of `Encrypted` that reads the deadline and never reports it would now send acknowledgements only with requests. `Sender` reports it. |
| `Definition`, `Parameter`, `ParameterType`, `Type`, `Flag`, `ParseError`, `ParamParseError` | Now `Clone` |
| `grammers-session` | `tokio` is optional, enabled by `sqlite-storage` (default) and `session-import-files` |

Behaviour worth knowing: identifiers of the messages in one packet are consecutive; grown
buffers are released one to two minutes after the last large transfer instead of exactly
one; an acknowledgement can leave earlier than before, never later.

An application pinned to an earlier revision of this fork is unaffected until it moves
its pin, and needs no source change for the commits of this work. Which applications use
the fork, and at which revision, is recorded with the task and not in this repository.

The release profile was not measured in this work: both harnesses use Cargo's default
release profile at both revisions. A consumer's `lto = true`, `codegen-units = 1` and
`target-cpu=native` are untested here, neither confirmed nor contradicted. The AES-NI path
is selected at run time, so it does not depend on `target-cpu`. The system allocator
remains the advice on the strength of the 2026-10-31 measurement. `panic = "abort"` would
remove the unwinding that checked runners rely on to tear down their children.

## Reproduction

Build one step at a time with bounded parallelism on a small host (see below).

```sh
# For the old revision: `git archive <rev> | tar -x -C <dir>` and build there.
nice cargo test --release --locked -j 2 -p grammers-mtsender --test loopback --no-run
nice cargo test --release --locked -j 2 -p grammers-client --features session-file --test bot_workload --no-run

python3 scripts/compare-loopback-r1.py --baseline <old> --candidate <new> --output <dir> --runs 14 --window-ms 2000
python3 scripts/compare-overload.py --baseline <old> --candidate <new> --output <dir> --pairs 12
python3 scripts/compare-bot-workload.py --binary base=<old> --binary final=<new> --rounds 10 --scenario closed64 --output <dir>
<loopback binary> --exact loopback_session_footprint --ignored --nocapture --test-threads=1

# System calls per thread.
gcc -O2 -shared -fPIC benchmarks/2026-11-09/tools/sysc.c -o sysc.so -ldl -lpthread
LD_PRELOAD=./sysc.so LOOPBACK_CLIENT_CPUS=0 LOOPBACK_SERVER_CPUS=2,3 LOOPBACK_PAYLOAD=plain \
    LOOPBACK_SHORT=1 LOOPBACK_WINDOW_MS=3000 <loopback binary> --exact loopback_rpc_cost --ignored --nocapture --test-threads=1

# CPU samples: a scratch copy with pprof added, never part of the library.
python3 benchmarks/2026-11-09/tools/mkprof.py . <scratch>
(cd <scratch> && CARGO_PROFILE_RELEASE_DEBUG=line-tables-only cargo test --release -j 2 -p grammers-mtsender --test loopback --no-run)
LOOPBACK_PROFILE=out.folded LOOPBACK_SHORT=1 LOOPBACK_WINDOW_MS=30000 LOOPBACK_PAYLOAD=plain <scratch binary> --exact loopback_rpc_cost --ignored --nocapture --test-threads=1
python3 benchmarks/2026-11-09/tools/foldstat.py out.folded loopback

# Crypto components.
nice cargo test --release --locked -j 2 -p grammers-crypto --lib -- --ignored --nocapture --test-threads=1 benchmark_
```

Sizes used: 64 sessions and 16 callers for 2 s windows, 400 sessions for the census, 64
pools at 120,000 RPC/s for 1.5 s, one session for the bot scenarios. No harness process
exceeded 14 MiB.

## Validation

At `6cd992d`, Rust/Cargo 1.99.1, Linux x86_64, locked offline dependencies, all local.

| Check | Result |
|---|---|
| `cargo fmt --all --check` | Passed |
| Release workspace tests and doctests, all features | 805 passed, 0 failed, 49 ignored |
| Size and depth guards (`depth_tests.rs`) | Passed, in that run |
| Strict Clippy, all targets and features: crypto, mtproto, mtsender (also without `proxy`), session, tl-parser | Passed |
| Warning-free Rustdoc, all features: the same five crates | Passed |
| Session features at `e6d3f5e`: none / import / string / file import / sqlite / default | 70 / 70+22 / 70 / 90+22 / 83 / 83 passed |
| New tests checked by breaking what they test | AES key schedule (two ways), digest padding and block offset: all caught |

Not run: Clippy and Rustdoc over the crates this work did not touch (client, tl-types,
tl-gen, the command-line crate); the minimal-client feature combinations; the Python
evidence checker, whose inputs did not change. Ignored tests are benchmarks and network
probes, including the test against Telegram, which was not run.

### Building on a small host

`cargo test --release --workspace --all-features` in one command exhausted this machine's
memory (5.9 GB RAM, 4 GB swap). The cause was not isolated; several large test binaries
building and linking at once is the likely one. The test row above comes from such a run
and should not be reproduced that way here. Every other heavy command ran alone, per crate,
with `-j 2`, under `ulimit -v 4000000` and `nice`, after checking for 1.5 GB free.
