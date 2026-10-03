// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! This module contains additional, manual structures for some TL types.

use std::{
    borrow::Cow,
    cell::RefCell,
    io::{Cursor as IoCursor, Read, Write},
};

use flate2::Compression;
use flate2::bufread::GzDecoder;
use flate2::write::GzEncoder;
use grammers_tl_types::{self as tl, Cursor, Deserializable, Identifiable, Serializable};

use crate::mtp;

/// This struct represents the following TL definition:
///
/// ```tl
/// message msg_id:long seqno:int bytes:int body:Object = Message;
/// ```
///
/// Messages are what's ultimately sent to Telegram.
///
/// Each message has its own unique identifier, and the body is simply
/// the serialized request that should be executed on the server, or
/// the response object from Telegram.
pub(crate) struct Message {
    pub msg_id: i64,
    pub seq_no: i32,
    pub body: Vec<u8>,
}

impl Message {
    // msg_id (8 bytes), seq_no (4 bytes), bytes (4 len)
    pub const SIZE_OVERHEAD: usize = 16;
}

/// An envelope borrowing decrypted bytes until a reply/update must escape.
/// Owned bodies are used for gzip wrappers so their decoded allocation can move.
pub(crate) struct MessageView<'a> {
    pub msg_id: i64,
    pub seq_no: i32,
    pub body: Cow<'a, [u8]>,
}

impl<'a> MessageView<'a> {
    pub fn deserialize(buf: &mut Cursor<'a>) -> Result<Self, tl::deserialize::Error> {
        let msg_id = i64::deserialize(buf)?;
        let seq_no = i32::deserialize(buf)?;
        let len = i32::deserialize(buf)?;
        let len = usize::try_from(len).map_err(|_| tl::deserialize::Error::UnexpectedEof)?;
        if len >= MessageContainer::MAXIMUM_SIZE {
            return Err(tl::deserialize::Error::UnexpectedEof);
        }
        Ok(Self {
            msg_id,
            seq_no,
            body: Cow::Borrowed(buf.read_slice(len)?),
        })
    }

    /// Peek the constructor ID from the body.
    pub fn constructor_id(&self) -> Result<u32, tl::deserialize::Error> {
        u32::from_bytes(&self.body)
    }

    /// Determines whether this server message needs acknowledgement.
    pub fn requires_ack(&self) -> bool {
        // > Content-related Message
        // >   A message requiring an explicit acknowledgment.
        // > [...]
        // > (msg_seqno) [...] twice the number of "content-related" messages
        // > [...] and subsequently incremented by one if the current message
        // > is a content-related message.
        // https://core.telegram.org/mtproto/description#content-related-message
        self.seq_no % 2 == 1
    }
}

impl Serializable for Message {
    fn serialize(&self, buf: &mut impl Extend<u8>) {
        self.msg_id.serialize(buf);
        self.seq_no.serialize(buf);
        (self.body.len() as i32).serialize(buf);
        buf.extend(self.body.iter().copied());
    }
}

impl Deserializable for Message {
    fn deserialize(buf: &mut Cursor) -> Result<Self, tl::deserialize::Error> {
        let message = MessageView::deserialize(buf)?;
        Ok(Self {
            msg_id: message.msg_id,
            seq_no: message.seq_no,
            body: message.body.into_owned(),
        })
    }
}

/// This struct represents the following TL definition:
///
/// ```tl
/// rpc_result#f35c6d01 req_msg_id:long result:Object = RpcResult;
/// ```
pub struct RpcResult {
    pub req_msg_id: i64,
    pub result: Vec<u8>,
}

impl RpcResult {
    pub fn borrowed_parts(body: &[u8]) -> Result<(i64, &[u8]), tl::deserialize::Error> {
        let mut cursor = Cursor::from_slice(body);
        let constructor = u32::deserialize(&mut cursor)?;
        if constructor != Self::CONSTRUCTOR_ID {
            return Err(tl::deserialize::Error::UnexpectedConstructor { id: constructor });
        }
        let req_msg_id = i64::deserialize(&mut cursor)?;
        Ok((req_msg_id, &body[cursor.pos()..]))
    }

    /// Peek the constructor ID from the body.
    /// Reuses the decrypted message allocation instead of allocating another result.
    pub fn from_owned(mut body: Vec<u8>) -> Result<Self, tl::deserialize::Error> {
        let (req_msg_id, result) = Self::borrowed_parts(&body)?;
        let start = body.len() - result.len();
        body.copy_within(start.., 0);
        body.truncate(body.len() - start);
        Ok(Self {
            req_msg_id,
            result: body,
        })
    }
}

impl Identifiable for RpcResult {
    #[allow(clippy::unreadable_literal)]
    const CONSTRUCTOR_ID: u32 = 0xf35c6d01;
}

impl Deserializable for RpcResult {
    fn deserialize(buf: &mut Cursor) -> Result<Self, tl::deserialize::Error> {
        let constructor_id = u32::deserialize(buf)?;
        if constructor_id != Self::CONSTRUCTOR_ID {
            return Err(tl::deserialize::Error::UnexpectedConstructor { id: constructor_id });
        }

        let req_msg_id = i64::deserialize(buf)?;
        let mut result = Vec::new();
        buf.read_to_end(&mut result)?;

        Ok(Self { req_msg_id, result })
    }
}

/// This struct represents the following TL definition:
///
/// ```tl
/// msg_container#73f1f8dc messages:vector<message> = MessageContainer;
/// ```
pub(crate) struct MessageContainer {
    #[cfg(test)]
    pub messages: Vec<Message>,
}

impl MessageContainer {
    /// Validate all inner envelopes before handling any, just like the owned
    /// decoder, then walk them without allocating a Vec of copied messages.
    pub fn borrowed_messages(bytes: &[u8]) -> Result<MessageIter<'_>, tl::deserialize::Error> {
        let mut cursor = Cursor::from_slice(bytes);
        let constructor = u32::deserialize(&mut cursor)?;
        if constructor != Self::CONSTRUCTOR_ID {
            return Err(tl::deserialize::Error::UnexpectedConstructor { id: constructor });
        }
        let count = usize::try_from(i32::deserialize(&mut cursor)?)
            .map_err(|_| tl::deserialize::Error::UnexpectedEof)?;
        let data = &bytes[cursor.pos()..];
        if count > data.len() / Message::SIZE_OVERHEAD {
            return Err(tl::deserialize::Error::UnexpectedEof);
        }
        let mut check = Cursor::from_slice(data);
        for _ in 0..count {
            MessageView::deserialize(&mut check)?;
        }
        Ok(MessageIter {
            cursor: Cursor::from_slice(data),
            remaining: count,
        })
    }

    // constructor id (4 bytes), inner vec len (4 bytes)
    pub const SIZE_OVERHEAD: usize = 8;

    /// Maximum size in bytes for the inner payload of the container.
    /// Telegram will close the connection if the payload is bigger.
    /// The overhead of the container itself is subtracted.
    pub const MAXIMUM_SIZE: usize = 1_044_456 - Self::SIZE_OVERHEAD;

    /// Maximum amount of messages that can't be sent inside a single
    /// container, inclusive. Beyond this limit Telegram will respond
    /// with `BAD_MESSAGE` `64` (invalid container).
    ///
    /// This limit is not 100% accurate and may in some cases be higher.
    /// However, sending up to 100 requests at once in a single container
    /// is a reasonable conservative value, since it could also depend on
    /// other factors like size per request, but we cannot know this.
    pub const MAXIMUM_LENGTH: usize = 100;
}

impl Identifiable for MessageContainer {
    #[allow(clippy::unreadable_literal)]
    const CONSTRUCTOR_ID: u32 = 0x73f1f8dc;
}

pub(crate) struct MessageIter<'a> {
    cursor: Cursor<'a>,
    remaining: usize,
}

impl<'a> Iterator for MessageIter<'a> {
    type Item = Result<MessageView<'a>, tl::deserialize::Error>;
    fn next(&mut self) -> Option<Self::Item> {
        if self.remaining == 0 {
            return None;
        }
        self.remaining -= 1;
        Some(MessageView::deserialize(&mut self.cursor))
    }
}

#[cfg(test)]
impl Deserializable for MessageContainer {
    fn deserialize(buf: &mut Cursor) -> Result<Self, tl::deserialize::Error> {
        let constructor_id = u32::deserialize(buf)?;
        if constructor_id != Self::CONSTRUCTOR_ID {
            return Err(tl::deserialize::Error::UnexpectedConstructor { id: constructor_id });
        }

        let len = i32::deserialize(buf)?;
        assert!(len >= 0);
        let len = len as usize;
        let mut messages = Vec::with_capacity(len.min(Self::MAXIMUM_LENGTH));
        for _ in 0..len {
            messages.push(Message::deserialize(buf)?);
        }

        Ok(Self { messages })
    }
}

/// This struct represents the following TL definition:
///
/// ```tl
/// msg_copy#e06046b2 orig_message:Message = MessageCopy;
/// ```
///
/// Note that this is "not used", in favour of `msg_container`.
// Even though we use `MessageCopy::CONSTRUCTOR_ID, the dead code lint fires.
#[allow(dead_code)]
pub(crate) struct MessageCopy {
    pub orig_message: Vec<Message>,
}

impl Identifiable for MessageCopy {
    #[allow(clippy::unreadable_literal)]
    const CONSTRUCTOR_ID: u32 = 0xe06046b2;
}

/// This struct represents the following TL definition:
///
/// ```tl
/// gzip_packed#3072cfa1 packed_data:string = Object;
/// ```
pub(crate) struct GzipPacked {
    pub packed_data: Vec<u8>,
}

/// Reuse the inflate state across independent replies on a runtime thread,
/// rather than retaining a decoder on every connection. Inputs stay owned so
/// no borrowed transport buffer can escape a call or survive a reset.
struct GzipScratch {
    decoder: GzDecoder<IoCursor<Vec<u8>>>,
}

const GZIP_SCRATCH_LIMIT: usize = 64 * 1024;

thread_local! {
    static GZIP_SCRATCH: RefCell<Option<GzipScratch>> = const { RefCell::new(None) };
}

impl GzipScratch {
    fn new() -> Self {
        Self {
            decoder: GzDecoder::new(IoCursor::new(Vec::new())),
        }
    }

    fn decode(&mut self, packed: &[u8]) -> Result<Vec<u8>, mtp::DeserializeError> {
        let mut input = std::mem::take(self.decoder.get_mut());
        input.set_position(0);
        input.get_mut().clear();
        input.get_mut().extend_from_slice(packed);
        self.decoder.reset(input);

        // ISIZE is only a bounded allocation hint, never a validation result
        // or an output limit. The decoder still checks the full CRC and size.
        let hint = packed
            .last_chunk::<4>()
            .map_or(0, |bytes| u32::from_le_bytes(*bytes) as usize);
        let mut output = Vec::with_capacity(hint.min(GZIP_SCRATCH_LIMIT));
        self.decoder
            .read_to_end(&mut output)
            .map_err(|_| mtp::DeserializeError::DecompressionFailed)?;
        // A single-member decoder stops at its footer. Preserve write_all's
        // rejection of trailing data instead of silently accepting a prefix.
        if self.decoder.get_ref().position() != packed.len() as u64 {
            return Err(mtp::DeserializeError::DecompressionFailed);
        }
        Ok(output)
    }

    fn retainable(&self) -> bool {
        self.decoder.get_ref().get_ref().capacity() <= GZIP_SCRATCH_LIMIT
    }
}

impl GzipPacked {
    #[cfg(test)]
    pub fn new(unpacked_data: &[u8]) -> Self {
        Self::new_prefixed(&[], unpacked_data)
    }

    pub fn new_prefixed(prefix: &[u8], unpacked_data: &[u8]) -> Self {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
        encoder.write_all(prefix).unwrap();
        // Safe to unwrap, in-memory data should not fail
        encoder.write_all(unpacked_data).unwrap();
        let packed_data = encoder.finish().unwrap();
        Self { packed_data }
    }

    #[cfg(test)]
    pub fn decompress(&self) -> Result<Vec<u8>, mtp::DeserializeError> {
        Self::decompress_payload(&self.packed_data)
    }

    /// Decode the TL envelope by borrowing compressed bytes from its input.
    pub fn decompress_from_bytes(bytes: &[u8]) -> Result<Vec<u8>, mtp::DeserializeError> {
        let mut cursor = Cursor::from_slice(bytes);
        let constructor = u32::deserialize(&mut cursor)?;
        if constructor != Self::CONSTRUCTOR_ID {
            return Err(tl::deserialize::Error::UnexpectedConstructor { id: constructor }.into());
        }
        Self::decompress_payload(cursor.read_bytes()?)
    }

    fn decompress_payload(packed: &[u8]) -> Result<Vec<u8>, mtp::DeserializeError> {
        GZIP_SCRATCH
            .try_with(|slot| {
                // Release the RefCell borrow before decoding. A nested call can
                // use its own scratch without poisoning or borrowing this decoder.
                let mut scratch = slot.borrow_mut().take().unwrap_or_else(GzipScratch::new);
                let result = scratch.decode(packed);
                if scratch.retainable() {
                    scratch.decoder.get_mut().get_mut().clear();
                    slot.borrow_mut().get_or_insert(scratch);
                }
                result
            })
            .unwrap_or_else(|_| GzipScratch::new().decode(packed))
    }
}

impl Identifiable for GzipPacked {
    #[allow(clippy::unreadable_literal)]
    const CONSTRUCTOR_ID: u32 = 0x3072cfa1;
}

impl Serializable for GzipPacked {
    fn serialize(&self, buf: &mut impl Extend<u8>) {
        Self::CONSTRUCTOR_ID.serialize(buf);
        self.packed_data.serialize(buf);
    }
}

impl Deserializable for GzipPacked {
    fn deserialize(buf: &mut Cursor) -> Result<Self, tl::deserialize::Error> {
        let constructor_id = u32::deserialize(buf)?;
        if constructor_id != Self::CONSTRUCTOR_ID {
            return Err(tl::deserialize::Error::UnexpectedConstructor { id: constructor_id });
        }

        let packed_data = Vec::<u8>::deserialize(buf)?;
        Ok(Self { packed_data })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gzip_decompress() {
        let rpc_result = [
            1, 109, 92, 243, 132, 41, 150, 69, 54, 75, 49, 94, 161, 207, 114, 48, 254, 140, 1, 0,
            31, 139, 8, 0, 0, 0, 0, 0, 0, 3, 149, 147, 61, 75, 195, 80, 20, 134, 79, 62, 20, 84,
            170, 1, 17, 28, 68, 28, 132, 110, 183, 247, 38, 185, 249, 154, 58, 10, 130, 116, 116,
            170, 54, 109, 18, 11, 173, 169, 109, 90, 112, 210, 209, 81, 112, 112, 22, 127, 131, 56,
            232, 232, 224, 143, 112, 112, 238, 159, 168, 55, 31, 234, 77, 91, 82, 26, 184, 57, 36,
            79, 222, 115, 222, 251, 38, 9, 170, 27, 218, 209, 62, 128, 113, 76, 234, 181, 83, 82,
            55, 31, 175, 223, 101, 0, 216, 249, 120, 217, 219, 102, 181, 244, 244, 186, 203, 10, 8,
            108, 109, 18, 221, 70, 132, 234, 136, 152, 20, 81, 13, 222, 132, 148, 43, 11, 184, 144,
            241, 178, 138, 49, 113, 176, 171, 90, 142, 175, 106, 45, 199, 79, 46, 217, 145, 63, 53,
            126, 117, 241, 92, 49, 215, 215, 48, 17, 197, 185, 185, 179, 156, 252, 113, 49, 227,
            91, 60, 39, 148, 240, 190, 196, 127, 95, 134, 217, 116, 176, 238, 89, 177, 47, 181,
            200, 151, 180, 156, 206, 229, 247, 35, 229, 252, 176, 156, 8, 198, 252, 126, 138, 184,
            144, 241, 57, 57, 106, 139, 114, 148, 167, 115, 178, 73, 46, 199, 34, 46, 100, 124,
            206, 126, 245, 162, 185, 98, 166, 227, 242, 55, 16, 81, 49, 159, 227, 18, 125, 93, 222,
            207, 202, 76, 14, 126, 172, 163, 69, 126, 148, 76, 87, 178, 9, 139, 213, 66, 148, 185,
            177, 48, 0, 159, 211, 52, 167, 118, 198, 27, 189, 145, 134, 6, 145, 215, 65, 205, 176,
            11, 240, 201, 158, 171, 150, 36, 104, 177, 90, 211, 37, 184, 99, 63, 11, 30, 2, 124,
            63, 200, 73, 253, 98, 141, 206, 199, 233, 119, 18, 63, 11, 207, 34, 76, 38, 147, 155,
            120, 193, 248, 48, 185, 23, 207, 186, 117, 214, 146, 26, 247, 57, 56, 1, 184, 63, 19,
            18, 189, 82, 102, 51, 47, 162, 168, 55, 112, 42, 149, 8, 117, 189, 10, 123, 247, 65,
            219, 95, 247, 195, 97, 127, 112, 53, 108, 244, 61, 144, 221, 246, 101, 192, 116, 171,
            65, 24, 6, 29, 47, 13, 83, 73, 203, 15, 58, 186, 13, 141, 216, 3, 0, 0,
        ];
        let _rpc_result_id = &rpc_result[0..4];
        let _msg_id = &rpc_result[4..12];
        let gzipped = &rpc_result[12..rpc_result.len()];
        let gzip = GzipPacked::from_bytes(gzipped).unwrap();
        assert_eq!(gzip.decompress().unwrap().len(), 984);
    }
}

#[cfg(test)]
mod optimization_tests {
    use super::*;

    fn message_wire(msg_id: i64, body: &[u8]) -> Vec<u8> {
        Message {
            msg_id,
            seq_no: 1,
            body: body.to_vec(),
        }
        .to_bytes()
    }

    #[test]
    fn borrowed_envelope_preserves_fields_and_points_into_input() {
        for size in [0, 4, 512, 8192] {
            let wire = message_wire(99, &vec![7; size]);
            let owned = Message::from_bytes(&wire).unwrap();
            let mut cursor = Cursor::from_slice(&wire);
            let borrowed = MessageView::deserialize(&mut cursor).unwrap();
            assert_eq!(borrowed.msg_id, owned.msg_id);
            assert_eq!(borrowed.seq_no, owned.seq_no);
            assert_eq!(borrowed.body.as_ref(), owned.body);
            assert!(matches!(borrowed.body, Cow::Borrowed(_)));
            assert_eq!(
                borrowed.body.as_ptr(),
                wire[Message::SIZE_OVERHEAD..].as_ptr()
            );
            assert_eq!(cursor.pos(), wire.len());
            for end in 0..wire.len() {
                assert!(MessageView::deserialize(&mut Cursor::from_slice(&wire[..end])).is_err());
            }
        }
    }

    #[test]
    fn borrowed_containers_match_owned_decoder_and_validate_before_iteration() {
        for count in [0, 1, 3, 101] {
            let mut wire = MessageContainer::CONSTRUCTOR_ID.to_bytes();
            wire.extend((count as i32).to_bytes());
            for id in 0..count {
                wire.extend(message_wire(id, &true.to_bytes()));
            }
            let owned = MessageContainer::from_bytes(&wire).unwrap();
            let borrowed: Vec<_> = MessageContainer::borrowed_messages(&wire)
                .unwrap()
                .map(Result::unwrap)
                .collect();
            assert_eq!(borrowed.len(), owned.messages.len());
            for (left, right) in borrowed.iter().zip(&owned.messages) {
                assert_eq!(left.msg_id, right.msg_id);
                assert_eq!(left.body.as_ref(), right.body);
                assert!(matches!(left.body, Cow::Borrowed(_)));
            }
            for end in 0..wire.len() {
                assert!(MessageContainer::borrowed_messages(&wire[..end]).is_err());
            }
        }
        for count in [-1i32, i32::MAX] {
            let mut wire = MessageContainer::CONSTRUCTOR_ID.to_bytes();
            wire.extend(count.to_bytes());
            assert!(MessageContainer::borrowed_messages(&wire).is_err());
        }
        assert!(MessageContainer::borrowed_messages(&[0; 8]).is_err());
    }

    #[test]
    #[ignore = "isolated decoder copy benchmark; no encryption, sockets or Telegram"]
    fn benchmark_borrowed_response_envelope() {
        use std::{hint::black_box, time::Instant};
        for size in [512, 2048, 8192] {
            let wire = message_wire(99, &rpc_wire(&vec![7; size]));
            let mut samples = [Vec::new(), Vec::new()];
            for round in 0..8 {
                for borrowed in [round % 2 == 0, round % 2 != 0] {
                    let start = Instant::now();
                    for _ in 0..100_000 {
                        if borrowed {
                            let message =
                                MessageView::deserialize(&mut Cursor::from_slice(black_box(&wire)))
                                    .unwrap();
                            let (id, result) = RpcResult::borrowed_parts(&message.body).unwrap();
                            black_box((id, result.to_vec()));
                        } else {
                            let message = Message::from_bytes(black_box(&wire)).unwrap();
                            black_box(RpcResult::from_owned(message.body).unwrap());
                        }
                    }
                    samples[usize::from(borrowed)]
                        .push(start.elapsed().as_nanos() as f64 / 100_000.0);
                }
            }
            for sample in &mut samples {
                sample.sort_by(f64::total_cmp);
            }
            println!(
                "{size}-byte reply median: owned envelope {:.1} ns, borrowed {:.1} ns",
                samples[0][4], samples[1][4]
            );
        }
    }

    fn rpc_wire(body: &[u8]) -> Vec<u8> {
        let mut wire = RpcResult::CONSTRUCTOR_ID.to_bytes();
        wire.extend(42i64.to_bytes());
        wire.extend_from_slice(body);
        wire
    }

    #[test]
    fn rpc_result_reuses_allocation_and_matches_the_original_decoder() {
        for size in [0, 4, 1024, 8192] {
            let wire = rpc_wire(&vec![7; size]);
            let expected = RpcResult::from_bytes(&wire).unwrap();
            let ptr = wire.as_ptr();
            let actual = RpcResult::from_owned(wire).unwrap();
            assert_eq!(actual.req_msg_id, expected.req_msg_id);
            assert_eq!(actual.result, expected.result);
            assert_eq!(actual.result.as_ptr(), ptr);
        }
        for size in 0..12 {
            assert!(RpcResult::from_owned(rpc_wire(&[])[..size].to_vec()).is_err());
        }
        assert!(RpcResult::from_owned(vec![0; 16]).is_err());
    }

    #[test]
    fn borrowed_gzip_keeps_crc_and_truncation_validation() {
        for size in [0, 16, 1024, 8192] {
            let original: Vec<_> = (0..size).map(|i| (i * 37) as u8).collect();
            let gzip = GzipPacked::new(&original);
            let wire = gzip.to_bytes();
            assert_eq!(GzipPacked::decompress_from_bytes(&wire).unwrap(), original);
            for cut in 0..wire.len() {
                assert!(GzipPacked::decompress_from_bytes(&wire[..cut]).is_err());
            }
            let mut corrupt = gzip.packed_data.clone();
            let crc = corrupt.len() - 8;
            corrupt[crc] ^= 1;
            assert!(
                GzipPacked::decompress_from_bytes(
                    &GzipPacked {
                        packed_data: corrupt
                    }
                    .to_bytes()
                )
                .is_err()
            );
        }
    }

    fn legacy_gzip_decode(packed: &[u8]) -> Result<Vec<u8>, mtp::DeserializeError> {
        let mut decoder = flate2::write::GzDecoder::new(Vec::new());
        decoder
            .write_all(packed)
            .map_err(|_| mtp::DeserializeError::DecompressionFailed)?;
        decoder
            .finish()
            .map_err(|_| mtp::DeserializeError::DecompressionFailed)
    }

    #[test]
    fn reused_gzip_preserves_validation_and_recovers_after_corruption() {
        let original: Vec<u8> = (0..2048).map(|n| (n * 37) as u8).collect();
        let packed = GzipPacked::new(&original).packed_data;
        let mut invalid = vec![Vec::new(), vec![0; 24]];
        for end in 0..packed.len() {
            invalid.push(packed[..end].to_vec());
        }
        for footer_offset in [4, 8] {
            let mut bad = packed.clone();
            let at = bad.len() - footer_offset;
            bad[at] ^= 1;
            invalid.push(bad);
        }
        let mut trailing = packed.clone();
        trailing.push(0);
        invalid.push(trailing);
        let mut concatenated = packed.clone();
        concatenated.extend_from_slice(&packed);
        invalid.push(concatenated);
        for bad in invalid {
            assert!(legacy_gzip_decode(&bad).is_err(), "fixture was valid");
            assert!(GzipPacked::decompress_payload(&bad).is_err());
            assert_eq!(GzipPacked::decompress_payload(&packed).unwrap(), original);
        }
        let retained = GzipPacked::decompress_payload(&packed).unwrap();
        let other = GzipPacked::new(b"another independent member");
        assert_eq!(other.decompress().unwrap(), b"another independent member");
        assert_eq!(
            retained, original,
            "a later decode modified an owned earlier result"
        );
    }

    fn random_bytes(count: usize) -> Vec<u8> {
        let mut seed = 0x517c_c1b7_2722_0a95u64;
        (0..count)
            .map(|_| {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                seed as u8
            })
            .collect()
    }

    #[test]
    fn gzip_scratch_is_bounded_and_handles_optional_headers() {
        let mut encoder = flate2::GzBuilder::new()
            .filename("fixture.bin")
            .comment("independent stream")
            .extra([1, 2, 3, 4])
            .write(Vec::new(), Compression::fast());
        encoder.write_all(b"with optional header fields").unwrap();
        let packed = encoder.finish().unwrap();
        assert_eq!(
            GzipPacked::decompress_payload(&packed).unwrap(),
            legacy_gzip_decode(&packed).unwrap()
        );
        let original = random_bytes(GZIP_SCRATCH_LIMIT * 2);
        let packed = GzipPacked::new(&original).packed_data;
        assert!(packed.len() > GZIP_SCRATCH_LIMIT);
        assert_eq!(GzipPacked::decompress_payload(&packed).unwrap(), original);
        GZIP_SCRATCH.with(|slot| assert!(slot.borrow().is_none(), "retained oversized input"));
        assert_eq!(
            GzipPacked::new(b"small again").decompress().unwrap(),
            b"small again"
        );
        GZIP_SCRATCH.with(|slot| assert!(slot.borrow().as_ref().unwrap().retainable()));
    }

    #[test]
    #[ignore = "local gzip decoder comparison; no sockets or server traffic"]
    fn benchmark_reused_gzip_decoder() {
        use std::{hint::black_box, time::Instant};
        for (size, random) in [
            (512, false),
            (2048, false),
            (8192, false),
            (65536, false),
            (2048, true),
        ] {
            let original = if random {
                random_bytes(size)
            } else {
                (0..size)
                    .map(|i| (i.wrapping_mul(37) % 251) as u8)
                    .collect()
            };
            let packed = GzipPacked::new(&original).packed_data;
            assert_eq!(legacy_gzip_decode(&packed).unwrap(), original);
            assert_eq!(GzipPacked::decompress_payload(&packed).unwrap(), original);
            let count = (8 * 1024 * 1024 / size).clamp(1000, 20000);
            let mut samples = [Vec::new(), Vec::new()];
            for round in 0..8 {
                for reused in [round % 2 == 0, round % 2 != 0] {
                    let began = Instant::now();
                    for _ in 0..count {
                        if reused {
                            black_box(GzipPacked::decompress_payload(black_box(&packed)).unwrap());
                        } else {
                            black_box(legacy_gzip_decode(black_box(&packed)).unwrap());
                        }
                    }
                    samples[usize::from(reused)]
                        .push(began.elapsed().as_nanos() as f64 / count as f64);
                }
            }
            for sample in &mut samples {
                sample.sort_by(f64::total_cmp);
            }
            println!(
                "gzip plain={size} packed={} random={random}: fresh {:.1} ns; reused {:.1} ns",
                packed.len(),
                samples[0][4],
                samples[1][4]
            );
        }
    }

    #[test]
    fn malformed_message_lengths_return_errors_without_panicking() {
        for len in [-1i32, MessageContainer::MAXIMUM_SIZE as i32, i32::MAX, 100] {
            let mut wire = 42i64.to_bytes();
            wire.extend(1i32.to_bytes());
            wire.extend(len.to_bytes());
            assert!(Message::from_bytes(&wire).is_err());
        }
    }

    #[test]
    #[ignore = "isolated allocation/copy benchmark; run release with nocapture"]
    fn benchmark_rpc_result_reuse() {
        use std::hint::black_box;
        use std::time::Instant;
        const N: usize = 200_000;
        for size in [512, 2048, 8192] {
            let wire = rpc_wire(&vec![7; size]);
            let mut old = Vec::new();
            let mut new = Vec::new();
            for round in 0..10 {
                for reuse in [round % 2 == 0, round % 2 != 0] {
                    let start = Instant::now();
                    for _ in 0..N {
                        let input = black_box(&wire).clone(); // Both own a decrypted envelope.
                        if reuse {
                            black_box(RpcResult::from_owned(input).unwrap());
                        } else {
                            black_box(RpcResult::from_bytes(&input).unwrap());
                        }
                    }
                    let ns = start.elapsed().as_nanos() as f64 / N as f64;
                    if reuse {
                        new.push(ns);
                    } else {
                        old.push(ns);
                    }
                }
            }
            old.sort_by(f64::total_cmp);
            new.sort_by(f64::total_cmp);
            println!(
                "{size}-byte RPC envelope median: allocate {:.1} ns, reuse {:.1} ns",
                old[5], new[5]
            );
        }
    }
}
