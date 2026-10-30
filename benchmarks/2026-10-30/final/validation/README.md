# Current local validation

[qr-correction-commands.json](qr-correction-commands.json) records execution against
[the current source](../source.json), using Rust/Cargo 1.99.1, Linux x86_64,
locked offline dependencies and one Cargo build job. Source and checker hashes
were verified before and after each command. These are parent-run checks;
reviewer execution and hosted Windows CI are not claimed.

| Command group | Result |
|---|---|
| [fmt](qr-correction-fmt.log) | Passed |
| [check](qr-correction-check.log) | Passed |
| [clippy](qr-correction-clippy.log) | Passed |
| [rustdoc](qr-correction-rustdoc.log) | Passed |
| [workspace](qr-correction-workspace.log) | 689 passed, 44 ignored |
| [client-minimal](qr-correction-client-minimal.log) | 52 passed, 0 ignored |
| [debug-mentions](qr-correction-debug-mentions.log) | 2 passed, 0 ignored |
| [python](qr-correction-python.log) | 28 passed |
| [session-memory](qr-correction-session-memory.log) | 55 passed, 3 ignored |
| [session-import](qr-correction-session-import.log) | 77 passed, 3 ignored |
| [session-string](qr-correction-session-string.log) | 62 passed, 3 ignored |
| [session-files](qr-correction-session-files.log) | 97 passed, 3 ignored |
| [debug-self-response](qr-correction-debug-self-response.log) | 1 passed, 0 ignored |

Check, Clippy and Rustdoc ran sequentially in a newly created, initially empty
analysis target, separate from the ordinary test target. Retained logs contain
workspace Checking, Documenting and Generated output. The gate rejects empty or
cached-only analysis logs. Shared dependencies between these commands are explicit.

Session configurations disable defaults and add only the named optional feature.
Ignored benchmarks/network tests do not count as passes. New receiver regressions
also cover pre-login service pushes in three envelopes,
inferred 401 rejection without RPC storms, non-PTS bots, imported TooLong, and
first-recovery bot limits retained across cancellation. Earlier regressions
cover imported-style key-only sessions, Common reconnect and channel deadline
recovery, confirmed proof/self-read counts, unknown/session errors preserving PTS,
negative lookup cadence across cancellation, peer-only batches and mixed live
traffic unable to starve recovery. Client fixtures exercise real public send/get_me
paths with a test-only RPC responder, including failed preflight with zero mutating
requests. These fixtures do not execute transport/retry machinery. Stateful code-32
sender replay checks a subsequent encrypted packet sequence exactly once while
another invocation remains live. SRP refresh and peerless guest cases are covered.

## Historical evidence

[The second acceptance correction](acceptance-r2-commands.json) passed 684 tests
with 44 ignored, bound to [its archived source](../performance/post-acceptance-round2/source/source.json).
Those commands and logs are historical, not current validation.

[The first acceptance correction](acceptance-r1-commands.json) passed 673 tests
with 43 ignored, bound to [its archived source](../performance/post-acceptance-round1/source/source.json).
Its commands and original logs are retained, not presented as current execution.

[acceptance-commands.json](acceptance-commands.json) and
[opus2-commands.json](opus2-commands.json) both bind the archived
[post-Opus2 source](../performance/post-opus2/source/source.json), with 651 passed
and 42 ignored. Their cached analysis logs do not establish fresh analysis of
the current source. [The preliminary execution](opus2-preliminary.json) was
excluded from final validation because source changed during it. Its logs remain
historical records, not a substitute for the current fresh-target run above.

Historical [604-test validation](commands.json) and
[routing validation](routing-commands.json) bind to `performance/measured-source`.
The [631-test first Opus correction](opus-commands.json) binds to
`performance/post-opus/source`. The
[negative scheduling control](fairness-negative-control.json) intentionally
failed its regression on altered historical source and remains a separate
causal control, not a failure of the current suite.

[The historical clone proof](opus2-portability.json) and
[historical staged inventory](opus2-index-audit.json) identify their earlier
snapshots and exclusions. Complete final index, privacy, clone, review and
publication receipts live outside this tree under
[PUBLICATION.md](../../../../PUBLICATION.md); they include all final public
artifacts without a self-reference exemption. The gate log is also external.

No live Telegram traffic was repeated. Earlier no-login test-DC probes remain
historical evidence and do not establish production compatibility.
