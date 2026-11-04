// Copyright 2026 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::io::{self, ErrorKind};
use std::net::SocketAddr;

use base64::{Engine, engine::general_purpose::STANDARD};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

// A setup-only budget, shared by informational and final response headers.
const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_HEADERS: usize = 64;
const MAX_INFORMATIONAL: usize = 4;

pub(super) fn request(
    addr: &SocketAddr,
    username: &str,
    password: &str,
    authenticate: bool,
) -> io::Result<String> {
    // Basic uses ':' to separate user-id and password; control characters are
    // forbidden by RFC 7617. Base64 prevents header injection in either case.
    if authenticate
        && (username.contains(':')
            || username
                .chars()
                .chain(password.chars())
                .any(char::is_control))
    {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "invalid HTTP proxy Basic credentials",
        ));
    }
    // SocketAddr's Display brackets IPv6, including the mandatory target port.
    // Scope IDs are local interface identifiers, not valid remote authorities.
    if matches!(addr, SocketAddr::V6(v6) if v6.scope_id() != 0) {
        return Err(io::Error::new(
            ErrorKind::InvalidInput,
            "scoped IPv6 targets are not supported by HTTP CONNECT",
        ));
    }
    let mut request = format!("CONNECT {addr} HTTP/1.1\r\nHost: {addr}\r\n");
    if authenticate {
        request.push_str("Proxy-Authorization: Basic ");
        STANDARD.encode_string(format!("{username}:{password}"), &mut request);
        request.push_str("\r\n");
    }
    request.push_str("\r\n");
    Ok(request)
}

pub(super) async fn handshake(stream: &mut TcpStream, request: &str) -> io::Result<()> {
    stream.write_all(request.as_bytes()).await?;
    let mut remaining = MAX_HEADER_BYTES;
    for _ in 0..=MAX_INFORMATIONAL {
        let header = read_header(stream, remaining).await?;
        remaining -= header.len();
        if response_done(&header)? {
            return Ok(());
        }
    }
    Err(io::Error::new(
        ErrorKind::InvalidData,
        "too many informational HTTP CONNECT responses",
    ))
}

pub(super) async fn handshake_buffered<S>(
    stream: &mut tokio::io::BufReader<S>,
    request: &str,
) -> io::Result<()>
where
    S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin,
{
    use tokio::io::AsyncBufReadExt;
    stream.write_all(request.as_bytes()).await?;
    stream.flush().await?;
    let mut remaining = MAX_HEADER_BYTES;
    for _ in 0..=MAX_INFORMATIONAL {
        let mut header = Vec::with_capacity(remaining.min(1024));
        loop {
            if header.len() == remaining {
                return Err(io::Error::new(
                    ErrorKind::InvalidData,
                    "HTTP CONNECT response headers exceed 16 KiB budget",
                ));
            }
            // BufReader retains any tunnel bytes in the same TLS record.
            let available = stream.fill_buf().await?;
            if available.is_empty() {
                return Err(io::Error::new(
                    ErrorKind::UnexpectedEof,
                    "EOF during HTTP CONNECT response",
                ));
            }
            let count = available.len().min(remaining - header.len());
            let previous = header.len();
            header.extend_from_slice(&available[..count]);
            let end = header[previous.saturating_sub(3)..]
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|offset| previous.saturating_sub(3) + offset + 4);
            if let Some(end) = end {
                header.truncate(end);
            }
            stream.consume(header.len() - previous);
            if end.is_some() {
                break;
            }
        }
        remaining -= header.len();
        if response_done(&header)? {
            return Ok(());
        }
    }
    Err(io::Error::new(
        ErrorKind::InvalidData,
        "too many informational HTTP CONNECT responses",
    ))
}

fn response_done(header: &[u8]) -> io::Result<bool> {
    let mut fields = [httparse::EMPTY_HEADER; MAX_HEADERS];
    let mut response = httparse::Response::new(&mut fields);
    match response.parse(header) {
        Ok(httparse::Status::Complete(end)) if end == header.len() => {}
        _ => {
            return Err(io::Error::new(
                ErrorKind::InvalidData,
                "malformed HTTP CONNECT response",
            ));
        }
    }
    match response.code.unwrap_or(0) {
        200..=299 => Ok(true),
        code @ 100..=199 if code != 101 => Ok(false),
        407 => Err(io::Error::new(
            ErrorKind::PermissionDenied,
            "HTTP proxy authentication required or rejected (407)",
        )),
        code => Err(io::Error::new(
            ErrorKind::ConnectionRefused,
            format!("HTTP CONNECT rejected with status {code}"),
        )),
    }
}

async fn read_header(stream: &mut TcpStream, limit: usize) -> io::Result<Vec<u8>> {
    let mut header = Vec::with_capacity(limit.min(1024));
    let mut chunk = [0; 1024];
    while header.len() < limit {
        let capacity = chunk.len().min(limit - header.len());
        // Peek first so that bytes following CRLFCRLF remain in the socket.
        // Consume every incomplete chunk before peeking again: partial headers
        // cannot busy-loop, and no buffering wrapper affects established I/O.
        let count = stream.peek(&mut chunk[..capacity]).await?;
        if count == 0 {
            return Err(io::Error::new(
                ErrorKind::UnexpectedEof,
                "EOF during HTTP CONNECT response",
            ));
        }
        let previous = header.len();
        header.extend_from_slice(&chunk[..count]);
        let end = header[previous.saturating_sub(3)..]
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|offset| previous.saturating_sub(3) + offset + 4);
        if let Some(end) = end {
            header.truncate(end);
        }
        stream.read_exact(&mut header[previous..]).await?;
        if end.is_some() {
            return Ok(header);
        }
    }
    Err(io::Error::new(
        ErrorKind::InvalidData,
        "HTTP CONNECT response headers exceed 16 KiB budget",
    ))
}
