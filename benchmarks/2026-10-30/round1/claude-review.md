VERDICT: changes

**Limits:** this session had no shell or Rust test runner, so nothing was compiled or run, including the suggested regression tests, and I couldn't run `git diff`. I read HEAD `8f109a6` against the base checkout at `<baseline-checkout>`. I re-checked findings 1-5 against the source, including Tokio 1.54 and glass_pumpkin 2.0.0. The rest come from read-only sub-reviews, and I spot-checked about half. The claimed 565 passed / 36 ignored matches the committed logs but wasn't rerun here.

## Blocking: code

1. **`grammers-mtsender/src/updates_receiver.rs:166-175`, `grammers-session/src/message_box/mod.rs:538-549`: live updates are delivered twice.**
   - **Problem:** base never read the socket queue during catch-up. The new `try_recv` and the home-failure gap now send socket updates through the forced branch, which returns them without advancing pts. `getDifference` then returns them again, so bots handle messages twice. A saved per-update `State` can also jump past the unfetched backlog.
   - **Fix:** restrict the forced branch to difference results. Postpone or drop pts-bearing socket updates for keys in diff, as the doc at `mod.rs:421-428` says, but still deliver non-pts updates. Test with `pts:100` and a socket update at pts 101: `get_difference().pts` stays 100 and the update arrives exactly once.

2. **`grammers-session/src/message_box/adaptor.rs:490-491`: valid Telegram input is rejected.**
   - **Problem:** the comment at `:560-564` says Telegram really sends peerless `updateNewChannelMessage(messageEmpty)`. The new `valid_update` rejects it. On the socket, the whole batch is dropped (`mod.rs:490`), including non-pts siblings such as callback queries. In a difference, `mod.rs:761,765,972` return `Err` without changing state, so the same request repeats and catch-up can wedge.
   - **Fix:** accept peerless messages and reject only a non-channel peer. Add a peerless fixture, because the existing test at `tests.rs:786` includes a peer.

3. **`grammers-mtsender/src/sender_pool.rs:821-830`: more than 128 cancelled cold waiters are never cancelled, and the runner spins.**
   - **Problem:** `oneshot::Sender::poll_closed` spends one coop unit per closed sender. Once the budget is gone it returns `Pending` and defers a wake, which the loop reads as a live waiter. Each poll revisits the same first 128, so `cancel_setup` never runs and one worker spins until setup ends. That is up to 30s on the managed client and unbounded with `connection_timeout: None`. With the budget at 0, `connection_pool.join_next` is starved too.
   - **Partial cancel, same code:** cancelled `Rpc`s keep their body and their count and byte permits while one live waiter remains, so a silent DC can cause `WouldBlock` on healthy DCs.
   - **Fix:** use `pending.requests.retain_mut(|rpc| rpc.tx.poll_closed(cx).is_pending())`, then return `Ready` when the list is empty. Test with 200 cold calls against a silent listener, drop them all, and assert the peer sees EOF within 1s.

4. **`grammers-crypto/src/two_factor_auth.rs:126`, `grammers-mtproto/src/authentication.rs:478`: a full primality test runs inline on the async thread.**
   - **Problem:** every fresh-key handshake, including the 404 fallback, runs `safe_prime::check`. With `required_checks(2048) = 16` on both q and p, that is about 32 2048-bit modexps plus BPSW. Base had no such check on this path. It blocks a Tokio worker, and on `current_thread` it stalls all I/O. The time hasn't been measured. Ledger line 31 ("Unknown valid primes") implies a fast path that doesn't exist.
   - **Fix:** skip only the primality step for Telegram's constant prime, keep the subgroup check, and offload unknown primes from `sender.rs:873`. Measure the cost.

5. **`grammers-mtproto/src/mtp/encrypted.rs:321-329,342-343`, `grammers-mtsender/src/sender.rs:755`: a fast clock now fails hard.**
   - **Problem:** `bad_msg` 17 corrects `time_offset` but not `last_msg_id`, so each retry uses `last_msg_id + 4`, which is still too high. The new cap of 3 retries turns this old bug into `BadMessage{17}`. `initConnection` then fails, and the next connection restarts at offset 0. A stored key with a clock more than 30s fast can't connect until the skew clears.
   - **Fix:** reset `last_msg_id = 0` when the offset changes, and add a code-17 regression test.

## Blocking: requirement gaps

6. **`OPTIMIZATION-2026-10.md:108-111`, `grammers-mtsender/tests/loopback.rs:702`: the Tokio claim wasn't tested.**
   - **Problem:** the client is hard-coded to current-thread, and the queue benchmark compares two channels that both run on Tokio. Kernel time is called unavailable, though `getrusage(RUSAGE_THREAD)` provides it. The 59% whole-process system time goes unanalysed.
   - **Fix:** run a multi-thread vs current-thread A/B with a per-thread user/sys split and a non-Tokio blocking-socket control. Record the conclusion that follows.

7. **`OPTIMIZATION-2026-10.md:41`: none of the P2 areas has an A/B.**
   - **Problem:** compression, packing, lookup, serialization, buffers and locks are all unmeasured, and "no evidence" stands in for "evidence rejects". Nothing measured improved: the final build is 2.57%/1.19% slower and heap per session rose.
   - **Fix:** add a measured row per proposal, or mark it "not evaluated", and adopt the ones that win.

8. **`grammers-mtsender/tests/loopback.rs:691,694,823-828`: the workload can't support the concurrency, tail or fairness scope.**
   - **Problem:** it uses one DC, warm routes and at most 16 calls in flight, with no large replies, cold paths or fairness measurement. ACK-only packets, salt prefetch, out-of-order replay insert and gzip limits are never reached. The client thread is about 99% saturated, so the throughput and CPU deltas are effectively one measurement. Closed-loop latency also has coordinated omission.
   - **Fix:** add open-loop, high-in-flight, multi-DC, large-reply and cross-pool scenarios, and run them on baseline and final.

## Non-blocking: code

9. `grammers-mtsender/src/sender.rs:573-579`: a pong delayed past 60s suppresses the next ping, so the server closes a healthy connection at T+75. Fix: if the old keepalive is `Sent`, replace its record and send a new ping. This bounds records rather than wire pings, and it supersedes the ledger's "one outstanding ping".
10. `grammers-mtsender/src/sender_pool.rs:1022-1035`: never-framed cold RPCs inherit `Read{may_have_been_sent:true}`, so a safe retry is refused. Fix: rewrite to `false` in the setup error branch.
11. `grammers-mtsender/src/sender.rs:379`: `since + limit` panics on a huge liveness timeout and stops the pool. Use `checked_add`.
12. `grammers-mtsender/src/sender.rs:55`, `runtime.rs:447`: a liveness timeout of 60s or less kills idle connections, because no ping is sent first. Fix: use a ping interval of `min(PING_DELAY, limit/2)`.
13. `grammers-mtproto/src/mtp/encrypted.rs:1614-1626` runs before `:1641`: `push` consumes ACK and salt state before validating the request. Make `request_fits` the first statement.
14. `grammers-mtproto/src/mtp/encrypted.rs:1615-1618,1673-1681`: `finalize` can add a pending ACK to the salt-only packet. A `bad_msg` then names the container, and `salt_request_msg_id` stays set, stopping prefetch. Skip the ACK for a salt-only buffer.
15. `grammers-mtproto/src/mtp/encrypted.rs:328,338`: authenticated `msg_id` values can overflow `i32` arithmetic, which panics in debug and wraps in release. Use `i64` or reject implausible offsets.
16. `grammers-session/src/message_box/mod.rs:552,653`: a qts update can create Secondary without Common, and a later gap reaches the `.expect`. Don't create the entry while Common is missing, and treat saved `{pts:0, qts>0}` as empty.
17. `grammers-mtsender/src/updates_receiver.rs:256`: `CHANNEL_INVALID` and other definitive per-channel errors block every later channel. End that channel's difference and keep propagating flood and auth errors.
18. `grammers-mtsender/src/updates_receiver.rs:185-191`: a missing channel hash stalls the remaining channels for up to 15 minutes. `continue` when prepare returned `None`.
19. `grammers-mtsender/src/updates_receiver.rs:169`: an empty box ignores home failure, so there is no RPC and no reconnect. Re-arm `should_get_state` on a new generation.
20. `grammers-client/src/message/message.rs:708`: `reply_album(vec![])` still panics, which misses the empty-album requirement. Use `if let Some(first)`.
21. `grammers-client/src/client/files.rs:240,287` (pre-existing): retrying after an error returns `Ok(None)`, so a truncated download looks complete. Restore the request on error and keep `PreFailed` sticky.
22. `grammers-mtsender/src/sender_pool.rs:466-475`: a body larger than `max_pending_bytes` reports transient overload but can never succeed. Return `InvalidInput`.
23. `grammers-mtsender/src/configuration.rs:226`: `AutoSleep` retries non-connection `Io` errors such as `InvalidInput`. Exclude those kinds.
24. `grammers-mtproto/src/authentication.rs:661-663`: `dh_gen_retry` returns `DhGenFail`, and `Error::DhGenRetry` is never constructed. Map it correctly.
25. Wrong diagnostics. Fix each mapping:
    - `grammers-mtproto/src/manual_tl.rs:196,208,212` surface as "buffer too small" instead of `InvalidEnvelope`.
    - `grammers-mtproto/src/mtp/plain.rs:110-114` reports misalignment as too long.
    - `grammers-mtsender/src/errors.rs:436-441` categorises oversized packets as `Network`.
26. `grammers-crypto/src/factorize.rs:76-77,133,150`: the comment says restarts share a budget, but each attempt gets a fresh 250,000, and the `r=65536` round can never finish. Estimated failure is about 1e-4 for balanced 64-bit PQ, and the legacy `factorize` panics on it. Cap `m` and share one budget.
27. `grammers-mtsender/src/sender_pool.rs:1049-1054`: the wrapper future stores `sender` twice, which breaks the AGENTS.md rule, and the footprint test misses it. The estimated saving is 700-900 B/session. Have `run_sender` return `(dc_id, result)` and size-test the spawned shape.
28. `grammers-mtsender/src/sender.rs:385-389`: the ACK deadline cycle likely re-registers the timer once per RPC with `ack_batch=1`. This is the likely avoidable part of the slowdown, but it's unmeasured. Reset only when the deadline moves earlier or the timer has elapsed, then re-run the A/B.

## Missing tests

29. `grammers-mtproto/src/authentication.rs:775-782`: all six fixtures fail inside `check_p_and_g`. Padding of 16 or more, hash mismatch, nonces, long `g_a`, time overflow and `g_a` bounds have no malformed test. Build fixtures on the vector's real prime.
30. `grammers-crypto/src/factorize.rs:191,202-205,219`: only the exit status is checked, so a filter matching no tests still passes. Budget exhaustion is only tested at 0. Assert a child marker, and add 4294967291², a three-prime composite and a balanced 64-bit semiprime.
31. `grammers-mtproto/src/manual_tl.rs:201-207`: the authenticated container checks can kill a connection, but no test reaches them through `process_plaintext`. Add accept and reject cases.
32. `grammers-mtsender/src/sender_pool.rs:1864-1865,2011-2012`, `tests/loopback.rs:764-765`: each test drops the caller and then calls `quit()`, so Quit alone passes them, and `OPTIMIZATION-2026-10.md:39` overclaims this coverage. Add runner-level tests for caller-only cancel, partial cancel, disconnect during setup, and a stale completion at `:1015`.
33. `grammers-mtsender/src/sender_pool.rs:805-806,1108,1195,1721`: fairness budgets and concurrent config merges are untested. The receiver test increments the counter by hand, and nothing drives `UpdatesReceiver::next`. Add these with a local responder.
34. There are no fuzz or property tests and no stated reason, although `process_plaintext`, `MessageView` and transport `unpack` can run without I/O. Add them or document why not.

## Docs and evidence

35. `OPTIMIZATION-2026-10.md:22-24,222,226-227,248`: these still say "uncommitted" and give HEAD as `7774ed6`. Update them. `client-minimal.log` also predates the final mtsender fix, so rerun that build.
36. `AGENTS.md` has no 2026-10-30 section, and `CHANGELOG.md` omits the new variants on exhaustive enums (`grammers-mtsender/src/errors.rs:16,229`). Record both.
37. `PERFORMANCE.md:12-16`: it calls the deltas "costs" with no noise caveat, though per-execution medians change sign. Run 20+ CPU-pinned pairs with confidence intervals, and reword.
38. `grammers-mtsender/tests/loopback.rs:806-810`: the census stops at nine RPCs. The derived saturated figure is 16,775 vs 7,911 B/session (+112%), against a headline of +10.1%. Run past 1024 replies and lead with the measured result.
39. `benchmarks/2026-10-30/source.json:4` has no command or commit, so it can't tie the binary to `8f109a6`, and `scripts/compare-loopback.py:4` says "same" harness although the files differ. Fix both.
40. `gzip-budget.log:8`, `queue-handoff.log:9`, `validation/final-check.log:8` and `OPTIMIZATION-2026-10.md:17-19,225` publish host paths and task IDs in a public repo. Sanitize them. No credentials or user data were found.

## Unverified risk

`grammers-mtproto/src/manual_tl.rs:201-207` tears down the connection on any container ID or seqno violation, with no captured Telegram traffic behind it. If real traffic ever trips this, every bot enters a reconnect loop. The packet is already authenticated, so log `seq_no > seq` instead of failing until there is evidence.

## Checked and held up

- **Numbers:** every headline benchmark figure and percentage recomputes from the raw logs, and the hashes in the docs match `binaries.json`.
- **Harness:** the latency patch is the only difference in the rpc-cost path.
- **Limits:** worst-case outbound is 1,044,472 bytes, under `0xff000`, and the 1 MiB inbound reply fits the receive cap.
- **Gzip and replay:** budgets are shared across levels and siblings, exact-limit output still validates the footer, and replay IDs are recorded after handling.
- **Delivery:** no path found retries an ambiguous send, and the 404 fallback works through the new wrappers.
- **Pool:** config mutex scope, permit ordering, generation checks and `queue.rs` behaviour are correct.
