Native saved report 20516; original full-text SHA-256 `0397688f34fe22380950417b56f2171b6f9eec16606b2604f5d3c45f589e4681`.
The public transcript substitutes execution-location placeholders only.

VERDICT: changes

Do not publish this candidate: findings 1 and 2 are blocking. Finding 1 is a new regression that silently and permanently loses channel updates in a documented configuration.

**Candidate reviewed:** `<candidate-worktree>`, branch `<candidate-branch>`, HEAD `5f0cb05` plus the staged correction, cumulative against `7774ed6`. Paths below are relative to that root.
- **Identity:** `.git` refs confirm the branch and HEAD, and the live source matched the staged patch wherever I read both. I did not recompute any hashes.
- **Reviewer:** `claude-opus-5-5`, with three source and evidence subagents requested as `opus`. The runner must keep the native receipt.
- **Execution limit:** neither I nor any subagent had a shell, so nothing was built or run. The 631 passing tests, Clippy and the benchmarks remain parent-run only.

## Blocking

1. **Channel updates are lost permanently when an access hash never arrives** (`grammers-mtsender/src/updates_receiver.rs:84`, `grammers-session/src/message_box/mod.rs:634-639,1094-1103`)
   - **Problem:** the fix for finding 8 retries every second with no limit, and socket updates for a pending channel are dropped in the meantime.
   - **Trigger:** `auto_cache_peers: false` (documented for memory-only use) never caches the hash (`client/chats.rs:926`). A gap, or 15 quiet minutes on the channel, makes it pending.
   - **Effect:** every later update from that channel is silently dropped, and the channel is polled every second for as long as the process runs. `5f0cb05` removed the entry, so the next update was delivered. A raw `UpdatesReceiver` that never caches peers, or a channel seen only as `min`, hits the same path.
   - **Fix:** limit the deferral (about 30-60 seconds or a set number of attempts), then warn and fall back to the old removal. Add a test where the hash never arrives and a later socket update is delivered. Reopen ledger row 8.

2. **The replay-floor teardown discards valid results and can trigger falsely** (`grammers-mtproto/src/mtp/encrypted.rs:545-547,584-591`, `grammers-mtsender/src/sender.rs:659`)
   - **Discarded results:** a container holding `[rpc_result, below-floor message]` clears the decoded result. The caller then gets `Read { may_have_been_sent: true }` for a call that succeeded, so a user retry can send it twice. ACK-and-drop delivered those siblings; this teardown was my round-one suggestion, and I missed this effect.
   - **False trigger:** with a window of 7..4099, container `[5003, 9]` fails while `[9, 5003]` passes, because the floor is read live. A window that is merely full also counts as evicted, so the comment at `:585-587` is wrong there.
   - **Unknown:** I can't tell how often Telegram sends such IDs, and at thousands of requests per second 1024 IDs pass in under a second.
   - **Fix:** deliver below-floor `rpc_result` and `rpc_error` normally, because `sender.rs:729,743,753` already ignores retired IDs. Suppress their own-update decoding, and ACK-and-drop everything else. That clears stuck requests with no teardown. Track the highest evicted ID as the real floor, and add a mixed-sibling test at sender level.

## Should fix before publishing

3. **The liveness check can't see a reply the reactor hasn't reported** (`grammers-mtsender/src/sender.rs:432-445`)
   - **Problem:** Tokio's `poll_read` returns `Pending` before any syscall when readiness isn't cached (`tokio-1.54.0/src/io/poll_evented.rs:172`). After a stall, a reply already in the socket can still produce `TimedOut`, so the second scenario of finding 11 remains open. This only affects users who opt in to liveness.
   - **Tests:** the new tests hide this by calling `peek` first (`:2041,2055`).
   - **Fix:** force a reactor turn before inspecting, and return any non-empty batch (`:448-453`). `yield_now` may not turn the driver on a busy multi-thread worker, so a short timer wake is safer. Add a test with no `peek` and an already-expired `since`.

4. **`check_deadlines` makes catch-up roughly cubic** (`grammers-session/src/message_box/mod.rs:327-347`)
   - **Problem:** the `Vec::contains` filter now runs on every `next()` iteration while anything is pending, with no await, and `load` marks all saved entries pending (`:196`). The baseline returned immediately in this state.
   - **Estimate:** 5,000 channels cost about 12.5M comparisons per iteration and about 4×10^10 over a full catch-up, all on an async worker. This is derived, not measured, and `tests/loopback.rs` doesn't exercise this path.
   - **Fix:** use a pending flag on `LiveEntry`, or recompute only while a deferred entry exists. Add a release benchmark at around 10,000 channels.

5. **The evidence gate doesn't deliver what the ledger claims** (`scripts/check-optimization-evidence.py`)
   - `:214-219`: upstream commits aren't reachable from the fork, so a fresh clone fails unless the Codeberg tip is fetched first. `opus-portability.json:2` ran "sharing existing Git objects". The audit also read 160 links and 221 artifacts there, against the final 162 and 222.
   - `:156,164`: `git diff` without `--full-index` depends on abbreviation length and local Git config.
   - `:84` and `scripts/test_check_optimization_evidence.py:163`: private host paths and the branch shape ship publicly, and the scan at `:222` covers only `benchmarks/2026-10-30`. This reopens part of finding 17.
   - `:187` scans only `final/validation`, though `REVIEW-OPUS-CORRECTION.md:32,79` says "every" and "all historical/current". `:192` makes source binding optional, and benchmark binary hashes are never checked, so stale benchmarks can pass.
   - **Fix:** add an explicit `--verify-upstream`, use isolated full-index diffs, require named rows bound to the current manifest, and scan all tracked text with externally supplied patterns. Rerun portability last, from a real clone.

6. **Documents state superseded or wrong facts**
   - `AGENTS.md:9,42-43,62`: `REVIEW-FINAL.md` is still called current, and the clock-reset rule and the 599 test count are out of date. The new rules aren't recorded.
   - `CHANGELOG.md:26-27` contradicts `:126-127`, and the new section sits under `2026-10-21`.
   - `OPTIMIZATION-2026-10.md:42` says the receiver uses checked paths, but `updates_receiver.rs:104` calls `load`.
   - `message_box/mod.rs:123,207-208,1094`: the `load` repair, the `try_load` QTS discard and the `defer_channel_difference` panic are undocumented or misdescribed.
   - `REVIEW-OPUS-CORRECTION.md:66` calls RSS inconclusive, omitting the heap run's 11,308 to 14,356 KiB (+27%), which I confirmed in the logs.
   - `REVIEW-OPUS-CORRECTION.md:6,91` and `REVIEW-FINAL.md:103-128,142-143` carry "no commit", private-handoff and redacted-evidence wording that must be corrected before the commit.

## Missing or weak tests

- `encrypted.rs:1875-1897` uses a single inner message, so finding 2 isn't pinned.
- `encrypted.rs:1924-1964` passes for almost any reset threshold, so the +3 second boundary needs its own cases.
- `sender_pool.rs:1897-1901` skips `update_config`, so the rearm ordering isn't covered.
- `sender.rs:1889-1894` can pass on a slow host before the handshake reaches the gate.
- `manual_tl.rs:595,632`: no maximum-size container goes through `process_plaintext`.

## Low severity

- `grammers-mtsender/src/errors.rs:465-470`: negative codes such as `-503` are classed as ordinary RPC rejections, though the call may have executed. Telegram's use of those codes is from memory.
- `encrypted.rs:1280`: an empty `future_salts` reply replaces a working salt with 0, and a test pins this. Keep the current salts instead.
- `encrypted.rs:1644-1676`: ACK-first framing can delay a request within about 52 bytes of the limit while inbound traffic continues.

## Checked and accepted, by reading

- **Ledger rows:** 1, 2, 4, 5, 6, 7, 10 and 12 hold. Rows 3 and 13 are acceptable as recorded deferrals. The TL recursion stack-overflow risk from row 3 stays open and must be disclosed at publication.
- **Rejected leads:** the SRP oversize panic is false, because `client/auth.rs:109-112` validates first. `client/retry_policy.rs` matches `ctx.error` literally, but no `mod retry_policy` appears in the crate and `runtime.rs` imports `AutoSleep` from `grammers_mtsender`, so I don't count it as a live defect.
- **Teardown mechanics:** permits, routes and delivery certainty on failure paths held up in the sender pass.
- **Numbers:**
  - All validation totals match their logs: 631 passed, 41 ignored, plus the optional session builds.
  - The five main medians match raw data: plain -2.91%, gzip -2.05%, overload -8.36%, busy p99 +118.80%, quiet p99 -28.26%.
  - Heap of 7,911 to 15,951 B/session also matches.
  - Bootstrap intervals were not recomputed, and the binaries aren't retained, so the binary-to-source link is parent-asserted.
- **Disclosed costs:** I accept the 8 KiB replay cache and the cooperative-budget tail tradeoff as disclosed. Plain throughput is measurably slower, and the candidate correctly claims no general optimization.

## Not verified

Codeberg was not fetched independently, real Telegram behavior is unconfirmed, and session backends, the obfuscated transport and `dispatcher.rs` were not reviewed.

## After the fixes

- Any Rust change invalidates `final/source.json`, `source.patch`, `source-inputs.json` and `artifact-hashes.json`, so regenerate them and rerun validation.
- Findings 2 and 3 touch the measured path and need fresh paired benchmarks. Findings 1 and 4 don't, if that reuse is stated explicitly.
- Another Opus review is required before any push. Publish only from `<candidate-branch>`, because this runner branch's `REVIEW-HANDOFF.md` and `REVIEW-CANDIDATE/` contain private host paths.
