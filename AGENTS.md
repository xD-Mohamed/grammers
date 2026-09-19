# grammers fork

This is a public library fork. Canonical upstream is Codeberg; see UPSTREAM.md
for remotes, branch roles and the update procedure. Keep upstream history and
Apache-2.0/MIT licenses intact. This is a cross-host Git fork with no native GitHub
fork relationship. The archived GitHub repository is not the update source.

The Cargo workspace includes client, session storage, sender, MTProto, crypto,
TL parser/generator/types and the grammers command-line crate. Follow the upstream
README and each crate's guide when changing that component.

Keep changes focused, preserve unrelated work, and verify them with relevant
tests plus `cargo check --workspace --all-targets`. Performance work requires
release benchmarks and clear distinction between local and live measurements.
Never add credentials, application session files, user inventories or unrelated
application source to this public repository. Push only when authorized by the
user's task. This checkout's publication policy is separate from sibling projects.

Initial setup copies the canonical upstream implementation without modifications.
Do not begin performance patches until the user requests them.
