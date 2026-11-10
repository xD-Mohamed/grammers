# RSS accounting in the bot workload

The harness formerly read steady RSS from `/proc/self/status`'s `VmRSS`. Linux
updates that counter asynchronously. Its documentation recommends the page-table
statistics in `smaps` for a more accurate snapshot; `smaps_rollup` sums those fields.
See [the kernel's proc documentation](https://docs.kernel.org/6.12/filesystems/proc.html).

`proc_status_kib("VmRSS")` now reads `Rss:` from `/proc/self/smaps_rollup`. The
high-water field still comes from `VmHWM` and remains approximate: it may even be
less than the page-table steady reading, so it is not an exact peak.

The read is after the CPU, context-switch and latency snapshots, and before
allocation accounting begins. Both binaries in a comparison use the same helper.
It scans live mappings, rather than freezing all workload threads into a globally
atomic snapshot. Total RSS includes resident code and shared libraries as well as
anonymous memory. An unchanged allocator heap does not by itself disprove an RSS
increase.

The helper allocates temporary text storage and can briefly stall the current
worker. Its allocation cost is outside both accounting windows, but cache state and
backlog can carry into the next window. Accordingly, old and corrected runs are
kept separate, and small heap shifts are also compared with tcache disabled.

This measurement fix does not establish the cause of the older peers-step RSS
increase. That needs its original pair, `7e79532` to `482a4ef`, or a separately
documented bounded diagnostic of those binaries.

## Validation of the harness fix

Validated against `59069f3` with only this helper changed, in a tracked-source
scratch copy (including the shared `includes` directory). One command at a time,
`-j 1`, nice/ionice, 4,000,000 KiB virtual-memory cap and the memory watchdog:

- `cargo fmt --all -- --check` passed.
- `cargo test --locked --release -j 1 -p grammers-client --all-features`: 132 unit
  tests, 2 API tests, 1 dependency test and 69 doctests passed; 4 ignored.
- `cargo clippy --locked -j 1 -p grammers-client --all-features --all-targets -- -D warnings` passed.
- `RUSTDOCFLAGS='-D warnings' cargo doc --locked -j 1 -p grammers-client --all-features --no-deps` passed.
- The default-plus-session-file bot workload built and ran once against its separate
  local mock, with a one-second CPU window and half-second warm/allocation windows.
  It exercises the new proc read; zero RPC errors and no lost updates.

[Full logs and watchdog summaries](validation/rss-harness). The release test command
peaked at 685 MiB in one process, 784 MiB across the command; available memory stayed
at or above 1,540 MiB, swap grew 5 MiB. The initial scratch verification omitted
`includes`; it was corrected before the passing run. No Telegram traffic or consumer
access was involved.
