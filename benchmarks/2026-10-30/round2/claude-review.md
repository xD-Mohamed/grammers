VERDICT: changes

This is the second review round, so the blocking findings below should go to the correction continuation, with no push until they are resolved. I had no shell, so this review comes from reading the source at `5b16ac4`. I did not build or run anything, so the 590-test result and all performance figures are the implementer's.

## Blocking

1. **`.gitignore:13-17`: the round-one JSON evidence is not versioned.**
   - **Problem:** `*.json` is ignored and only the top-level and `first-candidate` exceptions exist, so all 18 JSON files under `benchmarks/2026-10-30/round1/` match the ignore rule. The index has `round1/README.md` and `binaries.json`, but not `round1/provenance.json` or `round1/paired/summary.json` (the two I checked). The provider receipts, samples and hashes the docs link to would be missing from the published commit, which breaks requirements 2 and 4.
   - **Fix:** add `!benchmarks/2026-10-30/round1/**/*.json`, stage the files, and check the links against a clean checkout.

2. **`grammers-mtsender/src/sender_pool.rs:1397-1409`: queued RPCs lose delivery certainty when a connection fails.**
   - **Problem:** `break Err(err)` drops `rpc_rx`, so calls that were never framed get `Dropped("request response channel was closed")` (`invocation_tracker.rs:143-145`). That is categorized as `Closed`, and `AutoSleep` won't retry it. Calls already in `Sender.requests` correctly get `Read { may_have_been_sent: false }` and can retry. The original error is lost for exactly the queue that grows under load.
   - **Fix:** on `Err`, call `rpc_rx.close()`, drain with `try_recv`, drop each permit, then send `Read { error: shared, may_have_been_sent: false }`. Add a socket-failure test with queued RPCs that checks the error, the retry decision and permit release.

3. **`grammers-mtsender/src/sender.rs:635-652`: new linear scan on the response path, and the overload regression is unattributed.**
   - **Problem:** every result now scans all requests, and then `pop_request` scans them again. The check duplicates `sender.rs:825-828` and makes the `Serialized` arm at `sender.rs:748-754` dead code. `REVIEW-ROUND1.md` reports overload completions falling 87,880 -> 76,691 (-12.7%) and busy p99 rising 28.21 -> 80.79 ms, from one pair with no attributed cause. This scan is my leading suspect, because it costs most on a busy pool with many requests in flight, but I have not measured it.
   - **Fix:** remove the pre-scan and raise the protocol error from the existing `Serialized` arms. Rerun the overload A/B with repeated pairs, and attribute or bound the regression as requirement 2 demands.

4. **`grammers-mtsender/src/sender.rs:899`: factorization still runs on the async runtime thread.**
   - **Problem:** only step 3 moved to `run_dh_job`. `step2` can run up to 1,250,000 steps (`factorize.rs:73`) on a plaintext, pre-authentication response, and no async timeout can preempt it. A hostile peer can stall other DCs on that worker, or a whole current-thread runtime. My rough estimate is hundreds of milliseconds; the docs give no worst-case figure.
   - **Fix:** run `step2` through `run_dh_job`. Add a current-thread progress test using a budget-exhausting `pq`, and record the measured worst case.

5. **`grammers-mtsender/src/updates_receiver.rs:151-175`: GetState recovery can be lost permanently.**
   - **Problem:** `should_get_state` is cleared before the RPC. A `next()` cancelled during GetState (the usual `select!` pattern), or a failed `set_update_state` at line 164, leaves the box empty with nothing to re-arm it. Updates carrying a `seq` are then dropped as gaps, because `try_begin_get_diff` has no entry to work from, until a short update happens to create one.
   - **Fix:** clear the flag only after `set_state` succeeds. Add tests for cancelling mid-GetState and for a storage failure.

6. **`grammers-mtproto/src/manual_tl.rs:208-218`, `grammers-mtproto/src/mtp/encrypted.rs:527-538`: the new connection-fatal checks have never met a real server.**
   - **Problem:** I checked the implementer's disagreement against Telegram's description page, and the spec does support the container `msg_id` and `seqno` rules. The risk is operational. If real traffic violates any new check, every connection enters a reconnect loop that fails in-flight RPCs, and the docs state no live test ran.
   - **Fix:** run the existing read-only tests at `grammers-mtsender/tests/lib.rs:24-26` and `:76-78` once and record the result. They use the public test DC, make no account login, and are not stress traffic. If they can't run or show no containers, make `seq_no > parent` warn-only, since rejecting it gains little against an already authenticated peer.

## Non-blocking

7. **`grammers-mtsender/src/updates_receiver.rs:147`: GetState is re-armed without a login check.**
   - **Problem:** `create` deliberately skips GetState when logged out (lines 114-115), but this path doesn't. After any home-connection failure, every `next()` returns 401 until login, which ends a `while let Ok(..)` loop. The test at `sender_pool.rs:2073-2153` asserts this behaviour on a session with no self user.
   - **Fix:** apply the same guard, give that test a self user, and add a logged-out test.

8. **`grammers-mtsender/src/sender_pool.rs:833-842`: the cold-waiter scan is quadratic.**
   - **Problem:** each `drive` pass polls every waiter, so N queued calls cost about N²/2 polls, roughly 200 million at 20,000 waiters. Low-level admission is unlimited by default, and the harness capped in-flight work at 1024, so this was never exercised.
   - **Fix:** poll new waiters once, and rescan only after a closure wake (a flag waker or a per-waiter future set). Add a benchmark above 1024 waiters.

9. **`grammers-mtsender/src/updates_receiver.rs:114-115`, `grammers-session/src/message_box/mod.rs:115-121`: the secondary-only repair misses saved channels.**
   - **Problem:** `load` clears the orphan `qts`, but saved channels keep `is_empty()` false, so GetState never runs and account state is not relearned. This contradicts the rule in `AGENTS.md`.
   - **Fix:** decide on GetState from the missing Common entry, and relax the `debug_assert!` in `set_state` at `mod.rs:354` while preserving channels. Add a test for `{pts: 0, qts: 5, channels: [..]}`.

10. **`grammers-mtproto/src/mtp/encrypted.rs:988-992`: a failed salt prefetch kills the connection.**
    - **Problem:** salt prefetch is optional because the current salt still works, yet the fourth consecutive failure tears down the connection and fails every in-flight RPC.
    - **Fix:** stop prefetching at that point and keep the connection. The next `bad_server_salt` already recovers the salt.

11. **`grammers-mtproto/src/mtp/encrypted.rs:1791`: the property test is narrower than claimed.**
    - **Problem:** all 4096 trials mutate one small `rpc_result`, with no containers, gzip or service constructors. `REVIEW-ROUND1.md` calls these structured authenticated-packet mutations.
    - **Fix:** seed the corpus with those shapes and also assert the budgets, or narrow the claim.

12. **`OPTIMIZATION-2026-10.md:21,230-231,254`, `REVIEW-ROUND1.md:5,292`, `benchmarks/2026-10-30/round1/README.md:6,25`: stale statements.**
    - **Problem:** they still say the correction is uncommitted and that this turn must not commit, which is now false. Round-one finding 35 was the same defect and is marked accepted.
    - **Fix:** rewrite them against the final SHA at the final commit.

## Accepted

- **Packet limit:** the `0xff000` reserve is correct, because the 27-byte padding allowance matches `grammers-crypto/src/lib.rs:110-113`.
- **Replay cache:** I would keep the 1024-entry cache for now. It doubles saturated per-session heap (+8 KiB), but nothing shows a smaller size is safe, so this does not block.
- **No findings in:** gzip budgets and the exact-limit footer probe, DH check ordering, retained timer logic, catch-up pts handling, or setup generation identity.

## Not reviewed

- Client parsers, session storage backends, transports, `runtime.rs` and the benchmark scripts.
- Upstream attribution: Codeberg returned an anti-scraper page, so the `ad7b4725` and `1e31e73` claims and the disposition of `d654b04` are unverified.
