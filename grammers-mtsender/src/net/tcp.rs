// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
#[cfg(not(feature = "proxy"))]
pub use tokio::net::tcp::{ReadHalf, WriteHalf};
use tracing::{info, warn};

use super::ServerAddr;

pub enum NetStream {
    Tcp(TcpStream),
    #[cfg(feature = "proxy")]
    ProxySocks5(tokio_socks::tcp::Socks5Stream<TcpStream>),
    #[cfg(feature = "proxy")]
    Tls(Box<TlsParts>),
    #[cfg(feature = "proxy")]
    FakeTls(Box<Parts<super::mtproxy::FakeTlsStream<TcpStream>>>),
}

#[cfg(feature = "proxy")]
type BufferedTls = tokio::io::BufReader<super::tls_proxy::TlsStream>;
#[cfg(feature = "proxy")]
type TlsParts = Parts<BufferedTls>;
#[cfg(feature = "proxy")]
pub(crate) struct Parts<S> {
    reader: tokio::io::ReadHalf<S>,
    writer: super::tls_proxy::FlushWriter<tokio::io::WriteHalf<S>>,
}

#[cfg(feature = "proxy")]
pub enum ReadHalf<'a> {
    Tcp(tokio::net::tcp::ReadHalf<'a>),
    Tls(&'a mut tokio::io::ReadHalf<BufferedTls>),
    FakeTls(&'a mut tokio::io::ReadHalf<super::mtproxy::FakeTlsStream<TcpStream>>),
}
#[cfg(feature = "proxy")]
impl tokio::io::AsyncRead for ReadHalf<'_> {
    fn poll_read(
        self: std::pin::Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut tokio::io::ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        match self.get_mut() {
            Self::Tcp(reader) => std::pin::Pin::new(reader).poll_read(cx, buf),
            Self::Tls(reader) => std::pin::Pin::new(reader).poll_read(cx, buf),
            Self::FakeTls(reader) => std::pin::Pin::new(reader).poll_read(cx, buf),
        }
    }
}
#[cfg(feature = "proxy")]
pub enum WriteHalf<'a> {
    Tcp(tokio::net::tcp::WriteHalf<'a>),
    Tls(&'a mut super::tls_proxy::FlushWriter<tokio::io::WriteHalf<BufferedTls>>),
    FakeTls(
        &'a mut super::tls_proxy::FlushWriter<
            tokio::io::WriteHalf<super::mtproxy::FakeTlsStream<TcpStream>>,
        >,
    ),
}
#[cfg(feature = "proxy")]
impl WriteHalf<'_> {
    pub(crate) async fn write(&mut self, buffer: &[u8]) -> std::io::Result<usize> {
        match self {
            Self::Tcp(writer) => writer.write(buffer).await,
            Self::Tls(writer) => writer.write(buffer).await,
            Self::FakeTls(writer) => writer.write(buffer).await,
        }
    }
    #[cfg(test)]
    pub(crate) async fn write_all(&mut self, mut buffer: &[u8]) -> std::io::Result<()> {
        while !buffer.is_empty() {
            let count = self.write(buffer).await?;
            if count == 0 {
                return Err(std::io::ErrorKind::WriteZero.into());
            }
            buffer = &buffer[count..];
        }
        Ok(())
    }
}

impl NetStream {
    pub(crate) fn split(&mut self) -> (ReadHalf<'_>, WriteHalf<'_>) {
        #[cfg(not(feature = "proxy"))]
        {
            let Self::Tcp(stream) = self;
            stream.split()
        }
        #[cfg(feature = "proxy")]
        match self {
            Self::Tcp(stream) => {
                let (reader, writer) = stream.split();
                (ReadHalf::Tcp(reader), WriteHalf::Tcp(writer))
            }
            Self::ProxySocks5(stream) => {
                let (reader, writer) = stream.split();
                (ReadHalf::Tcp(reader), WriteHalf::Tcp(writer))
            }
            Self::Tls(parts) => (
                ReadHalf::Tls(&mut parts.reader),
                WriteHalf::Tls(&mut parts.writer),
            ),
            Self::FakeTls(parts) => (
                ReadHalf::FakeTls(&mut parts.reader),
                WriteHalf::FakeTls(&mut parts.writer),
            ),
        }
    }

    pub(crate) async fn connect(addr: &ServerAddr) -> Result<Self, std::io::Error> {
        info!("connecting...");
        let stream = match addr {
            ServerAddr::Tcp { address } => NetStream::Tcp(TcpStream::connect(address).await?),
            #[cfg(feature = "proxy")]
            ServerAddr::Proxied { address, proxy } => {
                Self::connect_proxy_stream(address, proxy).await?
            }
        };
        stream.disable_nagle();
        Ok(stream)
    }

    /// Every write is one complete MTProto packet, so Nagle's algorithm can
    /// only hold a request back until an earlier segment is acknowledged.
    /// Failing to disable it leaves a working, if slower, connection.
    fn disable_nagle(&self) {
        let result = match self {
            Self::Tcp(stream) => stream.set_nodelay(true),
            #[cfg(feature = "proxy")]
            Self::ProxySocks5(stream) => stream.set_nodelay(true),
            #[cfg(feature = "proxy")]
            Self::Tls(_) => Ok(()), // Set before splitting the TLS stream.
            #[cfg(feature = "proxy")]
            Self::FakeTls(_) => Ok(()),
        };
        if let Err(err) = result {
            warn!("failed to disable Nagle's algorithm: {err}");
        }
    }

    #[cfg(feature = "proxy")]
    async fn connect_proxy_stream(
        addr: &std::net::SocketAddr,
        proxy_url: &str,
    ) -> Result<NetStream, std::io::Error> {
        use std::io::{self, ErrorKind};

        let proxy = url::Url::parse(proxy_url)
            .map_err(|err| io::Error::new(ErrorKind::InvalidData, err))?;
        if super::mtproxy::is_mtproxy_url(&proxy) {
            let proxy = super::mtproxy::parse_url(&proxy)?;
            let address = resolve_proxy(&proxy.host, proxy.port).await?;
            let stream = TcpStream::connect(address).await?;
            if proxy.secret.is_faketls() {
                if let Err(error) = stream.set_nodelay(true) {
                    warn!("failed to disable Nagle's algorithm: {error}");
                }
                let stream = super::mtproxy::connect_faketls(stream, &proxy.secret).await?;
                let (reader, writer) = tokio::io::split(stream);
                return Ok(NetStream::FakeTls(Box::new(Parts {
                    reader,
                    writer: super::tls_proxy::FlushWriter::new(writer),
                })));
            }
            return Ok(NetStream::Tcp(stream));
        }
        let scheme = proxy.scheme();
        if !matches!(
            scheme,
            "socks4" | "socks4a" | "socks5" | "socks5h" | "http" | "https"
        ) {
            return Err(io::Error::new(
                ErrorKind::ConnectionAborted,
                "proxy scheme not supported",
            ));
        }
        let host = proxy
            .host()
            .ok_or_else(|| io::Error::new(ErrorKind::NotFound, "proxy host is missing from url"))?;
        let port = proxy
            .port_or_known_default()
            .ok_or_else(|| io::Error::new(ErrorKind::NotFound, "proxy port is missing from url"))?;
        if matches!(scheme, "http" | "https")
            && (!matches!(proxy.path(), "" | "/")
                || proxy.query().is_some()
                || proxy.fragment().is_some())
        {
            return Err(io::Error::new(
                ErrorKind::InvalidInput,
                "HTTP proxy URL cannot have a path, query or fragment",
            ));
        }
        let username = percent_encoding::percent_decode_str(proxy.username())
            .decode_utf8()
            .map_err(|_| io::Error::new(ErrorKind::InvalidInput, "proxy username is not UTF-8"))?;
        let password = percent_encoding::percent_decode_str(proxy.password().unwrap_or(""))
            .decode_utf8()
            .map_err(|_| io::Error::new(ErrorKind::InvalidInput, "proxy password is not UTF-8"))?;
        // Build and validate authentication before DNS or opening a connection.
        if matches!(scheme, "socks4" | "socks4a") {
            if !addr.is_ipv4() {
                return Err(io::Error::new(
                    ErrorKind::InvalidInput,
                    "SOCKS4 requires an IPv4 target",
                ));
            }
            if proxy.password().is_some() || username.contains('\0') {
                return Err(io::Error::new(
                    ErrorKind::InvalidInput,
                    "SOCKS4 supports a user ID without NUL, not a password",
                ));
            }
        }
        let http_request = if matches!(scheme, "http" | "https") {
            Some(super::http_connect::request(
                addr,
                &username,
                &password,
                !username.is_empty() || proxy.password().is_some(),
            )?)
        } else {
            None
        };
        let proxy_addr = resolve_proxy(&host.to_string(), port).await?;

        match http_request {
            Some(request) => {
                let mut stream = TcpStream::connect(proxy_addr).await?;
                if scheme == "https" {
                    if let Err(error) = stream.set_nodelay(true) {
                        warn!("failed to disable Nagle's algorithm: {error}");
                    }
                    let tls = super::tls_proxy::connect(stream, &host.to_string()).await?;
                    let mut buffered = tokio::io::BufReader::with_capacity(1024, tls);
                    super::http_connect::handshake_buffered(&mut buffered, &request).await?;
                    let (reader, writer) = tokio::io::split(buffered);
                    Ok(NetStream::Tls(Box::new(TlsParts {
                        reader,
                        writer: super::tls_proxy::FlushWriter::new(writer),
                    })))
                } else {
                    super::http_connect::handshake(&mut stream, &request).await?;
                    Ok(NetStream::Tcp(stream))
                }
            }
            None => {
                if matches!(scheme, "socks4" | "socks4a") {
                    let stream = if username.is_empty() {
                        tokio_socks::tcp::Socks4Stream::connect(proxy_addr, addr).await
                    } else {
                        tokio_socks::tcp::Socks4Stream::connect_with_userid(
                            proxy_addr, addr, &username,
                        )
                        .await
                    }
                    .map_err(|err| io::Error::new(ErrorKind::ConnectionAborted, err))?;
                    return Ok(NetStream::Tcp(stream.into_inner()));
                }
                if username.is_empty() {
                    Ok(NetStream::ProxySocks5(
                        tokio_socks::tcp::Socks5Stream::connect(proxy_addr, addr)
                            .await
                            .map_err(|err| io::Error::new(ErrorKind::ConnectionAborted, err))?,
                    ))
                } else {
                    Ok(NetStream::ProxySocks5(
                        tokio_socks::tcp::Socks5Stream::connect_with_password(
                            proxy_addr, addr, &username, &password,
                        )
                        .await
                        .map_err(|err| io::Error::new(ErrorKind::ConnectionAborted, err))?,
                    ))
                }
            }
        }
    }

    pub(crate) async fn disconnect(&mut self) -> std::io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.shutdown().await,
            #[cfg(feature = "proxy")]
            Self::ProxySocks5(stream) => stream.shutdown().await,
            #[cfg(feature = "proxy")]
            Self::Tls(parts) => parts.writer.shutdown().await,
            #[cfg(feature = "proxy")]
            Self::FakeTls(parts) => parts.writer.shutdown().await,
        }
    }
}

#[cfg(feature = "proxy")]
async fn resolve_proxy(host: &str, port: u16) -> std::io::Result<std::net::SocketAddr> {
    use std::{
        io,
        net::{IpAddr, SocketAddr},
    };
    let host = host
        .strip_prefix('[')
        .and_then(|host| host.strip_suffix(']'))
        .unwrap_or(host);
    if let Ok(ip) = host.parse::<IpAddr>() {
        return Ok(SocketAddr::new(ip, port));
    }
    let resolver = hickory_resolver::Resolver::builder_tokio()
        .map_err(io::Error::other)?
        .build()
        .map_err(io::Error::other)?;
    let response = resolver.lookup_ip(host).await.map_err(io::Error::other)?;
    let ip = response.iter().next().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::NotFound,
            "proxy host did not return any ip address",
        )
    })?;
    Ok(SocketAddr::new(ip, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(feature = "proxy")]
    #[tokio::test]
    async fn invalid_proxy_urls_preserve_error_kind_and_context_without_connecting() {
        let address = "127.0.0.1:9".parse().unwrap();
        for (url, kind, message) in [
            (
                "socks5:path",
                std::io::ErrorKind::NotFound,
                "proxy host is missing from url",
            ),
            (
                "socks5://127.0.0.1",
                std::io::ErrorKind::NotFound,
                "proxy port is missing from url",
            ),
            (
                "unsupported://127.0.0.1:1080",
                std::io::ErrorKind::ConnectionAborted,
                "proxy scheme not supported",
            ),
        ] {
            let Err(error) = NetStream::connect_proxy_stream(&address, url).await else {
                panic!("invalid proxy unexpectedly connected");
            };
            assert_eq!(error.kind(), kind);
            assert_eq!(error.to_string(), message);
        }
    }

    #[tokio::test]
    async fn connections_disable_nagle() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        match NetStream::connect(&ServerAddr::Tcp { address })
            .await
            .unwrap()
        {
            NetStream::Tcp(stream) => assert!(stream.nodelay().unwrap()),
            #[cfg(feature = "proxy")]
            NetStream::ProxySocks5(_) => unreachable!("a direct address connects without a proxy"),
            #[cfg(feature = "proxy")]
            NetStream::Tls(_) => unreachable!("a direct address connects without a proxy"),
            #[cfg(feature = "proxy")]
            NetStream::FakeTls(_) => unreachable!("a direct address connects without a proxy"),
        }
    }
}
