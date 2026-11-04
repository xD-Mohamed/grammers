// Copyright 2026 - developers of the `grammers` project.
// Licensed under the Apache License, Version 2.0 or the MIT license, at your option.
// Wire references: https://core.telegram.org/mtproto/mtproto-transports and
// https://github.com/tdlib/td/tree/master/td/mtproto (Boost Software License).

use base64::{Engine as _, engine::general_purpose};
use grammers_crypto::{DequeBuffer, ObfuscatedCipher};
use grammers_mtproto::transport::{Abridged, Error, Tagged as _, Transport, UnpackedOffset};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use std::{fmt, io};
use std::{
    pin::Pin,
    task::{Context, Poll},
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf};
use url::{Host, Url};

const MAX_DOMAIN: usize = 182;
const MAX_PACKET: usize = 1024 * 1024 + 8 * 1024 - 4;

fn invalid(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, reason)
}
fn random(bytes: &mut [u8]) -> io::Result<()> {
    getrandom::fill(bytes).map_err(|_| io::Error::other("MTProxy entropy is unavailable"))
}

#[derive(Clone)]
pub(crate) struct Secret {
    key: [u8; 16],
    padded: bool,
    domain: Option<String>,
}
impl fmt::Debug for Secret {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Secret([redacted])")
    }
}
impl Secret {
    pub(crate) fn parse(encoded: &str) -> io::Result<Self> {
        if encoded.len() > 2 * (17 + MAX_DOMAIN) {
            return Err(invalid("MTProxy secret is too long"));
        }
        let decoded = grammers_crypto::hex::opt_from_hex(encoded)
            .or_else(|| general_purpose::URL_SAFE_NO_PAD.decode(encoded).ok())
            .or_else(|| general_purpose::URL_SAFE.decode(encoded).ok())
            .or_else(|| general_purpose::STANDARD_NO_PAD.decode(encoded).ok())
            .or_else(|| general_purpose::STANDARD.decode(encoded).ok())
            .ok_or_else(|| invalid("Invalid MTProxy secret encoding"))?;
        let (key, padded, domain) = match decoded.as_slice() {
            key if key.len() == 16 => (key, false, None),
            [0xdd, key @ ..] if key.len() == 16 => (key, true, None),
            [0xee, rest @ ..] if rest.len() > 16 && rest.len() <= 16 + MAX_DOMAIN => {
                let name = std::str::from_utf8(&rest[16..])
                    .map_err(|_| invalid("Invalid MTProxy fake-TLS hostname"))?;
                let Host::Domain(domain) =
                    Host::parse(name).map_err(|_| invalid("Invalid MTProxy fake-TLS hostname"))?
                else {
                    return Err(invalid("MTProxy fake-TLS requires a DNS hostname"));
                };
                let domain = if name.is_ascii() {
                    name.to_owned()
                } else {
                    domain
                };
                if domain.len() > MAX_DOMAIN
                    || domain.is_empty()
                    || domain.split('.').any(|label| {
                        label.is_empty()
                            || label.len() > 63
                            || label.starts_with('-')
                            || label.ends_with('-')
                            || !label
                                .bytes()
                                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
                    })
                {
                    return Err(invalid("Invalid MTProxy fake-TLS hostname"));
                }
                (&rest[..16], true, Some(domain))
            }
            _ => return Err(invalid("Unsupported MTProxy secret format")),
        };
        Ok(Self {
            key: key.try_into().unwrap(),
            padded,
            domain,
        })
    }
    pub(crate) fn is_faketls(&self) -> bool {
        self.domain.is_some()
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Proxy {
    pub(crate) host: String,
    pub(crate) port: u16,
    pub(crate) secret: Secret,
}

pub(crate) fn is_mtproxy_url(url: &Url) -> bool {
    matches!(url.scheme(), "mtproxy" | "mtp" | "tg")
        || (matches!(url.scheme(), "http" | "https")
            && matches!(url.host_str(), Some("t.me" | "telegram.me"))
            && url.path() == "/proxy")
}

/// Parse URLs without exposing any input string in diagnostics.
pub(crate) fn parse_url(url: &Url) -> io::Result<Proxy> {
    if !url.username().is_empty() || url.password().is_some() || url.fragment().is_some() {
        return Err(invalid("Invalid MTProxy URL components"));
    }
    let is_link = match (url.scheme(), url.host_str(), url.path()) {
        ("mtproxy" | "mtp", _, "" | "/") => false,
        ("tg", Some("proxy"), "" | "/") => true,
        ("http" | "https", Some("t.me" | "telegram.me"), "/proxy") => true,
        _ => return Err(invalid("Invalid MTProxy URL")),
    };
    let mut server = None;
    let mut port = None;
    let mut secret = None;
    for (name, value) in url.query_pairs() {
        let slot = match name.as_ref() {
            "secret" => &mut secret,
            "server" if is_link => &mut server,
            "port" if is_link => &mut port,
            _ => return Err(invalid("Unsupported MTProxy URL parameter")),
        };
        if slot.replace(value.into_owned()).is_some() {
            return Err(invalid("Duplicate MTProxy URL parameter"));
        }
    }
    let (host, port) = if is_link {
        let server = server.ok_or_else(|| invalid("MTProxy hostname is missing"))?;
        let host = if let Ok(ip) = server.parse::<std::net::IpAddr>() {
            ip.to_string()
        } else {
            Host::parse(&server)
                .map_err(|_| invalid("Invalid MTProxy hostname"))?
                .to_string()
        };
        let port = port
            .and_then(|p| p.parse::<u16>().ok())
            .filter(|p| *p != 0)
            .ok_or_else(|| invalid("Invalid MTProxy port"))?;
        (host, port)
    } else {
        let host = url
            .host()
            .ok_or_else(|| invalid("MTProxy hostname is missing"))?;
        let port = url
            .port()
            .filter(|p| *p != 0)
            .ok_or_else(|| invalid("MTProxy port is missing or invalid"))?;
        (host.to_string(), port)
    };
    let host = host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned();
    if host.is_empty() {
        return Err(invalid("MTProxy hostname is missing"));
    }
    Ok(Proxy {
        host,
        port,
        secret: Secret::parse(
            secret
                .as_deref()
                .ok_or_else(|| invalid("MTProxy secret is missing"))?,
        )?,
    })
}

pub(crate) struct MtProxyTransport {
    cipher: ObfuscatedCipher,
    head: Option<[u8; 64]>,
    decrypted: usize,
    padded: bool,
    abridged: Abridged,
}
impl MtProxyTransport {
    pub(crate) fn new(secret: &Secret, dc_id: i16) -> io::Result<Self> {
        if dc_id == 0 {
            return Err(invalid("MTProxy DC id must be nonzero"));
        }
        let mut nonce = [0; 64];
        loop {
            random(&mut nonce)?;
            if nonce[0] != 0xef
                && nonce[4..8] != [0; 4]
                && ![
                    *b"HEAD",
                    *b"POST",
                    *b"GET ",
                    *b"OPTI",
                    [0xdd; 4],
                    [0xee; 4],
                    [0x16, 0x03, 0x01, 0x02],
                ]
                .iter()
                .any(|prefix| prefix == &nonce[..4])
            {
                break;
            }
        }
        Ok(Self::with_nonce(secret, dc_id, nonce))
    }
    fn with_nonce(secret: &Secret, dc_id: i16, mut nonce: [u8; 64]) -> Self {
        nonce[56..60].copy_from_slice(&if secret.padded { [0xdd; 4] } else { [0xef; 4] });
        nonce[60..62].copy_from_slice(&dc_id.to_le_bytes());
        let mut cipher = ObfuscatedCipher::with_secret(&nonce, &secret.key);
        let mut encrypted = nonce;
        cipher.encrypt(&mut encrypted);
        nonce[56..64].copy_from_slice(&encrypted[56..64]);
        let mut abridged = Abridged::new();
        abridged.init_tag();
        Self {
            cipher,
            head: Some(nonce),
            decrypted: 0,
            padded: secret.padded,
            abridged,
        }
    }
}
impl Transport for MtProxyTransport {
    fn pack(&mut self, buffer: &mut DequeBuffer<u8>) {
        if self.padded {
            assert!(buffer.len().is_multiple_of(4));
            let mut padding = [0; 16];
            random(&mut padding).expect("MTProxy transport padding entropy is unavailable");
            buffer.extend(&padding[..usize::from(padding[0] & 15)]);
            let size = u32::try_from(buffer.len()).expect("MTProxy packet is too large");
            buffer.extend_front(&size.to_le_bytes());
        } else {
            self.abridged.pack(buffer);
        }
        self.cipher.encrypt(buffer.as_mut());
        if let Some(head) = self.head.take() {
            buffer.extend_front(&head);
        }
    }
    fn unpack(&mut self, buffer: &mut [u8]) -> Result<UnpackedOffset, Error> {
        if buffer.len() < self.decrypted {
            return Err(Error::BadLen { got: -1 });
        }
        self.cipher.decrypt(&mut buffer[self.decrypted..]);
        self.decrypted = buffer.len();
        let offset = if self.padded {
            unpack_padded(buffer)
        } else {
            self.abridged.unpack(buffer)
        }?;
        self.decrypted -= offset.next_offset;
        Ok(offset)
    }
}

fn unpack_padded(buffer: &[u8]) -> Result<UnpackedOffset, Error> {
    if buffer.len() < 4 {
        return Err(Error::MissingBytes);
    }
    let size = i32::from_le_bytes(buffer[..4].try_into().unwrap());
    if size < 4 || size as usize > MAX_PACKET {
        return Err(Error::BadLen { got: size });
    }
    let size = size as usize;
    if buffer.len() < 4 + size {
        return Err(Error::MissingBytes);
    }
    let payload = &buffer[4..4 + size];
    if size < 20 {
        let status = i32::from_le_bytes(payload[..4].try_into().unwrap());
        return if status < 0 {
            Err(Error::BadStatus {
                status: status.wrapping_neg() as u32,
            })
        } else {
            Err(Error::BadLen { got: size as i32 })
        };
    }
    let payload_size = if payload[..8] == [0; 8] {
        let body_size = i32::from_le_bytes(payload[16..20].try_into().unwrap());
        if body_size <= 0 || body_size % 4 != 0 {
            return Err(Error::BadLen { got: body_size });
        }
        20usize
            .checked_add(body_size as usize)
            .ok_or(Error::BadLen { got: body_size })?
    } else {
        if size < 40 {
            return Err(Error::BadLen { got: size as i32 });
        }
        24 + (size - 24) / 16 * 16
    };
    if payload_size > size || size - payload_size > 15 {
        return Err(Error::BadLen { got: size as i32 });
    }
    Ok(UnpackedOffset {
        data_range: 4..4 + payload_size,
        next_offset: 4 + size,
    })
}

const TLS_RECORD_MAX: usize = 16 * 1024;
const TLS_INCOMING_MAX: usize = u16::MAX as usize;
const TLS_CCS: [u8; 6] = [0x14, 0x03, 0x03, 0, 1, 1];

fn hmac_sha256(key: &[u8], pieces: &[&[u8]]) -> [u8; 32] {
    authenticator(key, pieces).finalize().into_bytes().into()
}
fn authenticator(key: &[u8], pieces: &[&[u8]]) -> Hmac<Sha256> {
    let mut mac = Hmac::<Sha256>::new_from_slice(key).expect("HMAC accepts any key length");
    for piece in pieces {
        mac.update(piece);
    }
    mac
}
fn tls_error(reason: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}
fn extension(out: &mut Vec<u8>, kind: u16, data: &[u8]) {
    out.extend(kind.to_be_bytes());
    out.extend((data.len() as u16).to_be_bytes());
    out.extend(data);
}

/// Independent RFC8446 ClientHello, padded to the stock MTProxy minimum.
/// Authenticated camouflage, not actual TLS encryption or a browser fingerprint.
fn client_hello(secret: &Secret, timestamp: u32) -> io::Result<Vec<u8>> {
    let domain = secret
        .domain
        .as_deref()
        .ok_or_else(|| invalid("MTProxy secret has no fake-TLS hostname"))?;
    let mut hello = Vec::with_capacity(517);
    hello.extend([0x16, 0x03, 0x01, 0x02, 0, 1, 0, 1, 0xfc, 3, 3]);
    hello.extend([0; 32]);
    hello.push(32);
    let mut session = [0; 32];
    random(&mut session)?;
    hello.extend(session);
    hello.extend([0, 2, 0x13, 1, 1, 0]);
    let ext_at = hello.len();
    hello.extend([0, 0]);
    let mut sni = Vec::with_capacity(domain.len() + 5);
    sni.extend(((domain.len() + 3) as u16).to_be_bytes());
    sni.push(0);
    sni.extend((domain.len() as u16).to_be_bytes());
    sni.extend(domain.as_bytes());
    extension(&mut hello, 0, &sni);
    extension(&mut hello, 43, &[2, 3, 4]);
    extension(&mut hello, 10, &[0, 2, 0, 29]);
    extension(&mut hello, 13, &[0, 4, 8, 4, 4, 3]);
    let mut share = [0; 38];
    share[..6].copy_from_slice(&[0, 36, 0, 29, 0, 32]);
    random(&mut share[6..])?;
    share[37] &= 0x7f;
    extension(&mut hello, 51, &share);
    extension(&mut hello, 45, &[1, 1]);
    let padding = 517usize
        .checked_sub(hello.len() + 4)
        .ok_or_else(|| invalid("MTProxy fake-TLS hostname is too long"))?;
    extension(&mut hello, 21, &vec![0; padding]);
    let ext_len = (hello.len() - ext_at - 2) as u16;
    hello[ext_at..ext_at + 2].copy_from_slice(&ext_len.to_be_bytes());
    let mut digest = hmac_sha256(&secret.key, &[&hello]);
    for (byte, time) in digest[28..].iter_mut().zip(timestamp.to_le_bytes()) {
        *byte ^= time;
    }
    hello[11..43].copy_from_slice(&digest);
    Ok(hello)
}

/// Parent connection setup owns the deadline and cancellation of this handshake.
pub(crate) async fn connect_faketls<S: AsyncRead + AsyncWrite + Unpin>(
    mut inner: S,
    secret: &Secret,
) -> io::Result<FakeTlsStream<S>> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| io::Error::other("MTProxy fake-TLS clock is before Unix epoch"))?
        .as_secs() as u32;
    let hello = client_hello(secret, timestamp)?;
    inner.write_all(&hello).await?;
    inner.flush().await?;
    let mut head = [0; 5];
    inner.read_exact(&mut head).await?;
    let size = usize::from(u16::from_be_bytes([head[3], head[4]]));
    if head[..3] != [0x16, 3, 3] || !(76..=TLS_INCOMING_MAX).contains(&size) {
        return Err(tls_error("Invalid MTProxy fake-TLS ServerHello record"));
    }
    let mut response = head.to_vec();
    response.resize(5 + size, 0);
    inner.read_exact(&mut response[5..]).await?;
    let handshake_len = (usize::from(response[6]) << 16)
        | (usize::from(response[7]) << 8)
        | usize::from(response[8]);
    if response[5] != 2 || handshake_len != size - 4 || response[9..11] != [3, 3] {
        return Err(tls_error("Invalid MTProxy fake-TLS ServerHello envelope"));
    }
    if response[43] != 32
        || response[44..76] != hello[44..76]
        || response[76..79] != [0x13, 1, 0]
        || usize::from(u16::from_be_bytes([response[79], response[80]])) != response.len() - 81
    {
        return Err(tls_error(
            "Invalid MTProxy fake-TLS ServerHello negotiation",
        ));
    }
    let mut extension_at = 81;
    while extension_at < response.len() {
        if response.len() - extension_at < 4 {
            return Err(tls_error("Invalid MTProxy fake-TLS ServerHello extension"));
        }
        let size = usize::from(u16::from_be_bytes([
            response[extension_at + 2],
            response[extension_at + 3],
        ]));
        extension_at += 4;
        if size > response.len() - extension_at {
            return Err(tls_error("Invalid MTProxy fake-TLS ServerHello extension"));
        }
        extension_at += size;
    }
    let mut ccs = [0; 6];
    inner.read_exact(&mut ccs).await?;
    if ccs != TLS_CCS {
        return Err(tls_error("Invalid MTProxy fake-TLS ChangeCipherSpec"));
    }
    response.extend(ccs);
    inner.read_exact(&mut head).await?;
    let size = usize::from(u16::from_be_bytes([head[3], head[4]]));
    if head[..3] != [0x17, 3, 3] || !(1..=TLS_INCOMING_MAX).contains(&size) {
        return Err(tls_error("Invalid MTProxy fake-TLS greeting data record"));
    }
    response.extend(head);
    let at = response.len();
    response.resize(at + size, 0);
    inner.read_exact(&mut response[at..]).await?;
    let mut server_random = [0; 32];
    server_random.copy_from_slice(&response[11..43]);
    response[11..43].fill(0);
    if authenticator(&secret.key, &[&hello[11..43], &response])
        .verify_slice(&server_random)
        .is_err()
    {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "MTProxy fake-TLS server authentication failed",
        ));
    }
    Ok(FakeTlsStream::new(inner))
}

/// Only record headers and positions survive yields; payloads are not copied.
pub(crate) struct FakeTlsStream<S> {
    inner: S,
    rx_header: [u8; 5],
    rx_at: usize,
    rx_left: usize,
    tx_header: [u8; 11],
    tx_at: usize,
    tx_len: usize,
    tx_left: usize,
    first: bool,
}
impl<S> FakeTlsStream<S> {
    fn new(inner: S) -> Self {
        Self {
            inner,
            rx_header: [0; 5],
            rx_at: 0,
            rx_left: 0,
            tx_header: [0; 11],
            tx_at: 0,
            tx_len: 0,
            tx_left: 0,
            first: true,
        }
    }
}
impl<S: AsyncRead + Unpin> AsyncRead for FakeTlsStream<S> {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let this = self.get_mut();
        if out.remaining() == 0 {
            return Poll::Ready(Ok(()));
        }
        if this.rx_left == 0 {
            while this.rx_at < 5 {
                let mut read = ReadBuf::new(&mut this.rx_header[this.rx_at..]);
                match Pin::new(&mut this.inner).poll_read(cx, &mut read) {
                    Poll::Pending => return Poll::Pending,
                    Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                    Poll::Ready(Ok(())) => {}
                }
                let count = read.filled().len();
                if count == 0 {
                    return Poll::Ready(if this.rx_at == 0 {
                        Ok(())
                    } else {
                        Err(io::Error::new(
                            io::ErrorKind::UnexpectedEof,
                            "MTProxy fake-TLS record header ended early",
                        ))
                    });
                }
                this.rx_at += count;
            }
            let size = usize::from(u16::from_be_bytes([this.rx_header[3], this.rx_header[4]]));
            if this.rx_header[..3] != [0x17, 3, 3] || !(1..=TLS_INCOMING_MAX).contains(&size) {
                return Poll::Ready(Err(tls_error(
                    "Invalid MTProxy fake-TLS application record",
                )));
            }
            this.rx_left = size;
            this.rx_at = 0;
        }
        let limit = out.remaining().min(this.rx_left);
        let mut read = ReadBuf::new(out.initialize_unfilled_to(limit));
        match Pin::new(&mut this.inner).poll_read(cx, &mut read) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Ready(Ok(())) => {
                let count = read.filled().len();
                if count == 0 {
                    return Poll::Ready(Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "MTProxy fake-TLS record body ended early",
                    )));
                }
                this.rx_left -= count;
                out.advance(count);
                Poll::Ready(Ok(()))
            }
        }
    }
}
impl<S: AsyncWrite + Unpin> AsyncWrite for FakeTlsStream<S> {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let this = self.get_mut();
        if bytes.is_empty() {
            return Poll::Ready(Ok(0));
        }
        if this.tx_left == 0 {
            this.tx_left = bytes.len().min(TLS_RECORD_MAX);
            this.tx_at = 0;
            let at = if this.first {
                this.first = false;
                this.tx_header[..6].copy_from_slice(&TLS_CCS);
                6
            } else {
                0
            };
            this.tx_len = at + 5;
            this.tx_header[at..at + 3].copy_from_slice(&[0x17, 3, 3]);
            this.tx_header[at + 3..at + 5].copy_from_slice(&(this.tx_left as u16).to_be_bytes());
        }
        while this.tx_at < this.tx_len {
            match Pin::new(&mut this.inner).poll_write(cx, &this.tx_header[this.tx_at..this.tx_len])
            {
                Poll::Pending => return Poll::Pending,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Ready(Ok(0)) => return Poll::Ready(Err(io::ErrorKind::WriteZero.into())),
                Poll::Ready(Ok(n)) => this.tx_at += n,
            }
        }
        let count = bytes.len().min(this.tx_left);
        match Pin::new(&mut this.inner).poll_write(cx, &bytes[..count]) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
            Poll::Ready(Ok(0)) => Poll::Ready(Err(io::ErrorKind::WriteZero.into())),
            Poll::Ready(Ok(n)) => {
                this.tx_left -= n;
                Poll::Ready(Ok(n))
            }
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_flush(cx)
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.get_mut().inner).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod faketls_tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::timeout;
    const DEADLINE: Duration = Duration::from_secs(5);
    #[tokio::test]
    async fn mtproxy_local_sender_invokes_through_classic_dd_and_ee_tunnels() {
        use grammers_mtproto::mtp;
        use grammers_tl_types::{self as tl, Serializable};
        use tokio::net::TcpListener;
        timeout(DEADLINE, async {
            for mode in 0..3 {
                let secret = Secret {
                    key: [0x71; 16],
                    padded: mode != 0,
                    domain: (mode == 2).then(|| "example.com".into()),
                };
                let encoded = match mode {
                    0 => "71".repeat(16),
                    1 => format!("dd{}", "71".repeat(16)),
                    _ => format!("ee{}6578616d706c652e636f6d", "71".repeat(16)),
                };
                let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
                let proxy = format!(
                    "mtproxy://{}?secret={encoded}",
                    listener.local_addr().unwrap()
                );
                let server = async {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    if mode == 2 {
                        let mut hello = [0; 517];
                        socket.read_exact(&mut hello).await.unwrap();
                        socket
                            .write_all(&server_greeting(&hello, &secret.key))
                            .await
                            .unwrap();
                        let mut ccs = [0; 6];
                        socket.read_exact(&mut ccs).await.unwrap();
                        assert_eq!(ccs, TLS_CCS);
                        let mut header = [0; 5];
                        socket.read_exact(&mut header).await.unwrap();
                        assert_eq!(&header[..3], &[0x17, 3, 3]);
                    }
                    let mut head = [0; 64];
                    socket.read_exact(&mut head).await.unwrap();
                    let mut reverse = head;
                    reverse.reverse();
                    let mut cipher = ObfuscatedCipher::with_secret(&reverse, &secret.key);
                    let mut clear = head;
                    cipher.decrypt(&mut clear);
                    assert_eq!(
                        &clear[56..60],
                        &if mode == 0 { [0xef; 4] } else { [0xdd; 4] }
                    );
                    assert_eq!(&clear[60..62], &2i16.to_le_bytes());
                    let mut body = Vec::new();
                    let mut abridged = Abridged::new();
                    let offset = loop {
                        let mut byte = [socket.read_u8().await.unwrap()];
                        cipher.decrypt(&mut byte);
                        body.push(byte[0]);
                        let result = if secret.padded {
                            unpack_padded(&body)
                        } else {
                            abridged.unpack(&mut body)
                        };
                        match result {
                            Ok(offset) => break offset,
                            Err(Error::MissingBytes) => {}
                            error => panic!("bad request: {error:?}"),
                        }
                    };
                    let plain = &body[offset.data_range];
                    assert_eq!(&plain[..8], &[0; 8]);
                    assert_eq!(
                        &plain[20..],
                        &tl::functions::ReqPqMulti { nonce: [123; 16] }.to_bytes()
                    );
                    let response = tl::enums::ResPq::Pq(tl::types::ResPq {
                        nonce: [123; 16],
                        server_nonce: [45; 16],
                        pq: vec![0x01, 0x43],
                        server_public_key_fingerprints: vec![],
                    })
                    .to_bytes();
                    let mut plain = vec![0; 8];
                    plain.extend(1i64.to_le_bytes());
                    plain.extend((response.len() as i32).to_le_bytes());
                    plain.extend(response);
                    let mut reply = if secret.padded {
                        let mut bytes = ((plain.len() + 11) as u32).to_le_bytes().to_vec();
                        bytes.extend(plain);
                        bytes.extend([0x35; 11]);
                        bytes
                    } else {
                        let mut buffer = DequeBuffer::with_capacity(plain.len(), 8);
                        buffer.extend(plain);
                        let mut transport = Abridged::new();
                        transport.init_tag();
                        transport.pack(&mut buffer);
                        buffer.as_ref().to_vec()
                    };
                    cipher.encrypt(&mut reply);
                    if mode == 2 {
                        reply = record(&reply);
                    }
                    for chunk in reply.chunks(3) {
                        socket.write_all(chunk).await.unwrap();
                    }
                };
                let client = async {
                    let transport = crate::ProxyTransport::for_proxy(&proxy, 2).unwrap();
                    let mut sender = crate::Sender::connect(
                        transport,
                        mtp::Plain::new(),
                        crate::ServerAddr::Proxied {
                            address: "192.0.2.1:443".parse().unwrap(),
                            proxy: proxy.clone(),
                        },
                    )
                    .await
                    .unwrap();
                    let tl::enums::ResPq::Pq(result) = sender
                        .invoke(&tl::functions::ReqPqMulti { nonce: [123; 16] })
                        .await
                        .unwrap();
                    assert_eq!(result.nonce, [123; 16]);
                    assert_eq!(result.server_nonce, [45; 16]);
                    sender.disconnect().await.unwrap();
                };
                tokio::join!(client, server);
                let result = crate::Sender::connect(
                    Abridged::new(),
                    mtp::Plain::new(),
                    crate::ServerAddr::Proxied {
                        address: "192.0.2.1:443".parse().unwrap(),
                        proxy,
                    },
                )
                .await;
                assert!(
                    matches!(result, Err(error) if error.kind() == io::ErrorKind::InvalidInput)
                );
            }
        })
        .await
        .unwrap();
    }
    fn secret() -> Secret {
        Secret {
            key: [0x71; 16],
            padded: true,
            domain: Some("example.com".into()),
        }
    }
    fn server_greeting(hello: &[u8], key: &[u8]) -> Vec<u8> {
        let mut response = vec![0; 127];
        response[..11].copy_from_slice(&[0x16, 3, 3, 0, 122, 2, 0, 0, 118, 3, 3]);
        response[43] = 32;
        response[44..76].copy_from_slice(&hello[44..76]);
        response[76..81].copy_from_slice(&[0x13, 1, 0, 0, 46]);
        response[81..89].copy_from_slice(&[0, 0x33, 0, 36, 0, 29, 0, 32]);
        response[121..127].copy_from_slice(&[0, 0x2b, 0, 2, 3, 4]);
        response.extend(TLS_CCS);
        response.extend([0x17, 3, 3, 0, 64]);
        response.extend([0x62; 64]);
        let tag = hmac_sha256(key, &[&hello[11..43], &response]);
        response[11..43].copy_from_slice(&tag);
        response
    }
    fn record(bytes: &[u8]) -> Vec<u8> {
        let mut result = vec![0x17, 3, 3];
        result.extend((bytes.len() as u16).to_be_bytes());
        result.extend(bytes);
        result
    }
    #[test]
    fn mtproxy_faketls_hmac_matches_rfc4231() {
        assert_eq!(
            grammers_crypto::hex::to_hex(&hmac_sha256(&[0x0b; 20], &[b"Hi There"])),
            "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
        );
    }
    #[test]
    fn mtproxy_faketls_hello_authenticates_sni_and_timestamp() {
        let secret = secret();
        let hello = client_hello(&secret, 0x11223344).unwrap();
        assert_eq!(hello.len(), 517);
        assert_eq!(&hello[..11], &[0x16, 3, 1, 2, 0, 1, 0, 1, 0xfc, 3, 3]);
        assert_eq!(hello[43], 32);
        assert_eq!(&hello[76..82], &[0, 2, 0x13, 1, 1, 0]);
        assert_eq!(
            usize::from(u16::from_be_bytes([hello[82], hello[83]])),
            517 - 84
        );
        let mut found_sni = false;
        let mut at = 84;
        while at < hello.len() {
            let kind = u16::from_be_bytes([hello[at], hello[at + 1]]);
            let size = usize::from(u16::from_be_bytes([hello[at + 2], hello[at + 3]]));
            at += 4;
            let data = &hello[at..at + size];
            if kind == 0 {
                assert_eq!(u16::from_be_bytes([data[0], data[1]]) as usize, size - 2);
                assert_eq!(&data[5..], b"example.com");
                found_sni = true;
            }
            at += size;
        }
        assert!(found_sni);
        let mut zeroed = hello.clone();
        zeroed[11..43].fill(0);
        let mut expected = hmac_sha256(&secret.key, &[&zeroed]);
        for (byte, time) in expected[28..].iter_mut().zip(0x11223344u32.to_le_bytes()) {
            *byte ^= time;
        }
        assert_eq!(&hello[11..43], &expected);
        assert_ne!(
            &hello[11..43],
            &client_hello(&secret, 0x11223344).unwrap()[11..43]
        );
    }
    #[tokio::test]
    async fn mtproxy_faketls_split_greeting_and_bidirectional_records() {
        timeout(DEADLINE, async {
            let (client, mut server) = tokio::io::duplex(64);
            let secret = secret();
            let payload = vec![0x52; TLS_RECORD_MAX + 177];
            let client = async {
                let mut stream = connect_faketls(client, &secret).await.unwrap();
                let mut reply = [0; 5];
                stream.read_exact(&mut reply).await.unwrap();
                assert_eq!(&reply, b"reply");
                stream.write_all(&payload).await.unwrap();
                let mut done = [0; 4];
                stream.read_exact(&mut done).await.unwrap();
                assert_eq!(&done, b"done");
            };
            let server = async {
                let mut hello = [0; 517];
                server.read_exact(&mut hello).await.unwrap();
                let mut response = server_greeting(&hello, &secret.key);
                response.extend(record(b"reply"));
                for chunk in response.chunks(3) {
                    server.write_all(chunk).await.unwrap();
                }
                let mut ccs = [0; 6];
                server.read_exact(&mut ccs).await.unwrap();
                assert_eq!(ccs, TLS_CCS);
                let mut got = Vec::new();
                while got.len() < payload.len() {
                    let mut head = [0; 5];
                    server.read_exact(&mut head).await.unwrap();
                    assert_eq!(&head[..3], &[0x17, 3, 3]);
                    let size = usize::from(u16::from_be_bytes([head[3], head[4]]));
                    assert!((1..=TLS_RECORD_MAX).contains(&size));
                    let at = got.len();
                    got.resize(at + size, 0);
                    server.read_exact(&mut got[at..]).await.unwrap();
                }
                assert_eq!(got, payload);
                server.write_all(&record(b"done")).await.unwrap();
            };
            tokio::join!(client, server);
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn mtproxy_faketls_rejects_bad_auth_malformed_and_short_greetings() {
        timeout(DEADLINE, async {
            for mode in 0..4 {
                let (client, mut server) = tokio::io::duplex(1024);
                let secret = secret();
                let client = async { connect_faketls(client, &secret).await.err().unwrap() };
                let server = async {
                    let mut hello = [0; 517];
                    server.read_exact(&mut hello).await.unwrap();
                    let mut response = server_greeting(&hello, &secret.key);
                    match mode {
                        0 => response[11] ^= 1,
                        1 => response[0] = 0x17,
                        2 => response.truncate(43),
                        3 => response = vec![0x16, 3, 3, 0, 0],
                        _ => unreachable!(),
                    }
                    server.write_all(&response).await.unwrap();
                    drop(server);
                };
                let (error, ()) = tokio::join!(client, server);
                assert_eq!(
                    error.kind(),
                    match mode {
                        0 => io::ErrorKind::PermissionDenied,
                        2 => io::ErrorKind::UnexpectedEof,
                        _ => io::ErrorKind::InvalidData,
                    }
                );
                assert!(!error.to_string().contains("717171"));
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn mtproxy_faketls_record_eof_and_full_u16_length_are_supported() {
        timeout(DEADLINE, async {
            for (wire, kind) in [
                (vec![0x17], io::ErrorKind::UnexpectedEof),
                (vec![0x17, 3, 3, 0, 3, 1, 2], io::ErrorKind::UnexpectedEof),
                (vec![0x17, 3, 3, 0, 0], io::ErrorKind::InvalidData),
                (vec![0x16, 3, 3, 0, 1, 0], io::ErrorKind::InvalidData),
            ] {
                let (client, mut server) = tokio::io::duplex(128);
                server.write_all(&wire).await.unwrap();
                drop(server);
                let error = FakeTlsStream::new(client)
                    .read_to_end(&mut Vec::new())
                    .await
                    .unwrap_err();
                assert_eq!(error.kind(), kind);
            }
            let (client, mut server) = tokio::io::duplex(1024);
            let payload = vec![0x41; u16::MAX as usize];
            let writer = async {
                server.write_all(&record(&payload)).await.unwrap();
                drop(server);
            };
            let reader = async {
                let mut got = Vec::new();
                FakeTlsStream::new(client)
                    .read_to_end(&mut got)
                    .await
                    .unwrap();
                assert_eq!(got, payload);
            };
            tokio::join!(writer, reader);
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn mtproxy_faketls_cancelled_headers_resume_without_duplicate_plaintext() {
        timeout(DEADLINE, async {
            let (client, mut server) = tokio::io::duplex(1);
            let mut stream = FakeTlsStream::new(client);
            let payload = [0x51; 88];
            assert!(
                timeout(Duration::from_millis(10), stream.write_all(&payload))
                    .await
                    .is_err()
            );
            assert_eq!(stream.tx_left, payload.len());
            let writer = async {
                stream.write_all(&payload).await.unwrap();
            };
            let reader = async {
                let mut wire = vec![0; 11 + payload.len()];
                server.read_exact(&mut wire).await.unwrap();
                assert_eq!(&wire[..6], &TLS_CCS);
                assert_eq!(&wire[6..11], &[0x17, 3, 3, 0, 88]);
                assert_eq!(&wire[11..], &payload);
            };
            tokio::join!(writer, reader);
            let (client, mut server) = tokio::io::duplex(16);
            let mut stream = FakeTlsStream::new(client);
            server.write_all(&[0x17, 3]).await.unwrap();
            assert!(
                timeout(Duration::from_millis(10), stream.read_exact(&mut [0; 1]))
                    .await
                    .is_err()
            );
            assert_eq!(stream.rx_at, 2);
            server.write_all(&[3, 0, 3, 1, 2, 3]).await.unwrap();
            let mut data = [0; 3];
            stream.read_exact(&mut data).await.unwrap();
            assert_eq!(data, [1, 2, 3]);
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn mtproxy_faketls_setup_cancellation_drops_socket() {
        timeout(DEADLINE, async {
            let (client, mut server) = tokio::io::duplex(1024);
            let secret = secret();
            assert!(
                timeout(Duration::from_millis(10), connect_faketls(client, &secret))
                    .await
                    .is_err()
            );
            let mut hello = [0; 517];
            server.read_exact(&mut hello).await.unwrap();
            assert_eq!(server.read(&mut [0; 1]).await.unwrap(), 0);
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn mtproxy_faketls_zero_nonempty_write_is_an_error() {
        struct Zero;
        impl AsyncWrite for Zero {
            fn poll_write(
                self: Pin<&mut Self>,
                _: &mut Context<'_>,
                _: &[u8],
            ) -> Poll<io::Result<usize>> {
                Poll::Ready(Ok(0))
            }
            fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
                Poll::Ready(Ok(()))
            }
            fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
                Poll::Ready(Ok(()))
            }
        }
        assert_eq!(
            FakeTlsStream::new(Zero)
                .write_all(b"data")
                .await
                .unwrap_err()
                .kind(),
            io::ErrorKind::WriteZero
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn secret(padded: bool) -> Secret {
        Secret {
            key: [0x71; 16],
            padded,
            domain: None,
        }
    }
    fn plain_payload() -> Vec<u8> {
        let mut p = vec![0; 24];
        p[8..16].copy_from_slice(&1i64.to_le_bytes());
        p[16..20].copy_from_slice(&4i32.to_le_bytes());
        p[20..].copy_from_slice(&[1, 2, 3, 4]);
        p
    }
    fn packet(payload: &[u8]) -> DequeBuffer<u8> {
        let mut b = DequeBuffer::with_capacity(payload.len() + 128, 128);
        b.extend(payload);
        b
    }
    fn server(header: &[u8], key: &[u8; 16]) -> (ObfuscatedCipher, [u8; 64]) {
        let mut reverse: [u8; 64] = header.try_into().unwrap();
        reverse.reverse();
        let mut c = ObfuscatedCipher::with_secret(&reverse, key);
        let mut clear: [u8; 64] = header.try_into().unwrap();
        c.decrypt(&mut clear);
        (c, clear)
    }
    #[test]
    fn mtproxy_secrets_and_links_are_bounded_and_redacted() {
        let key = "71717171717171717171717171717171";
        for encoded in [
            key.to_owned(),
            general_purpose::URL_SAFE_NO_PAD.encode([0x71; 16]),
            general_purpose::STANDARD.encode([0x71; 16]),
        ] {
            assert_eq!(Secret::parse(&encoded).unwrap().key, [0x71; 16]);
        }
        assert!(Secret::parse(&format!("dd{key}")).unwrap().padded);
        let tls = Secret::parse(&format!("ee{key}6578616d706c652e636f6d")).unwrap();
        assert_eq!(tls.domain.as_deref(), Some("example.com"));
        assert!(tls.is_faketls());
        assert!(!format!("{tls:?}").contains(key));
        for input in [
            "mtproxy://example.com:443?secret=",
            "mtproxy://example.com:443?secret=bad",
            "mtproxy://example.com:443?secret=bad&secret=bad",
            "mtproxy://user:pass@example.com:443?secret=bad",
        ] {
            let e = parse_url(&Url::parse(input).unwrap()).unwrap_err();
            assert!(!e.to_string().contains(input));
        }
        for url in [
            format!("mtproxy://example.com:443?secret={key}"),
            format!("tg://proxy?server=example.com&port=443&secret={key}"),
            format!("https://t.me/proxy?server=example.com&port=443&secret={key}"),
        ] {
            let p = parse_url(&Url::parse(&url).unwrap()).unwrap();
            assert_eq!((p.host.as_str(), p.port), ("example.com", 443));
            assert_eq!(p.secret.key, [0x71; 16]);
        }
        assert_eq!(
            parse_url(&Url::parse(&format!("mtproxy://[::1]:443?secret={key}")).unwrap())
                .unwrap()
                .host,
            "::1"
        );
        for bytes in [
            vec![0; 15],
            vec![0; 17],
            vec![0xdd; 18],
            vec![0xee; 17],
            [vec![0xee; 17], vec![b'a'; MAX_DOMAIN + 1]].concat(),
        ] {
            assert!(Secret::parse(&grammers_crypto::hex::to_hex(&bytes)).is_err());
        }
    }
    #[test]
    fn mtproxy_raw_and_padded_handshakes_embed_tag_dc_and_stream_counters() {
        for padded in [false, true] {
            for dc in [2, -4, 10002] {
                let secret = secret(padded);
                let mut client = MtProxyTransport::with_nonce(&secret, dc, [0x31; 64]);
                let payload = plain_payload();
                let mut b = packet(&payload);
                client.pack(&mut b);
                let (mut server, clear) = server(&b[..64], &secret.key);
                assert_eq!(&clear[56..60], &if padded { [0xdd; 4] } else { [0xef; 4] });
                assert_eq!(&clear[60..62], &dc.to_le_bytes());
                let mut body = b[64..].to_vec();
                server.decrypt(&mut body);
                let offset = if padded {
                    unpack_padded(&body).unwrap()
                } else {
                    Abridged::new().unpack(&mut body).unwrap()
                };
                assert_eq!(&body[offset.data_range], payload);
                let mut second = packet(&payload);
                client.pack(&mut second);
                assert!(second.len() < 64);
                server.decrypt(second.as_mut());
                let offset = if padded {
                    unpack_padded(second.as_ref()).unwrap()
                } else {
                    Abridged::new().unpack(second.as_mut()).unwrap()
                };
                assert_eq!(&second[offset.data_range], payload);
            }
        }
    }
    #[test]
    fn mtproxy_partial_reply_keeps_decryption_position_and_removes_padding() {
        for padded in [false, true] {
            let secret = secret(padded);
            let mut client = MtProxyTransport::with_nonce(&secret, 2, [0x21; 64]);
            let mut first = packet(&plain_payload());
            client.pack(&mut first);
            let (mut server, _) = server(&first[..64], &secret.key);
            let payload = plain_payload();
            let mut reply = if padded {
                let mut bytes = ((payload.len() + 11) as u32).to_le_bytes().to_vec();
                bytes.extend(&payload);
                bytes.extend([0x33; 11]);
                bytes
            } else {
                let mut bytes = packet(&payload);
                let mut abridged = Abridged::new();
                abridged.init_tag();
                abridged.pack(&mut bytes);
                bytes.as_ref().to_vec()
            };
            server.encrypt(&mut reply);
            let mut read = Vec::new();
            for (index, byte) in reply.iter().enumerate() {
                read.push(*byte);
                let result = client.unpack(&mut read);
                if index + 1 < reply.len() {
                    assert_eq!(result, Err(Error::MissingBytes));
                } else {
                    let offset = result.unwrap();
                    assert_eq!(&read[offset.data_range], payload);
                    assert_eq!(offset.next_offset, reply.len());
                }
            }
        }
        for padding in 0..16 {
            let mut encrypted = vec![1; 56];
            encrypted.extend(std::iter::repeat_n(0x35, padding));
            let mut bytes = (encrypted.len() as u32).to_le_bytes().to_vec();
            bytes.extend(encrypted);
            assert_eq!(unpack_padded(&bytes).unwrap().data_range, 4..60);
        }
    }
    #[test]
    fn mtproxy_padded_malformed_lengths_and_statuses_are_rejected() {
        for size in [-1i32, 0, 3, (MAX_PACKET + 1) as i32] {
            assert_eq!(
                unpack_padded(&size.to_le_bytes()),
                Err(Error::BadLen { got: size })
            );
        }
        let mut error = 15u32.to_le_bytes().to_vec();
        error.extend((-404i32).to_le_bytes());
        error.extend([0; 11]);
        assert_eq!(unpack_padded(&error), Err(Error::BadStatus { status: 404 }));
        let mut body = 24u32.to_le_bytes().to_vec();
        body.extend(plain_payload());
        body[20..24].copy_from_slice(&100i32.to_le_bytes());
        assert!(matches!(unpack_padded(&body), Err(Error::BadLen { .. })));
    }
}
