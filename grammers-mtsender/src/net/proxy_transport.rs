// Copyright 2026 - developers of the `grammers` project.
// Licensed under the Apache License, Version 2.0 or the MIT license, at your option.

use super::mtproxy;
use grammers_crypto::DequeBuffer;
use grammers_mtproto::transport::{self, Transport};

/// Abridged transport, selecting secret-mixed obfuscation and padding for MTProxy.
///
/// The sender pool configures this automatically for each DC. Low-level senders
/// using MTProxy must pass this transport and the same proxy URL in `ServerAddr`.
/// HTTP(S) and SOCKS proxies also work with ordinary MTProto transports.
pub struct ProxyTransport {
    direct: transport::Abridged,
    proxy: Option<Box<mtproxy::MtProxyTransport>>,
}
impl Default for ProxyTransport {
    fn default() -> Self {
        Self::new()
    }
}
impl ProxyTransport {
    pub(crate) fn for_connection(
        proxy_url: &str,
        dc: &grammers_session::types::DcOption,
        use_ipv6: bool,
    ) -> std::io::Result<Self> {
        let url = url::Url::parse(proxy_url)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
        if !mtproxy::is_mtproxy_url(&url) {
            return Ok(Self::new());
        }
        Self::for_proxy(proxy_url, dc_tag(dc, use_ipv6)?)
    }
    /// An ordinary abridged transport with no proxy-specific framing.
    pub fn new() -> Self {
        Self {
            direct: transport::Abridged::new(),
            proxy: None,
        }
    }
    /// Select framing for a proxy URL and the destination's MTProxy DC tag.
    /// Production DCs use their positive ID; test DCs add 10000, and media DCs
    /// use the negative ID. Errors never contain the URL or secret.
    pub fn for_proxy(proxy_url: &str, dc_id: i16) -> std::io::Result<Self> {
        let url = url::Url::parse(proxy_url)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidInput, error))?;
        if mtproxy::is_mtproxy_url(&url) {
            let proxy = mtproxy::parse_url(&url)?;
            Ok(Self {
                direct: transport::Abridged::new(),
                proxy: Some(Box::new(mtproxy::MtProxyTransport::new(
                    &proxy.secret,
                    dc_id,
                )?)),
            })
        } else {
            Ok(Self::new())
        }
    }
}

fn dc_tag(dc: &grammers_session::types::DcOption, use_ipv6: bool) -> std::io::Result<i16> {
    // The session trait exposes endpoints rather than a test-mode flag.
    // Match the same official endpoints as the optional session importers.
    let test = if use_ipv6 {
        matches!(
            dc.ipv6.ip().segments(),
            [0x2001, 0xb28, 0xf23d, 0xf001 | 0xf003, 0, 0, 0, 0xe]
                | [0x2001, 0x67c, 0x4e8, 0xf002, 0, 0, 0, 0xe]
        )
    } else {
        matches!(
            dc.ipv4.ip().octets(),
            [149, 154, 175, 10] | [149, 154, 167, 40] | [149, 154, 175, 117]
        )
    };
    dc.id
        .checked_add(if test { 10000 } else { 0 })
        .and_then(|id| i16::try_from(id).ok())
        .filter(|id| *id != 0)
        .ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "MTProxy DC ID is out of range",
            )
        })
}

impl Transport for ProxyTransport {
    fn supports_mtproxy(&self) -> bool {
        self.proxy.is_some()
    }
    fn pack(&mut self, buffer: &mut DequeBuffer<u8>) {
        if let Some(proxy) = &mut self.proxy {
            proxy.pack(buffer);
        } else {
            self.direct.pack(buffer);
        }
    }
    fn unpack(&mut self, buffer: &mut [u8]) -> Result<transport::UnpackedOffset, transport::Error> {
        if let Some(proxy) = &mut self.proxy {
            proxy.unpack(buffer)
        } else {
            self.direct.unpack(buffer)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mtproxy_tags_follow_the_selected_dc_and_test_endpoint() {
        let mut dc = grammers_session::types::DcOption {
            id: 2,
            ipv4: "192.0.2.1:443".parse().unwrap(),
            ipv6: "[2001:db8::2]:443".parse().unwrap(),
            auth_key: None,
        };
        assert_eq!(dc_tag(&dc, false).unwrap(), 2);
        dc.id = 4;
        assert_eq!(dc_tag(&dc, true).unwrap(), 4);
        dc.ipv4 = "149.154.167.40:80".parse().unwrap();
        assert_eq!(dc_tag(&dc, false).unwrap(), 10004);
        assert_eq!(dc_tag(&dc, true).unwrap(), 4);
        dc.ipv6 = "[2001:67c:4e8:f002::e]:80".parse().unwrap();
        assert_eq!(dc_tag(&dc, true).unwrap(), 10004);
        dc.id = i32::MAX;
        assert!(dc_tag(&dc, false).is_err());
        assert!(ProxyTransport::for_connection("http://127.0.0.1:8080", &dc, false).is_ok());
    }
}
