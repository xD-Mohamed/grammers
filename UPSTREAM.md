# Maintaining this fork

Canonical upstream: https://codeberg.org/Lonami/grammers

Initial Codeberg baseline: `82eba650098b54e4c2cb9de83fdab7ea2d21a957`.

The working `main` branch comes directly from Codeberg, with its full history
and licenses intact. This is an independently maintained Git fork hosted on
GitHub; GitHub does not support native fork relationships to Codeberg repositories.
Codeberg is the source for future updates, using the commands below rather than
GitHub's **Sync fork** button. The archived GitHub repository is not used.

## Remotes and branches

- `origin`: https://github.com/xD-Mohamed/grammers.git
- `upstream`: https://codeberg.org/Lonami/grammers.git
- `main`: our working branch, tracking `origin/main`.
- `upstream/master`: canonical upstream development.

## Bring in upstream changes

Start with a clean working tree. From this checkout:

```sh
git switch main
git fetch upstream --tags
git log --oneline main..upstream/master
git merge upstream/master
cargo test --workspace
cargo check --workspace --all-targets
git push origin main
```

Resolve any merge conflicts and finish validation before pushing. Keep fork
changes in small, focused commits so upstream updates remain easy to review.
Performance changes need release benchmarks and integration checks; successful
compilation alone does not establish lower CPU use or better latency.

The original Apache-2.0/MIT licensing and attribution are preserved. See
[PERFORMANCE.md](PERFORMANCE.md) for the fork's implementation changes, measured
microbenchmarks and validation limits.
