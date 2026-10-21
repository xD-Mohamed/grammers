// Copyright 2026 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Portable home-datacenter authorization strings (`session-string` feature).
//!
//! Version 1 preserves exactly one DC's key, IPv4/IPv6 endpoints and preferred
//! address family. It is not a storage backup: other DCs, peers, update state and
//! API ID are omitted. No file, network or authorization operation is performed
//! by the codec. A structurally valid key is not proof of a logged-in account;
//! the application must check authorization after connecting.
//!
//! # Wire format
//!
//! The ASCII prefix `grammers:1:` is followed by canonical URL-safe, unpadded
//! base64 of the following 332 bytes. All integers are big endian. Offsets are
//! byte offsets in the decoded value; there are no variable-length fields or
//! route counts, and trailing bytes are forbidden.
//!
//! | Offset | Bytes | Meaning |
//! | --- | --- | --- |
//! | 0 | 4 | Magic `GRAM` |
//! | 4 | 1 | Version, exactly 1 |
//! | 5 | 1 | Flags: bit 0 prefers IPv6; other bits zero |
//! | 6 | 2 | Reserved, zero |
//! | 8 | 4 | Positive signed i32 home DC ID |
//! | 12 | 4 | IPv4 address octets |
//! | 16 | 2 | IPv4 port |
//! | 18 | 16 | IPv6 address octets |
//! | 34 | 2 | IPv6 port |
//! | 36 | 4 | IPv6 flow information |
//! | 40 | 4 | IPv6 scope ID |
//! | 44 | 256 | Nonzero home-DC authentication key |
//! | 300 | 32 | SHA-256 of ASCII prefix followed by bytes 0..300 |
//!
//! The unused endpoint may be the unavailable sentinel (unspecified address,
//! zero port, and zero IPv6 flow/scope). The preferred endpoint must be present.
//! When IPv6 is preferred, an unspecified IPv4 placeholder may retain a nonzero
//! port from a foreign import; it is preserved but can never be selected.
//! Other endpoints must have a nonzero port and a non-unspecified, non-multicast
//! address; IPv4 broadcast is also rejected. Custom and test DCs are supported.
//! The complete string is 454 ASCII bytes; inputs over 1024 bytes are rejected
//! before decoding. The checksum detects accidental corruption, **not** malicious
//! modification. Strings are unencrypted credentials: protect them like passwords.
//! Debug output is redacted, but explicit secret access and conversion to ordinary
//! session data exposes credentials. Memory is not guaranteed to be zeroized.

use std::collections::HashMap;
use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};

use crate::storages::MemorySession;
use crate::types::DcOption;
use crate::{Session, SessionData};

const PREFIX: &str = "grammers:1:";
const PAYLOAD_LEN: usize = 300;
const DECODED_LEN: usize = PAYLOAD_LEN + 32;
const ENCODED_LEN: usize = 443;
const MAX_INPUT_LEN: usize = 1024;

/// Credential-free errors from native session parsing or export.
///
/// Storage errors are deliberately reduced to [`Self::Storage`]. The original
/// error is neither retained nor formatted: arbitrary storage errors may contain
/// credentials. As a result, this type cannot diagnose the underlying storage
/// failure, and its [`std::error::Error::source`] is always `None`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SessionStringError {
    /// Input exceeds the encoded size limit.
    TooLong,
    /// Input is not a native session string.
    InvalidPrefix,
    /// The textual or binary format version is unsupported.
    UnsupportedVersion,
    /// Base64 is invalid, padded or noncanonical.
    InvalidEncoding,
    /// The fixed payload length is incorrect.
    InvalidLength,
    /// Magic, reserved fields or flags are invalid.
    InvalidHeader,
    /// The corruption checksum does not match.
    InvalidChecksum,
    /// Home DC is nonpositive or inconsistent with its saved route.
    InvalidDc,
    /// A saved endpoint is invalid or the preferred endpoint is unavailable.
    InvalidEndpoint,
    /// The source has no home-DC route.
    MissingDc,
    /// The source has no home-DC authentication key.
    MissingAuthKey,
    /// The authentication key is all zero.
    InvalidAuthKey,
    /// The source storage could not be read (details redacted).
    Storage,
}

impl fmt::Display for SessionStringError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::TooLong => "native session string exceeds size limit",
            Self::InvalidPrefix => "invalid native session string prefix",
            Self::UnsupportedVersion => "unsupported native session string version",
            Self::InvalidEncoding => "invalid native session string encoding",
            Self::InvalidLength => "invalid native session string length",
            Self::InvalidHeader => "invalid native session string header",
            Self::InvalidChecksum => "native session string checksum mismatch",
            Self::InvalidDc => "invalid or inconsistent home datacenter",
            Self::InvalidEndpoint => "invalid or unavailable datacenter endpoint",
            Self::MissingDc => "home datacenter route is missing",
            Self::MissingAuthKey => "home datacenter authentication key is missing",
            Self::InvalidAuthKey => "invalid home datacenter authentication key",
            Self::Storage => "session storage read failed (details redacted)",
        })
    }
}

impl std::error::Error for SessionStringError {}

/// An encoded credential, with redacted `Debug` and no implicit `Display`.
pub struct SessionString(String);

impl SessionString {
    /// Explicitly borrow the unencrypted credential. Do not log or share it.
    pub fn expose_secret(&self) -> &str {
        &self.0
    }

    /// Take the unencrypted credential, removing this wrapper's debug protection.
    pub fn into_inner(self) -> String {
        self.0
    }
}

impl fmt::Debug for SessionString {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionString([REDACTED])")
    }
}

/// Validated home-DC credentials and exact connection endpoints.
///
/// Validation is local and does not establish whether Telegram accepts the key.
pub struct NativeSession {
    dc: DcOption,
    prefer_ipv6: bool,
}

impl fmt::Debug for NativeSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("NativeSession([REDACTED])")
    }
}

impl NativeSession {
    /// Parse a canonical v1 native string without network, file or storage access.
    pub fn parse(input: &str) -> Result<Self, SessionStringError> {
        if input.len() > MAX_INPUT_LEN {
            return Err(SessionStringError::TooLong);
        }
        let versioned = input
            .strip_prefix("grammers:")
            .ok_or(SessionStringError::InvalidPrefix)?;
        let encoded = versioned
            .strip_prefix("1:")
            .ok_or(SessionStringError::UnsupportedVersion)?;
        // Check alphabet separately so whitespace and padding are always rejected.
        if !encoded
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(SessionStringError::InvalidEncoding);
        }
        if encoded.len() != ENCODED_LEN {
            return Err(SessionStringError::InvalidLength);
        }
        let mut bytes = [0; DECODED_LEN];
        let len = URL_SAFE_NO_PAD
            .decode_slice(encoded, &mut bytes)
            .map_err(|_| SessionStringError::InvalidEncoding)?;
        if len != DECODED_LEN {
            return Err(SessionStringError::InvalidLength);
        }
        let checksum = Sha256::new()
            .chain_update(PREFIX.as_bytes())
            .chain_update(&bytes[..PAYLOAD_LEN])
            .finalize();
        if bytes[PAYLOAD_LEN..] != checksum[..] {
            return Err(SessionStringError::InvalidChecksum);
        }
        if &bytes[..4] != b"GRAM" {
            return Err(SessionStringError::InvalidHeader);
        }
        if bytes[4] != 1 {
            return Err(SessionStringError::UnsupportedVersion);
        }
        if bytes[5] & !1 != 0 || bytes[6..8] != [0, 0] {
            return Err(SessionStringError::InvalidHeader);
        }
        let dc = DcOption {
            id: i32::from_be_bytes(field(&bytes, 8)),
            ipv4: SocketAddrV4::new(
                Ipv4Addr::from(field::<4>(&bytes, 12)),
                u16::from_be_bytes(field(&bytes, 16)),
            ),
            ipv6: SocketAddrV6::new(
                Ipv6Addr::from(field::<16>(&bytes, 18)),
                u16::from_be_bytes(field(&bytes, 34)),
                u32::from_be_bytes(field(&bytes, 36)),
                u32::from_be_bytes(field(&bytes, 40)),
            ),
            auth_key: Some(field(&bytes, 44)),
        };
        Self::validated(dc, bytes[5] == 1)
    }

    /// Export only the home-DC route and its authentication key.
    ///
    /// Works with concrete or erased sessions. Reads only `home_dc_id` and
    /// `dc_option`; no peer query or storage write is performed. Reads are not
    /// an atomic storage snapshot: callers should avoid concurrent session
    /// mutation while exporting. Storage errors lose their original detail to
    /// prevent credential disclosure. The supplied address preference is saved
    /// explicitly because [`Session`] does not store it.
    pub fn from_session<S: Session + ?Sized>(
        session: &S,
        prefer_ipv6: bool,
    ) -> Result<Self, SessionStringError> {
        let home_dc = session
            .home_dc_id()
            .map_err(|_| SessionStringError::Storage)?;
        if home_dc <= 0 {
            return Err(SessionStringError::InvalidDc);
        }
        let dc = session
            .dc_option(home_dc)
            .map_err(|_| SessionStringError::Storage)?
            .ok_or(SessionStringError::MissingDc)?;
        if dc.id != home_dc {
            return Err(SessionStringError::InvalidDc);
        }
        Self::validated(dc, prefer_ipv6)
    }

    fn validated(dc: DcOption, prefer_ipv6: bool) -> Result<Self, SessionStringError> {
        if dc.id <= 0 {
            return Err(SessionStringError::InvalidDc);
        }
        let key = dc
            .auth_key
            .as_ref()
            .ok_or(SessionStringError::MissingAuthKey)?;
        if key.iter().all(|&b| b == 0) {
            return Err(SessionStringError::InvalidAuthKey);
        }
        // IPv6-only imports retain the source port on an unspecified IPv4
        // placeholder. Preserve it without ever treating it as a usable route.
        let ipv4_absent = dc.ipv4.ip().is_unspecified() && (dc.ipv4.port() == 0 || prefer_ipv6);
        let ipv6_absent = dc.ipv6.ip().is_unspecified()
            && dc.ipv6.port() == 0
            && dc.ipv6.flowinfo() == 0
            && dc.ipv6.scope_id() == 0;
        let ipv4_valid = dc.ipv4.port() != 0
            && !dc.ipv4.ip().is_unspecified()
            && !dc.ipv4.ip().is_multicast()
            && !dc.ipv4.ip().is_broadcast();
        let ipv6_valid =
            dc.ipv6.port() != 0 && !dc.ipv6.ip().is_unspecified() && !dc.ipv6.ip().is_multicast();
        if (!ipv4_valid && !ipv4_absent)
            || (!ipv6_valid && !ipv6_absent)
            || (prefer_ipv6 && !ipv6_valid)
            || (!prefer_ipv6 && !ipv4_valid)
        {
            return Err(SessionStringError::InvalidEndpoint);
        }
        Ok(Self { dc, prefer_ipv6 })
    }

    /// Encode the validated state as an unencrypted, canonical v1 credential.
    pub fn encode(&self) -> SessionString {
        let mut bytes = [0; DECODED_LEN];
        bytes[..4].copy_from_slice(b"GRAM");
        bytes[4] = 1;
        bytes[5] = u8::from(self.prefer_ipv6);
        bytes[8..12].copy_from_slice(&self.dc.id.to_be_bytes());
        bytes[12..16].copy_from_slice(&self.dc.ipv4.ip().octets());
        bytes[16..18].copy_from_slice(&self.dc.ipv4.port().to_be_bytes());
        bytes[18..34].copy_from_slice(&self.dc.ipv6.ip().octets());
        bytes[34..36].copy_from_slice(&self.dc.ipv6.port().to_be_bytes());
        bytes[36..40].copy_from_slice(&self.dc.ipv6.flowinfo().to_be_bytes());
        bytes[40..44].copy_from_slice(&self.dc.ipv6.scope_id().to_be_bytes());
        if let Some(key) = &self.dc.auth_key {
            bytes[44..PAYLOAD_LEN].copy_from_slice(key);
        }
        let checksum = Sha256::new()
            .chain_update(PREFIX.as_bytes())
            .chain_update(&bytes[..PAYLOAD_LEN])
            .finalize();
        bytes[PAYLOAD_LEN..].copy_from_slice(&checksum);
        let mut output = String::with_capacity(PREFIX.len() + ENCODED_LEN);
        output.push_str(PREFIX);
        URL_SAFE_NO_PAD.encode_string(bytes, &mut output);
        SessionString(output)
    }

    /// Whether the exact IPv6 endpoint must be selected when connecting.
    pub fn prefer_ipv6(&self) -> bool {
        self.prefer_ipv6
    }

    /// Transfer credentials into session data with only the saved home route.
    ///
    /// Peers and update state start empty; no other datacenter is synthesized.
    /// The address-family preference must be applied separately when connecting.
    pub fn into_session_data(self) -> SessionData {
        SessionData {
            home_dc: self.dc.id,
            dc_options: HashMap::from([(self.dc.id, self.dc)]),
            peer_infos: HashMap::new(),
            updates_state: Default::default(),
        }
    }

    /// Create an in-memory storage without any file, network or login operation.
    ///
    /// Call [`Self::prefer_ipv6`] before consuming this value and pass that
    /// preference to the connection configuration. The memory storage itself
    /// neither disables automatic peer caching nor persists subsequent changes.
    pub fn into_memory_session(self) -> MemorySession {
        MemorySession::from(self.into_session_data())
    }
}

// Every call uses a fixed offset within the already length-checked buffer.
fn field<const N: usize>(bytes: &[u8; DECODED_LEN], offset: usize) -> [u8; N] {
    let mut result = [0; N];
    result.copy_from_slice(&bytes[offset..offset + N]);
    result
}
