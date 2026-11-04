# Compatible upstream ideas: focused continuation, 2026-11-04

Baseline: `b48890ddf5186632ff7cb7835a51000e619337bb`, initially both local and
remote `main`, with a clean working tree. Work branch: `codex/upstream-ideas-20261104`.
Canonical Codeberg `HEAD` and `refs/heads/master` were checked again with
`git ls-remote`; both still point to `7b414b1d28537f4c354629c3ceeabe771a86f3b4`.
This continues the completed intake; it does not repeat the twenty-commit review.
Source commits, authors and patch hashes remain in [source.json](source.json).

The owner's clarified criterion is the useful idea implemented within this
fork's interfaces, semantics and performance requirements. An upstream API
difference is a design constraint, not by itself a reason to reject the idea.

## What changed

**`3122892`: make header serialization easier to optimize.** The useful purpose
of upstream's slice sinks is bulk copying instead of individual byte writes.
One independent part of that patch keeps `StackBuffer`'s cursor in a local
variable until extension finishes. Applying just that part to our existing
`Extend<u8>` implementation lets LLVM combine integer-byte writes into wider
stores. The buffer stays private, stack allocated and the same size; the two
existing encrypted-packet/container header call sites keep their serializers.
There is no added allocation, public method, generated code or runtime dispatch.
Upstream credit: HBcao and Lonami Exo, commit
`3122892a965a0c9d3a06b8eb291129a9899d3c71`.

Keep `Serializable::serialize(&mut impl Extend<u8>)`, downstream trait
implementations/sinks and `to_bytes` unchanged. Generic `DequeBuffer` appends
already delegate to `Vec::extend`; retaining that route preserves specialization
and arbitrary iterators. The earlier full byte-string sink probe has mixed
results across sizes and does not support a blanket replacement. The cursor
adaptation obtains a specific measured benefit without a second serialization
API or parallel generated implementations.

**`7b414b1`: delete traits redundant in upstream's new design.** There is no
independent protocol feature, correctness fix or allocation reduction in deleting
these impls. They remain used in this design. Preserve both public `DequeBuffer`
extensions (`T` and `&T`) and the private stack extension. Its small transport
production change merely changes how the same CRC bytes are appended.

**`6ce4c54`: avoid a TTL-builder overflow panic.** Expose the benefit through
additive `InputMedia::try_media_ttl` and `InputVideo::try_ttl`, both returning
`Result<Self, TryFromIntError>`. They use the existing checked integer conversion.
The old setters and conversion bodies are unchanged. Preserve optional TTLs,
Unlimited=0, ViewableOnce=`i32::MAX`, whole-second duration mapping and the
documented ordering of `media_ttl` before setting the media. Reject overflow;
do not silently clamp or turn an explicitly supplied zero into an absent field.
Legacy callers keep their exact contract; callers handling untrusted durations
can opt into errors without catching panics. This adapts the purpose of Lonami
Exo's `6ce4c54c0ad712a6f91f6344119f58be105f9533`, rather than its API/semantics.

## Audit of the completed intake and other deferred ideas

- `61cc9ff` only adds `MsgId::into_inner`. `b48890d` adds `upload_media` and uses
  the same response conversion in albums. Comparison against their parents
  found no removed/renamed public signature. Album upload order, error types,
  media conversion, peer/business fields and result cardinality are retained.
  No predecessor API defect was found that required a correction.
- Keep the fork's DH implementation: exact-size/range/residue and safe-prime
  checks before arithmetic, published-prime subgroup checks, fallible entropy
  and bounded blocking jobs already cover `d2c6e84` more strictly.
- `a984cac` seeks reusable I/O-independent framing and easier testing. Our
  generic MTP/Transport interfaces and synthetic/local I/O tests already cover
  the present use cases. The independent parsing helpers correspond to
  `sender.rs::deserialize_updates_like` and `deserialize_request`, including constructor
  checks. Relocating framing/buffer ownership would add no identified behavior
  here. A future alternate-I/O runtime could justify a dedicated compatible
  design; it is not implemented by this continuation.
- `4101f5e` enables runtime custom framing in pools. The public low-level
  `connect` and `connect_with_auth` already take generic transports. Our static
  pool transport and per-connection `ProxyTransport` cover current framing and
  secret-dependent MTProxy setup. A high-level custom-transport factory remains
  a possible additive feature, but no additional protocol needs it here. Do not
  add per-connection boxes/default-path dispatch merely for parity of structure.
- `84cfb99`/`d654b04` separate encryption/padding/buffer ownership and move a
  public type. Our encryption already works in place in the cipher backend and
  uses minimal valid padding. No eliminated copy or required capability was
  identified. Keep that architecture and both existing public exports.
- `e564eef`/`4ca2263` add/document explicit back-slice append. Its purpose is
  already served by the Vec-delegating extensions for contiguous input; the
  measured gain here is in the private stack cursor. No additional append API
  or unrelated call-site rewrite is necessary.
- Layer 232 adds schema functionality, not just an API style. Adopting its wire
  changes requires a separate versioned schema/compatibility intake, generated
  depth/size checks and refreshed method documentation. Layer 230 is retained;
  no partial schema or misleading layer-number bump is made.

Proxy/network, replay/recovery, DH, TL generator/schema and album source files
are byte-identical to the baseline. No dependency or Cargo manifest changes were made.
The [main decision table](README.md#disposition-of-every-commit-after-the-integration-ancestor)
now judges these purposes explicitly rather than treating an upstream API break
as the reason to discard a useful behavior.

## Component measurements

[header-probe.rs](header-probe.rs) uses the actual unchanged TL serializers and
current `StackBuffer`, paired with the exact pre-change cursor implementation
from the baseline. Both variants are in one optimized binary with the same
inputs and function boundaries. It checks exact bytes for zero, signed extrema
and other values before timing. Scalar fields model header construction without
adding a tuple-memory layout dependency. Rust 1.99.1 / LLVM 23.1.1, x86-64 Linux,
CPU 0 affinity; one warmup per variant, 15 alternating-order pairs, ten million
operations per sample. No concurrent build/benchmark ran during the final probe.

| Header | Baseline median ns | Current median ns | Paired observations |
|---|---:|---:|---|
| 16-byte salt/session | 12.650 | 1.728 | Current faster in 15/15; paired reductions 85.2–88.4% |
| 24-byte container | 2.391 | 2.381 | Within noise: 8/15 wins; paired changes −5.4% to +2.9% |

[Raw samples](header-probe.csv) include function-call/black-box overhead. These
are component measurements on one compiler/host, not end-to-end CPU, RPC
latency or live throughput results. No container speedup is claimed. Generated
assembly confirms wider writes; its focused excerpts and reproduction command
are in [header-codegen.txt](header-codegen.txt). No heap-size change is claimed.

```sh
rustc --edition=2024 -O benchmarks/2026-11-04-upstream/header-probe.rs -o /tmp/grammers-header-probe
taskset -c 0 /tmp/grammers-header-probe
rustc --edition=2024 -O --emit=asm benchmarks/2026-11-04-upstream/header-probe.rs -o /tmp/grammers-header-probe.s
```

## Validation and limits

[Exact commands, exit codes, durations and log hashes](continuation-validation.json)
record 168 passing focused test executions: 116 protocol, 29 local network,
five album/response mapping, seven TTL and two external API tests, with the nine
TTL/API checks also run without client default features. Six ignored protocol
tests were not run. The earlier 781-test full-workspace receipt remains scoped
to the predecessor; this continuation does not repeat it.

TTL tests compare legacy/new serialized photo/video bytes, preservation of
builder fields, optional clearing, zero, subsecond truncation, reserved values,
maximum representable seconds and overflow errors. Existing panic behavior is
retained. The external test compiles a downstream `Serializable` implementation,
an Extend-only sink, owned/borrowed arbitrary `DequeBuffer<u16>` iterators and old
TTL call patterns. All-target/all-feature workspace check compiles the existing
examples, including albums/proxies and managed/legacy client use. Formatting,
strict workspace Clippy and warning-free workspace Rustdoc pass.

The 29 localhost tests cover HTTP/HTTPS CONNECT, SOCKS4/5 and raw/dd/ee MTProxy,
including rejection/cancellation paths. There is no direct fallback added.
No real Telegram session, ignored live test, consumer deployment, hosted CI,
schema migration or force push was used. All work was performed on GPT/Codex.
