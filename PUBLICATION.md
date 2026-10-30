# Reviewing and publishing one immutable candidate

Acceptance and publication receipts are recorded **outside this tree**. Checked-in
reports describe their historical observations; no file in this tree is a claim
that a later review, commit or push has happened.

1. Finish source, documentation, tests and measurements. Stage every intended
   public artifact and regenerate the source and artifact manifests. The artifact
   manifest excludes only itself; gate output is external.
2. Record the original checkout path, branch, full checkpoint HEAD, staged tree
   from `git write-tree`, and SHA-256 of `final/source-inputs.json` in an external
   receipt. Run `scripts/check-optimization-evidence.py` with `--expect-worktree`,
   `--expect-branch`, `--expect-head` and `--expect-tree` set to those exact values.
   Supply the retained executables and external privacy patterns; preserve both
   the patterns and the scan output for reviewers. Require exit status 0 and
   the complete PASS; omitted privacy patterns return INCOMPLETE with status 2.
3. Repeat the evidence check in an independent upload-pack clone without object
   alternates or hardlinks, overlaying the complete staged snapshot before any
   commit. Require the same tree and manifest. Keep its receipt outside the
   candidate to avoid a self-referential hash. Exercise a deliberately wrong tree.
4. Obtain independent GPT-6 Astra and Claude Opus 5.5 acceptance of that exact
   source and evidence. The Opus receipt must confirm the original absolute path,
   branch, full HEAD, staged tree and source manifest, and distinguish direct
   verification from parent-reported facts. Record execution/capacity limitations;
   a model receipt or a local test pass is not acceptance.
5. Only under publication authorization, commit the **unchanged index** with a
   conventional subject. Require `git rev-parse HEAD^{tree}` to equal the accepted
   tree. Any tree change requires renewed checks and acceptance. Existing checkpoint
   history is preserved.
6. Push the original reviewed branch normally, never the runner scaffold or by
   force. Compare the full remote SHA from `git ls-remote` with the local commit.
   Clone that remote branch afresh, verify HEAD and tree, and run the evidence
   gate with `--expect-head`, `--expect-tree` and the retained external
   `--privacy-patterns`; require the complete privacy-checked PASS and preserve
   both the patterns and result externally. Require exit status 0 as well as PASS.
   Publish the branch and commit links only with this observed remote evidence.

Receipts may alternatively be added in a separate, explicitly identified follow-up
commit that changes no files under `benchmarks/2026-10-30/final/`; such a receipt
commit is distinct from the accepted implementation commit. This procedure does
not authorize a commit or push when the active task explicitly prohibits it.
