// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

#[cfg(feature = "proxy")]
mod http_connect;
#[cfg(feature = "proxy")]
pub(crate) mod mtproxy;
#[cfg(feature = "proxy")]
mod proxy_transport;
mod tcp;
#[cfg(feature = "proxy")]
mod tls_proxy;
#[cfg(feature = "proxy")]
pub use proxy_transport::ProxyTransport;

pub use tcp::NetStream;
pub(crate) use tcp::{ReadHalf, poll_readable};

#[cfg(all(test, feature = "proxy"))]
mod proxy_tests;

/// Represents a socket address which may be proxied.
#[derive(Clone)]
pub enum ServerAddr {
    /// Socket address whose connection should be proxied.
    #[cfg(feature = "proxy")]
    Proxied {
        address: std::net::SocketAddr,
        proxy: String,
    },
    /// Proxy address for direct connection.
    Tcp { address: std::net::SocketAddr },
}

impl std::fmt::Debug for ServerAddr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(feature = "proxy")]
            Self::Proxied { address, .. } => f
                .debug_struct("Proxied")
                .field("address", address)
                .field("proxy", &"[redacted]")
                .finish(),
            Self::Tcp { address } => f.debug_struct("Tcp").field("address", address).finish(),
        }
    }
}
