// Copyright 2026 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

#![cfg(feature = "session-import")]

use std::net::{Ipv4Addr, Ipv6Addr};

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE, URL_SAFE_NO_PAD};
use grammers_session::Session;
use grammers_session::import::{ImportError, ImportedSession, SessionFormat};
use serde_json::json;

const FAKE_KEY: [u8; 256] = [0xa5; 256];

fn pyrogram_bytes(current: bool, wide_id: bool, test_mode: bool) -> Vec<u8> {
    let mut bytes = vec![2];
    if current {
        bytes.extend_from_slice(&12345_u32.to_be_bytes());
    }
    bytes.push(u8::from(test_mode));
    bytes.extend_from_slice(&FAKE_KEY);
    if wide_id {
        bytes.extend_from_slice(&9_876_543_210_u64.to_be_bytes());
    } else {
        bytes.extend_from_slice(&123_456_u32.to_be_bytes());
    }
    bytes.push(1);
    bytes
}

fn telethon_bytes(ip: &[u8], port: u16) -> Vec<u8> {
    let mut bytes = vec![2];
    bytes.extend_from_slice(ip);
    bytes.extend_from_slice(&port.to_be_bytes());
    bytes.extend_from_slice(&FAKE_KEY);
    bytes
}

fn gotd_document() -> serde_json::Value {
    json!({
        "Version": 1,
        "Data": {
            "DC": 2,
            "Addr": "149.154.167.40:443",
            "AuthKey": STANDARD.encode(FAKE_KEY),
            // Independently computed SHA-1 tail for the synthetic key above.
            "AuthKeyID": "q7WuKV9CY7g=",
            "Salt": 123,
            "Config": { "TestMode": true, "Expires": 100, "DCOptions": [] }
        }
    })
}

fn assert_home_key_only(imported: ImportedSession) {
    let home_dc = imported.dc_id();
    assert!(imported.session_data().peer_infos.is_empty());
    assert!(imported.session_data().updates_state.channels.is_empty());
    for (id, option) in &imported.session_data().dc_options {
        assert!(if *id == home_dc {
            option.auth_key.as_ref() == Some(&FAKE_KEY)
        } else {
            option.auth_key.is_none()
        });
    }
    let memory = imported.into_memory_session();
    assert_eq!(memory.home_dc_id().unwrap(), home_dc);
    let option = memory.dc_option(home_dc).unwrap().unwrap();
    assert!(option.auth_key.as_ref() == Some(&FAKE_KEY));
}

#[test]
fn pyrogram_imports_current_and_both_legacy_formats_with_optional_padding() {
    for (current, wide, expected_user) in [
        (true, true, 9_876_543_210),
        (false, true, 9_876_543_210),
        (false, false, 123_456),
    ] {
        let bytes = pyrogram_bytes(current, wide, false);
        for input in [URL_SAFE.encode(&bytes), URL_SAFE_NO_PAD.encode(&bytes)] {
            let imported = ImportedSession::from_pyrogram(&input).unwrap();
            assert_eq!(imported.format(), SessionFormat::Pyrogram);
            assert_eq!(imported.api_id(), current.then_some(12345));
            assert_eq!(imported.user_id(), Some(expected_user));
            assert_eq!(imported.is_bot(), Some(true));
            assert_eq!(imported.test_mode(), Some(false));
            assert!(!imported.prefer_ipv6());
            assert_eq!(imported.dc_id(), 2);
            assert_home_key_only(imported);
        }
    }
}

#[test]
fn pyrogram_zero_application_id_is_missing_metadata() {
    for test_mode in [false, true] {
        let mut bytes = pyrogram_bytes(true, true, test_mode);
        bytes[1..5].fill(0);
        for input in [URL_SAFE.encode(&bytes), URL_SAFE_NO_PAD.encode(&bytes)] {
            let imported = ImportedSession::from_pyrogram(&input).unwrap();
            assert_eq!(imported.api_id(), None);
            assert_eq!(imported.dc_id(), 2);
            assert_eq!(imported.user_id(), Some(9_876_543_210));
            assert_eq!(imported.is_bot(), Some(true));
            assert_eq!(imported.test_mode(), Some(test_mode));
            assert_home_key_only(imported);
        }
    }
}

#[test]
fn pyrogram_application_id_metadata_respects_signed_range() {
    for id in [1_u32, i32::MAX as u32] {
        let mut bytes = pyrogram_bytes(true, true, false);
        bytes[1..5].copy_from_slice(&id.to_be_bytes());
        let imported = ImportedSession::from_pyrogram(&URL_SAFE_NO_PAD.encode(bytes)).unwrap();
        assert_eq!(imported.api_id(), Some(id as i32));
    }
    for id in [i32::MAX as u32 + 1, u32::MAX] {
        let mut bytes = pyrogram_bytes(true, true, false);
        bytes[1..5].copy_from_slice(&id.to_be_bytes());
        assert!(matches!(
            ImportedSession::from_pyrogram(&URL_SAFE_NO_PAD.encode(bytes)),
            Err(ImportError::InvalidApiId)
        ));
    }
}

#[test]
fn pyrogram_test_sessions_have_only_test_routes() {
    let input = URL_SAFE_NO_PAD.encode(pyrogram_bytes(true, true, true));
    let imported = ImportedSession::from_pyrogram(&input).unwrap();
    assert_eq!(imported.test_mode(), Some(true));
    let data = imported.session_data();
    assert_eq!(data.dc_options.len(), 3);
    for (id, ip) in [
        (1, "149.154.175.10"),
        (2, "149.154.167.40"),
        (3, "149.154.175.117"),
    ] {
        let option = &data.dc_options[&id];
        assert_eq!(option.ipv4.ip().to_string(), ip);
        assert_eq!(option.ipv4.port(), 80);
        assert_eq!(option.ipv6.port(), 80);
    }
    assert_home_key_only(imported);
}

#[test]
fn pyrogram_rejects_invalid_fields_and_lengths() {
    let original = pyrogram_bytes(true, true, false);
    let mut cases = Vec::new();
    for (index, value) in [(0, 0), (0, 99), (5, 2), (270, 2)] {
        let mut bytes = original.clone();
        bytes[index] = value;
        cases.push(bytes);
    }
    for range in [6..262, 262..270] {
        let mut bytes = original.clone();
        bytes[range].fill(0);
        cases.push(bytes);
    }
    cases.push(original[..270].to_vec());
    let mut trailing = original.clone();
    trailing.push(0);
    cases.push(trailing);
    for mut bytes in cases {
        assert!(ImportedSession::from_pyrogram(&URL_SAFE_NO_PAD.encode(&bytes)).is_err());
        // Missing optional metadata must not bypass structural/key validation.
        bytes[1..5].fill(0);
        assert!(ImportedSession::from_pyrogram(&URL_SAFE_NO_PAD.encode(&bytes)).is_err());
    }
    assert!(ImportedSession::from_pyrogram("secret-not-base64!").is_err());
}

#[test]
fn pyrogram_rejects_production_only_dc_in_test_mode() {
    let mut bytes = pyrogram_bytes(true, true, true);
    bytes[0] = 4;
    assert!(matches!(
        ImportedSession::from_pyrogram(&URL_SAFE_NO_PAD.encode(bytes)),
        Err(ImportError::InvalidDc)
    ));
}

#[test]
fn pyrogram_supports_dc_203_without_copying_its_key_to_other_dcs() {
    let mut bytes = pyrogram_bytes(true, true, false);
    bytes[0] = 203;
    let imported = ImportedSession::from_pyrogram(&URL_SAFE_NO_PAD.encode(bytes)).unwrap();
    let home = &imported.session_data().dc_options[&203];
    assert_eq!(home.ipv4.to_string(), "91.105.192.100:443");
    assert_eq!(home.ipv6.to_string(), "[2a0a:f280:203:a:5000::100]:443");
    assert_home_key_only(imported);
}

#[test]
fn telethon_preserves_ipv4_endpoint_and_does_not_invent_metadata() {
    let input = format!(
        "1{}",
        URL_SAFE.encode(telethon_bytes(&[149, 154, 167, 41], 8443))
    );
    let imported = ImportedSession::from_telethon(&input).unwrap();
    assert_eq!(imported.format(), SessionFormat::Telethon);
    assert_eq!(imported.api_id(), None);
    assert_eq!(imported.user_id(), None);
    assert_eq!(imported.is_bot(), None);
    assert_eq!(imported.test_mode(), Some(false));
    assert!(!imported.prefer_ipv6());
    let home = &imported.session_data().dc_options[&2];
    assert_eq!(*home.ipv4.ip(), Ipv4Addr::new(149, 154, 167, 41));
    assert_eq!(home.ipv4.port(), 8443);
    assert_eq!(*home.ipv6.ip(), home.ipv4.ip().to_ipv6_mapped());
    assert_eq!(home.ipv6.port(), 8443);
    assert_home_key_only(imported);
}

#[test]
fn telethon_preserves_ipv6_endpoint_and_requires_ipv6_preference() {
    let ip: Ipv6Addr = "2001:db8::20".parse().unwrap();
    let input = format!(
        "1{}",
        URL_SAFE_NO_PAD.encode(telethon_bytes(&ip.octets(), 443))
    );
    let imported = ImportedSession::from_telethon(&input).unwrap();
    assert!(imported.prefer_ipv6());
    assert_eq!(imported.test_mode(), None);
    assert_eq!(imported.session_data().dc_options.len(), 1);
    let home = &imported.session_data().dc_options[&2];
    assert_eq!(*home.ipv6.ip(), ip);
    assert!(home.ipv4.ip().is_unspecified());
    assert_home_key_only(imported);
}

#[test]
fn telethon_recognizes_test_endpoints_without_production_fallbacks() {
    let input = format!(
        "1{}",
        URL_SAFE.encode(telethon_bytes(&[149, 154, 167, 40], 443))
    );
    let imported = ImportedSession::from_telethon(&input).unwrap();
    assert_eq!(imported.test_mode(), Some(true));
    assert!(!imported.session_data().dc_options.contains_key(&4));
    assert_eq!(imported.session_data().dc_options[&2].ipv4.port(), 443);
    assert_home_key_only(imported);
}

#[test]
fn telethon_rejects_versions_truncation_trailing_bytes_and_invalid_fields() {
    let original = telethon_bytes(&[149, 154, 167, 41], 443);
    assert!(matches!(
        ImportedSession::from_telethon(&format!("2{}", URL_SAFE.encode(&original))),
        Err(ImportError::UnsupportedVersion)
    ));
    let mut cases = vec![original[..262].to_vec()];
    let mut trailing = original.clone();
    trailing.push(0);
    cases.push(trailing);
    for range in [0..1, 1..5, 5..7, 7..263] {
        let mut bytes = original.clone();
        bytes[range].fill(0);
        cases.push(bytes);
    }
    for bytes in cases {
        assert!(ImportedSession::from_telethon(&format!("1{}", URL_SAFE.encode(bytes))).is_err());
    }
}

#[test]
fn gotd_imports_version_one_loader_json_and_verifies_key_id() {
    let imported = ImportedSession::from_gotd_json(&gotd_document().to_string()).unwrap();
    assert_eq!(imported.format(), SessionFormat::Gotd);
    assert_eq!(imported.api_id(), None);
    assert_eq!(imported.user_id(), None);
    assert_eq!(imported.is_bot(), None);
    assert_eq!(imported.test_mode(), Some(true));
    assert_eq!(
        imported.session_data().dc_options[&2].ipv4.to_string(),
        "149.154.167.40:443"
    );
    assert!(!imported.session_data().dc_options.contains_key(&4));
    assert_home_key_only(imported);
}

#[test]
fn gotd_allows_missing_optional_key_id_and_preserves_ipv6_address() {
    let mut document = gotd_document();
    document["Data"]
        .as_object_mut()
        .unwrap()
        .remove("AuthKeyID");
    document["Data"]["Addr"] = json!("[2001:db8::30]:8080");
    let imported = ImportedSession::from_gotd_json(&document.to_string()).unwrap();
    assert!(imported.prefer_ipv6());
    assert_eq!(
        imported.session_data().dc_options[&2].ipv6.to_string(),
        "[2001:db8::30]:8080"
    );
    assert_home_key_only(imported);
}

#[test]
fn gotd_rejects_invalid_versions_keys_key_ids_addresses_and_environment() {
    for (pointer, value) in [
        ("/Version", json!(0)),
        ("/Version", json!(2)),
        ("/Data/DC", json!(0)),
        ("/Data/DC", json!(99)),
        ("/Data/AuthKey", json!(STANDARD.encode([0_u8; 256]))),
        ("/Data/AuthKey", json!(STANDARD.encode([1_u8; 255]))),
        ("/Data/AuthKey", json!("not-base64!")),
        ("/Data/AuthKeyID", json!("AAAAAAAAAAA=")),
        ("/Data/AuthKeyID", json!("AQ==")),
        ("/Data/AuthKeyID", json!(null)),
        ("/Data/Addr", json!("telegram.example:443")),
        ("/Data/Addr", json!("127.0.0.1:0")),
        ("/Data/Addr", json!("0.0.0.0:443")),
        ("/Data/Config/TestMode", json!(1)),
        ("/Data/Config/TestMode", json!(null)),
    ] {
        let mut document = gotd_document();
        *document.pointer_mut(pointer).unwrap() = value;
        assert!(
            ImportedSession::from_gotd_json(&document.to_string()).is_err(),
            "accepted invalid field {pointer}"
        );
    }
}

#[test]
fn gotd_rejects_missing_and_duplicate_relevant_fields() {
    for field in ["DC", "Addr", "AuthKey", "Config"] {
        let mut document = gotd_document();
        document["Data"].as_object_mut().unwrap().remove(field);
        assert!(ImportedSession::from_gotd_json(&document.to_string()).is_err());
    }
    let document = gotd_document().to_string();
    for duplicate in [
        document.replace("\"Version\":1", "\"Version\":1,\"Version\":1"),
        document.replace("\"DC\":2", "\"DC\":2,\"DC\":2"),
        document.replace(
            "\"AuthKeyID\":",
            "\"AuthKeyID\":\"q7WuKV9CY7g=\",\"AuthKeyID\":",
        ),
        document.replace("\"TestMode\":true", "\"TestMode\":true,\"TestMode\":false"),
        format!("{document} trailing"),
    ] {
        assert!(ImportedSession::from_gotd_json(&duplicate).is_err());
    }
}

#[test]
fn gotd_rejects_known_endpoint_environment_mismatch() {
    let mut document = gotd_document();
    document["Data"]["Config"]["TestMode"] = json!(false);
    assert!(matches!(
        ImportedSession::from_gotd_json(&document.to_string()),
        Err(ImportError::InvalidAddress)
    ));
    document["Data"]["Config"]["TestMode"] = json!(true);
    document["Data"]["Addr"] = json!("149.154.167.41:443");
    assert!(matches!(
        ImportedSession::from_gotd_json(&document.to_string()),
        Err(ImportError::InvalidAddress)
    ));
}

#[test]
fn gotd_preserves_explicit_production_endpoint_and_session_data_conversion() {
    let mut document = gotd_document();
    document["Data"]["Config"]["TestMode"] = json!(false);
    document["Data"]["Addr"] = json!("149.154.167.41:8443");
    let imported = ImportedSession::from_gotd_json(&document.to_string()).unwrap();
    assert_eq!(imported.test_mode(), Some(false));
    let data = imported.into_session_data();
    assert_eq!(data.home_dc, 2);
    assert_eq!(data.dc_options[&2].ipv4.port(), 8443);
    assert!(data.dc_options[&2].auth_key.as_ref() == Some(&FAKE_KEY));
    assert!(data.dc_options[&1].auth_key.is_none());
}

#[test]
fn telethon_ipv6_test_endpoint_is_classified_and_preserved() {
    let ip: Ipv6Addr = "2001:67c:4e8:f002::e".parse().unwrap();
    let input = format!("1{}", URL_SAFE.encode(telethon_bytes(&ip.octets(), 80)));
    let imported = ImportedSession::from_telethon(&input).unwrap();
    assert_eq!(imported.test_mode(), Some(true));
    assert!(imported.prefer_ipv6());
    assert!(!imported.session_data().dc_options.contains_key(&4));
    assert_eq!(*imported.session_data().dc_options[&2].ipv6.ip(), ip);
    assert_home_key_only(imported);
}

#[test]
fn auto_detection_handles_supported_formats() {
    for (input, format) in [
        (
            URL_SAFE_NO_PAD.encode(pyrogram_bytes(true, true, false)),
            SessionFormat::Pyrogram,
        ),
        (
            format!(
                "1{}",
                URL_SAFE.encode(telethon_bytes(&[149, 154, 167, 41], 443))
            ),
            SessionFormat::Telethon,
        ),
        (gotd_document().to_string(), SessionFormat::Gotd),
    ] {
        let imported = ImportedSession::parse(&input).unwrap();
        assert_eq!(imported.format(), format);
        assert_home_key_only(imported);
    }
}

#[test]
fn diagnostics_never_include_input_or_auth_key() {
    let secret_marker = "synthetic-secret-value-do-not-echo";
    for input in [
        secret_marker.to_owned(),
        format!("{{\"Version\":\"{secret_marker}\"}}"),
    ] {
        let error = ImportedSession::parse(&input).unwrap_err();
        assert!(!format!("{error:?}: {error}").contains(secret_marker));
        assert!(std::error::Error::source(&error).is_none());
    }
    let imported =
        ImportedSession::from_pyrogram(&URL_SAFE_NO_PAD.encode(pyrogram_bytes(true, true, false)))
            .unwrap();
    let diagnostic = format!("{imported:?}");
    assert!(!diagnostic.contains(&STANDARD.encode(FAKE_KEY)));
    assert!(!diagnostic.contains("165, 165"));
    assert!(!diagnostic.contains("9876543210"));
}

#[test]
fn oversized_inputs_are_rejected_before_decoding() {
    assert!(matches!(
        ImportedSession::from_pyrogram(&"A".repeat(513)),
        Err(ImportError::InputTooLarge)
    ));
    assert!(matches!(
        ImportedSession::from_telethon(&"1".repeat(513)),
        Err(ImportError::InputTooLarge)
    ));
    assert!(matches!(
        ImportedSession::from_gotd_json(&" ".repeat(1024 * 1024 + 1)),
        Err(ImportError::InputTooLarge)
    ));
}
