// Copyright 2026 - developers of the `grammers` project.
// SPDX-License-Identifier: MIT OR Apache-2.0

use grammers_client::media::{InputMedia, TimeToLive};
use grammers_crypto::DequeBuffer;
use grammers_tl_types::{self as tl, Serializable};

// Downstream implementations and sinks only need the original Extend contract.
struct CustomRequest(i64);

impl Serializable for CustomRequest {
    fn serialize(&self, buf: &mut impl Extend<u8>) {
        tl::functions::Ping { ping_id: self.0 }.serialize(buf);
    }
}

#[derive(Default)]
struct CustomSink(Vec<u8>);

impl Extend<u8> for CustomSink {
    fn extend<T: IntoIterator<Item = u8>>(&mut self, iter: T) {
        self.0.extend(iter);
    }
}

#[test]
fn downstream_serializers_sinks_and_deque_extensions_remain_compatible() {
    let request = CustomRequest(123);
    let mut sink = CustomSink::default();
    request.serialize(&mut sink);
    let mut deque = DequeBuffer::with_capacity(0, 8);
    request.serialize(&mut deque);
    assert_eq!(deque.as_ref(), sink.0);
    assert_eq!(request.to_bytes(), sink.0);

    // Both public impls work for non-byte buffers and arbitrary iterators too.
    let mut deque = DequeBuffer::<u16>::with_capacity(0, 2);
    deque.extend(1..=2);
    deque.extend(&[3, 4]);
    deque.extend_front(&[0]);
    assert_eq!(deque.as_ref(), &[0, 1, 2, 3, 4]);
}

#[test]
fn legacy_ttl_call_patterns_and_conversion_remain_available() {
    let _: InputMedia = InputMedia::new().media_ttl(None);
    let _: InputMedia = InputMedia::new().media_ttl(Some(TimeToLive::Unlimited));
    assert_eq!(i32::try_from(TimeToLive::Unlimited), Ok(0));
    assert_eq!(i32::try_from(TimeToLive::ViewableOnce), Ok(i32::MAX));
}
