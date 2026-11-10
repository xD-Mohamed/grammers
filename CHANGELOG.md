# Changelog

## Unreleased: smaller rich-message inputs

- `tl::enums::InputRichMessage::{Message, Html, Markdown}` now hold their existing
  payload structs in `Box`. Requests with no rich message add no allocation;
  populated rich messages add one. Wire and session formats are unchanged.
  `tl::functions::messages::SendMessage` shrinks from 920 to 472 B, which also
  keeps `grammers_session::updates::UpdatesLike` clear of Clippy's
  `large_enum_variant` without boxing the request of every sent message.
- Payload struct literals, `.into()`, field access and rich-message builders keep
  compiling. Direct enum construction must box the payload or use `.into()`;
  moving out requires `*value`.

  ```rust
  // before
  let rich = tl::enums::InputRichMessage::Html(raw);
  if let tl::enums::InputRichMessage::Html(value) = rich { consume(value); }
  // after
  let rich: tl::enums::InputRichMessage = raw.into();
  if let tl::enums::InputRichMessage::Html(value) = rich { consume(*value); }
  ```

## Unreleased: accurate first write timestamps

- `InvocationTracker::sent_at()` preserves the actual precise write time when
  the first concurrent write reports arrive out of order. Previously, a later
  write reported first could become the shared timestamp origin and make an
  earlier write appear to have happened at that later time. The origin is now
  initialized before returning the first tracked permit. This adds one initial
  clock reading per process; timestamps remain precise and require no API changes.

## Unreleased: smaller webpage and game values

- `tl::enums::WebPage::Page` and `tl::enums::Game::Game` now hold their existing
  payload structs in `Box`. This shrinks the containing media and message values
  without adding allocations for ordinary photos or documents. Full webpages and
  games each add one allocation; empty, pending and unchanged webpage variants
  retain their existing shapes. The TL wire and session formats are unchanged.
- Payload struct literals, `.into()`, field access and variant matches keep
  compiling. Direct enum construction must box the payload or use `.into()`.
  Moving out requires `*value`; destructure that owned value for nested patterns.
  `Update::WebPage`, `Update::ChannelWebPage`, `MessageMedia::WebPage` and
  `MessageMedia::Game` retain their inline wrapper structs.

  ```rust
  // before
  let page = tl::enums::WebPage::Page(raw_page);
  let game = tl::enums::Game::Game(raw_game);
  if let tl::enums::WebPage::Page(value) = page { consume(value); }
  // after
  let page: tl::enums::WebPage = raw_page.into();
  let game: tl::enums::Game = raw_game.into();
  if let tl::enums::WebPage::Page(value) = page { consume(*value); }
  ```

  The temporary outer boxes on `Update::WebPage` and `Update::ChannelWebPage`
  introduced at `acaf759` are removed: the full page now owns the only new box.
  Consumers of that intermediate revision restore inline wrapper construction
  and extraction:

  ```rust
  // at acaf759
  let update = tl::enums::Update::WebPage(Box::new(wrapper));
  if let tl::enums::Update::WebPage(value) = update { consume(*value); }
  // after (the same change applies to ChannelWebPage)
  let update = tl::enums::Update::WebPage(wrapper);
  if let tl::enums::Update::WebPage(value) = update { consume(value); }
  ```

## Unreleased: smaller raw updates

- Ten large `tl::enums::Update` variants now hold their existing payload structs
  in `Box`: `SentWalletTransaction`, `ServiceNotification`, `SentPhoneCode`,
  `DraftMessage`, `ChannelParticipant`, `NotifySettings`,
  `BotPrecheckoutQuery`, `PeerWallpaper`, `Theme`, and `ChatParticipant`.
  Ordinary message-update variants stay inline. The TL wire and session formats
  are unchanged. These ten variants add one allocation when present.
- Payload struct literals, `.into()`, field access, `Update::Variant(_)` matches
  and accessors keep compiling. Direct enum construction must box the payload or
  use `.into()`. Moving out requires `*value`; destructure that owned value for a
  nested struct pattern.

  ```rust
  // before
  let update = tl::enums::Update::ServiceNotification(value);
  if let tl::enums::Update::ServiceNotification(value) = update { consume(value); }
  // after
  let update: tl::enums::Update = value.into();
  if let tl::enums::Update::ServiceNotification(value) = update { consume(*value); }
  ```

## Unreleased: smaller reply headers

- `tl::types::MessageReplyHeader::reply_media` is now
  `Option<Box<tl::enums::MessageMedia>>`. Absent reply media reserves one pointer;
  ordinary replies add no allocation. The TL wire and session formats are unchanged.
  Callers constructing populated fields must box them; borrowed matches use
  `as_deref()`, and moving out of a populated field uses `*media`.

  ```rust
  // before
  header.reply_media = Some(media);
  if let Some(tl::enums::MessageMedia::Photo(photo)) = &header.reply_media { /* ... */ }
  // after
  header.reply_media = Some(Box::new(media));
  if let Some(tl::enums::MessageMedia::Photo(photo)) = header.reply_media.as_deref() { /* ... */ }
  ```

## Unreleased: the typed client path, and the kernel's coarse clock

[Measurements](benchmarks/2026-11-10-typed/README.md). Public types change in this section,
with the owner's approval; each break is listed with what to write instead.

- An update carrying a new or edited message no longer clones that message. One busy bot
  connection with updates handles 6% more requests per second at saturation with 5.7% less
  CPU per request, and every request costs two allocations and 2.7 kB less. At fixed
  rates the allocations are saved and the CPU does not measurably change.
- On Linux and Android, message identifiers, the update receiver's deadlines and the
  ageing of written peers read the kernel's coarse clock, the time of its last tick,
  where they read the precise clock before. **Where the precise clock is a system call**,
  as on a KVM guest without a vDSO clock source, one busy bot connection with updates
  handles 10% more requests per second at saturation with 8.9% less CPU per request, and
  the raw path spends 3% less CPU per request. Where the kernel serves the precise clock
  from the vDSO nothing measurable changes. Those readings can be one tick old, 1 to
  10 ms: `UpdatesReceiver::last_clock_read` returns one of them.
- `grammers_mtproto::coarse_clock::instant` and `unix_time` are new, and
  `grammers_mtsender::coarse_clock` re-exports them. `grammers-mtproto` depends on `libc`
  on Linux and Android, which every build already contained, and calls `clock_gettime`
  through it in one `unsafe` block.
- A request costs 1.75 allocations and 1.8 kB less on a bot connection with updates, with
  no measured change in CPU: the peers of a response are kept as a list when there are
  sixteen or fewer, responses without peers share one empty map, the reply to one sent
  message is sorted out without the maps an album needs, and a reply that holds updates
  is copied out of the read buffer once. `PeerMap::iter` and `iter_peers` now go through
  a few peers in the order they arrived.
- `mtp::Encrypted`'s builder has a new option, `own_updates_in_results`, off by default.
  A driver that turns it on is given `Deserialization::OwnUpdate` with an empty `update`
  and reads the update from the body of the `RpcResult` that follows. The connections
  this library makes turn it on.
- Decoding a request's reply once for its caller and for the stream of updates was built
  two ways and gained nothing: see the measurements. Nothing in the library changed for it.

### Breaking

- `update::Message` has no `raw` field. That field held the raw update, and so a second
  copy of the message. `Message::raw()` returns a `&tl::enums::Update` built by cloning
  the message the first time it is called, and `Message::into_raw()` returns it owned
  without cloning. `Update::raw()` still compiles and still returns a reference.

  ```rust
  // before
  let update: &tl::enums::Update = &message.raw;
  // after
  let update: &tl::enums::Update = message.raw();      // built on the first call
  let update: tl::enums::Update = message.into_raw();  // owned, no clone
  ```

  `update::Message` dereferences to `message::Message`, whose own `raw` field is the
  `tl::enums::Message`. Code that still writes `.raw` on an `update::Message` now names
  that field: it stops compiling where a `tl::enums::Update` was expected, and it changes
  meaning silently where the type is not checked, such as `{:?}` formatting.

  The old field was a copy taken on arrival. The rebuilt update is the message as it is
  when first asked for: a change made through `DerefMut` before that shows in it, one
  made after does not. The `Debug` output of `update::Message` no longer prints the raw
  update: it prints the message, the update's kind and its `pts`.

## Unreleased: the update path's clock

[Measurements](benchmarks/2026-11-10/README.md). Nothing that compiled before stops compiling.

- Receiving updates reads the clock once per batch instead of four times. Where the clock
  is a system call, as on a KVM guest without a vDSO clock source, one busy bot connection
  with updates handles 14% more requests per second at saturation with 12% less CPU per
  request; where the kernel serves the clock from the vDSO nothing measurable changes.
- `MessageBoxes::check_deadlines_at` and `MessageBoxes::process_updates_at` are new: the
  same as the methods without a time, at a reading of the clock the caller already has.
  `UpdatesReceiver::last_clock_read` is new and returns the receiver's last reading.
- `io_uring` was measured as a ceiling for the socket path and not adopted on Linux 5.15:
  see the measurements. Nothing in the library changed for it.

## Unreleased: TL layer 233

[Sizes, evidence and sites](UPSTREAM.md). The handwritten API is unchanged.

- The schema is layer 233, byte-identical to upstream `8fb8135`, and the advertised layer
  follows it: 35 new constructors, 17 new functions, 11 changed definitions.
- `Update` stays under its size guard: 760 B, where this schema alone made it 1,264 B
  (752 B at layer 230). `wallet.card` is held in a `Box`, and `wallet.nftItem`'s new
  `gift` is an `Option<Box<StarGift>>`. Both are new in this layer; wire format is unchanged.
- `updateMessageWidgetData` advances the common `pts` sequence, as in TDLib, Telegram
  Desktop and Telegram for Android. Unhandled, each would have been followed by a
  difference request. One naming a channel is rejected like `updatePinnedMessages`.
- `Media::from_raw` returns `None` for `messageMediaWidget`; the raw value stays on the message.
- Replies rebuilt for `updateShortSentMessage` carry the request's `quote_rich`.

### Fixed

- Bot updates carrying `qts` advance it: `updateMessagePollVote`,
  `updateBotBusinessConnect`, `updateManagedBot`, `updateBotGuestChatQuery` and
  `updateBotStarsSubscription` were delivered without doing so, and each was followed by
  a difference request that delivered it a second time. `updateBotDeleteBusinessMessage`
  is one event however many messages it names: one naming several was dropped until a
  difference returned it.

### Compatibility

- Generated types follow Telegram's schema. Code building `tl::types::{User, Channel,
  ChannelFull, UserFull, MessageReplyHeader, InputReplyToMessage, UrlAuthResultRequest}`
  or `tl::types::wallet::NftItem` with a struct literal needs their new fields, and so do
  calls to `channels.toggleForum` (`sidebar`), `messages.acceptUrlAuth`
  (`tonconnect_session`) and `payments.getStarGiftWithdrawalUrl` (`tmp_password` replaces
  `password`). Exhaustive matches on `Update`, `MessageMedia`, `InputMedia`,
  `MessageAction`, `BotInlineMessage`, `InputBotInlineMessage` and `WalletTransactionPeer`
  need the new variants.
- No variant that existed at layer 230 changed its payload.

## Unreleased: request path, and Codeberg upstream reviewed through e61cf85

[Measurements](benchmarks/2026-11-09/README.md). Nothing that compiled before stops compiling.

- AES-IGE uses AES-NI directly on x86-64 where the processor has it, about twice as fast,
  and incoming packets are decrypted and authenticated in one pass. Output is
  byte-identical; other targets and processors use the previous code.
  `grammers_crypto::aes::ige_decrypt_hashed` is new.
- Padding randomness is requested from the system for sixteen messages at a time.
- The request path reads the clock about three times per request instead of eight. The
  messages of one packet take consecutive identifiers.
- `Mtp::on_packet_sent` is new and optional: a driver reports when a packet was written,
  and acknowledgements queued afterwards count their delay from then, never later than
  before. A driver that reads `Mtp::service_deadline` must call `Mtp::on_service_timeout`
  when it passes; `Encrypted` then no longer checks the delay itself.
- A connection holds socket buffers only while a packet is in them. Grown buffers are
  released one to two minutes after the last large transfer, judged at keepalive ticks.
- `grammers-tl-parser`: `Definition`, `Parameter`, `ParameterType`, `Type`, `Flag`,
  `ParseError` and `ParamParseError` are `Clone` (idea from upstream `d16333b`).
- `grammers-session`: `tokio` is optional, enabled by `sqlite-storage` (a default feature)
  and `session-import-files`. The memory-only library no longer depends on it (idea from
  upstream `a852bec`).
- `grammers-mtproto` no longer depends on `bytes`, which it never used (as upstream `b8b4be1`).
- Reviewed all 10 upstream commits after `7b414b1`. Layer 233 was left for its own change,
  the section above: see [UPSTREAM.md](UPSTREAM.md) for each commit and the reasons.

## Unreleased: reviewed Codeberg upstream through 7b414b1

- Add `InputMedia::try_media_ttl` and `InputVideo::try_ttl` to report TTL overflow
  without panicking or clamping. Existing setters, `Option<TimeToLive>`, integer
  conversions and wire values remain unchanged (purpose adapted from `6ce4c54`).
- Improve private stack-header writes with a local cursor (from `3122892`),
  retaining `Serializable::serialize` and both public `DequeBuffer::Extend` impls.
  [Compatible adaptations and local measurements](benchmarks/2026-11-04-upstream/compatible-ideas.md).
- Add `MsgId::into_inner()` to correlate protocol identifiers with external
  diagnostics and custom transport integrations (upstream `c8d6979`).
- Add `Client::upload_media()` to resolve input media without sending a message
  (adapted from upstream `58f7bd1`). Albums use the same helper and continue to
  reject empty/incomplete inputs and unsupported upload responses explicitly.
- Review all 20 commits since the last full upstream integration, including three
  newer than the previous review. DH safety checks are already present; retain
  the existing serialization, crypto and transport APIs. Layer 230 is unchanged.
  [Disposition and local validation](benchmarks/2026-11-04-upstream/README.md).

## Unreleased: proxy transports

- The existing `proxy` feature, `ClientBuilder::proxy` and
  `ConnectionParams::proxy_url` now support HTTP/HTTPS CONNECT, SOCKS4 (plus
  SOCKS4a/5h numeric-target aliases), and MTProxy classic, dd and ee fake TLS.
  Existing SOCKS5 and direct connections remain supported.
- HTTP(S) supports optional Basic authentication with percent-decoded UTF-8
  credentials. CONNECT response parsing is bounded, handles partial replies
  and keeps subsequent tunnel bytes. Proxy errors never fall back to direct.
- HTTPS verifies the proxy's certificate and server name against bundled Mozilla
  roots. Fake TLS authenticates the MTProxy secret; it is a different protocol.
- Proxy URLs are redacted in `ServerAddr` debug output and omitted from errors.
- `ProxyTransport` supplies MTProxy framing for low-level senders; the sender pool
  chooses it for each destination automatically. The `Transport` trait has a
  backward-compatible default `supports_mtproxy` method to reject incompatible framing.
- [Configuration and limits](docs/proxies.md), including the local-only validation
  scope and experimental WEB relay exclusion.

## Unreleased: bot workload performance and compliance audit

The [measurements](benchmarks/2026-10-31-bot/README.md) and the
[compliance audit with its sources](COMPLIANCE-2026-10-31.md). All local mock traffic.

Against `7774ed6` on one session, in a closed loop with 64 callers: CPU per RPC -54.50%,
allocations per RPC -21.32%, bytes allocated -56.35%, steady RSS -21.31%. At 5,000 RPC/s:
CPU -20.16%. At 500 RPC/s there is no measured change in CPU (-4.72% [-12.42, +6.06]), but
RSS is -29.21%. With a session in a SQLite file at 500 RPC/s: CPU -78.78%. These figures cannot
raise Telegram's rate limits, which its servers enforce.

### Fixed

- Bursts no longer lose updates. A connection decoding more than the update queue holds
  now gives way to its consumer once per batch before dropping. Dropped updates had cost a
  gap, recovery requests to Telegram and, at 256 concurrent callers on one thread, most of
  the client's throughput.
- **A flood wait during update recovery was not honoured:** the next attempt came after
  one second whatever Telegram asked for. It now waits for at least that long.
- Failed recovery requests now back off from 1 s to 60 s instead of repeating every
  second, each channel on its own. Updates beyond a rejected channel's gap wait for it,
  bounded, instead of being dropped, and no other traffic is needed to retry while they do.
- A final difference short of an update the socket already delivered no longer restarts
  recovery in a loop. It is asked again with the same growing delay.
- Repeated connection attempts are spaced out: the first retry is immediate, then 1 s
  doubling to 30 s per datacenter, however fast a caller's own loop runs.
- The `Debug` of `DcOption` and `authentication::Finished` no longer prints the auth key.
  Logs no longer include whole updates when a sent message cannot be matched or a deadline
  passes, or a channel's access hash.

### Performance

- `Client::send_message`'s future is 1,712 B instead of 28,232 B, and no longer clones
  updates per reply. This was most of the live heap with many concurrent senders.
- Peers unchanged since last written are not written to the session again. They are written
  again every ten minutes, because a `Session` may forget them. The table of what was written
  starts empty and grows to at most about 512 KiB.
- Generated `enums::Message` and `EphemeralMessage` hold their largest variants in a `Box`:
  `Update` is 752 B instead of 5,240 B, the client's `Update` 880 B instead of 7,928 B.
- A batch of updates no longer goes through the standard stable sort, whose scratch space is
  at least 48 elements of a large type.
- Requests reserve their serialization buffer once, a connection reuses its update vector
  between reads, and reads filling the buffer grow it up to 16 KiB until idle for a minute.
  Reads cost scheduler budget per 2 KiB, which is a measured trade between pools sharing a
  thread: against `7774ed6`, busy p99 is +28.11% and quiet p99 -22.49%, where the previous commit
  had +96.45% and -43.87%. See the measurements for how that choice looks from each side.

### Breaking changes

- `tl::enums::Message::{Message, Service}` and the `EphemeralMessage` variants hold a `Box`.
  Field access and moves out of the variant still work, and `.into()` still builds one.
  Patterns such as `Message::Message(tl::types::Message { id, .. })` must bind the variant
  and read its fields instead, and `Message::Message(value)` must become `value.into()`.
- `deserialize::Error::DepthLimitExceeded`, from the previous commit, needs a new arm in
  exhaustive matches: that enum is not `#[non_exhaustive]`.
- `MessageBoxes` gained `retry_channel_without_delay`. A `Session` sees `cache_peer` less
  often: only for changed peers, and otherwise about every ten minutes per active peer.
- Behaviour: a connection retry can now wait up to 30 s, during which calls to that datacenter
  stay pending, and a failing `UpdateStream::next` can hold a recovery back for up to 60 s.
  Neither delays live updates.

## Unreleased audit follow-up

See the [follow-up ledger](REVIEW-FOLLOWUP.md) for the status of every remaining
audit item, and [its measurements](benchmarks/2026-10-31/README.md).

- Generated TL deserialization rejects values nested beyond 64 boxed levels with
  `deserialize::Error::DepthLimitExceeded`, replacing a process-aborting stack
  overflow. This is a library resource policy and can reject schema-valid values
  nested deeper. Exhaustive matches on that enum need a new arm; generated code now
  requires a cursor with `enter()`. Too-deep socket updates are treated as a gap.
- A failed update-recovery RPC, or failing to store its result, pauses retries for
  about one second. Errors still reach the caller, and live updates keep flowing.
- A difference reply that cannot be decoded no longer wedges its sequence: after 30
  consecutive failures its backlog is discarded with a warning. Those updates are
  lost. One channel's unreadable reply does not block its siblings.
- Socket updates received during a failed channel recovery are retained (at most 512
  across channels) and delivered when it ends, instead of being lost. Call the new
  `MessageBoxes::take_released_updates` if using message boxes directly.
- Bots try the zero-hash channel form before discarding a backlog with no cached
  access. This follows TDLib and has not been observed against Telegram.
- Copying authorization to a DC with no key creates the key first with one
  `help.getNearestDc`, so the import is attributed to a known key.
- Up to 8 already-queued requests now share a write, charging the cooperative budget
  per request. Local overload completion is +9.27% with busy p99 -19.07%; quiet p50 and
  p95 are about +5%. Closed-loop throughput shows no resolved change.
- Replay history stores 6 bytes per ID and also evicts IDs over 300 seconds of server
  time older than the newest, so quiet sessions keep far less. Socket buffers grown
  beyond 16 KiB are released after 60 seconds without a larger transfer, and read
  buffer growth stops exactly at the receive limit instead of reserving 2 MiB.
- The evidence checker refuses Python optimization and reports that no checks ran when
  privacy patterns are absent (status 2).

## 2026-10-30: protocol limits, initialization and recovery

- Bound handshake factorization and gzip expansion/work/nesting; validate DH inputs
  before arithmetic. Cache only the published safe prime and offload DH CPU work.
- Separate complete outbound packet limits from large inbound responses; preserve
  ACKs on deferral and keep salt-prefetch packets identifiable.
- Initialize each DC independently with shared failures, cancellation and byte
  admission. Correct clock skew and bound protocol rejection retries.
- Preserve delivery certainty, avoid ambiguous automatic replay, and keep original
  errors inspectable. Definitive local input errors are not retried by AutoSleep.
- Correct update catch-up duplication, peerless channel messages, orphan qts state,
  missing/inaccessible channels and home-connection failure recovery.
- GetState recovery errors now reach the update-stream caller, retaining state
  for an explicit subsequent next call instead of silently waiting or spinning.
- Empty reply albums fail locally; download errors remain errors on subsequent calls.
- Preserve the original connection failure for queued, unframed calls; close the
  route and release admission before notifying them. Avoid repeated full request
  scans on replies and full cold-waiter scans on every arrival.
- Keep GetState recovery armed across cancellation/storage errors, preserve saved
  channels when relearning primary state, and avoid logged-out recovery requests.
- Offload PQ factorization alongside DH work. Disable optional salt prefetch after
  repeated rejection without closing the connection. Diagnose unexpected container
  child sequence ordering with a warning while preserving other validation.
- Reject malformed GetState counters and live update metadata before changing
  recovery state. Repair invalid saved rows/counters with warnings at stream startup;
  strict `MessageBoxes::try_load` remains available for callers requiring rejection. Checked peer constructors reject extreme IDs
  without overflowing in debug builds.

### Post-review recovery corrections

- Preserve zero-QTS updates and valid siblings; skip malformed difference elements
  while applying validated response state. Recognize login after stream creation.
- Limit missing channel access-hash retries to 30 deferrals, then warn and release
  that saved backlog position so live delivery resumes without a peer cache.
  Remember exhaustion for that receiver lifetime so later recovery episodes do
  not repeat the grace; usable access or a successful difference resets it.
  The first grace suppresses live channel PTS updates and may lose them permanently
  when no hash becomes available, including with `auto_cache_peers: false`. (The
  follow-up above now retains those received during the grace.)
- Avoid repeated pending-queue scans during catch-up deadline checks.
- Deliver delayed RPC replies below the actual replay floor without replaying own
  updates or discarding valid siblings. Keep container replay decisions stable.
- Give the reactor a bounded opportunity to report buffered replies before opt-in
  liveness expiry, retaining the probe across cancellation.
- Preserve shared setup RPC errors, reject invalid mention IDs, retain working salts
  on empty replies, and prevent ACK traffic from starving near-limit requests.
- Classify unknown negative RPC codes as `UncertainDelivery`; default retry remains
  conservative.
- Retain an update batch before awaiting peer caching; cancellation no longer
  loses it. Preserve local channel PTS when receiving `updateChannelTooLong`,
  and let other channels progress after channel-scoped RPC rejection.
- Return unmapped send results through the existing fallback, preserve peerless
  channel edits as raw updates, and invalidate copied authorization after key rotation.
- Retire failed/wrapped optional salt replies, preserve sibling results on sequence
  correction, and offload SRP validation/proof work behind bounded admission.

- Preserve recovery for imported sessions without a cached self user: account
  updates establish provisional login proof; pre-login service pushes do not.
  Missing proof uses paced self-metadata checks, at most once per second.
  First channel recovery also resolves unknown account kind with one lookup.
- Preserve channel positions and propagate session-wide errors; unknown channel
  rejections defer recovery without erasing saved PTS or blocking other channels.
- Resolve missing self identity before sending to Saved Messages; `get_me` caches
  explicit self metadata even with automatic peer caching disabled.
- Refresh invalid SRP parameters once when B is outside the supplied prime, and
  preserve peerless guest-chat queries as raw updates.

### Known limitations and measured costs

Generated TL decoding is bounded to 64 boxed levels, so an authenticated deep value
is rejected instead of overflowing the stack. Values built by applications, serde or
third-party code are outside this, as are stacks well below 2 MiB, including
unoptimized builds on a 1 MiB Windows main thread.

Receive-arm-only budget placement was measured and rejected for its quiet-pool cost.
No stack profile was obtained because `perf` was unavailable; socket/runtime/thread
controls do not establish that Tokio is a bottleneck or justify replacing it.

Correctness costs against the historical baseline are still material, and no general
speedup is established. Against `7774ed6`, plain throughput is -4.23% [-8.23%, +4.81%],
overload completion +1.27% [+0.18%, +6.88%], busy p99 +96.45% and quiet p99 -43.87%.
Heap after a burst of 1100 replies is 7,911 to 14,031 B/session, and falls towards the
baseline as traffic slows. See the
[current measurements](benchmarks/2026-10-31/README.md).
The figures below describe the published `2ffd991` and earlier. At `2ffd991`, twelve
pairs measured plain throughput -3.15% [-7.23%, -1.53%], overload completion -0.91%
[-6.17%, +3.65%], busy p99 +124.27%, quiet p99 -45.47% and saturated heap 7,911 to
16,079 B/session, with dedicated heap-run process RSS +29.55%. Such figures vary between
sessions: a rerun with the current harness measured `2ffd991` at plain +2.16% and
overload -6.45%.
The historical post-Opus2 comparison measured plain throughput -5.80%,
CPU/RPC +5.94%, overload completion -3.58%, busy p99 +115.73% and quiet p99 -45.69%.
Saturated client heap rose from 7,911 to 16,079 B/session; heap-run whole-process
RSS rose 28.15%. Throughput/CPU losses remain unattributed. See
[measurement identities, intervals and scope](benchmarks/2026-10-30/final/README.md)
for the current build and historical comparisons; local/mock runs do not establish
production capacity.

The historical acceptance-round1 1 MiB reply scenarios used 90,504 KiB candidate
whole-process max RSS versus 15,052 KiB baseline at 100 offered calls/s, and
353,152 versus 71,652 KiB at 2,000/s. The baseline failed about half its calls because
it could not accept the valid large replies, so these are not equivalent-work
memory comparisons. The absolute candidate memory footprint matters for many
bots in one process; these figures include the local responder. Reruns at `2ffd991`
measured 92,388/15,056 KiB at 100 calls/s and 372,304/71,924 KiB at 2,000/s, with the
same baseline-failure caveat. Peak figures have not improved, because they are dominated
by traffic in flight. One 1 MiB reply no longer pins memory: a session holds about 1.06
MB right after it, and about 7.9 KB from the first transfer 60 s after it. Keepalive
replies provide that transfer, so an idle session releases within about two minutes.

Obfuscation and encrypted-packet padding still panic if operating-system entropy
fails. Fallible handshake and EncryptedBuilder setup do not cover those paths.

Pre-login service pushes, including QR login-token notifications, are delivered
without starting account recovery. Account updates provide provisional login
proof; a rejected initial GetState (401) warns with the RPC name and returns to
paced self-metadata discovery. It does not retry account recovery until confirmed
self metadata is cached by completing login or a successful explicit `get_me()`.
That call caches self metadata even with automatic peer caching disabled. A revoked
key will not recover merely by waiting; a failed `get_me()` surfaces its error.
Errors after cached-self or successful GetState confirmation still propagate.

Logout retains cached self metadata and existing stream state. Prefer a fresh
update stream after re-login. A stream used during QR/re-login can therefore return
account recovery 401 errors until login/2FA completes, at most about once per second,
and it delivers live updates between them. Tokens can also be checked explicitly on
your own schedule.
Ordinary live delivery does not read self metadata. If account kind is unknown,
the first channel recovery reads it once: cached bots retain limit 100,000;
missing metadata uses the valid conservative limit of 100 for that receiver.

Unknown channel-difference errors (400/403/406) preserve the channel's PTS and
allow sibling channels to recover. Contiguous live PTS updates resume after a failed
attempt, including those received during it. Updates beyond a remaining PTS gap stay
withheld until that gap is recovered: if Telegram keeps rejecting recovery they stay
withheld, rather than silently skipping data. The failure is logged as a warning.

Missing-hash grace postpones about 30 seconds of channel updates on the first recovery
episode, or longer under scheduling delays. Those received during it are retained, up
to 512 across channels, and delivered when it ends. Known bots then try the zero-hash
form. Otherwise, the backlog from before them is discarded and cannot be reconstructed
without access; user accounts have no fallback here. Exhaustion memory is per receiver,
grows with distinct exhausted channels, and is not persisted. New receivers can
therefore encounter a new grace.
Peer caching is required for reliable channel catch-up. Cancelled peer-cache work
is retained and retried on the next stream call before buffered updates are delivered.

Opt-in `liveness_timeout` measures time without a **complete valid packet**.
Partial transport progress does not reset it. Configure enough time for the
largest slow response, or keep the default `None`; trickled bytes cannot keep an
otherwise unresponsive connection alive indefinitely.

### Compatibility and migration

Exhaustive matches must handle new `InvocationError::{Read, Shared, BadMessage}`
and `ReadError::Shared` variants. `Read` includes `may_have_been_sent`; retry only
when application semantics permit it. Use `root_cause`, `category`, `transport_error`
and `std::error::Error::source` instead of assuming every connection error is `Io`.
`DeserializeError::{InvalidEnvelope, ResourceLimit}` and authentication
`Error::{InvalidPq, InvalidDhParameters, RandomnessUnavailable}` also require exhaustive-match updates.
`DhGenRetry` is now reported distinctly from `DhGenFail`.
The legacy tuple-returning `grammers_crypto::factorize` now panics on invalid
non-semiprimes such as `30` or exhausted work; use fallible `try_factorize` for
untrusted inputs. Session construction offers `Builder::try_finish`; the legacy
`finish` retains its entropy-failure panic while sender setup uses the fallible path.

New public configuration fields include `max_pending_bytes`, `liveness_timeout`
and `decode_limits`. Update explicit struct literals or use `..Default::default()`.
Low-level unlimited admission remains the default. Oversized bodies relative to
the entire byte budget return `InvalidInput`; temporary exhaustion is `WouldBlock`.
The conservative uncompressed request ceiling is now 1,044,372 bytes, less any
automatic wrapper. Inbound 1 MiB file data remains supported.

See [the scope ledger](OPTIMIZATION-2026-10.md) and [review corrections](REVIEW-ROUND1.md)
for resource policies, measured costs and remaining review gates.

## 2026-10-21: managed client and portable sessions

### Added

- `Client::builder`, `start`, `connect`, `run`, `run_until`, and awaited `stop`.
  Device settings are grouped in defaultable `DeviceConfig`.
- Explicit session sources for native SQLite, native strings, Pyrogram and
  Telethon strings/files, and gotd JSON text/files. Direct named parsers are
  available without constructing a client.
- Native `grammers:1:` session strings with bounded canonical parsing, exact
  home-DC routes, explicit secret extraction, and redacted diagnostics.
- `client.export_session_string()` returns a native `String` from every
  supported session source; `export_session_secret()` offers a redacted wrapper.
- Request admission limits, connection/status events, typed error categories,
  wait-duration helpers, and cumulative `RetryBudget`.
- Defaultable `SendOptions` and `send_message_with_options`, preserving the
  existing two-argument send API and rich message payloads.
- Optional sequential dispatcher/filter composition and explicit QR login,
  including existing 2FA handling.
- Updated examples and new lifecycle, error, session export, dispatcher,
  QR, send-options, and offline native-format demonstrations.
- GitHub Pages guides and automatically generated Rust API/method reference,
  with local parameter types, source-backed account availability, code colors,
  search, and copy controls.

### Fixed

- Expected/unsupported auth responses and malformed password state now return
  errors instead of panicking. Password-token diagnostics are redacted.
- Downloads no longer detach workers on cancellation. Work is bounded, learned
  file routes are retained, and chunk/write/flush failures propagate.
- Runner and handler unwinds have explicit outcomes. Concurrent startup,
  shutdown deadlines and terminal failures are shared consistently.
- QR home-DC migration no longer leaves a stale connection role that can hide
  update-recovery notifications.
- Foreign imports preserve source files, reject unsupported database versions,
  and bound large SQLite values before materializing them.
- Managed native file sources validate the schema before writing, so a foreign
  SQLite file cannot be accidentally migrated as a grammers session.
- IPv6-only imported sessions can be exported and restored with their exact
  endpoint metadata; unused IPv4 placeholders cannot become active routes.
- Feature-gated examples and doctests compile in supported minimal builds.

### Compatibility and limits

- Stop is terminal; construct a new client from the same session to restart.
  Existing manually managed sender pools keep their previous defaults.
- Sequential download errors now preserve their original InvocationError
  variants rather than being wrapped as generic I/O errors.
- Native strings are unencrypted portable authorization data, not complete
  database backups. They omit peer/update caches and additional DC keys.
- CDN redirects return an actionable unsupported error. Full CDN transfers
  are not implemented. Interrupted downloads may leave partial files.
- Telegram account permissions are not inferred where official metadata is
  absent or does not match the local constructor.
- The TL layer remains 230. The public upstream remains a reviewed update
  source, with history and licenses preserved.

### Verification and dependencies

Local release workspace verification: 511 passed, 30 intentionally ignored.
All-target/all-feature check, strict Clippy, Rustdoc and optional build tests
passed on Rust 1.94.1. No live account authorization or consumer deployment was
used. Hosted checks must be read from the associated GitHub Actions run.

The lockfile records 49 compatible dependency updates, including Tokio 1.54.0
and stable glass_pumpkin 2.0.0. See [PERFORMANCE.md](PERFORMANCE.md) for baseline
comparisons: no universal speedup is claimed, and one synthetic route benchmark
shows additional overhead. Dependency metadata's Rust floor is not a tested
minimum toolchain promise.
