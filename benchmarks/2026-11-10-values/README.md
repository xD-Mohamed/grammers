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

- [ ] 1.1 `grammers-tl-types` with `impl-serde` as one unit: `cargo check`, with peak memory
- [ ] 1.2 The same unit under Clippy
- [ ] 1.3 Windows and macOS: `cargo check --target` on the real targets, crate by crate
- [ ] 1.4 Ignored tests: every one listed with what it needs
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
