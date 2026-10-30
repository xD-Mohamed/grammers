# Corrections from the acceptance review

This ledger describes the original candidate at checkpoint
`5f0cb0511416e666b5476c078c0aaa86d9bce69e`, cumulative against `7774ed6`.
[Source metadata](source.json) identifies the corrected Rust/Cargo inputs,
validation and measured executables. The runner scaffold is not the candidate.
Acceptance and publication are recorded outside this tree under
[PUBLICATION.md](../../../PUBLICATION.md), allowing the reviewed index to be
committed unchanged without self-referential review or clone receipts.

The acceptance review returned **changes**. Its native receipt confirmed
`claude-opus-5-5` (requested alias `opus`); requested effort is not native-confirmed
effort. The reviewer and its workers had no shell. Full reports/native receipts
are retained in the external handoff. No reading-only verdict is described as
an independent test execution.

## QR and account-kind correction after independent Opus review

The next independent Opus review returned **changes** on the previously corrected
candidate. All four findings are valid; none is dismissed. Its suggestions are
checked against QR, imported sessions and reconnects, not treated as acceptance.

- Pre-login pushes (`LoginToken`, `SentPhoneCode`, `DcOptions`, `Config`,
  `LangPackTooLong`, `LangPack`, and conservatively `ServiceNotification`) remain
  deliverable but do not prove login. Peer-only batches also do not prove login.
  Non-PTS account updates and `UpdatesTooLong` still allow imported sessions
  without cached self metadata to initialize/recover.
- Account-shaped pushes provide provisional proof. An initial GetState 401 clears
  that proof and disarms GetState; queued account updates and reconnects cannot
  rearm it. The receiver waits for the existing one-second self-metadata checks.
  Cached-self or successful validated GetState confirmation remains durable, so
  subsequent authorization failures still reach callers. Invalid replies and
  storage failures do not confirm login or advance saved state.
- The first channel recovery reads unknown account kind once. Cached bots use
  limit 100,000; absent metadata uses the valid limit 100. The kind is stored only
  after the lookup finishes, so cancellation/storage errors can retry; cancellation
  later in the RPC preserves the resolved kind. Ordinary live delivery does no
  self lookup. Service-only backlog cannot indefinitely postpone login discovery.
- The evidence CLI returns status 2 with INCOMPLETE when privacy patterns are
  omitted, status 0 only for the complete checked invocation, and nonzero for
  failed checks. The independent-clone receipt explicitly tests this status.
- REVIEW-FINAL.md now points to this current ledger. Unknown channel 400/403/406
  errors retain PTS and sibling fairness but withhold that channel's live PTS
  updates while recovery keeps failing, logging a warning; CHANGELOG discloses it.

Regression fixtures cover seven service variants in all three batch envelopes,
non-PTS bot traffic, inferred AUTH_KEY_UNREGISTERED/SESSION_PASSWORD_NEEDED 401s,
queued updates across cancellation and reconnect, cached-self 401 propagation,
imported TooLong and channel recovery, and cached-bot limit selection retained
across cancelled recovery. These are local RPC fixtures, not observations of the
exact error Telegram returns during QR/2FA.

Source, current measurements and validation receipts are regenerated. Prior
second-acceptance measurements remain historical, bound to their archived source.
Final independent Astra and Opus receipts must identify the new exact tree;
acceptance of the previous tree does not transfer.

Primary protocol references: [updates before login](https://core.telegram.org/api/updates)
and [QR token completion](https://core.telegram.org/api/qr-login).

## Historical second acceptance review: corrections and dispositions

The second native Opus review also returned **changes**, identifying regressions
from its earlier recommendations. Its complete report and native receipt are
preserved externally; neither recommendation nor runner success was treated as
acceptance. This section describes the corrections to that reviewed source.

| Finding | Disposition |
|---|---|
| 1: imported sessions without self cache lose updates | Account updates establish provisional login proof, refined by the QR correction above. Connection-closed/malformed and pre-login service markers do not. Restored state alone still requires proof. `get_me` validates and explicitly caches identity, bot kind and available hash even with automatic caching disabled. Tests cover key-only imported-style sessions, Common reconnect recovery, channel deadline recovery, PTS retention and future live delivery. |
| 2: session errors erase channel PTS | AUTH_KEY, SESSION, CONNECTION and API_ID error prefixes propagate with original errors/state, as do auth/flood/transport failures. Only documented channel-specific names discard a position. Unknown 400/403/406 and ordinary 500 errors end the attempt temporarily with PTS intact, allowing the next channel to progress. Tests cover AUTH_KEY_DUPLICATED 406, API_ID_INVALID 400, three unknown status codes and sibling progress. |
| 3: per-batch self lookup | The receiver consults self metadata only when initialization/recovery lacks proof, at most once per second across cancelled calls. Confirmed proof lasts for its lifetime; provisional proof can be rejected. Unknown account kind is resolved once on channel recovery. Ordinary live delivery does not read self metadata; tests inject lookup failures and count reads. A new SQLite component measures 1,000/100,000 peers with 256 deliveries after idle-boundary yields; source/control/binary receipts are retained. |
| 4: post-delivery self lookup error | Resolve self identity from cache or users.getUsers and cache it before either sendMessage or sendMedia. Keep InputPeerSelf on the wire. Short-sent/unmapped responses use the prepared identity. Public-path RPC-fixture tests cover 16 cache/hash/media/reply combinations plus lookup failures with zero mutating requests. This is a test-only RPC seam, not a transport integration test. |
| 5: missing large-reply memory disclosure | Publish both historical 1 MiB RSS figures (90,504/15,052 KiB at 100 calls/s; 353,152/71,652 KiB at 2,000/s), their responder-inclusive scope and baseline half-failure caveat. Current reruns report absolute RSS as well; ratios do not compare equivalent successful work. |
| 6: incomplete evidence gate | Check all 42 scenario executions, configuration, source/binary identity, successful named tests/process exits and parsed metrics against samples. Bind retry-pass and SQLite component logs to their build receipts and parsed samples. Missing/empty privacy patterns cannot produce a complete PASS; remote-clone publication checks require the retained patterns. Negative tests exercise stale identities, malformed/duplicate markers, missing/extra/failed logs and altered metrics. |
| 7: stale current-status text | Historical ledgers are labelled historical; exact-tree acceptance/publication receipts remain external under PUBLICATION.md. |

Additional low-priority findings:

- Narrow the entropy guarantee: handshake RNG and EncryptedBuilder setup are
  fallible; obfuscation and packet padding still panic if entropy fails.
- SRP B equal to or greater than p takes the existing one-time parameter refresh
  path. Tests cover equality, greater-than and rejection of bad replacement data.
- GuestChatQuery remains Raw if its main or any reference message is peerless,
  preserving the complete update and state.
- The encrypted sender replay fixture uses state-mutating bad_msg code 32 and
  checks a subsequent ACK-only packet's sequence advanced exactly once. It also
  replays a completed RPC while another request remains live.
- **Conservative deferral:** an auth import begun without a target key remains
  uncached. Caching a key first observed after the await could attribute the import
  to a replacement key. Normal file recovery begins after a target-DC RPC whose
  initialization persisted the key; direct internal calls can perform one extra
  export/import once a new key becomes stable. Ongoing external key mutation is
  outside that one-extra-call bound. Existing finite file-recovery limits remain.

After the QR correction, unknown account kind is read once on channel recovery.
Cached bots use limit 100,000; absent self metadata uses the valid limit 100.
Ordinary live delivery still performs no self read. Completely empty batches are suppressed while peer-only batches
remain deliverable, and a busy pending key cannot indefinitely postpone recovery. Independent Astra
reading found that deliverable Config/peer-only batches could still starve catch-up.
A persisted live/recovery preference now alternates those opportunities; regression
tests cancel two recovery attempts without consuming the remaining live backlog,
then complete recovery and confirm live delivery resumes.

## Historical first acceptance review dispositions

| Finding | Correction and evidence |
|---|---|
| 1: repeated missing-hash grace | Exhaustion survives channel-entry removal for the receiver lifetime. Later episodes end on their first missing hash; usable access or successful recovery resets it. Fake-clock tests cover exact 999 ms/1 ms cadence, a second episode and counter resets; receiver tests cover another episode and mid-grace hash arrival. First-grace loss is disclosed publicly. |
| 2: immutable reviewed tree | PUBLICATION.md defines external receipts, exact-tree review, unchanged-index conventional commit, normal push, remote-SHA comparison and fresh remote-clone verification. Checked-in reports describe historical facts rather than a changing acceptance status. |
| 3: channel errors wedge siblings | The broad status-code classification introduced here was corrected in the second review above: only explicit channel names discard PTS; unknown rejections preserve it. |
| 4: ChannelTooLong skips backlog | Use a positive hint only when local PTS is absent; smaller, zero and larger hints preserve an existing position. |
| 5: cancellation loses batch | Buffer before peer persistence can suspend and retain interrupted cache work for resumption. The storage fixture verifies ordered, single delivery, persisted hash and saved state after cancellation. |
| 6: send-reply panic | Unmappable Updates return one missing mapping per input through the existing fallback, without resending. Self identity is now resolved before mutation, as corrected in the second review above. |
| 7: cached analysis evidence | Check/Clippy/Rustdoc use a new initially empty analysis target. The gate requires workspace Checking and Documenting/Generated output; negative tests reject empty/cached-only logs. |
| 8: stale gate and identity | Delete the unhashed gate output/exemption. Print full HEAD, branch, staged tree and manifest hash; add --expect-tree and explicitly report unconstrained identity. Final clone receipts are external. |
| 9: public limits | README links CHANGELOG's generated-TL process-abort risk, receive-arm-only deferral, measured correctness costs and legacy factorize panic. |
| 10: benchmark scope | The current measurement directory records a fresh executable and scenario runs. Historical post-Opus2 warm/cold/1 MiB omissions are explicit. |
| 11: coverage claim | Correct the historical decoder-level claim. New Full transport/encrypted sender fixtures replay consumed bad_msg and completed rpc_result, plain and gzip, while a third invocation stays live, then completes and releases capacity. This is not a live network replay. |

Additional corrections preserve peerless channel edits as Raw, saturate sequence-33
correction at zero without dropping siblings, initially gated recovery on cached self (corrected by the second review above), retire failed/wrapped internal salt requests, and bind copied authorization
to the DC auth key. A ready-only retry queue removes repeated deferred-prefix and
minimum-deadline scans. Its component benchmark measures retry passes, separately
from terminal sorted-vector removal and full catch-up throughput.

Handshake entropy failures return a typed error; sender construction uses additive
try_finish while legacy finish retains its entropy-failure panic. SRP checking and
proof computation use at most four blocking jobs; started work retains admission
after cancellation. B encoding follows the reference's 248..256-byte bound and
0 < B < p. SRP B is verifier-mixed, not raw DH g^b: blindly applying the
raw-authorization-DH interval is not justified. This does not claim a complete SRP
arithmetic/independent-entropy redesign or rejection of every leading-zero encoding.

Liveness remains a complete-valid-packet deadline: counting partial bytes permits
indefinite postponement by trickle traffic. Public docs explain large/slow-transfer
sizing or disabling this opt-in limit. Legacy Io retry docs now match AutoSleep's
exact whitelist and delivery limitation.

## Verification and limits

[Current validation](validation/README.md) links commands, counts and raw logs.
[Current measurements](performance/post-qr-correction/README.md) identify the
source/binaries and local throughput, tails, CPU, RSS and allocation scope.
Historical cached analysis logs are not fresh analysis of this source. No hosted
Windows run, account authorization, live Telegram stress, consumer change or
deployment is claimed.

Three correction workers were requested as gpt-6-astra/xhigh without downgrade.
Their shell/edit calls were rejected; the parent applied patches and ran checks.
Cross-reading exposed cache-resumption and self-peer fallback cases. These scoped
inspections do not replace independent Opus acceptance of the final original-path
candidate, recorded externally.

Generated-TL recursion and receive-arm-only budgeting remain explicit deferrals.
Exhausted-channel IDs consume memory until access/success clears them and are not
persisted. Terminal entry removal retains sorted-vector costs. Local measurements
do not establish a general optimization or attribute historical losses to Tokio.

## Primary sources

- [Session-wide error meanings](https://core.telegram.org/api/errors).
- [Channel difference errors](https://core.telegram.org/method/updates.getChannelDifference).
- [ChannelTooLong PTS semantics](https://core.telegram.org/constructor/updateChannelTooLong).
- [Telegram SRP](https://core.telegram.org/api/srp) and the
  [pinned TDLib reference](https://github.com/tdlib/td/blob/4d06d1ba3a1978476fe2b6575de8388439f6baa3/td/telegram/PasswordManager.cpp).
