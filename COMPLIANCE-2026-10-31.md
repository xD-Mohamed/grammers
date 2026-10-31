# Telegram compliance and safety audit (2026-10-31)

What this library does against official sources: [core.telegram.org](https://core.telegram.org)
and, where it is silent, TDLib at commit `4d06d1ba`. Conclusions come from reading the
code and the sources, and from local tests. **Nothing here was exercised against Telegram:**
see [what remains unverified](#unverified). "Unconfirmed" means no source settled it.

TDLib references: [`NetQueryDelayer.cpp`], [`AuthData.cpp`], [`ChatManager.cpp`],
[`MessagesManager.cpp`], [`UpdatesManager.cpp`], [`ConnectionCreator.cpp`].

## Summary

| Area | Result |
|---|---|
| FLOOD_WAIT and rate limits | **Fixed one violation.** Now honoured on every path |
| Retries and reconnects | **Fixed two gaps.** Both now back off |
| Non-idempotent requests | Compliant; one case rests with applications |
| MTProto security checks | Compliant; two documented, deliberate looser points |
| Update recovery | Compliant; retries now follow TDLib's spacing |
| Zero access hash for bots | **Confirmed by Telegram's documentation.** Kept |
| Layer, api_id, identification | Honest; nothing impersonates a client |
| Secrets in logs and files | **Fixed these exposures.** None found in the tree |
| TL nesting limit | Sound for the schema; documented as breaking |
| Features evading limits | None found; session import reported below |

## FLOOD_WAIT and rate limits

[Errors](https://core.telegram.org/api/errors): "FLOOD_WAIT_X: A wait of X seconds is
required". The same page lists `FLOOD_PREMIUM_WAIT_X` and, in its example, a
`SLOWMODE_WAIT` "wait %d seconds before sending another message". TDLib treats all three
as code 420 ([`NetQueryDelayer.cpp:35-38`]).

- **Ordinary calls: compliant.** `AutoSleep` sleeps the server's full wait, for waits up to
  60 s and one retry by default (`configuration.rs`). A longer wait, or a second one,
  returns to the caller with `InvocationError::retry_after()`. Nothing shortens or hides it.
- **Update recovery: was non-compliant, fixed.** A flood wait on `updates.getState`,
  `getDifference` or `getChannelDifference` paused for one second whatever the wait said,
  so the next `next()` asked again during it. `pause_recovery_after` now honours the full
  wait (regression `recovery_honours_a_flood_wait_longer_than_its_own_delay`).

Telegram's limits are the server's. [Bot FAQ](https://core.telegram.org/bots/faq): "avoid
sending more than one message per second" in a chat, "not be able to send more than 20
messages per minute" in a group, "about 30 messages per second" in bulk "unless they enable
paid broadcasts". No client change can raise them, and the library neither enforces nor
evades them: a caller exceeding them gets the errors above.

## Retries and reconnects

Telegram gives no delays for these, so this follows TDLib.

- **Update recovery: fixed.** It retried once a second for as long as a failure lasted. It
  now doubles from 1 s to 60 s, as [`UpdatesManager.cpp:1339-1341`] does ("`retry_time_ *=
  2`", then 60 to 80 s). A success resets it. Channels back off individually, as
  [`MessagesManager.cpp:34306-34316`] (`retry_get_channel_difference_later`).
- **Reconnects: fixed.** Nothing spaced out repeated connection attempts, so a caller's
  retry loop reconnected as fast as it failed. The first retry is now immediate, then 1 s
  doubling to 30 s per datacenter, and a connection dying within 30 s counts as a failure.
  [`ConnectionCreator.cpp:104-108`] allows about one attempt a second, then spaces them.
- **Unchanged and bounded:** 500 and -503 are not retried by default, protocol resends stop
  after 3, salt prefetches after 4, and there is no reconnect loop without a caller.

## Non-idempotent requests

[Service messages](https://core.telegram.org/mtproto/service_messages_about_messages): re-send
"with a better msg_id and/or server_salt" after a rejection, and a message that seems lost
"must not be re-sent on its own with the same msg_id".

- Connection drop: **nothing is resent**. Each request fails with `may_have_been_sent` set
  from what reached the socket (`sender.rs` `on_error`).
- `bad_msg_notification` 16, 17 and 48, where Telegram says the message was not processed:
  re-sent with a new msg_id and the same body, at most 3 times. Code 20, where it cannot
  tell: not re-sent, the request fails. A flood wait retry resends the same bytes, so the
  same `random_id`, which is what lets Telegram recognize a repeat.
- `AutoSleep` retries an I/O failure only when it proves the request was not written.
  An earlier reading of `InvocationError::Io` as possibly delivered does not hold: within the
  library it is only produced by connection setup and admission, before anything is sent.
- **Left to applications:** retrying `send_message` after an uncertain error makes a new
  `random_id`, so Telegram sees a new message. The error's `may_have_been_sent` says when.

## MTProto security checks

| Check | Source | Result |
|---|---|---|
| `msg_key` | [Guidelines](https://core.telegram.org/mtproto/security_guidelines): "**MUST** check" | Compliant; now also constant-time, which is not required |
| `session_id` | "equals to that of an active session" | Compliant (`encrypted.rs`) |
| Parity | "odd parity for messages from server to client" | Compliant |
| Length, padding | "in the range from 12 to 1024 bytes" | Compliant |
| Duplicates | "lower than all or equal to any of the stored values... ignored" | Compliant: exact IDs, plus a floor for those evicted |
| `auth_key_id`, nonces, `new_nonce_hash`, DH | Same guidelines | Compliant; `g_a`, `g_b` range checks enforced as hard errors, stricter than "recommended" |
| Time window | "over 30 seconds in the future or over 300 seconds in the past are to be ignored", "but only if it is certain of its time" | **Looser, deliberate**: see below |
| Server salt | "must check that it has, in fact, recently sent a message with the specified msg_id" | **Partly**: see below |
| `seq_no` | Rules described, nothing a client must reject | Only non-negative checked. Unconfirmed whether more is needed |
| `msgs_state_req`, `msg_resend_req` | Optional ("may explicitly request") | Not implemented; ignored. Unconfirmed whether Telegram relies on an answer |

- **Time window.** Replay is compared between server IDs, 300 s apart at most, not against
  the local clock. TDLib applies the window only once its clock is synchronized
  ([`AuthData.cpp:158-164`]). Adding it here means a clock step can silently drop every
  update, on a check that cannot be tried without Telegram, for traffic already
  authenticated by `msg_key` and bound to a fresh `session_id`. **Left out and reported.**
  Should an authenticated peer send an ID over 300 s beyond the next, ordering becomes
  stricter until reconnection, and is logged.
- **Salt.** `bad_server_salt` is applied without matching its `bad_msg_id` to a sent message,
  though stale notifications change nothing. The packet is authenticated. **Reported, left.**

## Update recovery

[Updates](https://core.telegram.org/api/updates): "wait up to 0.5 seconds", "postpone updates
received via the socket while filling gaps", re-invoke a non-final difference "immediately".

- Compliant: gap detection by `pts`/`pts_count` and `seq`, the 0.5 s wait, postponing,
  non-final pages, honouring a difference's `timeout`, `updatesTooLong` and
  `updateChannelTooLong` starting recovery, and `differenceTooLong`.
- "Do not re-invoke" a final difference: this is about polling. One is only asked for again
  on a detected gap or at its timeout, including when a final position falls short of an
  update the socket already delivered, spaced by the delays above.

### Open items answered

**A user account with no access hash.** There is no compliant way to recover that backlog.
[Peers](https://core.telegram.org/api/peers): "if you have only a user/channel/supergroup ID
without any kind of access hash, you **cannot** interact with that peer." The exception
lists internal users only. `inputChannelFromMessage` needs a message that mentioned the channel,
which TDLib tracks ([`ChatManager.cpp:2702-2708`]) and this library does not. So the
backlog from before the live updates is lost after about 30 s of grace, with a warning.
Running `iter_dialogs` once after login caches the hashes, as the docs say.

**An unknown rejection for a channel.** Now safe and bounded, and nothing is skipped: its
position does not move, so the next successful difference has everything. Requests space out to
one a minute, as TDLib's do. While updates wait beyond the gap the retry needs no other traffic;
with none waiting, the next attempt comes with the next gap or the 15-minute timeout, so a
channel rejected for good does not cost a request a minute. At most 512 updates are retained
across channels, with a warning beyond that, and 512 per channel's open gap, which simply stops
adding: all of these come back with the difference. **The limit:** while Telegram keeps
rejecting, that channel's updates beyond the gap stay undelivered, and many busy failing
channels each hold their own gap in memory. TDLib retries the same way.

**The 64-level TL nesting limit.** Sound against the schema: its [own scan](benchmarks/2026-10-31/tl-depth/schema-scan.txt)
needs at most 23 boxed levels with each recursive type met once, and only six type groups
recurse. Only repeating those goes further: an Instant View with roughly 19 nested lists,
`RichText` over about 57 levels, or as deep a `JSONValue`. Neither Telegram's documentation
nor TDLib's parser ([`tl_parsers.h`]) states a limit, so whether real data can reach it
is unconfirmed; none is known to. Such data is rejected rather than overflowing the stack,
becomes a gap in a socket update, and is abandoned after 30 unreadable replies of a
difference, with a warning. Boxing message variants did not change what counts as a level.
Adding `deserialize::Error::DepthLimitExceeded` breaks exhaustive matches, because that
enum is not `#[non_exhaustive]`.

## Zero access hash for bots

**Supported by Telegram's documentation, kept.** [Peers](https://core.telegram.org/api/peers):
"Zero access hash: equal to `0`, must be used by bots when only a min access hash (or no access
hash) is available locally". TDLib builds the same form for bots: "`if
(td_->auth_manager_->is_bot() && channel_id.is_valid()) { return
make_tl_object<telegram_api::inputChannel>(channel_id.get(), 0);`" ([`ChatManager.cpp:2696-2701`]).

It is only tried by a bot, after the missing hash's grace, and a rejection takes the ordinary
paths. **Unconfirmed:** neither source names `updates.getChannelDifference`, though the rule
is general, and it was never tried against Telegram.

## Layer, api_id and client identification

- `LAYER` is generated from the schema's own comment (230), not set by hand.
- Defaults are honest: `device_model` is `"{os} {bits}"`, `app_version` this crate's version,
  `system_version` the real OS, and `lang_pack` empty. No code path or default names an
  official client, and no official `api_id` appears.
- No `api_id` or `api_hash` ships. [Obtaining api_id](https://core.telegram.org/api/obtaining_api_id):
  "It is necessary that you obtain your **own API id**". Examples read `TG_ID` and `TG_HASH`.
- [API terms](https://core.telegram.org/api/terms): the name has no "Telegram" (§2.3), and
  the logo is a stylized letter, not Telegram's (§2.4). The README now states the terms and
  that this is unofficial, since "your users must be aware" (§2.2) falls to whoever
  builds on this library.

## Secrets

- **Fixed:** the derived `Debug` of `DcOption` and of `authentication::Finished` printed the
  permanent auth key, though nothing logged either. A failed send mapping logged whole
  updates, with other people's messages, at warn; a deadline logged the same at debug. A
  rejected channel logged its access hash at info and each request at trace. All are gone,
  with a test for `DcOption`.
- **Searched** every tracked file, including benchmarks, for bot tokens, 32-digit hex, session
  strings, private paths, other projects' names and IPs. The only matches are fixtures: the
  upstream documentation's example token and `api_hash` in `auth.rs`, commented as "obviously
  fake", and a handshake vector in `authentication.rs`.
- **Unconfirmed:** `trace!` of raw updates still prints them, at a level off by default.
  Git history was not searched beyond these commits.

## Features evading limits

**None found.** Searched source, examples, docs and scripts for proxy rotation, flood
suppression, account farming, mass sending, member adding, invite spam, scraping and
fingerprint evasion. Proxy support is one SOCKS5 URL. `chats.rs` warns about a method that
causes flood waits.

**Reported, not changed:** the session importers read other libraries' session strings and
files. That is ordinary migration, with bounded parsing, redacted debug output and no network or
writes. They would equally accept a session the caller should not have, and a library cannot tell.
[Terms §1.4](https://core.telegram.org/api/terms) forbids acting "without the user's
knowledge and consent", which rests on whoever runs it.

## Unverified

Nothing here ran against Telegram, because no test account or bot was approved. The
[opt-in test](grammers-client/tests/live_telegram_bot.rs) checks the first group once given a bot:

- Whether Telegram accepts this layer and client identification, a fresh key exchange with it,
  salts and time correction live, `updates.getState`, a real pushed update.
- Not covered even by that test: flood waits, `bad_msg_notification`, reconnect spacing against
  real errors, gap recovery and every channel difference path, the zero hash form, and Telegram's real
  replies to a client with boxed message types, whose wire format tests show unchanged.

[`tl_parsers.h`]: https://github.com/tdlib/td/blob/4d06d1ba3a1978476fe2b6575de8388439f6baa3/tdutils/td/utils/tl_parsers.h
[`NetQueryDelayer.cpp`]: https://github.com/tdlib/td/blob/4d06d1ba3a1978476fe2b6575de8388439f6baa3/td/telegram/net/NetQueryDelayer.cpp
[`NetQueryDelayer.cpp:35-38`]: https://github.com/tdlib/td/blob/4d06d1ba3a1978476fe2b6575de8388439f6baa3/td/telegram/net/NetQueryDelayer.cpp#L35-L38
[`AuthData.cpp`]: https://github.com/tdlib/td/blob/4d06d1ba3a1978476fe2b6575de8388439f6baa3/td/mtproto/AuthData.cpp
[`AuthData.cpp:158-164`]: https://github.com/tdlib/td/blob/4d06d1ba3a1978476fe2b6575de8388439f6baa3/td/mtproto/AuthData.cpp#L158-L164
[`ChatManager.cpp`]: https://github.com/tdlib/td/blob/4d06d1ba3a1978476fe2b6575de8388439f6baa3/td/telegram/ChatManager.cpp
[`ChatManager.cpp:2696-2701`]: https://github.com/tdlib/td/blob/4d06d1ba3a1978476fe2b6575de8388439f6baa3/td/telegram/ChatManager.cpp#L2696-L2701
[`ChatManager.cpp:2702-2708`]: https://github.com/tdlib/td/blob/4d06d1ba3a1978476fe2b6575de8388439f6baa3/td/telegram/ChatManager.cpp#L2702-L2708
[`MessagesManager.cpp`]: https://github.com/tdlib/td/blob/4d06d1ba3a1978476fe2b6575de8388439f6baa3/td/telegram/MessagesManager.cpp
[`MessagesManager.cpp:34306-34316`]: https://github.com/tdlib/td/blob/4d06d1ba3a1978476fe2b6575de8388439f6baa3/td/telegram/MessagesManager.cpp#L34306-L34316
[`UpdatesManager.cpp`]: https://github.com/tdlib/td/blob/4d06d1ba3a1978476fe2b6575de8388439f6baa3/td/telegram/UpdatesManager.cpp
[`UpdatesManager.cpp:1339-1341`]: https://github.com/tdlib/td/blob/4d06d1ba3a1978476fe2b6575de8388439f6baa3/td/telegram/UpdatesManager.cpp#L1339-L1341
[`ConnectionCreator.cpp`]: https://github.com/tdlib/td/blob/4d06d1ba3a1978476fe2b6575de8388439f6baa3/td/telegram/net/ConnectionCreator.cpp
[`ConnectionCreator.cpp:104-108`]: https://github.com/tdlib/td/blob/4d06d1ba3a1978476fe2b6575de8388439f6baa3/td/telegram/net/ConnectionCreator.cpp#L104-L108
