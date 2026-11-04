// Copyright 2026 - developers of the `grammers` project.
// SPDX-License-Identifier: MIT OR Apache-2.0

use grammers_crypto::DequeBuffer;
use grammers_mtproto::mtp::{Encrypted, Mtp};
use grammers_tl_types::{Serializable, functions::Ping};

#[test]
fn public_message_ids_correlate_with_serialized_request_headers() {
    let mut mtp = Encrypted::build().finish([0; 256]);
    let mut buffer = DequeBuffer::with_capacity(256, 64);
    let body = Ping { ping_id: 123 }.to_bytes();
    let first = mtp.push(&mut buffer, &body).unwrap();
    let second_offset = buffer.len();
    let second = mtp.push(&mut buffer, &body).unwrap();
    assert_eq!(&buffer[..8], &first.into_inner().to_le_bytes());
    assert_eq!(
        &buffer[second_offset..second_offset + 8],
        &second.into_inner().to_le_bytes()
    );
    assert!(second.into_inner() > first.into_inner());
    assert_eq!(&buffer[16..second_offset], body);
}
