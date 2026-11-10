# The typed client path, and what earlier rounds had not verified (2026-11-10)

Baseline `4458f8d`. Everything here is local mock traffic on this host: nothing contacted
Telegram and no credential was used. Rust 1.99.1, 4-core KVM guest (Ryzen 9 7950X3D),
Linux 5.15, 5.9 GB RAM, shared with other services. Method and statistics are those of
[the 2026-11-09 round](../2026-11-09/README.md): changes are medians of paired percentage
changes with unadjusted paired bootstrap 95% intervals, and a change whose interval
includes zero is reported as no measured change.

The owner approved breaking public types for the typed path in this round. Each break is
in [CHANGELOG.md](../../CHANGELOG.md) with what to write instead.

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
  updates per request, a quarter of them the request's own. The clone was per update.
- **"Live heap" moved +0.43% [+0.27, +0.76] in the closed loop and +0.69% [+0.31, +1.08]
  at 500 per second, about 2 KiB, and that is the allocator's thread cache.** The harness
  reads `mallinfo`, which counts freed chunks parked in that cache as in use. With the
  cache off (`GLIBC_TUNABLES=glibc.malloc.tcache_count=0`,
  [6 rounds](update-message/bot-open500-no-thread-cache)) the figures are 212,848 B and
  212,816 B, +0.00% [-2.96, +1.26]. The change itself holds one message where there
  were two.
- The measured build and `7bd960f` differ by tests, a test-only accessor and comments.

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

## Reproduction

Build one step at a time with bounded parallelism on a small host.

```sh
# For the old revision: `git archive <rev> | tar -x -C <dir>` and build there.
nice cargo test --release --locked -j 2 -p grammers-client --features session-file --test bot_workload --no-run
python3 scripts/compare-bot-workload.py --binary base=<old> --binary final=<new> --rounds 30 --scenario closed64 --output <dir>
python3 scripts/compare-bot-workload.py --binary base=<old> --binary final=<new> --rounds 8 --scenario open5000 --output <dir>
python3 scripts/compare-bot-workload.py --binary base=<old> --binary final=<new> --rounds 12 --scenario open500 --output <dir>

# The profile: benchmarks/2026-10-31-bot/profile/mkprof.py and prof_patch.py on a scratch copy.

# impl-serde in parts.
python3 benchmarks/2026-11-10-typed/tools/serde_parts.py . <scratch dir>
```
