# Current correction evidence

The candidate is checkpoint `5f0cb05` plus the correction identified by
[source metadata](source.json), [Rust/Cargo inputs](source-inputs.json) and
[the exact source patch](source.patch). See the historical
[second Opus correction ledger](../../../REVIEW-OPUS-ROUND2.md).
The [acceptance correction ledger](acceptance-review.md) records later source,
tests and evidence corrections. Acceptance/publication receipts are external
under [PUBLICATION.md](../../../PUBLICATION.md).

- [Validation commands and logs](validation/README.md) identify current and historical runs.
- [Current local measurements](performance/post-qr-correction/README.md) cover current source;
  earlier scheduling controls and comparisons remain historical.
- [Canonical upstream audit](upstream/README.md) preserves official source evidence
  and intake dispositions for the 17 recorded commits.
- [Historical scoped Astra source inspection](astra-opus2-source-review.md) and
  [gate inspection](astra-opus2-evidence-review.md) describe their execution limits.
- [Native receipts](provider-receipts.json) retain Opus's changes verdict and the
  distinction between native model identity and requested effort.

After staging the intended source and evidence, run:

```sh
python3 scripts/check-optimization-evidence.py
```

The gate exports the index, checks live/index equality, complete source/artifact
membership, local links, isolated full-index diffs, named current validation
scope/counts, source/tooling hashes and current benchmark receipt consistency.
Paired benchmark logs must contain exactly one successful result for the named
test. Current heap/allocation and deadline-component runs receive the same
checks, with exact log membership and separate baseline/control hashes. Current
scenario, retry-pass and SQLite receiver receipts are also checked against all
named logs, binary markers, configuration and parsed samples.
It checks every `*commands.json` under this measurement directory, including
historical component receipts. Other historical summaries remain hashed artifacts,
not newly executed validation. Extra schema/build inputs must match the checkpoint.
Ambient toolchain and machine state are documented by execution receipts, not
made reproducible by the repository-content gate.

Optional `--expect-worktree`, `--expect-branch`, `--expect-head` and `--expect-tree` constraints
bind a review to a selected candidate. Without them, ordinary main/detached
checkouts are supported. `--verify-upstream` verifies canonical Git objects;
first run the pinned fetch command in `upstream/source.json` when needed.
Without this option, the gate checks saved upstream patch hashes only.

`--privacy-patterns PATH` accepts an external JSON list of `label`/`regex`
objects and scans all tracked text. Keeping patterns external avoids publishing
local execution paths. `--baseline-binary PATH --candidate-binary PATH` checks
retained executable bytes against current build/measurement receipts. Without
these options, the output explicitly states the checks that were not performed.
Without privacy patterns the final status is INCOMPLETE and exit status is 2,
never PASS/0. Empty or invalid patterns fail validation with nonzero status.
No pattern scan proves the absence of every possible secret, and receipt/hash
consistency alone does not establish source-to-binary compilation or review acceptance.

[artifact-hashes.json](artifact-hashes.json) includes every final artifact except
itself. Gate output is external. Historical pre-Opus source is preserved in
`performance/measured-source`; the first Opus correction source is preserved in
`performance/post-opus/source`; the second correction is archived in
`performance/post-opus2/source`. External receipts bind the full reviewed tree.
