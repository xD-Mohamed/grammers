# Proxy connections

Enable the `grammers-client` **`proxy`** feature. Existing applications keep using
`Client::builder(api_id).proxy(url)` or `ConnectionParams { proxy_url: Some(url),
..Default::default() }` with their externally managed sender pool. The setting
applies to initial connections, reconnects, key regeneration and other DCs,
including login migration and file connections. A proxy failure returns an error;
it never opens a direct connection instead.

```rust
use grammers_client::Client;

let client = Client::builder(api_id)
    .proxy("http://127.0.0.1:8080")
    .build().await?;

// Alternatively, with Basic authentication (synthetic credentials):
let authenticated = Client::builder(api_id)
    .proxy("http://user:p%40ss%3Aword@127.0.0.1:8080")
    .build().await?; // username=user, password=p@ss:word
```

`build()` prepares local state. Start/run the client as usual, or use
`connect().await` for an MTProto connectivity check; it does not log in an account.
See the compiled [proxy example](../grammers-client/examples/proxy.rs), which
reads `TG_ID` and `TG_PROXY` from the environment and awaits shutdown on error too.
Never print or commit proxy URLs containing real credentials or secrets.

| Scheme / URL | Connection and authentication |
| --- | --- |
| `http://host:8080` | HTTP/1.1 CONNECT tunnel to Telegram's TCP endpoint |
| `http://user:password@host:8080` | Preemptive Basic proxy authentication |
| `https://user:password@host:8443` | CONNECT over certificate-verified TLS to the proxy |
| `socks5://user:password@host:1080` | Existing SOCKS5, optional username/password |
| `socks5h://host:1080` | SOCKS5 alias; destination DC addresses are already numeric |
| `socks4://userid@host:1080` | SOCKS4, optional user ID; IPv4 destinations only |
| `socks4a://userid@host:1080` | SOCKS4 alias for the numeric destination API |
| `mtproxy://host:443?secret=SECRET` | MTProxy secret-authenticated obfuscation |
| `tg://proxy?server=host&port=443&secret=SECRET` | Telegram MTProxy link |
| `https://t.me/proxy?server=host&port=443&secret=SECRET` | Telegram MTProxy link, not an HTTPS CONNECT URL |

HTTP defaults to port 80 and HTTPS to 443; other schemes need an explicit port.
Proxy addresses can be DNS names, IPv4 or bracketed IPv6 literals. Resolution
retains the previous SOCKS behavior: the first returned proxy IP is used.
The destination comes from the session's DC endpoints and IPv6 preference.
CONNECT authority and Host include the destination port and bracket IPv6.
SOCKS4 cannot represent IPv6 destinations and rejects them before connecting.

URL username/password fields are percent-decoded as UTF-8 (`%40` means `@`,
`%25` means `%`, and `+` stays `+`). Basic authentication rejects a colon in the
decoded username and control characters in either field. SOCKS4 rejects passwords
and NUL in the user ID. HTTP proxy URLs must have no path beyond `/`, query or
fragment. HTTP does not encrypt the proxy authentication exchange; use HTTPS
when TLS to the proxy is required. HTTPS uses rustls with bundled Mozilla roots,
certificate expiry and server-name/IP verification. There is no verification
bypass, private-CA configuration or system trust-store loading in this API.

MTProxy secrets accept hexadecimal or standard/URL-safe base64, including:

- 16-byte classic secrets: secret-mixed AES-256-CTR with abridged framing.
- `dd` followed by 16 secret bytes: obfuscated padded intermediate framing.
- `ee` followed by 16 secret bytes and the encoded SNI hostname: authenticated
  fake TLS carrying obfuscated padded intermediate framing. Fake TLS mimics TLS
  records; it is not HTTPS, does not verify a certificate, and does not promise a
  browser fingerprint. Keep the local clock accurate for its authenticated timestamp.

The pool derives each MTProxy DC tag from that connection's DC ID and recognizes
the built-in official test-DC endpoints. For a custom test gateway, use a low-level
`ProxyTransport::for_proxy(url, dc_tag)` with the explicit test tag (10000 + DC ID).
Low-level `Sender` callers using an MTProxy URL must supply `ProxyTransport` (or
a compatible transport implementing `supports_mtproxy`); ordinary abridged/full
framing is rejected before a socket opens. Use the same proxy URL in the transport
constructor and `ServerAddr::Proxied`. MTProxy secrets/endpoints are never cached
globally; every new connection generates fresh obfuscation state.

CONNECT responses accept HTTP/1.0 and 1.1, any final 2xx, and up to four
informational replies. The total header budget is 16 KiB with at most 64 fields
per reply. Malformed replies, EOF, oversized headers, redirects and failed status
codes are errors; 407 is `PermissionDenied`. Error messages omit reason phrases,
header values and credentials. Bytes after the headers remain available to MTProto.
Fake-TLS incoming records are bounded by their 16-bit length; outgoing records
are at most 16 KiB. Neither setup path spawns detached workers. Pool
`connection_timeout` includes DNS, proxy negotiation, TLS, key exchange and init;
the managed default is 30 seconds, while the low-level default remains unlimited.
Cancellation closes an uncommitted setup socket. TLS flush state survives sender
step cancellation without submitting the same serialized prefix twice.

Telegram's experimental **WEB** relay is not implemented here. It has a separate
HTTPS/WebSocket session protocol; it is not an HTTPS CONNECT or ee proxy.
Digest/NTLM proxy authentication, proxy chains/rotation and MTProxy advertising
tags are also outside this change. Telegram's server limits are unchanged.

Validation uses synthetic local HTTP(S)/SOCKS/MTProxy peers, protocol vectors and
cancellation tests, without credentials or live Telegram traffic. It establishes
local protocol behavior, not compatibility with every proxy provider.

Protocol references: [Kurigram proxy types](https://github.com/kurigram-org/kurigram/blob/master/pyrogram/enums/proxy_scheme.py),
[HTTP CONNECT](https://www.rfc-editor.org/rfc/rfc9110.html#section-9.3.6),
[Basic authentication](https://www.rfc-editor.org/rfc/rfc7617.html),
[Telegram transports](https://core.telegram.org/mtproto/mtproto-transports),
[TDLib MTProxy secret/transport](https://github.com/tdlib/td/tree/master/td/mtproto),
[rustls Tokio integration](https://docs.rs/tokio-rustls/latest/tokio_rustls/).
