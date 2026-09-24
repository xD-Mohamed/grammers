// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use log::{info, warn};
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
pub use tokio::net::tcp::{ReadHalf, WriteHalf};

use super::ServerAddr;

pub enum NetStream {
    Tcp(TcpStream),
    #[cfg(feature = "proxy")]
    ProxySocks5(tokio_socks::tcp::Socks5Stream<TcpStream>),
}

impl NetStream {
    pub(crate) fn split(&mut self) -> (ReadHalf<'_>, WriteHalf<'_>) {
        match self {
            Self::Tcp(stream) => stream.split(),
            #[cfg(feature = "proxy")]
            Self::ProxySocks5(stream) => stream.split(),
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
        use std::{
            io::{self, ErrorKind},
            net::{IpAddr, SocketAddr},
        };

        use hickory_resolver::Resolver;
        use url::Host;

        let proxy = url::Url::parse(proxy_url)
            .map_err(|err| io::Error::new(ErrorKind::InvalidData, err))?;
        let scheme = proxy.scheme();
        let host = proxy.host().ok_or(io::Error::new(
            ErrorKind::NotFound,
            format!("proxy host is missing from url: {}", proxy_url),
        ))?;
        let port = proxy.port().ok_or(io::Error::new(
            ErrorKind::NotFound,
            format!("proxy port is missing from url: {}", proxy_url),
        ))?;
        let username = proxy.username();
        let password = proxy.password().unwrap_or("");
        let socks_addr = match host {
            Host::Domain(domain) => {
                let resolver = Resolver::builder_tokio().unwrap().build().unwrap();
                let response = resolver.lookup_ip(domain).await.map_err(io::Error::other)?;
                let socks_ip_addr = response.iter().next().ok_or(io::Error::new(
                    ErrorKind::NotFound,
                    format!("proxy host did not return any ip address: {}", domain),
                ))?;
                SocketAddr::new(socks_ip_addr, port)
            }
            Host::Ipv4(v4) => SocketAddr::new(IpAddr::from(v4), port),
            Host::Ipv6(v6) => SocketAddr::new(IpAddr::from(v6), port),
        };

        match scheme {
            "socks5" => {
                if username.is_empty() {
                    Ok(NetStream::ProxySocks5(
                        tokio_socks::tcp::Socks5Stream::connect(socks_addr, addr)
                            .await
                            .map_err(|err| io::Error::new(ErrorKind::ConnectionAborted, err))?,
                    ))
                } else {
                    Ok(NetStream::ProxySocks5(
                        tokio_socks::tcp::Socks5Stream::connect_with_password(
                            socks_addr, addr, username, password,
                        )
                        .await
                        .map_err(|err| io::Error::new(ErrorKind::ConnectionAborted, err))?,
                    ))
                }
            }
            scheme => Err(io::Error::new(
                ErrorKind::ConnectionAborted,
                format!("proxy scheme not supported: {}", scheme),
            )),
        }
    }

    pub(crate) async fn disconnect(&mut self) -> std::io::Result<()> {
        match self {
            Self::Tcp(stream) => stream.shutdown().await,
            #[cfg(feature = "proxy")]
            Self::ProxySocks5(stream) => stream.shutdown().await,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        }
    }
}
