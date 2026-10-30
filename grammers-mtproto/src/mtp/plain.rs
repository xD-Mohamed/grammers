// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use grammers_crypto::DequeBuffer;
use grammers_tl_types::{Cursor, Deserializable, Serializable};

use super::{Deserialization, DeserializeError, Mtp, RpcResult};
use crate::MsgId;

/// An implementation of the [Mobile Transport Protocol] for plaintext
/// (unencrypted) messages.
///
/// The reason to separate the plaintext and encrypted implementations
/// for serializing messages is that, even though they are similar, the
/// benefits outweight some minor code reuse.
///
/// This way, the encryption key for [`Mtp`] is mandatory so errors
/// for trying to encrypt data without a key are completely eliminated.
///
/// Also, the plaintext part of the protocol does not need to deal with
/// the complexity of the full protocol once encrypted messages are used,
/// so being able to keep a simpler implementation separate is a bonus.
///
/// [Mobile Transport Protocol]: https://core.telegram.org/mtproto
#[non_exhaustive]
pub struct Plain;

#[allow(clippy::new_without_default)]
impl Plain {
    /// Creates a new instance of the plain (unencrypted) MTP.
    pub fn new() -> Self {
        Self
    }
}

impl Mtp for Plain {
    fn supports_keepalive(&self) -> bool {
        false
    }
    /// Wraps a request's data into a plain message (also known as
    /// [unencrypted messages]), and returns its serialized contents.
    ///
    /// Plain messages may be used for requests that don't require an
    /// Authorization Key to be present, such as those needed to generate
    /// the Authorization Key itself.
    ///
    /// [unencrypted messages]: https://core.telegram.org/mtproto/description#unencrypted-message
    fn push(&mut self, buffer: &mut DequeBuffer<u8>, request: &[u8]) -> Option<MsgId> {
        if !buffer.is_empty() || !self.request_is_valid(request) {
            return None;
        }

        0i64.serialize(buffer); // auth_key_id = 0

        // Even though https://core.telegram.org/mtproto/samples-auth_key
        // seems to imply the `msg_id` has to follow some rules, there is
        // no need to generate a valid `msg_id`, it seems. Just use `0`.
        0i64.serialize(buffer); // message_id

        (request.len() as i32).serialize(buffer); // message_data_length
        buffer.extend(request); // message_data

        Some(MsgId(0))
    }

    fn finalize(&mut self, buffer: &mut DequeBuffer<u8>) -> Option<MsgId> {
        (!buffer.is_empty()).then_some(MsgId(0))
    }

    /// Validates that the returned data is a correct plain message, and
    /// if it is, the method returns the inner contents of the message.
    ///
    /// [`serialize_plain_message`]: #method.serialize_plain_message
    fn deserialize(
        &mut self,
        payload: &mut [u8],
    ) -> Result<Vec<Deserialization>, DeserializeError> {
        crate::utils::check_message_buffer(payload)?;

        let mut buf = Cursor::from_slice(payload);
        let auth_key_id = i64::deserialize(&mut buf)?;
        if auth_key_id != 0 {
            return Err(DeserializeError::BadAuthKey {
                got: auth_key_id,
                expected: 0,
            });
        }

        let msg_id = i64::deserialize(&mut buf)?;
        // We can't validate it's close to our system time because our sytem
        // time may be wrong at this point (it only matters once encrypted
        // communication begins). However, we can validate the following:
        //
        // > server message identifiers modulo 4 yield 1 if
        // > the message is a response to a client message
        // https://core.telegram.org/mtproto/description#message-identifier-msg-id
        if msg_id <= 0 || (msg_id % 4) != 1 {
            return Err(DeserializeError::BadMessageId { got: msg_id });
        }

        let len = i32::deserialize(&mut buf)?;
        if len <= 0 {
            return Err(DeserializeError::NegativeMessageLength { got: len });
        }
        if len % 4 != 0 {
            return Err(DeserializeError::InvalidEnvelope {
                reason: "invalid message envelope",
            });
        }
        if len as usize > payload.len() - 20 {
            return Err(DeserializeError::TooLongMessageLength {
                got: len as usize,
                max_length: payload.len() - 20,
            });
        }

        Ok(vec![Deserialization::RpcResult(RpcResult {
            msg_id: MsgId(0),
            body: payload[20..20 + len as usize].into(),
        })])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const REQUEST: &[u8] = b"Hey!";

    fn reply(auth_key: i64, msg_id: i64, length: i32) -> Vec<u8> {
        let mut bytes = auth_key.to_bytes();
        msg_id.serialize(&mut bytes);
        length.serialize(&mut bytes);
        bytes.extend_from_slice(REQUEST);
        bytes
    }

    #[test]
    fn plain_deserialize_rejects_invalid_headers_and_lengths() {
        let mut mtp = Plain::new();
        for length in 0..20 {
            assert_eq!(
                mtp.deserialize(&mut vec![0; length]).err(),
                Some(DeserializeError::MessageBufferTooSmall)
            );
        }
        assert_eq!(
            mtp.deserialize(&mut reply(8, 1, 4)).err(),
            Some(DeserializeError::BadAuthKey {
                got: 8,
                expected: 0
            })
        );
        for id in [i64::MIN, -3, 0, 2, 3, 4] {
            assert_eq!(
                mtp.deserialize(&mut reply(0, id, 4)).err(),
                Some(DeserializeError::BadMessageId { got: id })
            );
        }
        for length in [i32::MIN, -4, 0] {
            assert_eq!(
                mtp.deserialize(&mut reply(0, 1, length)).err(),
                Some(DeserializeError::NegativeMessageLength { got: length })
            );
        }
        for length in [1, 3, 5, i32::MAX] {
            assert_eq!(
                mtp.deserialize(&mut reply(0, 1, length)).err(),
                Some(DeserializeError::InvalidEnvelope {
                    reason: "invalid message envelope"
                })
            );
        }
        for length in [8, i32::MAX - 3] {
            assert_eq!(
                mtp.deserialize(&mut reply(0, 1, length)).err(),
                Some(DeserializeError::TooLongMessageLength {
                    got: length as usize,
                    max_length: 4
                })
            );
        }
    }

    #[test]
    fn plain_deserialize_returns_only_the_declared_body() {
        let mut mtp = Plain::new();
        let mut bytes = reply(0, 5, 4);
        bytes.extend_from_slice(&[0; 16]);
        let results = mtp.deserialize(&mut bytes).unwrap();
        assert!(matches!(results.as_slice(),
            [Deserialization::RpcResult(result)] if result.msg_id == MsgId(0) && result.body == REQUEST));
    }

    #[test]
    fn ensure_finalize_preserves_buffer() {
        let mut buffer = DequeBuffer::with_capacity(0, 0);
        let mut mtp = Plain::new();

        mtp.push(&mut buffer, REQUEST);
        mtp.finalize(&mut buffer);
        assert_eq!(&buffer[buffer.len() - REQUEST.len()..], REQUEST);
    }

    #[test]
    fn ensure_only_one_push_allowed() {
        let mut buffer = DequeBuffer::with_capacity(0, 0);
        let mut mtp = Plain::new();

        assert!(mtp.push(&mut buffer, REQUEST).is_some());
        assert!(mtp.push(&mut buffer, REQUEST).is_none());
    }
}
