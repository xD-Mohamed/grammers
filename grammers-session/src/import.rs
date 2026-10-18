// Copyright 2026 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Offline imports of Pyrogram strings, Telethon strings, and gotd session JSON.
//!
//! Requires the `session-import` feature. Parsers accept only exported string
//! formats, not SQLite files or Telegram Desktop data. No I/O is performed.
//! Importing a well-formed key does not prove that it is authorized or still valid.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr, SocketAddrV4, SocketAddrV6};

use base64::Engine as _;
use base64::alphabet;
use base64::engine::{DecodePaddingMode, GeneralPurpose, GeneralPurposeConfig};
use serde::{Deserialize, Deserializer};
use sha1::{Digest as _, Sha1};

use crate::storages::MemorySession;
use crate::types::DcOption;
use crate::{KNOWN_DC_OPTIONS, SessionData};

const STRING_LIMIT: usize = 512;
const JSON_LIMIT: usize = 1024 * 1024;
const URL_SAFE: GeneralPurpose = GeneralPurpose::new(
    &alphabet::URL_SAFE,
    GeneralPurposeConfig::new().with_decode_padding_mode(DecodePaddingMode::Indifferent),
);
const STANDARD: GeneralPurpose =
    GeneralPurpose::new(&alphabet::STANDARD, GeneralPurposeConfig::new());

/// The source serialization format of an imported session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum SessionFormat {
    /// A current or legacy Pyrogram exported session string.
    Pyrogram,
    /// A version-one Telethon StringSession.
    Telethon,
    /// A version-one gotd session.Loader JSON document.
    Gotd,
}

/// A sanitized import failure. No variant contains input or authentication data.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum ImportError {
    /// Input exceeded the format's fixed size limit.
    InputTooLarge,
    /// The input does not use the required base64 alphabet or padding.
    InvalidEncoding,
    /// Decoded binary input has an unsupported length.
    InvalidLength,
    /// The serialized format version is not supported.
    UnsupportedVersion,
    /// The datacenter is not supported for the session's environment.
    InvalidDc,
    /// A boolean is not encoded as zero or one.
    InvalidBoolean,
    /// The application identifier is zero or outside the API's signed range.
    InvalidApiId,
    /// The stored user identifier is zero.
    InvalidUserId,
    /// The authentication key is not exactly 256 bytes or is all zero.
    InvalidAuthKey,
    /// The optional gotd key fingerprint is malformed or does not match.
    InvalidAuthKeyId,
    /// The endpoint is not a usable IP address and nonzero port.
    InvalidAddress,
    /// The JSON document is malformed or has missing, duplicate, or mistyped fields.
    InvalidJson,
}

impl fmt::Display for ImportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InputTooLarge => "session input exceeds the size limit",
            Self::InvalidEncoding => "invalid session base64 encoding",
            Self::InvalidLength => "unsupported session payload length",
            Self::UnsupportedVersion => "unsupported session format version",
            Self::InvalidDc => "invalid session datacenter",
            Self::InvalidBoolean => "invalid session boolean",
            Self::InvalidApiId => "invalid session application identifier",
            Self::InvalidUserId => "invalid session user identifier",
            Self::InvalidAuthKey => "invalid session authentication key",
            Self::InvalidAuthKeyId => "invalid session authentication key fingerprint",
            Self::InvalidAddress => "invalid session endpoint",
            Self::InvalidJson => "invalid session JSON document",
        })
    }
}

impl std::error::Error for ImportError {}

/// Validated imported session state and source metadata.
///
/// Parsing does not contact Telegram, read files, or authorize an account. The
/// authentication key is placed only on the home datacenter. Imported metadata
/// is untrusted until confirmed by Telegram. Debug output redacts credentials
/// and account identifiers.
pub struct ImportedSession {
    data: SessionData,
    format: SessionFormat,
    api_id: Option<i32>,
    user_id: Option<u64>,
    is_bot: Option<bool>,
    test_mode: Option<bool>,
    prefer_ipv6: bool,
}

impl fmt::Debug for ImportedSession {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImportedSession")
            .field("format", &self.format)
            .field("dc_id", &self.dc_id())
            .field("test_mode", &self.test_mode)
            .field("prefer_ipv6", &self.prefer_ipv6)
            .field("credentials", &"[REDACTED]")
            .finish_non_exhaustive()
    }
}

impl ImportedSession {
    /// Parses a current or legacy Pyrogram session string.
    ///
    /// Accepts URL-safe base64 with or without padding. Supports the 271-byte
    /// current layout and 263-/267-byte legacy layouts. Legacy layouts do not
    /// contain an application identifier. Inputs are limited to 512 bytes.
    ///
    /// # Errors
    /// Returns a sanitized [`ImportError`] for malformed or unsupported input.
    pub fn from_pyrogram(input: &str) -> Result<Self, ImportError> {
        check_size(input, STRING_LIMIT)?;
        let bytes = URL_SAFE
            .decode(input.trim())
            .map_err(|_| ImportError::InvalidEncoding)?;
        let (api_id, test_offset, user_size) = match bytes.len() {
            271 => {
                let id = u32::from_be_bytes(read_fixed(&bytes, 1)?);
                if id == 0 || id > i32::MAX as u32 {
                    return Err(ImportError::InvalidApiId);
                }
                (Some(id as i32), 5, 8)
            }
            263 => (None, 1, 4),
            267 => (None, 1, 8),
            _ => return Err(ImportError::InvalidLength),
        };
        let dc = i32::from(bytes[0]);
        let test_mode = parse_boolean(bytes[test_offset])?;
        let key_offset = test_offset + 1;
        let key = validate_key(&bytes[key_offset..key_offset + 256])?;
        let user_offset = key_offset + 256;
        let user_id = if user_size == 8 {
            u64::from_be_bytes(read_fixed(&bytes, user_offset)?)
        } else {
            u64::from(u32::from_be_bytes(read_fixed(&bytes, user_offset)?))
        };
        if user_id == 0 {
            return Err(ImportError::InvalidUserId);
        }
        let is_bot = parse_boolean(bytes[user_offset + user_size])?;
        let mut imported =
            Self::from_parts(SessionFormat::Pyrogram, dc, Some(test_mode), None, key)?;
        imported.api_id = api_id;
        imported.user_id = Some(user_id);
        imported.is_bot = Some(is_bot);
        Ok(imported)
    }

    /// Parses a Telethon version-one StringSession, preserving its endpoint.
    ///
    /// Accepts IPv4 and IPv6 layouts with optional URL-safe base64 padding.
    /// Known test endpoints use test routes; unrecognized endpoints import only
    /// their explicit route. Inputs are limited to 512 bytes.
    ///
    /// # Errors
    /// Returns a sanitized [`ImportError`] for malformed or unsupported input.
    pub fn from_telethon(input: &str) -> Result<Self, ImportError> {
        check_size(input, STRING_LIMIT)?;
        let encoded = input
            .trim()
            .strip_prefix('1')
            .ok_or(ImportError::UnsupportedVersion)?;
        let bytes = URL_SAFE
            .decode(encoded)
            .map_err(|_| ImportError::InvalidEncoding)?;
        let (ip, port_offset) = match bytes.len() {
            263 => (IpAddr::V4(Ipv4Addr::from(read_fixed::<4>(&bytes, 1)?)), 5),
            275 => (IpAddr::V6(Ipv6Addr::from(read_fixed::<16>(&bytes, 1)?)), 17),
            _ => return Err(ImportError::InvalidLength),
        };
        let port = u16::from_be_bytes(read_fixed(&bytes, port_offset)?);
        let endpoint = validate_endpoint(SocketAddr::new(ip, port))?;
        let key = validate_key(&bytes[port_offset + 2..])?;
        Self::from_parts(
            SessionFormat::Telethon,
            i32::from(bytes[0]),
            endpoint_environment(ip),
            Some(endpoint),
            key,
        )
    }

    /// Parses a gotd version-one session.Loader JSON document.
    ///
    /// Requires `Data.DC`, `Data.Addr`, `Data.AuthKey`, and
    /// `Data.Config.TestMode`. The optional `AuthKeyID` is verified against the
    /// SHA-1 fingerprint. Unrelated fields are ignored. Inputs are limited to
    /// one MiB. Hostnames are rejected; no DNS lookup is performed.
    ///
    /// # Errors
    /// Returns a sanitized [`ImportError`] for malformed or unsupported input,
    /// including duplicate required fields and inconsistent known environments.
    pub fn from_gotd_json(input: &str) -> Result<Self, ImportError> {
        check_size(input, JSON_LIMIT)?;
        let wrapper: GotdWrapper =
            serde_json::from_str(input).map_err(|_| ImportError::InvalidJson)?;
        if wrapper.version != 1 {
            return Err(ImportError::UnsupportedVersion);
        }
        let data = wrapper.data;
        let endpoint =
            validate_endpoint(data.addr.parse().map_err(|_| ImportError::InvalidAddress)?)?;
        if endpoint_environment(endpoint.ip())
            .is_some_and(|known_test| known_test != data.config.test_mode)
        {
            return Err(ImportError::InvalidAddress);
        }
        // The JSON bound limits allocations even for malformed credential fields.
        // Exact encoded lengths avoid allocating an unbounded decoded key.
        if data.auth_key.len() != 344 {
            return Err(ImportError::InvalidAuthKey);
        }
        let key = validate_key(
            &STANDARD
                .decode(&data.auth_key)
                .map_err(|_| ImportError::InvalidAuthKey)?,
        )?;
        if let Some(key_id) = data.auth_key_id {
            if key_id.len() != 12 {
                return Err(ImportError::InvalidAuthKeyId);
            }
            let key_id = STANDARD
                .decode(key_id)
                .map_err(|_| ImportError::InvalidAuthKeyId)?;
            if key_id.as_slice() != &Sha1::digest(key)[12..20] {
                return Err(ImportError::InvalidAuthKeyId);
            }
        }
        Self::from_parts(
            SessionFormat::Gotd,
            data.dc,
            Some(data.config.test_mode),
            Some(endpoint),
            key,
        )
    }

    /// Detects one of the supported exported string or JSON formats.
    ///
    /// An opening `{` selects gotd JSON; a leading `1` selects Telethon; all
    /// other inputs are validated as Pyrogram. Outer whitespace is permitted.
    ///
    /// # Errors
    /// Returns a sanitized [`ImportError`] from the selected parser.
    pub fn parse(input: &str) -> Result<Self, ImportError> {
        // Bound the raw input before trimming so padding cannot bypass the limit.
        check_size(input, JSON_LIMIT)?;
        match input.trim_start().as_bytes().first() {
            Some(b'{') => Self::from_gotd_json(input),
            Some(b'1') => Self::from_telethon(input),
            _ => Self::from_pyrogram(input),
        }
    }

    /// Returns the original serialization format.
    pub fn format(&self) -> SessionFormat {
        self.format
    }

    /// Returns the home datacenter identifier.
    pub fn dc_id(&self) -> i32 {
        self.data.home_dc
    }

    /// Returns the stored application identifier, if available.
    pub fn api_id(&self) -> Option<i32> {
        self.api_id
    }

    /// Returns the stored user identifier, if available.
    pub fn user_id(&self) -> Option<u64> {
        self.user_id
    }

    /// Returns the stored bot flag, if available.
    pub fn is_bot(&self) -> Option<bool> {
        self.is_bot
    }

    /// Returns the environment, or `None` for an unrecognized Telethon endpoint.
    pub fn test_mode(&self) -> Option<bool> {
        self.test_mode
    }

    /// Whether callers must set `ConnectionParams::use_ipv6` for the saved endpoint.
    pub fn prefer_ipv6(&self) -> bool {
        self.prefer_ipv6
    }

    /// Borrows the session state, including its secret authentication key.
    pub fn session_data(&self) -> &SessionData {
        &self.data
    }

    /// Transfers ownership of all imported session state.
    pub fn into_session_data(self) -> SessionData {
        self.data
    }

    /// Creates an in-memory session without I/O or network activity.
    pub fn into_memory_session(self) -> MemorySession {
        self.data.into()
    }

    fn from_parts(
        format: SessionFormat,
        dc: i32,
        test_mode: Option<bool>,
        endpoint: Option<SocketAddr>,
        key: [u8; 256],
    ) -> Result<Self, ImportError> {
        let valid_dc = if test_mode == Some(true) {
            (1..=3).contains(&dc)
        } else {
            (1..=5).contains(&dc) || dc == 203
        };
        if !valid_dc {
            return Err(ImportError::InvalidDc);
        }
        let mut dc_options = match test_mode {
            Some(true) => test_dc_options()
                .into_iter()
                .map(|option| (option.id, option))
                .collect(),
            Some(false) => KNOWN_DC_OPTIONS
                .iter()
                .cloned()
                .map(|option| (option.id, option))
                .collect(),
            None => std::collections::HashMap::new(),
        };
        let prefer_ipv6 = matches!(endpoint, Some(SocketAddr::V6(_)));
        if let Some(endpoint) = endpoint {
            // The other address family must never silently select a different
            // server. IPv4 can be mapped to IPv6; pure IPv6 has no IPv4 fallback.
            let (ipv4, ipv6) = match endpoint {
                SocketAddr::V4(address) => (
                    address,
                    SocketAddrV6::new(address.ip().to_ipv6_mapped(), address.port(), 0, 0),
                ),
                SocketAddr::V6(address) => (
                    SocketAddrV4::new(Ipv4Addr::UNSPECIFIED, address.port()),
                    address,
                ),
            };
            dc_options.insert(
                dc,
                DcOption {
                    id: dc,
                    ipv4,
                    ipv6,
                    auth_key: None,
                },
            );
        } else if dc == 203 {
            dc_options.insert(dc, dc_203());
        }
        let home = dc_options.get_mut(&dc).ok_or(ImportError::InvalidDc)?;
        home.auth_key = Some(key);
        Ok(Self {
            data: SessionData {
                home_dc: dc,
                dc_options,
                peer_infos: Default::default(),
                updates_state: Default::default(),
            },
            format,
            api_id: None,
            user_id: None,
            is_bot: None,
            test_mode,
            prefer_ipv6,
        })
    }
}

fn check_size(input: &str, limit: usize) -> Result<(), ImportError> {
    if input.len() > limit {
        Err(ImportError::InputTooLarge)
    } else {
        Ok(())
    }
}

fn read_fixed<const N: usize>(input: &[u8], offset: usize) -> Result<[u8; N], ImportError> {
    input
        .get(offset..offset.saturating_add(N))
        .and_then(|slice| slice.try_into().ok())
        .ok_or(ImportError::InvalidLength)
}

fn parse_boolean(value: u8) -> Result<bool, ImportError> {
    match value {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(ImportError::InvalidBoolean),
    }
}

fn validate_key(bytes: &[u8]) -> Result<[u8; 256], ImportError> {
    let key: [u8; 256] = bytes.try_into().map_err(|_| ImportError::InvalidAuthKey)?;
    if key.iter().all(|byte| *byte == 0) {
        return Err(ImportError::InvalidAuthKey);
    }
    Ok(key)
}

fn validate_endpoint(endpoint: SocketAddr) -> Result<SocketAddr, ImportError> {
    let ip = unmap_ipv4(endpoint.ip());
    if endpoint.port() == 0
        || ip.is_unspecified()
        || ip.is_multicast()
        || matches!(ip, IpAddr::V4(ip) if ip.is_broadcast())
    {
        Err(ImportError::InvalidAddress)
    } else {
        Ok(endpoint)
    }
}

fn unmap_ipv4(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(ip) => ip
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(ip)),
        ip => ip,
    }
}

fn endpoint_environment(ip: IpAddr) -> Option<bool> {
    let ip = unmap_ipv4(ip);
    let matches_ip = |option: &DcOption| {
        ip == IpAddr::V4(*option.ipv4.ip()) || ip == IpAddr::V6(*option.ipv6.ip())
    };
    if test_dc_options().iter().any(matches_ip) {
        Some(true)
    } else if KNOWN_DC_OPTIONS.iter().any(matches_ip) || matches_ip(&dc_203()) {
        Some(false)
    } else {
        None
    }
}

fn test_dc_options() -> [DcOption; 3] {
    [
        (
            1,
            [149, 154, 175, 10],
            [0x2001, 0xb28, 0xf23d, 0xf001, 0, 0, 0, 0xe],
        ),
        (
            2,
            [149, 154, 167, 40],
            [0x2001, 0x67c, 0x4e8, 0xf002, 0, 0, 0, 0xe],
        ),
        (
            3,
            [149, 154, 175, 117],
            [0x2001, 0xb28, 0xf23d, 0xf003, 0, 0, 0, 0xe],
        ),
    ]
    .map(|(id, ipv4, ipv6)| DcOption {
        id,
        ipv4: SocketAddrV4::new(Ipv4Addr::from(ipv4), 80),
        ipv6: SocketAddrV6::new(Ipv6Addr::from(ipv6), 80, 0, 0),
        auth_key: None,
    })
}

fn dc_203() -> DcOption {
    DcOption {
        id: 203,
        ipv4: SocketAddrV4::new(Ipv4Addr::new(91, 105, 192, 100), 443),
        ipv6: SocketAddrV6::new(
            Ipv6Addr::new(0x2a0a, 0xf280, 0x0203, 0x000a, 0x5000, 0, 0, 0x0100),
            443,
            0,
            0,
        ),
        auth_key: None,
    }
}

#[derive(Deserialize)]
struct GotdWrapper {
    #[serde(rename = "Version")]
    version: u32,
    #[serde(rename = "Data")]
    data: GotdData,
}

#[derive(Deserialize)]
struct GotdData {
    #[serde(rename = "DC")]
    dc: i32,
    #[serde(rename = "Addr")]
    addr: String,
    #[serde(rename = "AuthKey")]
    auth_key: String,
    #[serde(rename = "AuthKeyID", default, deserialize_with = "optional_key_id")]
    auth_key_id: Option<String>,
    #[serde(rename = "Config")]
    config: GotdConfig,
}

#[derive(Deserialize)]
struct GotdConfig {
    #[serde(rename = "TestMode")]
    test_mode: bool,
}

fn optional_key_id<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Option<String>, D::Error> {
    // Missing is allowed, but an explicitly null or mistyped key ID is invalid.
    String::deserialize(deserializer).map(Some)
}
