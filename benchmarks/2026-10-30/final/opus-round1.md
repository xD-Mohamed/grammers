Local routing redacted; complete original report retained by runner. No substantive review text changed.

VERDICT: changes

The latest staged correction introduces a regression that silently loses valid updates, so this candidate should not be published. Finding 1 is the most serious.

**Candidate reviewed:** `<candidate-worktree>`, branch `<candidate-branch>`, HEAD `5f0cb0511416e666b5476c078c0aaa86d9bce69e` plus the staged correction, against baseline `7774ed6`. All paths below are relative to that root. I did not treat the runner's scaffold as the candidate; I used only its handoff patches.
- **Identity checks:** the worktree's `.git` pointer and reflog confirm branch and HEAD. All 28 patch post-image blob abbreviations match the index, but that is a 7-hex match. SHA-256 values, `git status` and live-versus-index equality were not recomputed.
- **Reviewer:** `claude-opus-5-5`. Five source subagents were requested as `opus`. The runner must still keep its own native receipt.

**Execution limit:** no shell was available to me or to any subagent, so nothing was built or run. The 604 passing tests, Clippy, the benchmarks and the evidence checker remain parent-run only, and my reproduction of finding 1 did not run. Every finding comes from reading the code. Findings 1, 2, 4, 6-11 and the line ranges I cite from `check-optimization-evidence.py` were re-read by me; the rest were traced by subagents.

## Blocking

1. **`grammers-session/src/message_box/adaptor.rs:497-499`: valid `qts: 0` updates are rejected, and the staged correction caused this.**
   - **Problem:** `valid_update` moved from `from_update` to `raw_from_update`, which does not filter `NO_PTS`. Every qts arm sets `count: 1` (for example `:680-683`), so `qts: 0` fails `pts >= count`. `defs.rs:21-28` documents Telegram sending exactly this with `ChannelParticipant`.
   - **Socket effect:** `mod.rs:525-527` returns `Err(Gap)` and `updates_receiver.rs:335` discards it. The whole batch is lost silently and an unnecessary getDifference starts. `5f0cb05` delivered these updates through `mod.rs:563-568`, and chat-member updates are ordinary bot traffic.
   - **Difference effect:** `mod.rs:805,809,1016` reject the whole difference and leave state unchanged, so `next()` fails with the same request each time. Whether Telegram puts `qts: 0` in `other_updates` is unconfirmed.
   - **Fix:** `info.count >= 0 && (info.pts == NO_PTS || info.pts >= info.count) && <channel check>`. This keeps rejecting invalid channel IDs at pts 0.
   - **Tests and docs:** no test builds this update, so add socket, account-difference and channel-difference cases with siblings. Correct `REVIEW-FINAL.md:29-31`, which says valid behavior is preserved. Astra's acceptance of this correction missed the regression.

2. **`grammers-mtsender/src/sender.rs:1857`: this test should fail on Windows CI.**
   - **Problem:** `from_raw_os_error(104)` is ECONNRESET only on Linux, and `ci.yml:21,45` runs the workspace on `windows-latest`. There the kind falls outside `retryable_io` (`configuration.rs:210-225`), so `:1878-1880` fails for `written = None`. The Windows error mapping is from my knowledge of std, not a run.
   - **Fix:** use `io::ErrorKind::ConnectionReset.into()` and assert `.kind()` at `:1868`.

3. **`grammers-mtsender/src/sender.rs:88,675` and `grammers-mtproto/src/mtp/encrypted.rs:1554`: unbounded TL nesting with no recorded disposition.**
   - **Problem:** `DecodeLimits` covers MTProto wrappers only. `textBold` nests itself (`api.tl:915`) and is reachable through `updateWebPage`, `webPage.cached_page` and `pageBlockParagraph`. At about 4 bytes per level, 1 MiB holds about 260,000 levels. The decode is recursive and synchronous, so a stack overflow would abort the process, and checked runners cannot contain that. The stack depth needed is an estimate.
   - **Scope:** this is pre-existing and needs an authenticated peer. However, the owner listed bounded nesting and no quiet omissions, and the ledger never mentions it.
   - **Fix:** add a decode depth limit. At minimum, record an explicit deferral with its threat model in the `OPTIMIZATION-2026-10.md` scope matrix.

## Non-blocking

4. **`grammers-client/src/parsers/html.rs:121-125`, `parsers/markdown.rs:61-65`: the mention fix is incomplete.** `tg://user?id=2000000000000` passes `id > 0` and reaches `PeerId::user_unchecked` (`client/messages.rs:113`). That panics in debug and sends an invalid ID in release. `1099511627776` equals the self-user sentinel (`peer.rs:135`), so release builds would use the account's own hash. Fix: `.filter(|id| PeerId::user(*id).is_some())`, and add both values to the test vectors.

5. **`message_box/mod.rs:805-814,1016`, `updates_receiver.rs:222-229,288-295`: one malformed element wedges recovery.** Even after finding 1, a single bad `other_updates` entry rejects the difference on every retry, and restarts repeat it. Rotation happens only on apply (`mod.rs:997`). Fix: log and skip the element, then apply the validated state. For channels, at least end that channel's difference.

6. **`updates_receiver.rs:102-108`, `mod.rs:185-188`: one bad saved row blocks `stream_updates`.** An out-of-range channel row or a negative value fails every start with a generic `InvalidData`, and there is no repair path. The staged range check widens what gets rejected, and `client/dialogs.rs:99` persists server values unvalidated. Using `catch_up: false` escapes this but then overwrites saved channels. Fix: drop invalid rows with a warning and add a CHANGELOG note.

7. **`mod.rs:594-596`, `updates_receiver.rs:115-116`: qts updates are dropped when no Common entry exists.** If the stream starts before login, nothing re-arms GetState, so these updates vanish until a Common pts update or a home failure. Fix: deliver them with no message-box state, and re-check the self user while primary state is missing.

8. **`mod.rs:588-592`, `updates_receiver.rs:83`, `mod.rs:1043-1047`: a dropped socket update can be skipped permanently.** If the channel has no cached hash, its entry is popped and the next update recreates it with no gap detected. The trigger is narrow, because sockets are read only when nothing is pending, so it is essentially the same batch that began the difference. Fix: do not pop after a drop, or deliver the update live when no hash exists.

9. **`encrypted.rs:573-581`: messages below the replay floor are ACKed and dropped silently.** An `rpc_result` older than the 1024-ID window leaves its request in `Sent` indefinitely, since no ordinary RPC timeout exists. Whether Telegram reorders that far is unknown. Fix: ACK only exact duplicates, log below-floor IDs, and preferably fail the connection so callers get an error.

10. **`encrypted.rs:328-332`: any time-offset change resets the message-ID floor.** One second of jitter between two notifications can send a later ID below one already sent, inviting code 33, which can tear down the connection at `:1038`. Server enforcement is unverified. Fix: reset only when the corrected clock is more than a few seconds below the floor.

11. **`sender.rs:359-367`, `sender_pool.rs:1176-1183`: opt-in liveness can kill healthy connections.** Expiry is checked before any read, and setup storage time counts against the limit, so slow storage kills the connection on its first step. A runtime stall with a reply already in the kernel buffer gives `Read { may_have_been_sent: true }` for a call that succeeded. Fix: re-arm after the config merge and attempt one read before expiring.

12. **`updates_receiver.rs:277`, `client/auth.rs:196,292,371`, `client/files.rs:74,88`: literal `InvocationError::Rpc` matches miss `Shared(Rpc)`.** `sender_pool.rs:1110` produces `Shared` when two or more waiters share a failed setup. Fix: match through `root_cause()`, or give each waiter a plain `Rpc`.

13. **`sender_pool.rs:1427`: the budget placement is untested.** `consume_budget` charges every loop iteration, and the control removed it entirely. A receive-arm-only A/B might reduce the busy-pool tail, but that is speculative.

## Weak or missing tests

14. These tests can pass for the wrong reason:
    - `grammers-mtproto/src/authentication.rs:876-917`: bad-time fixtures still fail at `:489-492` if `check_p_and_g` (`:478`) is deleted, so the DH gate has no real test.
    - `sender_pool.rs:2456-2472`: bumps the failure generation itself.
    - `sender_pool.rs:1673`: the assertion cannot fail.
    - `sender.rs:1832`: only hits the top-of-step check.
    - `sender.rs:1695,1707`: build their own closures, so reverting step2/step3 offload would still pass.

15. These behaviors have no test:
    - `handle_future_salts` (`encrypted.rs:1240-1276`) and the error branches in `mtp/plain.rs:87-120`.
    - Out-of-order and inner replay.
    - Sibling gzip budget through real packets.
    - A partially written keepalive surviving replacement.
    - `run_dh_job` capacity and permit retention.
    - Setup-task panic with live waiters.
    - Any non-ignored pool race on a multi-thread runtime.

## Evidence and publication

16. **`scripts/check-optimization-evidence.py:137,205`:** the gate fails on `main` or a detached CI checkout, and it always prints "no commit or publication performed". `PERFORMANCE.md:5` and `REVIEW-FINAL.md:7-9,177` have the same problem. Fix: enforce the branch only when requested, and word these as "at measurement time".

17. **`check-optimization-evidence.py:162-171,198`:**
    - It skips `routing-commands.json` and ignores `FAILED` rows.
    - Its privacy pattern misses `duo/<id>`.
    - It puts host paths into a public file.

    `final/git-diagnostics.md:34-35,41,49-53` and `final/validation/git-check.json:2,4,24` expose run branches and control-plane details, contrary to `OPTIMIZATION-2026-10.md:239`. Fix: extend the checks, use placeholders and regenerate `artifact-hashes.json`.

18. **Commit subjects:** `8f109a6`, `5b16ac4` and `5f0cb05` are titled `duo review: ... (GPT)`. Decision: keep them, because the checker and ledgers pin those SHAs (`check-optimization-evidence.py:121-122`). Make the final commit conventional and say this in the ledger.

19. **`final/performance/final-build.json:1-23`, `OPTIMIZATION-2026-10.md:243`:** the build receipt has no rustc or cargo version, and the ledger still targets the round-two diff.

## Accepted, by reading

- **Round-two findings 1-12:** the dispositions hold in the source, apart from the new defects above. That includes queued delivery certainty, permit ordering, step2/step3 offload, cancel-safe GetState, wake-based cold waiters, Common detection and salt disabling.
- **Packet limit:** `0xff000` gives a 1,044,372-byte request limit, which I recomputed, and a single message or full container stays within it.
- **Handshake and limits:** DH check ordering and the gzip budgets are sound, and false factorization rejection is negligible at about 1e-14 per prime.
- **Delivery certainty:** no path reports `may_have_been_sent: false` for bytes that reached the wire.
- **Performance numbers:**
  - Eight median paired changes match a hand recomputation from raw logs, including overload -6.59%, busy p99 +113.28% and quiet p99 -43.38%.
  - Completed plus dropped calls equal 180,000 in all 48 overload and sender-budget logs.
  - Heap goes from 7,911 to 15,951 B/session. The bootstrap intervals were not re-run.
- **Tradeoffs:** I accept the 8 KiB replay cache and the cooperative-budget tail tradeoff as disclosed. The earlier -8.21% throughput loss remains unexplained, and the candidate correctly claims no general optimization.
- **Secrets scan:** no credentials or session data were found in live files. Index blobs were not scanned and blob sizes were not listed.

## Not verified

- **Upstream:** I read the Codeberg audit but did not fetch upstream independently.
- **Telegram behavior:** real server behavior comes from code comments and memory.
- **Not reviewed:** session storage backends, the obfuscated transport, proxy code and `dispatcher.rs`.

## Re-validation after fixes

Changing Rust source invalidates `final/source.json`, `source.patch`, `source-inputs.json` and `artifact-hashes.json`, so regenerate them and rerun the validation suite. Finding 1 is off the measured path, since `tests/loopback.rs` never uses `MessageBoxes` or `UpdatesReceiver`, so that fix alone does not need new benchmarks if the reuse is stated explicitly. Any change to `sender.rs` or `sender_pool.rs` production code needs a fresh look at that reuse. This candidate needs another Opus review after correction.
