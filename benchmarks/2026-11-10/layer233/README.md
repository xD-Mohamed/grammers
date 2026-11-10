# Layer 233 against layer 230 on the update path (2026-11-10)

Local mock traffic on this host only: nothing contacted Telegram and no credential was
used. Same host, method and statistics as [the 2026-11-09 round](../../2026-11-09/README.md):
medians of paired percentage changes with unadjusted paired bootstrap 95% intervals, and a
change whose interval includes zero is no measured change.

The question was whether taking the schema costs anything where `Update` values move:
`Update` is 760 B instead of 752 B, and the schema added fields to other types on that
path (`MessageReplyHeader.quote_rich`, `Channel.pinned_msg_id`).

`base` is the build of `27a3f75` that the 2026-11-09 round measured as its result (the
same executable, by hash); `final` is the layer 233 tree. The harness differs by the new
fields in its two mock values, which are unset and put the same bytes on the wire.

## One busy bot connection with updates, closed loop, 64 callers, 10 rounds

| Metric | Layer 230 | Layer 233 | Change |
|---|---:|---:|---:|
| RPC/s | 156,969 | 156,560 | +0.54% [-2.07, +3.49] |
| CPU µs/RPC | 6.37 | 6.37 | -0.23% [-2.52, +2.09] |
| p50 / p95 / p99 µs | 397 / 479 / 545 | 397 / 475 / 537 | +0.00% / -0.84% / -0.74%, each interval includes zero |
| Allocations per RPC | 21.44 | 21.44 | none |
| Bytes allocated per RPC | 16,657 | 16,838 | **+1.13%** [+0.15, +1.97] |
| Live heap B | 486,968 | 496,376 | **+1.95%** [+0.74, +2.59] |
| Steady RSS KiB | 6,510 | 6,330 | **-1.86%** [-3.56, -0.25] |

No measured change in throughput, CPU or latency. The same number of allocations are each
a little larger, about 180 B per request in all, which is the schema's larger types. The
lower RSS is not explained by anything in the change and is not claimed.

[Logs and summary](bot-closed64).

```
nice cargo test --release --locked -j 2 -p grammers-client --features session-file --test bot_workload --no-run
python3 scripts/compare-bot-workload.py --binary base=<old> --binary final=<new> --rounds 10 --scenario closed64 --output <dir>
```

The many-session raw path does not decode updates and was not re-measured for this change.
