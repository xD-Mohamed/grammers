#![cfg(feature = "session-string")]

use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use grammers_session::storages::MemorySession;
use grammers_session::types::{DcOption, PeerId, PeerInfo, UpdateState, UpdatesState};
use grammers_session::{BoxFuture, NativeSession, Session, SessionData, SessionStringError};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::net::{Ipv6Addr, SocketAddrV6};

// All fixture keys are deterministic dummy bytes, never used with Telegram.
fn route() -> DcOption {
    DcOption {
        id: 203,
        ipv4: "192.0.2.8:8443".parse().unwrap(),
        ipv6: SocketAddrV6::new("2001:db8::9".parse().unwrap(), 1443, 0x12345, 17),
        auth_key: Some(std::array::from_fn(|i| i as u8)),
    }
}
fn source(dc: DcOption) -> MemorySession {
    MemorySession::from(SessionData {
        home_dc: dc.id,
        dc_options: [(dc.id, dc), (1, DcOption { id: 1, ..route() })].into(),
        peer_infos: [(PeerId::chat_unchecked(8), PeerInfo::Chat { id: 8 })].into(),
        updates_state: UpdatesState {
            pts: 55,
            seq: 12,
            ..Default::default()
        },
    })
}
// Independent binary fixture keeps parser tests independent of the encoder.
fn payload() -> Vec<u8> {
    let mut p = Vec::from(*b"GRAM\x01\x01\x00\x00");
    p.extend_from_slice(&203_i32.to_be_bytes());
    p.extend_from_slice(&[192, 0, 2, 8, 0x20, 0xfb]);
    p.extend_from_slice(&[0x20, 1, 0x0d, 0xb8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 9]);
    p.extend_from_slice(&[0x05, 0xa3, 0, 1, 0x23, 0x45, 0, 0, 0, 17]);
    p.extend(0..=255);
    p
}
fn wire(mut payload: Vec<u8>) -> String {
    let checksum = Sha256::new()
        .chain_update(b"grammers:1:")
        .chain_update(&payload)
        .finalize();
    payload.extend_from_slice(&checksum);
    format!("grammers:1:{}", URL_SAFE_NO_PAD.encode(payload))
}

#[test]
fn native_roundtrip_preserves_only_home_route_and_exact_endpoints() {
    let session = source(route());
    let erased: &dyn Session<Error = _> = &session;
    let exported = NativeSession::from_session(erased, true).unwrap();
    let encoded = exported.encode();
    assert_eq!(encoded.expose_secret(), wire(payload()));
    let decoded = NativeSession::parse(encoded.expose_secret()).unwrap();
    assert!(decoded.prefer_ipv6());
    assert_eq!(decoded.encode().into_inner(), encoded.expose_secret());
    let data = decoded.into_session_data();
    assert_eq!(data.home_dc, 203);
    assert_eq!(data.dc_options, HashMap::from([(203, route())]));
    assert!(data.peer_infos.is_empty());
    assert_eq!(data.updates_state, UpdatesState::default());
    let memory = NativeSession::parse(encoded.expose_secret())
        .unwrap()
        .into_memory_session();
    assert_eq!(memory.home_dc_id().unwrap(), 203);
    assert_eq!(memory.dc_option(1).unwrap(), None);
    assert_eq!(memory.dc_option(203).unwrap(), Some(route()));
}

#[test]
fn native_preserves_test_routes_and_missing_unused_endpoint() {
    let mut dc = route();
    dc.id = 2;
    dc.ipv4 = "149.154.167.40:443".parse().unwrap();
    let native = NativeSession::from_session(&source(dc.clone()), false).unwrap();
    let parsed = NativeSession::parse(native.encode().expose_secret()).unwrap();
    assert!(!parsed.prefer_ipv6());
    assert_eq!(
        parsed.into_session_data().dc_options,
        [(2, dc.clone())].into()
    );
    dc.ipv4 = "0.0.0.0:0".parse().unwrap();
    let native = NativeSession::from_session(&source(dc.clone()), true).unwrap();
    assert_eq!(
        NativeSession::parse(native.encode().expose_secret())
            .unwrap()
            .into_session_data()
            .dc_options,
        [(2, dc.clone())].into()
    );
    assert_eq!(
        NativeSession::from_session(&source(dc), false).unwrap_err(),
        SessionStringError::InvalidEndpoint
    );
    let mut dc = route();
    dc.ipv6 = SocketAddrV6::new(Ipv6Addr::UNSPECIFIED, 0, 0, 0);
    assert!(NativeSession::from_session(&source(dc.clone()), false).is_ok());
    assert_eq!(
        NativeSession::from_session(&source(dc), true).unwrap_err(),
        SessionStringError::InvalidEndpoint
    );
}

#[test]
fn native_preserves_ipv6_import_placeholder_without_making_it_routable() {
    let mut dc = route();
    dc.ipv4 = "0.0.0.0:8443".parse().unwrap();
    let native = NativeSession::from_session(&source(dc.clone()), true).unwrap();
    let mut expected = payload();
    expected[12..16].fill(0);
    assert_eq!(native.encode().expose_secret(), wire(expected.clone()));
    assert_eq!(
        NativeSession::parse(&wire(expected.clone()))
            .unwrap()
            .into_session_data()
            .dc_options,
        [(dc.id, dc.clone())].into()
    );
    assert_eq!(
        NativeSession::from_session(&source(dc), false).unwrap_err(),
        SessionStringError::InvalidEndpoint
    );
    expected[5] = 0; // selecting the IPv4 placeholder must still fail
    assert_eq!(
        NativeSession::parse(&wire(expected.clone())).unwrap_err(),
        SessionStringError::InvalidEndpoint
    );
    expected[5] = 1;
    expected[18..34].fill(0); // IPv6 must be usable, not another placeholder
    assert_eq!(
        NativeSession::parse(&wire(expected)).unwrap_err(),
        SessionStringError::InvalidEndpoint
    );
}

#[test]
fn parser_rejects_malformed_headers_routes_and_keys_even_with_valid_checksum() {
    for (index, value, expected) in [
        (0, b'X', SessionStringError::InvalidHeader),
        (4, 2, SessionStringError::UnsupportedVersion),
        (5, 2, SessionStringError::InvalidHeader),
        (6, 1, SessionStringError::InvalidHeader),
        (7, 1, SessionStringError::InvalidHeader),
    ] {
        let mut p = payload();
        p[index] = value;
        assert_eq!(NativeSession::parse(&wire(p)).unwrap_err(), expected);
    }
    for dc in [0_i32, -1, i32::MIN] {
        let mut p = payload();
        p[8..12].copy_from_slice(&dc.to_be_bytes());
        assert_eq!(
            NativeSession::parse(&wire(p)).unwrap_err(),
            SessionStringError::InvalidDc
        );
    }
    for (range, prefer_ipv6) in [
        (12..16, false),
        (16..18, false),
        (18..34, true),
        (34..36, true),
    ] {
        let mut p = payload();
        p[5] = u8::from(prefer_ipv6);
        p[range].fill(0);
        assert_eq!(
            NativeSession::parse(&wire(p)).unwrap_err(),
            SessionStringError::InvalidEndpoint
        );
    }
    let mut p = payload();
    p[44..300].fill(0);
    assert_eq!(
        NativeSession::parse(&wire(p)).unwrap_err(),
        SessionStringError::InvalidAuthKey
    );
}

#[test]
fn parser_rejects_noncanonical_encoding_corruption_and_unbounded_input() {
    let valid = wire(payload());
    for input in [
        String::new(),
        valid.replace("grammers:", "other:"),
        format!(" {valid}"),
    ] {
        assert_eq!(
            NativeSession::parse(&input).unwrap_err(),
            SessionStringError::InvalidPrefix
        );
    }
    assert_eq!(
        NativeSession::parse(&valid.replace("grammers:1:", "grammers:2:")).unwrap_err(),
        SessionStringError::UnsupportedVersion
    );
    for input in [
        format!("{valid}="),
        format!("{valid}\n"),
        valid.replace(':', ": "),
    ] {
        assert!(NativeSession::parse(&input).is_err());
    }
    let mut bytes = URL_SAFE_NO_PAD
        .decode(valid.strip_prefix("grammers:1:").unwrap())
        .unwrap();
    bytes[100] ^= 1;
    assert_eq!(
        NativeSession::parse(&format!("grammers:1:{}", URL_SAFE_NO_PAD.encode(&bytes)))
            .unwrap_err(),
        SessionStringError::InvalidChecksum
    );
    for len in [0, 1, 299, 301, 302, 500] {
        let mut p = payload();
        p.resize(len, 0);
        assert_eq!(
            NativeSession::parse(&wire(p)).unwrap_err(),
            SessionStringError::InvalidLength
        );
    }
    assert_eq!(
        NativeSession::parse(&"x".repeat(1025)).unwrap_err(),
        SessionStringError::TooLong
    );
    // Last base64 symbol has two unused low bits; accepting them would permit aliases.
    let mut noncanonical = valid.into_bytes();
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let last = noncanonical.last_mut().unwrap();
    let index = alphabet.iter().position(|b| b == last).unwrap();
    *last = alphabet[index | 1];
    assert_eq!(
        NativeSession::parse(std::str::from_utf8(&noncanonical).unwrap()).unwrap_err(),
        SessionStringError::InvalidEncoding
    );
}

#[test]
fn diagnostics_never_expose_input_keys_or_encoded_credentials() {
    let native = NativeSession::from_session(&source(route()), true).unwrap();
    let encoded = native.encode();
    assert_eq!(format!("{native:?}"), "NativeSession([REDACTED])");
    assert_eq!(format!("{encoded:?}"), "SessionString([REDACTED])");
    let error = NativeSession::parse("grammers:1:secret-do-not-log").unwrap_err();
    assert!(!format!("{error} {error:?}").contains("secret-do-not-log"));
    assert!(std::error::Error::source(&error).is_none());
}

// A deliberately untrusted storage exercises redaction and inconsistent reads.
struct BadStorage {
    home: Result<i32, &'static str>,
    dc: Result<Option<DcOption>, &'static str>,
}
impl Session for BadStorage {
    type Error = &'static str;
    fn home_dc_id(&self) -> Result<i32, Self::Error> {
        self.home
    }
    fn dc_option(&self, _: i32) -> Result<Option<DcOption>, Self::Error> {
        self.dc.clone()
    }
    fn set_home_dc_id(&self, _: i32) -> BoxFuture<'_, Result<(), Self::Error>> {
        panic!("export must not write")
    }
    fn set_dc_option(&self, _: &DcOption) -> BoxFuture<'_, Result<(), Self::Error>> {
        panic!("export must not write")
    }
    fn peer(&self, _: PeerId) -> BoxFuture<'_, Result<Option<PeerInfo>, Self::Error>> {
        panic!("export must not read peers")
    }
    fn cache_peer(&self, _: PeerInfo) -> BoxFuture<'_, Result<(), Self::Error>> {
        panic!("export must not write")
    }
    fn updates_state(&self) -> BoxFuture<'_, Result<UpdatesState, Self::Error>> {
        panic!("export must not read updates")
    }
    fn set_update_state(&self, _: UpdateState) -> BoxFuture<'_, Result<(), Self::Error>> {
        panic!("export must not write")
    }
}

#[test]
fn export_rejects_missing_inconsistent_and_failing_storage() {
    for (source, expected) in [
        (
            BadStorage {
                home: Err("secret-key"),
                dc: Ok(Some(route())),
            },
            SessionStringError::Storage,
        ),
        (
            BadStorage {
                home: Ok(203),
                dc: Err("secret-key"),
            },
            SessionStringError::Storage,
        ),
        (
            BadStorage {
                home: Ok(203),
                dc: Ok(None),
            },
            SessionStringError::MissingDc,
        ),
        (
            BadStorage {
                home: Ok(1),
                dc: Ok(Some(route())),
            },
            SessionStringError::InvalidDc,
        ),
        (
            BadStorage {
                home: Ok(203),
                dc: Ok(Some(DcOption {
                    auth_key: None,
                    ..route()
                })),
            },
            SessionStringError::MissingAuthKey,
        ),
        (
            BadStorage {
                home: Ok(203),
                dc: Ok(Some(DcOption {
                    auth_key: Some([0; 256]),
                    ..route()
                })),
            },
            SessionStringError::InvalidAuthKey,
        ),
    ] {
        let error = NativeSession::from_session(&source, true).unwrap_err();
        assert_eq!(error, expected);
        assert!(!format!("{error} {error:?}").contains("secret-key"));
    }
}
