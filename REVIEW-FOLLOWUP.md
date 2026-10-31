# Audit follow-up ledger (2026-10-31)

Later the same day: the [bot workload measurements](benchmarks/2026-10-31-bot/README.md), the
[compliance audit](COMPLIANCE-2026-10-31.md) and the [second follow-up](#second-follow-up) below,
which supersede this ledger where they disagree.

Cumulative against the published commit `2ffd991` (tree `6132496e`). Its reviewer
approved that tree with non-blocking findings and explicit deferrals, not as proof
that every discovered issue was closed. This ledger records what happened to each.

## Status of every remaining item

| Item | Status | What now holds |
|---|---|---|
| Rejected imported-session GetState 401 | Fixed | Warns naming `updates.getState`. The docs state that completing login or a successful `get_me()` ends it, and that a revoked key will not recover by waiting. |
| Stale self metadata after logout, QR/re-login | Fixed | A failed recovery RPC, or failing to store its result, pauses retries for about 1 s, so a caller's retry loop is no longer an RPC loop. Live updates, including the push a re-login waits for, keep flowing during the pause. A `GetState` reply replaces any provisional position formed meanwhile. Docs give QR guidance. Service pushes remain non-proof. Cached self metadata is still kept on logout: the session API has no removal method. |
| Missing regressions | Fixed | Second 401 after Rejected then Confirmed propagates; mixed service/account batches give proof; a failed account-kind lookup is really retried. |
| Evidence checker | Fixed | Refuses `-O` and `PYTHONOPTIMIZE`; real process-level tests cover statuses 1 and 2; status 0 is tested by driving `main()` with validation mocked, because a complete pass applies only to the historical evidence tree. Without privacy patterns it says no checks ran. |
| Outside-tree measurement label | Fixed earlier | The external receipt reads "Retained post-qr-correction runs". Historical receipts stay historical. |
| Generated TL recursion (process abort) | Fixed | Decoding rejects values nested beyond 64 boxed levels with `DepthLimitExceeded`. Partial values and siblings drop safely. Deep socket updates become a gap, not a connection failure. |
| Receive-arm-only budget | Measured, rejected | Quiet p99 +38.75% [+29.19, +48.88] without a resolved throughput gain. Replaced by batching charged per request. |
| Missing access hash | Partly fixed | Socket updates received during the grace are retained (512 across channels) and delivered instead of lost. Known bots try the zero-hash form before discarding. **Unresolved:** a user account with no hash and no message reference still loses the backlog from before those updates. |
| Live PTS withheld on unknown channel errors | Partly fixed | Contiguous updates resume, including ones that arrived during the failed attempt. **Intentionally kept:** updates beyond a remaining gap stay withheld while Telegram keeps rejecting recovery, because skipping would silently lose data. Failures warn. |
| Auth import with no target key | Fixed | The key is created first with `help.getNearestDc`, so the import is attributed to a known key. Costs that one extra call only on this path. |
| Saturated heap, retained buffers | Reduced | See below. Saturated heap stays above the historical baseline. |
| Overload capacity and busy tail | Improved | See below. Busy tail stays above the historical baseline. |
| General throughput regression | **Not resolved** | No closed-loop gain is established. Against `7774ed6` the point estimates lean slower: plain -4.23% [-8.23, +4.81], gzip -2.46% [-7.45, +4.01], with intervals crossing zero. Any such loss remains unattributed. |

## New behaviour worth knowing

- A difference reply which cannot be decoded is reported, paced and retried. After 30
  consecutive failures that sequence's backlog is discarded with a warning so that
  updates can resume; this loses those updates and is otherwise a permanent wedge.
  One channel's unreadable reply does not block its siblings.
- A connection releases socket buffers grown beyond 16 KiB on the first transfer after
  60 s without a larger one; keepalive replies provide it within about two minutes. Replay history evicts by count (1024) and by age (300 s of server time, per
  [Telegram's security guidelines](https://core.telegram.org/mtproto/security_guidelines));
  evicted IDs stay rejected through the existing floor. One authenticated ID far in the
  future reduces reorder tolerance to a strict monotonic filter until real time catches up.
- `MessageBoxes` gained `take_released_updates`, `try_exhaust_channel_grace` and
  `discard_account_state`. Direct users that never call the first simply lose
  held updates to eviction, as those updates were previously lost.

## Measured results

[Complete tables and raw logs](benchmarks/2026-10-31/README.md). Against `2ffd991`:

| Metric | Result |
|---|---|
| Plain / gzip RPC/s | +2.59% [-0.89, +9.42] / -0.97% [-7.16, +6.98]: no resolved change |
| Overload completed | +9.27% [+7.63, +10.86] |
| Busy p99 / quiet p99 | -19.07% [-22.30, -14.17] / +2.82% [-5.19, +9.26] |
| Quiet p50 / p95 | +4.74% [+1.67, +5.61] / +5.19% [+3.18, +9.01]: small real cost |
| Heap, 1100 replies in a burst | 16,079 to 14,031 B/session |
| Heap, 1100 replies at one per 10 s | 16,079 to 8,079 B/session |
| One 1 MiB reply, then a small reply 61 s later | 2,104,070 to 7,926 B/session |

Against `7774ed6`, in the same session and with the same harness, overload completion is
+1.27% [+0.18, +6.88] where `2ffd991` measures -6.45% [-8.29, -4.98], and busy p99 is
+96.45% where `2ffd991` measures +142.66%. Quiet p99 is -43.87% and overload process RSS
+6.58%. Burst heap remains 77% above its 7,911 B. Peak RSS in the 1 MiB scenarios has not
improved; it is dominated by traffic in flight. These are local mock measurements.

## Residual limits

- Nothing here was exercised against Telegram. The zero-hash form follows
  [TDLib's bot behaviour](https://github.com/tdlib/td/blob/4d06d1ba3a1978476fe2b6575de8388439f6baa3/td/telegram/ChatManager.cpp#L2696)
  and is unverified live. A rejection takes the ordinary channel-error path.
- The 64-level limit is a resource policy that can reject schema-valid data nested
  beyond it. Manually constructed, serde or third-party values are outside it, as are
  stacks well below 2 MiB, including unoptimized builds on a 1 MiB Windows main thread.
- `deserialize::Error` gained a variant and is not `#[non_exhaustive]`, so exhaustive
  downstream matches need a new arm. `grammers-tl-gen` output now requires a `Cursor`
  with `enter()`.
- Hosted Windows CI was not observed. Earlier listed limits on entropy panics, opt-in
  liveness, terminal entry removal and CDN redirects are unchanged.

## Provenance and validation

GPT-6 Astra was unavailable, and the owner directed on 2026-10-31 that Claude alone
finish the work, with a fresh-eyes Claude subagent replacing the second-model and Duo
acceptance gates in [PUBLICATION.md](PUBLICATION.md). This is therefore **not** the
reciprocal Astra+Opus acceptance earlier rounds received. Two independent reviewer
subagents, given only the diff and the finding list, each returned changes:

- First pass: a failed recovery retried every 0.5 s with no new traffic, and the
  checker claimed checks passed when none ran. Both fixed.
- Second pass: a live update during the new pause could corrupt `set_state` and spin the
  receiver. Fixed, with a regression confirmed to fail without the fix. Its smaller
  findings on count-0 updates, unpaced error classes and stale counters are also fixed.

The reviewers read code only and ran nothing; every test and measurement was run by the
implementing session. Model identity is the subagents' inherited configuration; no
separate native receipt or effort confirmation exists. The evidence checker validates
the historical 2026-10-30 set bound to `6132496e`, not this tree, whose measurements are
bound by [their own build record](benchmarks/2026-10-31/final/build.json).

Validation of this tree, on Rust/Cargo 1.99.1, Linux x86_64 and locked offline
dependencies, all local:

| Check | Result |
|---|---|
| Format, all-target/all-feature check, strict Clippy, warning-free Rustdoc (fresh target) | Passed |
| Release workspace tests and doctests, all features | 720 passed, 0 failed, 45 ignored |
| Minimal client / debug mentions / debug self response | 53 / 2 / 1 passed |
| Session memory / import / string / file import | 63 / 85 / 70 / 105 passed, 3 ignored each |
| Evidence checker (Python) | 32 passed |
| Release sender suite repeated three more times | No failures |

Ignored tests are benchmarks and explicit network probes. Generated TL compilation can
exhaust memory on this 6 GB host with parallel jobs; one build job avoids that. Hosted
CI was not observed when this was written.

## Second follow-up

Against `79c1c0a`, with Claude working alone by the owner's direction and fresh-eyes subagents
given only the diff and a neutral list of intentions, never conclusions.

| Item | Status | What now holds |
|---|---|---|
| Review of `79c1c0a` | Done | Three approved with non-blocking findings. The real ones are fixed: a successful difference restarting recovery in a loop, unpaced storage failures, a test that did not test its name, untested 6-byte rebasing. Remaining smaller ones are listed below |
| Busy p99 (+96.45%) with the quiet gain (-43.87%) kept | **Not both.** | Busy +28.11%, quiet -22.49%, completed +13.14%, all against `7774ed6`. A measured trade: see [the curve](benchmarks/2026-10-31-bot/README.md#fairness-across-many-pools-on-one-thread) |
| User account with no access hash | Documented limit | Telegram's documentation says such a peer cannot be used. Details in the [audit](COMPLIANCE-2026-10-31.md#open-items-answered) |
| Unknown channel rejection | Bounded | Per-channel backoff to one request a minute at most, retained updates capped, position unmoved. Limit in the audit |
| 64-level TL limit | Sound; documented | 23 levels against the schema. Details in the audit. The error arm is listed as breaking |
| Performance and memory | Large measured gains in bot workloads | The [measurements](benchmarks/2026-10-31-bot/README.md). No gain in per-session idle heap, where replay history remains the reason |
| Zero access hash | Confirmed by Telegram's documentation | Kept |

Reviews of this work, each by a new subagent, and what came of them:

- Two parallel reviewers: one changes-requested finding, that a `Session` may forget peers, which
  a table of written peers has to allow for. Also fixed from them: unreadable channel replies growing
  the delay they should not, the account backoff being reset by channels, stale connection failures after a
  stable connection, one yield per overflowing update. The harness then charged allocator accounting
  to the CPU window, measured open-loop latency from task start and covered short replies only;
  all three fixed. Covering full replies is what exposed the standard sort's scratch space.
- Third reviewer: changes requested for one blocking bug, in the fix for the first review's main finding:
  a live update during the retry delay could leave an update undelivered for 15 minutes. Fixed by
  keeping those updates in the gap, which also fixed the same weakness on rejections.
- Fourth reviewer, on everything including these documents, with the raw logs: changes requested for errors
  in the documents. The heap log did not match its table, and two claims overstated retries and warnings.
  Also fixed from it: more `Debug` and log exposures of the same kind. It confirmed about 60 quoted figures
  against their summaries.
- The approval of the published tree is recorded with the publication, as `PUBLICATION.md` describes.

Still open and not changed, from those reviews or the audit:

- Should Telegram ever send two authenticated IDs more than 300 s apart in one direction, messages
  other than RPC results below the second would be dropped until reconnection. A warning now says so.
- Released updates carry no peers in their batch, so `Message::sender()` returns `None` for them until
  the session has them. The asynchronous variants fall back to the session.
- `Update::from_raw` still clones the message, because the public `raw` field and the message both hold it.
- No absolute inbound `msg_id` time window; `bad_server_salt` not matched to a sent message. See the audit.

Validation of this tree is in the [measurements](benchmarks/2026-10-31-bot/README.md#validation). Hosted
CI of the commit is reported with the publication, outside this file.
