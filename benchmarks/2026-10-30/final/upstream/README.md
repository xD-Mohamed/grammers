# Canonical Codeberg source verification

On 2026-10-30, `git ls-remote` against the canonical HTTPS Git endpoint returned
`1e31e73cd9797790905bbea82cd75adcbe6f1242` for `refs/heads/master`. Fetching
that exact commit with `--no-tags --no-write-fetch-head` succeeded. The web-page
reader failed for the same commit URLs; attribution below uses the fetched Git
objects, not the failed page reads. No remote configuration, integration branch,
schema, package version or upstream history was changed by this verification.

[source.json](source.json) contains the commands, observed tip, complete
17-commit inventory since integrated ancestor `633d7fe`, parent/tree identities,
changed paths and SHA-256 of each `git show --format=fuller --binary
--no-ext-diff COMMIT`. The four specifically challenged diffs are saved here:
[accounting](ad7b472.patch), [request validation](1e31e73.patch),
[buffer relocation](d654b04.patch), [layer 232](dfffb71.patch).
They retain upstream attribution; the repository's Apache-2.0/MIT licensing applies.

## Attribution checked against source

`ad7b4725` adds an encrypted-data ceiling derived from `0xff000`, subtracting
key/message-key, salt/session, padding and container/message overhead. It makes
message serialization fallible, including service messages. The fork adapts
that accounting with its actual 27-byte maximum padding and its update wrapper;
it does **not** copy upstream's outbound-derived inbound assertion or
`mem::take(pending_ack)` before fallible serialization. The exact-limit request,
ACK deferral and valid 1 MiB inbound tests cover those fork differences.

`1e31e73` rejects oversized typed/raw requests before creating the response
channel instead of relying on the framer assertion. The fork keeps its existing
`InvalidInput` category, constructor/alignment checks, shared and tracked entry
points, and the existing `Mtp::push` signature. This is adaptation, not a merge.

`d654b04` is a 100%-similarity move of `DequeBuffer` from crypto to MTProto,
plus import/export changes. Its predecessor `84cfb99` changes public
`encrypt_data_v2` from a `DequeBuffer` mutator to a padded-slice function returning
the message key. Moving the type alone would leave crypto dependent on a crate
which already depends on crypto. Adopting that chain requires an explicit
public-API compatibility design; it is deferred, not a rejected speedup.

`dfffb71` changes the actual schema to layer 232. It changes nine existing
constructor signatures (channel, channelFull, userFull, urlAuthResultRequest,
messageReplyHeader, inputReplyToMessage, wallet.nftItem,
messages.acceptUrlAuth, payments.getStarGiftWithdrawalUrl), adds 25 constructors/
methods, and changes handwritten channel/reply initializers. In particular,
getStarGiftWithdrawalUrl replaces `InputCheckPasswordSRP` with `tmp_password:bytes`.
A layer-number-only update would be wrong. This release retains layer 230;
layer 232 needs generated API/constructor compatibility, docs regeneration and
its own review. The saved source establishes the concrete deferred scope.

## Disposition of every newer commit

| Commit | Disposition and reason |
|---|---|
| `6ce4c54` | Defer the TTL API change: it removes `Option<TimeToLive>`, changes Unlimited from 0 to absent and clamps durations. The fork explicitly preserves that existing public signature/behavior. This is a compatibility decision, not measured performance rejection. |
| `5b80079` | Defer upstream manifest bumps; the fork already has its separately tested locked dependency refresh. Do not silently combine graph changes with protocol and fairness comparisons. |
| `15b7afa` | Defer upstream v0.11 package labels; this fork has additional APIs and is not that crates.io release. |
| `dfffb71` | Defer layer 232 with the concrete constructor/API scope above; keep schema and advertised layer together. |
| `a984cac` | Defer sans-I/O extraction. It moves connection ownership/framing between crates and changes error/update helpers; adapting SDK-owned permits, cancellation and delivery certainty needs a separate compatibility design and measurements. No runtime-replacement benefit is established. |
| `e564eef` | Defer the explicit slice append helper. Current generic extension already delegates to Vec; no A/B of this replacement was done. The upstream padding call differs from this fork's existing minimal-padding implementation. |
| `4101f5e` | Defer configurable boxed transports. It extends configuration and is based on the sans-I/O extraction; no current correctness requirement needs a new public transport representation. |
| `c8d6979` | Defer the independent MsgId accessor to an API intake; no current framing or error fix needs it. |
| `07b51ab` | Defer the cosmetic spelling fix: the two existing `canont retry request` log strings remain. This has no behavioral effect; it is not claimed as applied or as a performance change. |
| `98b7b88` | Defer Debug derivation on body-bearing response types; review redaction/privacy before expanding diagnostics. |
| `dad5581` | Not applicable independently: this fork still uses crypto directly in the sender. Removing the dependency would break compilation. |
| `84cfb99` | Defer the public crypto buffer/signature changes with the relocation chain described above. |
| `d654b04` | Defer relocation with compatibility and dependency-cycle rationale above; no claimed allocation/CPU win. |
| `4ca2263` | Defer documentation for the unadopted `extend_back` API alongside its introduction. |
| `58f7bd1` | Retain explicit failure for empty/incomplete albums and unsupported returned media. Upstream instead filters entries, changing result cardinality. Public `upload_media` and clone removal are deferred API/performance candidates, not needed for panic removal and not benchmarked here. |
| `ad7b472` | Adapt packet accounting, preserving ACK state and independent inbound limits. |
| `1e31e73` | Adapt early sender validation using the fork's existing error and request APIs. |

The deferred entries are explicitly unevaluated integration/performance changes,
not evidence that they would be slower. Acceptance of these dispositions is part
of the independent review gate; this source audit does not imply blanket approval.

Reproduce after fetching the exact tip:

```sh
git fetch --no-tags --no-write-fetch-head https://codeberg.org/Lonami/grammers.git \
  1e31e73cd9797790905bbea82cd75adcbe6f1242
git log --reverse --format='%H %s' 633d7fe..1e31e73
git show --format=fuller --binary --no-ext-diff ad7b4725
git show --format=fuller --binary --no-ext-diff 1e31e73
git show --format=fuller --binary --no-ext-diff d654b04
git show --format=fuller --binary --no-ext-diff dfffb71
```
