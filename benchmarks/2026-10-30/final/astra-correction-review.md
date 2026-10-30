# Astra correction inspection

Three source workers were explicitly requested as `gpt-6-astra` with `xhigh`.
They read the original candidate's protocol, sender and update paths. Native
shell and write tools were rejected, so the parent applied their proposed
patches and executed every test. This is not independent test execution.
The parent supplied fresh Git identity for checkpoint `5f0cb05`; workers could
read original-path source but could not independently execute Git.

Follow-up inspection caught a shortened channel timeout hidden behind the
recomputed deadline cache. The parent added the correction and a deterministic
regression. Returning the historical expired timestamp remains compatible while
the internal cache excludes deferred/ready work appropriately. After that fix,
the update worker found no additional new deadlock, lost gap, sibling-loss or
response-target defect in the inspected paths. Protocol inspection found no
additional issue after replay/time fixes and the packet tests.

These are scoped correction inspections, not final whole-candidate acceptance.
The new cumulative candidate requires Opus review after final evidence refresh.
Native completed-turn model receipts belong to the outer runner; requested
effort is not asserted as native-confirmed effort.
