// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::time::Instant;
use std::{borrow::Cow, mem};

use grammers_crypto::{AuthKey, DequeBuffer, decrypt_data_v2, encrypt_data_v2};
use grammers_tl_types::{self as tl, Cursor, Deserializable, Identifiable, Serializable};
use tracing::{info, warn};

use super::replay::ReplayIds;
use super::{
    DecodeLimits, Deserialization, DeserializationFailure, DeserializeError, Mtp, RpcResult,
    RpcResultError,
};
use crate::utils::StackBuffer;
use crate::{MsgId, manual_tl};

/// How many future salts to fetch or have stored at a given time.
///
/// Not an `usize` because the API expects a signed `i32`. Should be more than two because the
/// code expects to fetch more salts when going from two to one.
const NUM_FUTURE_SALTS: i32 = 64;

/// When switching to a different salt, how many seconds must have passed since its `valid_since`.
///
/// Used to prevent small fluctuations in the system clock.
const SALT_USE_DELAY: i32 = 60;

/// How much the clock must move between looks at whether the next salt can be
/// used. Salts overlap by many minutes, so a look every few seconds loses nothing,
/// and a busy connection does not read a second clock for every message.
const SALT_CHECK_INTERVAL: u64 = 16;

/// With acknowledgement batching enabled, the longest an acknowledgement may
/// wait for its batch to fill before it rides on the next outgoing message.
/// Acknowledgements only ever travel with outgoing messages, so an idle
/// connection still sends them with its next keepalive ping.
const ACK_MAX_DELAY: std::time::Duration = std::time::Duration::from_secs(10);

static UPDATE_IDS: [u32; 9] = [
    tl::types::UpdateShortMessage::CONSTRUCTOR_ID,
    tl::types::UpdateShortChatMessage::CONSTRUCTOR_ID,
    tl::types::UpdateShort::CONSTRUCTOR_ID,
    tl::types::UpdatesCombined::CONSTRUCTOR_ID,
    tl::types::Updates::CONSTRUCTOR_ID,
    tl::types::UpdateShortSentMessage::CONSTRUCTOR_ID,
    tl::types::messages::AffectedMessages::CONSTRUCTOR_ID,
    tl::types::messages::InvitedUsers::CONSTRUCTOR_ID,
    tl::types::messages::ChatInviteJoinResultOk::CONSTRUCTOR_ID,
];

struct DecodeBudget {
    authenticated: bool,
    replay_floor: i64,
    pending_received: Option<Vec<i64>>,
    limits: DecodeLimits,
    remaining: usize,
    work: usize,
}

impl DecodeBudget {
    fn new(limits: DecodeLimits) -> Self {
        Self {
            authenticated: false,
            replay_floor: 0,
            pending_received: None,
            limits,
            remaining: limits.expanded_bytes,
            work: limits.work,
        }
    }

    fn visit(&mut self, depth: usize) -> Result<(), DeserializeError> {
        if depth > self.limits.depth || self.work == 0 {
            return Err(DeserializeError::ResourceLimit {
                resource: "wrapper depth or message work",
            });
        }
        self.work -= 1;
        Ok(())
    }

    fn decompress(&mut self, bytes: &[u8], depth: usize) -> Result<Vec<u8>, DeserializeError> {
        if depth > self.limits.depth {
            return Err(DeserializeError::ResourceLimit {
                resource: "wrapper depth",
            });
        }
        let limit = self.limits.gzip_output.min(self.remaining);
        let result = manual_tl::GzipPacked::decompress_from_bytes_limited(bytes, limit);
        // Charge failed CRC/footer validation too: it may consume the full
        // budget before failing, and sibling RPCs share this same budget.
        self.remaining -= result.as_ref().map_or(limit, Vec::len);
        result
    }
}

/// A builder to configure [`Mtp`] instances.
///
/// Use the [`Encrypted::build`] method to create builder instances.
pub struct Builder {
    decode_limits: DecodeLimits,
    time_offset: i32,
    first_salt: i64,
    compression_threshold: Option<usize>,

    receive_updates: bool,
    ack_batch: usize,
}

/// An implementation of the [Mobile Transport Protocol] for ciphertext
/// (encrypted) messages.
///
/// [Mobile Transport Protocol]: https://core.telegram.org/mtproto
pub struct Encrypted {
    // Allocated on use, bounded per session to 1024 server envelopes and five
    // minutes of server time.
    received_ids: ReplayIds,
    // Highest ID actually evicted, not the smallest currently cached ID.
    received_floor: i64,
    decode_limits: DecodeLimits,
    /// The Authorization Key to use to encrypt payload.
    auth_key: AuthKey,

    /// The time offset from the server's time, in seconds.
    time_offset: i64,

    /// Salts that may be used when encrypting payload, sorted by valid date descending (so the last one is the one
    /// that can be used now).
    salts: Vec<tl::types::FutureSalt>,

    /// The `now` received from future salts along with the instant when it occured.
    ///
    /// Used to accurately determine when salts become valid.
    start_salt_time: Option<(i32, Instant)>,

    /// Internal request for salts which should not be propagated.
    salt_request_msg_id: Option<MsgId>,
    salt_request_failures: u8,

    /// The secure, random identifier for this instance.
    session_id: i64,

    /// The current message sequence number.
    sequence: i32,

    /// The ID of the last message.
    last_msg_id: i64,

    /// The system clock's seconds when a message ID last read it, and when the
    /// salts were last examined against the time.
    clock_secs: i64,
    salt_checked_secs: i64,

    /// Identifiers that need to be acknowledged to the server.
    ///
    /// A [Content-related Message] is "a message requiring an explicit
    /// acknowledgment. These include all the user and many service messages,
    /// virtually all with the exception of containers and acknowledgments."
    ///
    /// [Content-related Message]: https://core.telegram.org/mtproto/description#content-related-message
    pending_ack: Vec<i64>,

    /// No later than when the oldest entry of `pending_ack` was queued: that time,
    /// or the last time the driver reported a packet written (`last_sent`), which
    /// can only bring the deadline forward.
    pending_ack_since: Option<Instant>,
    last_sent: Option<Instant>,
    ack_flush_due: bool,
    /// Set once a driver has asked for [`Mtp::service_deadline`]. Such a driver
    /// reports the deadline through [`Mtp::on_service_timeout`], so the time is not
    /// also read here for every message built while acknowledgements wait.
    timer_driven: std::sync::atomic::AtomicBool,
    // ACK insertion deferred a user request; give its next attempt priority.
    request_deferred_by_ack: bool,

    /// Pending acknowledgements needed before they are attached to an
    /// outgoing message. 1 attaches them to every message.
    ack_batch: usize,

    /// If present, the threshold in bytes at which a message will be
    /// considered large enough to attempt compressing it. Otherwise,
    /// outgoing messages will never be compressed.
    compression_threshold: Option<usize>,

    /// Temporary deserialization results.
    deserialization: Vec<Deserialization>,

    receive_updates: bool,

    /// How many messages are there in the buffer.
    msg_count: usize,
}

impl Builder {
    /// Set per-packet decompression and processing limits.
    pub fn decode_limits(mut self, limits: DecodeLimits) -> Self {
        self.decode_limits = limits;
        self
    }
    /// Whether unsolicited API updates should be returned to the caller.
    /// Disabled mode also wraps API requests with invokeWithoutUpdates.
    pub fn receive_updates(mut self, enabled: bool) -> Self {
        self.receive_updates = enabled;
        self
    }

    /// Configures the time offset to Telegram servers.
    pub fn time_offset(mut self, offset: i32) -> Self {
        self.time_offset = offset;
        self
    }

    pub fn first_salt(mut self, first_salt: i64) -> Self {
        self.first_salt = first_salt;
        self
    }

    /// Configures the compression threshold for outgoing messages.
    pub fn compression_threshold(mut self, threshold: Option<usize>) -> Self {
        self.compression_threshold = threshold;
        self
    }

    /// How many acknowledgements to collect before attaching them to an
    /// outgoing message. See [`Encrypted::set_ack_batch`].
    pub fn ack_batch(mut self, count: usize) -> Self {
        self.ack_batch = count.max(1);
        self
    }

    /// Finishes the builder and returns the `MTProto` instance with all
    /// the configuration changes applied.
    pub fn finish(self, auth_key: [u8; 256]) -> Encrypted {
        self.try_finish(auth_key)
            .expect("failed to generate a secure session_id")
    }

    /// Construct a session, returning an error if secure randomness is unavailable.
    pub fn try_finish(self, auth_key: [u8; 256]) -> Result<Encrypted, getrandom::Error> {
        self.finish_with_rng(auth_key, getrandom::fill)
    }

    fn finish_with_rng<E>(
        self,
        auth_key: [u8; 256],
        fill: impl FnOnce(&mut [u8]) -> Result<(), E>,
    ) -> Result<Encrypted, E> {
        let mut session_id = [0; 8];
        fill(&mut session_id)?;
        Ok(Encrypted {
            received_ids: ReplayIds::new(),
            received_floor: 0,
            decode_limits: self.decode_limits,
            auth_key: AuthKey::from_bytes(auth_key),
            time_offset: i64::from(self.time_offset),
            salts: vec![tl::types::FutureSalt {
                valid_since: 0,
                valid_until: i32::MAX,
                salt: self.first_salt,
            }],
            start_salt_time: None,
            salt_request_msg_id: None,
            salt_request_failures: 0,
            session_id: i64::from_le_bytes(session_id),
            sequence: 0,
            last_msg_id: 0,
            clock_secs: 0,
            salt_checked_secs: 0,
            pending_ack: vec![],
            pending_ack_since: None,
            last_sent: None,
            ack_flush_due: false,
            timer_driven: std::sync::atomic::AtomicBool::new(false),
            request_deferred_by_ack: false,
            ack_batch: self.ack_batch,
            compression_threshold: self.compression_threshold,
            receive_updates: self.receive_updates,
            deserialization: Vec::new(),
            msg_count: 0,
        })
    }
}

impl Encrypted {
    /// Whether a serialized request can be framed on this type of connection.
    /// Requests need a constructor, complete TL words, and room for any automatic
    /// `invokeWithoutUpdates` prefix. Compression does not relax the input limit.
    /// This allows higher layers to reject invalid input before queueing it.
    pub fn request_fits(request: &[u8], receive_updates: bool) -> bool {
        let len = request.len();
        if len < 4 || !len.is_multiple_of(4) {
            return false;
        }
        let limit = manual_tl::MessageContainer::MAXIMUM_SIZE - manual_tl::Message::SIZE_OVERHEAD;
        // Ordinary requests fit even with the four-byte wrapper; only the last
        // word below the limit needs constructor-specific classification.
        len <= limit - 4
            || len <= limit - Self::without_updates_prefix_len(request, receive_updates)
    }

    fn without_updates_prefix_len(request: &[u8], receive_updates: bool) -> usize {
        if receive_updates {
            return 0;
        }
        let constructor = u32::from_bytes(request).unwrap_or(0);
        if matches!(
            constructor,
            tl::functions::Ping::CONSTRUCTOR_ID
                | tl::functions::PingDelayDisconnect::CONSTRUCTOR_ID
                | tl::functions::GetFutureSalts::CONSTRUCTOR_ID
                | tl::functions::RpcDropAnswer::CONSTRUCTOR_ID
                | tl::functions::DestroySession::CONSTRUCTOR_ID
                | tl::functions::InvokeWithoutUpdates::<tl::functions::help::GetConfig>::CONSTRUCTOR_ID
        ) {
            0
        } else {
            4
        }
    }

    /// Control unsolicited API updates; configure before initializing a connection.
    /// RPC results and protocol service messages are always processed normally.
    pub fn set_receive_updates(&mut self, enabled: bool) {
        self.receive_updates = enabled;
    }

    /// Change per-packet decoding resource limits.
    pub fn set_decode_limits(&mut self, limits: DecodeLimits) {
        self.decode_limits = limits;
    }

    /// Collect up to `count` acknowledgements before attaching them to an
    /// outgoing message, instead of wrapping every message in a container with
    /// one. An acknowledgement never waits more than ten seconds for its batch
    /// once another message is sent. 1 (the default) acknowledges on every message.
    pub fn set_ack_batch(&mut self, count: usize) {
        self.ack_batch = count.max(1);
    }

    /// Start building a new encrypted MTP.
    pub fn build() -> Builder {
        Builder {
            decode_limits: DecodeLimits::default(),
            time_offset: 0,
            compression_threshold: crate::DEFAULT_COMPRESSION_THRESHOLD,
            receive_updates: true,
            first_salt: 0,
            ack_batch: 1,
        }
    }

    /// The Authorization Key used for encryption and decryption.
    pub fn auth_key(&self) -> [u8; 256] {
        self.auth_key.to_bytes()
    }

    /// Correct our time offset based on a known valid message ID.
    fn correct_time_offset(&mut self, msg_id: i64) {
        // The clock identifiers are made from, so that the offset is one they add to.
        let now = crate::coarse_clock::unix_time()
            .as_secs()
            .min(i64::MAX as u64) as i64;

        let server_seconds = msg_id >> 32;
        self.time_offset = server_seconds - now;
        // Integer-second estimates can move backwards with ordinary latency
        // jitter. Preserve monotonic IDs unless a substantially fast clock
        // would otherwise keep generating IDs the server cannot accept.
        if (self.last_msg_id >> 32) > server_seconds + 3 {
            self.last_msg_id = 0;
        }
    }

    /// Generates a new unique message ID based on the current
    /// time (in ms) since epoch, applying a known time offset.
    ///
    /// The clock is read for the first message of a packet. The others are created
    /// in the same instant: a request, the acknowledgements beside it and their
    /// container used to read it three times, to find it had hardly moved. They take
    /// the identifiers that follow, as they did whenever it had not moved at all.
    fn get_new_msg_id(&mut self) -> i64 {
        if self.msg_count != 0 && self.last_msg_id != 0 {
            self.last_msg_id += 4;
            return self.last_msg_id;
        }
        // As of the kernel's last tick where it keeps one: identifiers only have to
        // follow the clock to within seconds, and to increase, which the floor below
        // sees to for every packet of the same tick.
        let now = crate::coarse_clock::unix_time();
        self.clock_secs = now.as_secs().min(i64::MAX as u64) as i64;

        let seconds = self.clock_secs.saturating_add(self.time_offset).max(0) as u64;
        let nanoseconds = now.subsec_nanos() as u64;
        let mut new_msg_id = ((seconds << 32) | (nanoseconds << 2)) as i64;

        if self.last_msg_id >= new_msg_id {
            new_msg_id = self.last_msg_id + 4;
        }

        self.last_msg_id = new_msg_id;
        new_msg_id
    }

    /// Generates the next sequence number depending on whether
    /// it should be for a content-related query or not.
    fn get_seq_no(&mut self, content_related: bool) -> i32 {
        if content_related {
            self.sequence += 2;
            self.sequence - 1
        } else {
            self.sequence
        }
    }

    fn serialize_msg(
        &mut self,
        buffer: &mut DequeBuffer<u8>,
        body: &[u8],
        content_related: bool,
    ) -> MsgId {
        let msg_id = self.serialize_msg_header(buffer, body.len(), content_related);
        buffer.extend(body);
        msg_id
    }

    fn message_fits(&self, buffer: &DequeBuffer<u8>, body_len: usize) -> bool {
        self.msg_count < manual_tl::MessageContainer::MAXIMUM_LENGTH
            && buffer
                .len()
                .checked_add(manual_tl::Message::SIZE_OVERHEAD)
                .and_then(|n| n.checked_add(body_len))
                .is_some_and(|n| n <= manual_tl::MessageContainer::MAXIMUM_SIZE)
    }

    fn serialize_msg_header(
        &mut self,
        buffer: &mut DequeBuffer<u8>,
        body_len: usize,
        content_related: bool,
    ) -> MsgId {
        let msg_id = self.get_new_msg_id();

        msg_id.serialize(buffer);
        self.get_seq_no(content_related).serialize(buffer);
        (body_len as i32).serialize(buffer);

        self.msg_count += 1;
        MsgId(msg_id)
    }

    fn queue_ack(&mut self, msg_id: i64) -> Result<(), DeserializeError> {
        // A peer that keeps sending while our socket is write-blocked must not
        // retain unbounded ACK metadata. Allow eight full protocol ACK batches.
        if self.pending_ack.len() >= 65_536 {
            return Err(DeserializeError::ResourceLimit {
                resource: "pending acknowledgements",
            });
        }
        if self.pending_ack.is_empty() {
            // A reply follows the request whose write the driver just timed. Counting
            // from that write spares a clock read per message received, and sends the
            // acknowledgement no later. After a long silence it leaves at once, alone,
            // where it would have left alone ten seconds later.
            self.pending_ack_since = Some(self.last_sent.unwrap_or_else(Instant::now));
        }
        self.pending_ack.push(msg_id);
        Ok(())
    }

    /// Whether the oldest pending acknowledgement has waited [`ACK_MAX_DELAY`].
    ///
    /// A driver that asked for the deadline says so with `on_service_timeout`,
    /// which sets `ack_flush_due`. Only without one is the clock read here.
    fn ack_delay_passed(&self) -> bool {
        if self.timer_driven.load(std::sync::atomic::Ordering::Relaxed) {
            return false;
        }
        self.pending_ack_since
            .is_none_or(|since| since.elapsed() >= ACK_MAX_DELAY)
    }

    /// Whether the pending acknowledgements go out with the next message.
    fn ack_due(&self) -> bool {
        let count = self.pending_ack.len();
        count != 0 && (self.ack_flush_due || count >= self.ack_batch || self.ack_delay_passed())
    }

    fn serialize_pending_ack(&mut self, buffer: &mut DequeBuffer<u8>) {
        // Telegram permits at most 8192 IDs in each acknowledgement. Only
        // consume IDs after capacity has been reserved, retaining the oldest
        // deadline when a full packet defers some or all acknowledgements.
        let available = manual_tl::MessageContainer::MAXIMUM_SIZE
            .saturating_sub(buffer.len() + manual_tl::Message::SIZE_OVERHEAD + 12)
            / 8;
        let count = self.pending_ack.len().min(8192).min(available);
        if count == 0 || !self.message_fits(buffer, 12 + count * 8) {
            return;
        }
        // Constructor + vector constructor + count + the i64 message IDs.
        self.serialize_msg_header(buffer, 12 + count * 8, false);
        tl::types::MsgsAck::CONSTRUCTOR_ID.serialize(buffer);
        0x1cb5_c415u32.serialize(buffer);
        (count as i32).serialize(buffer);
        for id in self.pending_ack.drain(..count) {
            id.serialize(buffer);
        }
        if self.pending_ack.is_empty() {
            self.pending_ack_since = None;
            self.ack_flush_due = false;
        }
    }

    fn get_current_salt(&self) -> i64 {
        self.salts.last().map(|s| s.salt).unwrap_or(0)
    }

    fn try_request_salts(&mut self, buffer: &mut DequeBuffer<u8>) {
        if buffer.is_empty()
            && self.salts.len() == 1
            && self.salt_request_msg_id.is_none()
            && self.salt_request_failures <= 3
            && self.get_current_salt() != 0
            && self.message_fits(buffer, 8)
        {
            // If salts are requested in a container leading to bad_msg,
            // the bad_msg_id will refer to the container, not the salts request.
            //
            // We don't keep track of containers and content-related messages they contain for simplicity.
            // This would break, because we couldn't identify the response.
            //
            // So salts are only requested once we have a valid salt to reduce the chances of this happening.
            info!("only one future salt remaining; asking for more salts");
            let body = tl::functions::GetFutureSalts {
                num: NUM_FUTURE_SALTS,
            }
            .to_bytes();
            self.salt_request_msg_id = Some(self.serialize_msg(buffer, &body, true));
        }
    }

    /// `finalize`, but without encryption.
    ///
    /// The buffer is *not* cleared, but is instead returned.
    fn finalize_plain(&mut self, buffer: &mut DequeBuffer<u8>) {
        if self.msg_count == 0 {
            return;
        }

        if self.msg_count != 1 {
            // Prepend a container, setting its message ID and sequence number.
            // + 8 because it has to include the constructor ID and length (4 bytes each).
            let len = (buffer.len() + 8) as i32;
            let mut header = StackBuffer::<MESSAGE_CONTAINER_HEADER_LEN>::new();

            // Manually `serialize_msg` because the container body was already written.
            self.get_new_msg_id().serialize(&mut header);
            self.get_seq_no(false).serialize(&mut header);

            len.serialize(&mut header);

            manual_tl::MessageContainer::CONSTRUCTOR_ID.serialize(&mut header);
            (self.msg_count as i32).serialize(&mut header);
            buffer.extend_front(&header.into_inner());
        }

        {
            // Prepend the message header
            let mut header = StackBuffer::<PLAIN_PACKET_HEADER_LEN>::new();
            self.get_current_salt().serialize(&mut header); // 8 bytes

            self.session_id.serialize(&mut header); // 8 bytes
            buffer.extend_front(&header.into_inner());
        }

        self.msg_count = 0;
    }

    // Called only after crypto has authenticated the entire plaintext.
    fn process_plaintext(
        &mut self,
        plaintext: &[u8],
    ) -> Result<Vec<Deserialization>, DeserializeError> {
        let mut buffer = Cursor::from_slice(plaintext);

        let _salt = i64::deserialize(&mut buffer)?;
        let session_id = i64::deserialize(&mut buffer)?;
        if session_id != self.session_id {
            return Err(DeserializeError::InvalidEnvelope {
                reason: "wrong session id",
            });
        }

        let message = manual_tl::MessageView::deserialize(&mut buffer)?;
        if !(12..=1024).contains(&(plaintext.len() - buffer.pos())) {
            return Err(DeserializeError::InvalidEnvelope {
                reason: "invalid encrypted padding",
            });
        }
        let mut budget = DecodeBudget::new(self.decode_limits);
        budget.authenticated = true;
        budget.replay_floor = self.received_floor;
        if let Err(error) = self.process_bounded(message, &mut budget, 0, false) {
            self.deserialization.clear();
            return Err(error);
        }
        if let Some(ids) = budget.pending_received {
            for id in ids {
                self.remember_received(id);
            }
        }

        // For simplicity, and to avoid passing too much stuff around (RPC results, updates),
        // the processing result is stored in self. After processing is done, that temporary
        // state is cleaned and returned with `mem::take`.
        Ok(mem::take(&mut self.deserialization))
    }
    #[cfg(test)]
    fn process_message(
        &mut self,
        message: manual_tl::MessageView<'_>,
    ) -> Result<(), DeserializeError> {
        let mut budget = DecodeBudget::new(self.decode_limits);
        self.process_bounded(message, &mut budget, 0, false)
    }

    fn process_bounded(
        &mut self,
        mut message: manual_tl::MessageView<'_>,
        budget: &mut DecodeBudget,
        mut depth: usize,
        in_container: bool,
    ) -> Result<(), DeserializeError> {
        let msg_id = message.msg_id;
        if budget.authenticated {
            if !in_container {
                Self::validate_header(&message)?;
            }
            if self.received_ids.contains(&msg_id)
                || budget
                    .pending_received
                    .as_ref()
                    .is_some_and(|ids| ids.binary_search(&msg_id).is_ok())
            {
                if message.requires_ack() {
                    self.queue_ack(msg_id)?;
                }
                return Ok(());
            }
        }
        let stale = budget.authenticated && msg_id <= budget.replay_floor;
        // Gzip replaces a body, not its authenticated envelope: acknowledge
        // once, and release the owned parent before descending to the next body.
        loop {
            budget.visit(depth)?;
            if message.constructor_id()? != manual_tl::GzipPacked::CONSTRUCTOR_ID {
                break;
            }
            depth += 1;
            let body = budget.decompress(&message.body, depth)?;
            message.body = Cow::Owned(body);
        }
        if message.requires_ack() {
            self.queue_ack(message.msg_id)?;
        }

        let constructor = message.constructor_id()?;
        if stale
            && constructor != manual_tl::RpcResult::CONSTRUCTOR_ID
            && constructor != manual_tl::MessageContainer::CONSTRUCTOR_ID
            && constructor != tl::types::Pong::CONSTRUCTOR_ID
            && constructor != tl::types::FutureSalts::CONSTRUCTOR_ID
            && constructor != tl::types::BadMsgNotification::CONSTRUCTOR_ID
            && constructor != tl::types::BadServerSalt::CONSTRUCTOR_ID
        {
            // Evicted replays and delayed messages are indistinguishable.
            // Suppress service/update side effects, but let stale RPC results
            // retire sent requests. Classify container children individually.
            return Ok(());
        }

        // Handle all the possible Service Messages:
        // * https://core.telegram.org/mtproto/service_messages
        // * https://core.telegram.org/mtproto/service_messages_about_messages
        //
        // The order of the `match` here is the same as the order in which the
        // items appear in the documentation (to make it easier to review).
        // TODO verify what needs ack and what doesn't
        let result = match constructor {
            // Response to an RPC query
            manual_tl::RpcResult::CONSTRUCTOR_ID => {
                self.handle_rpc_result(message, budget, depth, !stale)
            }

            // Service Messages about Messages
            // Acknowledgment of Receipt
            tl::types::MsgsAck::CONSTRUCTOR_ID => self.handle_ack(message),
            // Notice of Ignored Error Message
            tl::types::BadMsgNotification::CONSTRUCTOR_ID
            | tl::types::BadServerSalt::CONSTRUCTOR_ID => {
                self.handle_bad_notification(message, !stale)
            }
            // Request for Message Status Information
            tl::types::MsgsStateReq::CONSTRUCTOR_ID => self.handle_state_req(message),
            // Informational Message regarding Status of Messages
            tl::types::MsgsStateInfo::CONSTRUCTOR_ID => self.handle_state_info(message),
            // Voluntary Communication of Status of Messages
            tl::types::MsgsAllInfo::CONSTRUCTOR_ID => self.handle_msg_all(message),
            // Extended Voluntary Communication of Status of One Message
            tl::types::MsgDetailedInfo::CONSTRUCTOR_ID
            | tl::types::MsgNewDetailedInfo::CONSTRUCTOR_ID => self.handle_detailed_info(message),
            // Explicit Request to Re-Send Messages & Explicit Request to Re-Send Answers
            tl::types::MsgResendReq::CONSTRUCTOR_ID
            | tl::types::MsgResendAnsReq::CONSTRUCTOR_ID => self.handle_msg_resend(message),

            // Request for several future salts
            tl::types::FutureSalt::CONSTRUCTOR_ID => self.handle_future_salt(message),
            tl::types::FutureSalts::CONSTRUCTOR_ID => self.handle_future_salts(message, !stale),
            // Ping Messages (PING/PONG)
            tl::types::Pong::CONSTRUCTOR_ID => self.handle_pong(message),
            // Request to Destroy Session
            tl::types::DestroySessionOk::CONSTRUCTOR_ID
            | tl::types::DestroySessionNone::CONSTRUCTOR_ID => self.handle_destroy_session(message),
            // New Session Creation Notification
            tl::types::NewSessionCreated::CONSTRUCTOR_ID => {
                self.handle_new_session_created(message)
            }
            // Containers (Simple Container)
            manual_tl::MessageContainer::CONSTRUCTOR_ID => {
                if in_container {
                    return Err(DeserializeError::InvalidEnvelope {
                        reason: "nested simple container",
                    });
                }
                self.handle_container(message, budget, depth)
            }
            // Message Copies
            manual_tl::MessageCopy::CONSTRUCTOR_ID => self.handle_msg_copy(message),
            // Packed Object
            // HTTP Wait/Long Poll
            tl::types::HttpWait::CONSTRUCTOR_ID => self.handle_http_wait(message),
            _ => self.handle_update(message),
        };
        if result.is_ok() && budget.authenticated && !stale {
            if let Some(ids) = &mut budget.pending_received {
                if let Err(index) = ids.binary_search(&msg_id) {
                    ids.insert(index, msg_id);
                }
            } else {
                self.remember_received(msg_id);
            }
        }
        result
    }

    fn remember_received(&mut self, msg_id: i64) {
        // A delayed ID which cannot be retained is itself evicted, preserving a
        // monotonic floor at the highest ID that actually left the history.
        if let Some(evicted) = self.received_ids.remember(msg_id) {
            self.received_floor = self.received_floor.max(evicted);
        }
    }

    fn validate_header(message: &manual_tl::MessageView<'_>) -> Result<(), DeserializeError> {
        if message.msg_id <= 0 || message.msg_id % 2 != 1 || message.seq_no < 0 {
            return Err(DeserializeError::InvalidEnvelope {
                reason: "invalid server message header",
            });
        }
        Ok(())
    }

    fn fail_salt_request(&mut self, msg_id: MsgId) -> bool {
        if self.salt_request_msg_id != Some(msg_id) {
            return false;
        }
        self.salt_request_msg_id = None;
        self.salt_request_failures = self.salt_request_failures.saturating_add(1);
        if self.salt_request_failures == 4 {
            warn!("disabling optional salt prefetch after repeated failure");
        }
        true
    }

    /// **[Response to an RPC query]**
    ///
    /// A response to an RPC query is normally wrapped as follows:
    ///
    /// ```tl
    /// rpc_result#f35c6d01 req_msg_id:long result:Object = RpcResult;
    /// ```
    ///
    /// Here `req_msg_id` is the identifier of the message sent by the other
    /// party and containing an RPC query. This way, the recipient knows that
    /// the result is a response to the specific RPC query in question.
    ///
    /// At the same time, this response serves as acknowledgment of the other
    /// party's receipt of the `req_msg_id` message.
    ///
    /// Note that the response to an RPC query must also be acknowledged. Most
    /// frequently, this coincides with the transmission of the next message
    /// (which may have a container attached to it carrying a service message
    /// with the acknowledgment).
    ///
    /// **[RPC Error]**
    ///
    /// The result field returned in response to any RPC query may also
    /// contain an error message in the following format:
    ///
    /// ```tl
    /// rpc_result#f35c6d01 req_msg_id:long result:Object = RpcResult;
    /// ```
    ///
    /// **[Cancellation of an RPC Query]**
    ///
    /// In certain situations, the client does not want to receive a response
    /// to an already transmitted RPC query, for example because the response
    /// turns out to be long and the client has decided to do without it
    /// because of insufficient link capacity. Simply interrupting the TCP
    /// connection will not have any effect because the server would re-send
    /// the missing response at the first opportunity. Therefore, the client
    /// needs a way to cancel receipt of the RPC response message, actually
    /// acknowledging its receipt prior to it being in fact received, which
    /// will settle the server down and prevent it from re-sending the response.
    /// However, the client does not know the RPC response's msg_id prior to
    /// receiving the response; the only thing it knows is the req_msg_id.
    /// i.e. the msg_id of the relevant RPC query. Therefore, a special query
    /// is used:
    ///
    /// ```tl
    /// rpc_drop_answer#58e4a740 req_msg_id:long = RpcDropAnswer;
    /// ```
    ///
    /// The response to this query returns as one of the following messages
    /// wrapped in `rpc_result` and requiring an acknowledgment:
    ///
    /// ```tl
    /// rpc_answer_unknown#5e2ad36e = RpcDropAnswer;
    /// rpc_answer_dropped_running#cd78e586 = RpcDropAnswer;
    /// rpc_answer_dropped#a43ad8b7 msg_id:long seq_no:int bytes:int = RpcDropAnswer;
    /// ```
    ///
    /// The first version of the response is used if the server remembers
    /// nothing of the incoming req_msg_id (if it has already been responded
    /// to, for example). The second version is used if the response was
    /// canceled while the RPC query was being processed (where the RPC query
    /// itself was still fully processed); in this case, the same
    /// `rpc_answer_dropped_running` is also returned in response to the
    /// original query, and both of these responses require an acknowledgment
    /// from the client. The final version means that the RPC response was
    /// removed from the server's outgoing queue, and its `msg_id`, `seq_no`,
    /// and length in `bytes` are transmitted to the client.
    ///
    /// Note that `rpc_answer_dropped_running` and `rpc_answer_dropped` serve
    /// as acknowledgments of the server's receipt of the original query (the
    /// same one, the response to which we wish to forget). In addition, same
    /// as for any RPC queries, any response to `rpc_drop_answer` is an
    /// acknowledgment for `rpc_drop_answer` itself.
    ///
    /// As an alternative to using `rpc_drop_answer`, a new session may be
    /// created after the connection is reset and the old session is removed
    /// through `destroy_session`.
    ///
    /// [Response to an RPC query]: https://core.telegram.org/mtproto/service_messages#response-to-an-rpc-query
    /// [RPC Error]: https://core.telegram.org/mtproto/service_messages#rpc-error
    /// [Cancellation of an RPC Query]: https://core.telegram.org/mtproto/service_messages#cancellation-of-an-rpc-query
    fn handle_rpc_result(
        &mut self,
        message: manual_tl::MessageView<'_>,
        budget: &mut DecodeBudget,
        mut depth: usize,
        emit_own_updates: bool,
    ) -> Result<(), DeserializeError> {
        let (req_msg_id, mut result) = match message.body {
            Cow::Borrowed(body) => {
                let (id, result) = manual_tl::RpcResult::borrowed_parts(body)?;
                (id, Cow::Borrowed(result))
            }
            Cow::Owned(body) => {
                let rpc = manual_tl::RpcResult::from_owned(body)?;
                (rpc.req_msg_id, Cow::Owned(rpc.result))
            }
        };
        let msg_id = MsgId(req_msg_id);
        // A compressed payload follows the same validation and error dispatch
        // as an ordinary result; gzip is not evidence of RPC success.
        while matches!(
            u32::from_bytes(&result),
            Ok(manual_tl::GzipPacked::CONSTRUCTOR_ID)
        ) {
            depth += 1;
            match budget
                .visit(depth)
                .and_then(|()| budget.decompress(&result, depth))
            {
                Ok(body) => result = Cow::Owned(body),
                Err(error) => {
                    if self.fail_salt_request(msg_id) {
                        return Ok(());
                    }
                    self.deserialization
                        .push(Deserialization::Failure(DeserializationFailure {
                            msg_id,
                            error,
                        }));
                    return Ok(());
                }
            }
        }
        let inner_constructor = u32::from_bytes(&result);

        // Any error during a RPC result will be given to the user,
        // which means this method itself is doing its job `Ok`.
        let inner_constructor = match inner_constructor {
            Ok(x) => x,
            Err(e) => {
                if self.fail_salt_request(msg_id) {
                    return Ok(());
                }
                self.deserialization
                    .push(Deserialization::Failure(DeserializationFailure {
                        msg_id,
                        error: e.into(),
                    }));
                return Ok(());
            }
        };

        if self.salt_request_msg_id == Some(msg_id) {
            let valid_salts = inner_constructor == tl::types::FutureSalts::CONSTRUCTOR_ID
                && matches!(tl::enums::FutureSalts::from_bytes(&result),
                    Ok(tl::enums::FutureSalts::Salts(ref salts))
                        if salts.req_msg_id == req_msg_id
                            && salts.salts.0.len() <= NUM_FUTURE_SALTS as usize
                            && salts.salts.0.iter().all(|salt| salt.valid_since < salt.valid_until));
            if valid_salts {
                return self.handle_future_salts(
                    manual_tl::MessageView {
                        msg_id: message.msg_id,
                        seq_no: message.seq_no,
                        body: result,
                    },
                    emit_own_updates,
                );
            }
            // Optional service failures must not retain a permanent in-flight marker.
            self.fail_salt_request(msg_id);
            return Ok(());
        }

        match inner_constructor {
            // RPC Error
            tl::types::RpcError::CONSTRUCTOR_ID => match tl::enums::RpcError::from_bytes(&result) {
                Ok(tl::enums::RpcError::Error(error)) => self
                    .deserialization
                    .push(Deserialization::RpcError(RpcResultError { msg_id, error })),
                Err(e) => {
                    self.deserialization
                        .push(Deserialization::Failure(DeserializationFailure {
                            msg_id,
                            error: e.into(),
                        }))
                }
            },

            // rpc_drop_answer acknowledgements are results for that call,
            // not permission to discard the unrelated original request.
            _ => {
                if emit_own_updates {
                    self.store_own_updates(msg_id, &result);
                }
                self.deserialization
                    .push(Deserialization::RpcResult(RpcResult {
                        msg_id,
                        body: result.into_owned(),
                    }));
            }
        }

        Ok(())
    }

    /// Updates produced by `rpc_result` must be considered as any other updates, since they can
    /// change the `pts`. If this wasn't done, eventually higher levels would find gaps.
    ///
    /// Users may also be interested in handling updates produced by the client as if they were
    /// like any other.
    ///
    /// This MUST be done before storing the [`Deserialization::RpcResult`],
    /// because the stored update might need to reference the request, which
    /// must not have been dropped yet.
    fn store_own_updates(&mut self, msg_id: MsgId, body: &[u8]) {
        if !self.receive_updates {
            return;
        }
        match u32::from_bytes(body) {
            Ok(body_id) => {
                if UPDATE_IDS.contains(&body_id) {
                    self.deserialization.push(Deserialization::OwnUpdate {
                        msg_id,
                        update: body.to_vec(),
                    });
                }
            }
            Err(_err) => {
                // Failing is fine, it likely means the update was bad and eventually there will
                // be a gap to fill.
            }
        }
    }

    /// **[Acknowledgment of Receipt]**
    ///
    /// Receipt of virtually all messages (with the exception of some purely
    /// service ones as well as the plain-text messages used in the protocol
    /// for creating an Authorization Key) must be acknowledged.
    ///
    /// This requires the use of the following service message (not requiring
    /// an acknowledgment):
    ///
    /// ```tl
    /// msgs_ack#62d6b459 msg_ids:Vector long = MsgsAck;
    /// ```
    ///
    /// A server usually acknowledges the receipt of a message from a client
    /// (normally, an RPC query) using an RPC response. If a response is a
    /// long time coming, a server may first send a receipt acknowledgment,
    /// and somewhat later, the RPC response itself.
    ///
    /// A client normally acknowledges the receipt of a message from a server
    /// (usually, an RPC response) by adding an acknowledgment to the next RPC
    /// query if it is not transmitted too late (if it is generated, say,
    /// 60-120 seconds following the receipt of a message from the server).
    /// However, if for a long period of time there is no reason to send
    /// messages to the server or if there is a large number of
    /// unacknowledged messages from the server (say, over 16), the client
    /// transmits a stand-alone acknowledgment.
    ///
    /// [Acknowledgment of Receipt]: https://core.telegram.org/mtproto/service_messages_about_messages#acknowledgment-of-receipt
    fn handle_ack(&self, message: manual_tl::MessageView<'_>) -> Result<(), DeserializeError> {
        // TODO notify about this somehow
        let _ack = tl::enums::MsgsAck::from_bytes(&message.body)?;
        Ok(())
    }

    /// **[Notice of Ignored Error Message]**
    ///
    /// In certain cases, a server may notify a client that its incoming
    /// message was ignored for whatever reason. Note that such a notification
    /// cannot be generated unless a message is correctly decoded by the
    /// server.
    ///
    /// ```tl
    /// bad_msg_notification#a7eff811 bad_msg_id:long bad_msg_seqno:int error_code:int = BadMsgNotification;
    /// bad_server_salt#edab447b bad_msg_id:long bad_msg_seqno:int error_code:int new_server_salt:long = BadMsgNotification;
    /// ```
    ///
    /// Here, `error_code` can also take on the following values:
    ///
    /// * 16: `msg_id` too low (most likely, client time is wrong; it would
    ///   be worthwhile to synchronize it using msg_id notifications and re-
    ///   send the original message with the “correct” msg_id or wrap it in a
    ///   container with a new msg_id if the original message had waited too
    ///   long on the client to be transmitted)
    /// * 17: `msg_id` too high (similar to the previous case, the client time
    ///   has to be synchronized, and the message re-sent with the correct
    ///   `msg_id`)
    /// * 18: incorrect two lower order `msg_id` bits (the server expects
    ///   client message `msg_id` to be divisible by 4)
    /// * 19: container `msg_id` is the same as `msg_id` of a previously
    ///   received message (this must never happen)
    /// * 20: message too old, and it cannot be verified whether the server
    ///   has received a message with this `msg_id` or not
    /// * 32: `msg_seqno` too low (the server has already received a message
    ///   with a lower `msg_id` but with either a higher or an equal and odd
    ///   `seqno`)
    /// * 33: `msg_seqno` too high (similarly, there is a message with a
    ///   higher `msg_id` but with either a lower or an equal and odd `seqno`)
    /// * 34: an even `msg_seqno` expected (irrelevant message), but odd
    ///   received
    /// * 35: odd `msg_seqno` expected (relevant message), but even received
    /// * 48: incorrect server salt (in this case, the `bad_server_salt`
    ///   response is received with the correct salt, and the message is to be
    ///   re-sent with it)
    /// * 64: invalid container.
    ///
    /// The intention is that `error_code` values are grouped
    /// (`error_code >> 4`): for example, the codes `0x40 - 0x4f` correspond
    /// to errors in container decomposition.
    ///
    /// Notifications of an ignored message do not require acknowledgment
    /// (i.e., are irrelevant).
    ///
    /// **Important**: if `server_salt` has changed on the server or if client
    /// time is incorrect, any query will result in a notification in the
    /// above format. The client must check that it has, in fact, recently
    /// sent a message with the specified msg_id, and if that is the case,
    /// update its time correction value (the difference between the client's
    /// and the server's clocks) and the server salt based on msg_id and the
    /// `server_salt` notification, so as to use these to (re)send future
    /// messages. In the meantime, the original message (the one that caused
    /// the error message to be returned) must also be re-sent with a better
    /// `msg_id` and/or `server_salt`.
    ///
    /// In addition, the client can update the `server_salt` value used to
    /// send messages to the server, based on the values of RPC responses or
    /// containers carrying an RPC response, provided that this RPC response
    /// is actually a match for the query sent recently. (If there is doubt,
    /// it is best not to update since there is risk of a replay attack).
    ///
    /// [Notice of Ignored Error Message]: https://core.telegram.org/mtproto/service_messages_about_messages#notice-of-ignored-error-message
    fn handle_bad_notification(
        &mut self,
        message: manual_tl::MessageView<'_>,
        apply_service_state: bool,
    ) -> Result<(), DeserializeError> {
        let bad_msg = tl::enums::BadMsgNotification::from_bytes(&message.body)?;

        if !self.fail_salt_request(MsgId(bad_msg.bad_msg_id())) {
            self.deserialization
                .push(Deserialization::BadMessage(super::BadMessage {
                    msg_id: MsgId(bad_msg.bad_msg_id()),
                    code: bad_msg.error_code(),
                }));
        }

        if !apply_service_state {
            // A delayed rejection still retires/retries its matching request,
            // but must not roll back current salt, clock or sequence state.
            return Ok(());
        }

        let bad_msg = match bad_msg {
            tl::enums::BadMsgNotification::Notification(x) => x,
            tl::enums::BadMsgNotification::BadServerSalt(x) => {
                self.salts.clear();
                self.salts.push(tl::types::FutureSalt {
                    valid_since: 0,
                    valid_until: i32::MAX,
                    salt: x.new_server_salt,
                });
                self.salt_request_msg_id = None;

                info!("got bad salt; salts have been reset down to a single one");
                return Ok(());
            }
        };

        match bad_msg.error_code {
            16 => {
                // Sent `msg_id` was too low (our `time_offset` is wrong).
                self.correct_time_offset(message.msg_id);
            }
            17 => {
                // Sent `msg_id` was too high (our `time_offset` is wrong).
                self.correct_time_offset(message.msg_id);
            }
            32 => {
                // Sent `seq_no` was too low. Bump it by some large-ish value.
                // TODO start with a fresh session rather than guessing
                self.sequence =
                    self.sequence
                        .checked_add(64)
                        .ok_or(DeserializeError::InvalidEnvelope {
                            reason: "sequence overflow",
                        })?;
            }
            33 => {
                // Sent `seq_no` was too high (this error doesn't seem to occur).
                // TODO start with a fresh session rather than guessing
                self.sequence = self.sequence.saturating_sub(16).max(0);
            }
            _ => {
                // Just notify about it.
            }
        }

        Ok(())
    }

    /// **[Request for Message Status Information]**
    ///
    /// If either party has not received information on the status of its
    /// outgoing messages for a while, it may explicitly request it from the
    /// other party:
    ///
    /// ```tl
    /// msgs_state_req#da69fb52 msg_ids:Vector long = MsgsStateReq;
    /// ```
    ///
    /// [Request for Message Status Information]: https://core.telegram.org/mtproto/service_messages_about_messages#request-for-message-status-information
    fn handle_state_req(
        &self,
        _message: manual_tl::MessageView<'_>,
    ) -> Result<(), DeserializeError> {
        // TODO implement
        Ok(())
    }

    /// **[Informational Message regarding Status of Messages]**
    ///
    /// ```tl
    /// msgs_state_info#04deb57d req_msg_id:long info:string = MsgsStateInfo;
    /// ```
    ///
    /// Here, `info` is a string that contains exactly one byte of message
    /// status for each message from the incoming `msg_ids` list:
    ///
    /// * 1 = nothing is known about the message (`msg_id` too low, the other
    ///   party may have forgotten it)
    /// * 2 = message not received (`msg_id` falls within the range of stored
    ///   identifiers; however, the other party has certainly not received a
    ///   message like that)
    /// * 3 = message not received (`msg_id` too high; however, the other
    ///   party has certainly not received it yet)
    /// * 4 = message received (note that this response is also at the same
    ///   time a receipt acknowledgment)
    /// * +8 = message already acknowledged
    /// * +16 = message not requiring acknowledgment
    /// * +32 = RPC query contained in message being processed or processing
    ///   already complete
    /// * +64 = content-related response to message already generated
    /// * +128 = other party knows for a fact that message is already received
    ///
    /// This response does not require an acknowledgment. It is an
    /// acknowledgment of the relevant `msgs_state_req`, in and of itself.
    ///
    /// Note that if it turns out suddenly that the other party does not have
    /// a message that looks like it has been sent to it, the message can
    /// simply be re-sent. Even if the other party should receive two copies
    /// of the message at the same time, the duplicate will be ignored.
    /// (If too much time has passed, and the original `msg_id` is not longer
    /// valid, the message is to be wrapped in `msg_copy`).
    ///
    /// [Informational Message regarding Status of Messages]: https://core.telegram.org/mtproto/service_messages_about_messages#informational-message-regarding-status-of-messages
    fn handle_state_info(
        &mut self,
        _message: manual_tl::MessageView<'_>,
    ) -> Result<(), DeserializeError> {
        // TODO implement
        Ok(())
    }

    /// **[Voluntary Communication of Status of Messages]**
    ///
    /// Either party may voluntarily inform the other party of the status of
    /// the messages transmitted by the other party.
    ///
    /// ```tl
    /// msgs_all_info#8cc0d131 msg_ids:Vector long info:string = MsgsAllInfo;
    /// ```
    ///
    /// All message codes known to this party are enumerated, with the
    /// exception of those for which the +128 and the +16 flags are set.
    /// However, if the +32 flag is set but not +64, then the message status
    /// will still be communicated.
    ///
    /// This message does not require an acknowledgment.
    ///
    /// [Voluntary Communication of Status of Messages]: https://core.telegram.org/mtproto/service_messages_about_messages#voluntary-communication-of-status-of-messages
    fn handle_msg_all(
        &mut self,
        _message: manual_tl::MessageView<'_>,
    ) -> Result<(), DeserializeError> {
        // TODO implement
        Ok(())
    }

    /// **[Extended Voluntary Communication of Status of One Message]**
    ///
    /// Normally used by the server to respond to the receipt of a duplicate
    /// `msg_id`, especially if a response to the message has already been
    /// generated and the response is large. If the response is small, the
    /// server may re-send the answer itself instead. This message can also
    /// be used as a notification instead of resending a large message.
    ///
    /// ```tl
    /// msg_detailed_info#276d3ec6 msg_id:long answer_msg_id:long bytes:int status:int = MsgDetailedInfo;
    /// msg_new_detailed_info#809db6df answer_msg_id:long bytes:int status:int = MsgDetailedInfo;
    /// ```
    ///
    /// The second version is used to notify of messages that were created on
    /// the server not in response to an RPC query (such as notifications of
    /// new messages) and were transmitted to the client some time ago, but
    /// not acknowledged.
    ///
    /// Currently, `status` is always zero. This may change in future.
    ///
    /// This message does not require an acknowledgment.
    ///
    /// [Extended Voluntary Communication of Status of One Message]: https://core.telegram.org/mtproto/service_messages_about_messages#extended-voluntary-communication-of-status-of-one-message
    fn handle_detailed_info(
        &mut self,
        message: manual_tl::MessageView<'_>,
    ) -> Result<(), DeserializeError> {
        // TODO https://github.com/telegramdesktop/tdesktop/blob/8f82880b938e06b7a2a27685ef9301edb12b4648/Telegram/SourceFiles/mtproto/connection.cpp#L1790-L1820
        // TODO https://github.com/telegramdesktop/tdesktop/blob/8f82880b938e06b7a2a27685ef9301edb12b4648/Telegram/SourceFiles/mtproto/connection.cpp#L1822-L1845
        let msg_detailed = tl::enums::MsgDetailedInfo::from_bytes(&message.body)?;
        match msg_detailed {
            tl::enums::MsgDetailedInfo::Info(x) => {
                self.queue_ack(x.answer_msg_id)?;
            }
            tl::enums::MsgDetailedInfo::MsgNewDetailedInfo(x) => {
                self.queue_ack(x.answer_msg_id)?;
            }
        }
        Ok(())
    }

    /// **[Explicit Request to Re-Send Messages]**
    ///
    /// ```tl
    /// msg_resend_req#7d861a08 msg_ids:Vector long = MsgResendReq;
    /// ```
    ///
    /// The remote party immediately responds by re-sending the requested
    /// messages, normally using the same connection that was used to transmit
    /// the query. If at least one message with requested msg_id does not
    /// exist or has already been forgotten, or has been sent by the
    /// requesting party (known from parity), `MsgsStateInfo` is returned for
    /// all messages requested as if the `MsgResendReq` query had been a
    /// `MsgsStateReq` query as well.
    ///
    /// **[Explicit Request to Re-Send Answers]**
    ///
    /// ```tl
    /// msg_resend_ans_req#8610baeb msg_ids:Vector long = MsgResendReq;
    /// ```
    ///
    /// The remote party immediately responds by re-sending answers to the
    /// requested messages, normally using the same connection that was used
    /// to transmit the query. `MsgsStateInfo` is returned for all messages
    /// requested as if the `MsgResendReq` query had been a MsgsStateReq query
    /// as well.
    ///
    /// [Explicit Request to Re-Send Answers]: https://core.telegram.org/mtproto/service_messages_about_messages#explicit-request-to-re-send-answers
    /// [Explicit Request to Re-Send Messages]: https://core.telegram.org/mtproto/service_messages_about_messages#explicit-request-to-re-send-messages
    fn handle_msg_resend(
        &self,
        _message: manual_tl::MessageView<'_>,
    ) -> Result<(), DeserializeError> {
        // TODO implement
        // `msg_resend_ans_req` seems to never occur (it was even missing from `mtproto.tl`)
        Ok(())
    }

    /// **[Request for several future salts]**
    ///
    /// The client may at any time request from the server several (between
    /// 1 and 64) future server salts together with their validity periods.
    /// Having stored them in persistent memory, the client may use them to
    /// send messages in the future even if he changes sessions (a server
    /// salt is attached to the Authorization Key rather than being
    /// session-specific).
    ///
    /// ```tl
    /// get_future_salts#b921bd04 num:int = FutureSalts;
    /// future_salt#0949d9dc valid_since:int valid_until:int salt:long = FutureSalt;
    /// future_salts#ae500895 req_msg_id:long now:int salts:vector future_salt = FutureSalts;
    /// ```
    ///
    /// The client must check to see that the response's `req_msg_id` in fact
    /// coincides with `msg_id` of the query for `get_future_salts`. The
    /// server returns a maximum of `num` future server salts (may return
    /// fewer). The response serves as the acknowledgment of the query and
    /// does not require an acknowledgment itself.
    ///
    /// [Request for several future salts]: https://core.telegram.org/mtproto/service_messages#request-for-several-future-salts
    fn handle_future_salts(
        &mut self,
        message: manual_tl::MessageView<'_>,
        apply_service_state: bool,
    ) -> Result<(), DeserializeError> {
        let tl::enums::FutureSalts::Salts(salts) =
            tl::enums::FutureSalts::from_bytes(&message.body)?;
        if salts.salts.0.len() > NUM_FUTURE_SALTS as usize
            || salts.salts.0.iter().any(|s| s.valid_since >= s.valid_until)
        {
            return Err(DeserializeError::InvalidEnvelope {
                reason: "invalid future salts",
            });
        }

        let internal = self.salt_request_msg_id == Some(MsgId(salts.req_msg_id));
        if internal {
            // Response to internal request, do not propagate.
            self.salt_request_msg_id = None;
        } else {
            self.deserialization
                .push(Deserialization::RpcResult(RpcResult {
                    msg_id: MsgId(salts.req_msg_id),
                    body: message.body.into_owned(),
                }));
        }

        if !apply_service_state || salts.salts.0.is_empty() {
            // Empty or stale answers cannot replace current service state.
            // Retire the request but bound repeated ineffective prefetches.
            if internal {
                self.salt_request_failures = self.salt_request_failures.saturating_add(1);
            }
            return Ok(());
        }
        self.start_salt_time = Some((salts.now, Instant::now()));
        // New salts are examined by the next request, whenever the last look was.
        self.salt_checked_secs = 0;
        self.salt_request_failures = 0;
        self.salts = salts.salts.0;
        self.salts
            .sort_by_key(|salt| std::cmp::Reverse(salt.valid_since));
        info!("got {} future salts", self.salts.len());

        Ok(())
    }

    fn handle_future_salt(
        &mut self,
        _message: manual_tl::MessageView<'_>,
    ) -> Result<(), DeserializeError> {
        Err(DeserializeError::InvalidEnvelope {
            reason: "unsolicited future_salt",
        })
    }

    /// **[Ping Messages (PING/PONG)]**
    ///
    /// ```tl
    /// ping#7abe77ec ping_id:long = Pong;
    /// ```
    ///
    /// A response is usually returned to the same connection:
    ///
    /// ```tl
    /// pong#347773c5 msg_id:long ping_id:long = Pong;
    /// ```
    ///
    /// These messages do not require acknowledgments. A `pong` is
    /// transmitted only in response to a `ping` while a `ping` can be
    /// initiated by either side.
    ///
    /// **[Deferred Connection Closure + PING]**
    ///
    /// ```tl
    /// ping_delay_disconnect#f3427b8c ping_id:long disconnect_delay:int = Pong;
    /// ```
    ///
    /// Works like `ping`. In addition, after this is received, the server
    /// starts a timer which will close the current connection
    /// `disconnect_delay` seconds later unless it receives a new message of
    /// the same type which automatically resets all previous timers. If the
    /// client sends these pings once every 60 seconds, for example, it may
    /// set `disconnect_delay` equal to 75 seconds.
    ///
    /// [Ping Messages (PING/PONG)]: https://core.telegram.org/mtproto/service_messages#ping-messages-ping-pong
    /// [Deferred Connection Closure + PING]: https://core.telegram.org/mtproto/service_messages#deferred-connection-closure-ping
    fn handle_pong(&mut self, message: manual_tl::MessageView<'_>) -> Result<(), DeserializeError> {
        let tl::enums::Pong::Pong(pong) = tl::enums::Pong::from_bytes(&message.body)?;

        self.deserialization
            .push(Deserialization::RpcResult(RpcResult {
                msg_id: MsgId(pong.msg_id),
                body: message.body.into_owned(),
            }));
        Ok(())
    }

    /// **[Request to Destroy Session]**
    ///
    /// Used by the client to notify the server that it may forget the data
    /// from a different session belonging to the same user (i.e. with the
    /// same `auth_key_id`). The result of this being applied to the current
    /// session is undefined.
    ///
    /// ```tl
    /// destroy_session#e7512126 session_id:long = DestroySessionRes;
    /// destroy_session_ok#e22045fc session_id:long = DestroySessionRes;
    /// destroy_session_none#62d350c9 session_id:long = DestroySessionRes;
    /// ```
    ///
    /// [Request to Destroy Session]: https://core.telegram.org/mtproto/service_messages#request-to-destroy-session
    fn handle_destroy_session(
        &self,
        _message: manual_tl::MessageView<'_>,
    ) -> Result<(), DeserializeError> {
        // TODO implement
        Ok(())
    }

    /// **[New Session Creation Notification]**
    ///
    /// The server notifies the client that a new session (from the server's
    /// standpoint) had to be created to handle a client message. If, after
    /// this, the server receives a message with an even smaller `msg_id`
    /// within the same session, a similar notification will be generated for
    /// this `msg_id` as well. No such notifications are generated for high
    /// `msg_id` values.
    ///
    /// ```tl
    /// new_session_created#9ec20908 first_msg_id:long unique_id:long server_salt:long = NewSession
    /// ```
    ///
    /// The `unique_id` parameter is generated by the server every time a
    /// session is (re-)created.
    ///
    /// This notification must be acknowledged by the client. It is necessary,
    /// for instance, for the client to understand that there is, in fact, a
    /// "gap" in the stream of long poll notifications received from the
    /// server (the user may have failed to receive notifications during some
    /// period of time).
    ///
    /// Notice that the server may unilaterally destroy (close) any existing
    /// client sessions with all pending messages and notifications, without
    /// sending any notifications. This happens, for example, if the session
    /// is inactive for a long time, and the server runs out of memory. If the
    /// client at some point decides to send new messages to the server using
    /// the old session, already forgotten by the server, such a "new session
    /// created" notification will be generated. The client is expected to
    /// handle such situations gracefully.
    ///
    /// [New Session Creation Notification]: https://core.telegram.org/mtproto/service_messages#new-session-creation-notification
    fn handle_new_session_created(
        &mut self,
        message: manual_tl::MessageView<'_>,
    ) -> Result<(), DeserializeError> {
        // TODO notify upper layers about the need to use getDifference
        let new_session = tl::enums::NewSession::from_bytes(&message.body)?;
        match new_session {
            tl::enums::NewSession::Created(x) => {
                self.salts.clear();
                self.salts.push(tl::types::FutureSalt {
                    valid_since: 0,
                    valid_until: i32::MAX,
                    salt: x.server_salt,
                });
            }
        }
        Ok(())
    }

    /// **[Containers]**
    ///
    /// *Containers* are messages containing several other messages. Used
    /// for the ability to transmit several RPC queries and/or service
    /// messages at the same time, using HTTP or even TCP or UDP protocol.
    /// A container may only be accepted or rejected by the other party as
    /// a whole.
    ///
    /// **[Simple Container]**
    ///
    /// A simple container carries several messages as follows:
    ///
    /// ```tl
    /// msg_container#73f1f8dc messages:vector message = MessageContainer;
    /// ```
    ///
    /// Here message refers to any message together with its length and
    /// `msg_id`:
    ///
    /// ```tl
    /// message msg_id:long seqno:int bytes:int body:Object = Message;
    /// ```
    ///
    /// `bytes` is the number of bytes in the body serialization.
    ///
    /// All messages in a container must have `msg_id` lower than that of the
    /// container itself. A container does not require an acknowledgment and
    /// may not carry other simple containers. When messages are re-sent,
    /// they may be combined into a container in a different manner or sent
    /// individually.
    ///
    /// Empty containers are also allowed. They are used by the server,
    /// for example, to respond to an HTTP request when the timeout specified
    /// in `http_wait` expires, and there are no messages to transmit.
    ///
    /// [Containers]: https://core.telegram.org/mtproto/service_messages#containers
    /// [Simple Container]: https://core.telegram.org/mtproto/service_messages#simple-container
    fn handle_container(
        &mut self,
        message: manual_tl::MessageView<'_>,
        budget: &mut DecodeBudget,
        depth: usize,
    ) -> Result<(), DeserializeError> {
        let parent = budget
            .authenticated
            .then_some((message.msg_id, message.seq_no));
        // Prevalidate all envelopes once before any service-message side effect.
        let messages =
            manual_tl::MessageContainer::borrowed_messages_validated(&message.body, parent)?;
        if budget.authenticated {
            // Keep the floor and cache membership stable throughout a packet.
            budget.pending_received.get_or_insert_with(Vec::new);
        }
        for inner_message in messages {
            self.process_bounded(inner_message?, budget, depth + 1, true)?;
        }

        Ok(())
    }

    /// **[Message Copies]**
    ///
    /// In some situations, an old message with a `msg_id` that is no longer
    /// valid needs to be re-sent. Then, it is wrapped in a copy container:
    ///
    /// ```tl
    /// msg_copy#e06046b2 orig_message:Message = MessageCopy;
    /// ```
    ///
    /// Once received, the message is processed as if the wrapper were not
    /// there. However, if it is known for certain that the message
    /// `orig_message.msg_id` was received, then the new message is not
    /// processed (while at the same time, it and `orig_message.msg_id`
    /// are acknowledged). The value of `orig_message.msg_id` must be lower
    /// than the container's `msg_id`.
    ///
    /// This is not used at this time, because an old message can be wrapped
    /// in a simple container with the same result.
    ///
    /// [Message Copies]: https://core.telegram.org/mtproto/service_messages#message-copies
    fn handle_msg_copy(
        &self,
        _message: manual_tl::MessageView<'_>,
    ) -> Result<(), DeserializeError> {
        Err(DeserializeError::InvalidEnvelope {
            reason: "unsupported msg_copy",
        })
    }

    /// **[HTTP Wait/Long Poll]**
    ///
    /// The following special service query not requiring an acknowledgement
    /// (which must be transmitted only through an HTTP connection) is used to
    /// enable the server to send messages in the future to the client using
    /// HTTP protocol:
    ///
    /// ```tl
    /// http_wait#9299359f max_delay:int wait_after:int max_wait:int = HttpWait;
    /// ```
    ///
    /// When such a message (or a container carrying such a message) is
    /// received, the server either waits `max_delay` milliseconds, whereupon
    /// it forwards all the messages that it is holding on to the client if
    /// there is at least one message queued in session (if needed, by placing
    /// them into a container to which acknowledgments may also be added); or
    /// else waits no more than `max_wait` milliseconds until such a message
    /// is available. If a message never appears, an empty container is
    /// transmitted.
    ///
    /// The `max_delay` parameter denotes the maximum number of milliseconds
    /// that has elapsed between the first message for this session and the
    /// transmission of an HTTP response. The wait_after parameter works as
    /// follows: after the receipt of the latest message for a particular
    /// session, the server waits another wait_after milliseconds in case
    /// there are more messages. If there are no additional messages, the
    /// result is transmitted (a container with all the messages). If more
    /// messages appear, the `wait_after` timer is reset.
    ///
    /// At the same time, the max_delay parameter has higher priority than
    /// `wait_after`, and `max_wait` has higher priority than `max_delay`.
    ///
    /// This message does not require a response or an acknowledgement. If
    /// the container transmitted over HTTP carries several such messages,
    /// the behavior is undefined (in fact, the latest parameter will be
    /// used).
    ///
    /// If no `http_wait` is present in container, default values
    /// `max_delay=0` (milliseconds), `wait_after=0` (milliseconds), and
    /// `max_wait=25000` (milliseconds) are used.
    ///
    /// If the client's ping of the server takes a long time, it may make
    /// sense to set `max_delay` to a value that is comparable in magnitude
    /// to ping time.
    ///
    /// [HTTP Wait/Long Poll]: https://core.telegram.org/mtproto/service_messages#http-wait-long-poll
    fn handle_http_wait(
        &mut self,
        _message: manual_tl::MessageView<'_>,
    ) -> Result<(), DeserializeError> {
        // TODO implement
        Ok(())
    }

    /// Anything else that's not Service Message will be `Updates`.
    ///
    /// Since we handle all the possible service messages, we can
    /// safely treat whatever message body we received as `Updates`.
    fn handle_update(
        &mut self,
        message: manual_tl::MessageView<'_>,
    ) -> Result<(), DeserializeError> {
        if !self.receive_updates {
            return Ok(());
        }
        // TODO if this `Updates` cannot be deserialized, `getDifference` should be used
        self.deserialization
            .push(Deserialization::Update(message.body.into_owned()));
        Ok(())
    }
}

/// The maximum header length used by any available transport.
///
/// The largest transport is `Full`, needing 4 bytes for each of `len` and `seq`.
///
/// The `crc32` is appended at the end, so it is not part of the leading header.
pub const MAX_TRANSPORT_HEADER_LEN: usize = 4 + 4;

/// Amount of bytes used by the [Encrypted Message](https://core.telegram.org/mtproto/description#encrypted-message).
///
/// This accounts for both the `auth_key_id` (8 bytes) and `msg_key` (16 bytes).
pub const ENCRYPTED_PACKET_HEADER_LEN: usize = 8 + 16;

/// Amount of bytes used by the [`encrypted_data`](https://core.telegram.org/mtproto/description#encrypted-message-encrypted-data).
///
/// This accounts for both `salt` (8 bytes) and `session_id` (8 bytes).
///
/// The `message_id`, `seq_no` and `message_data_length` are not included
/// because those will have been appended at the current buffer position.
pub const PLAIN_PACKET_HEADER_LEN: usize = 8 + 8;

/// Amount of bytes used by a `msg_container`.
///
/// The message header for the container occupies the size of the message header
/// (`msg_id`, `seq_no` and `size`) followed by the container header (`constructor`, `len`).
pub const MESSAGE_CONTAINER_HEADER_LEN: usize = (8 + 4 + 4) + (4 + 4);

impl Mtp for Encrypted {
    fn request_is_valid(&self, request: &[u8]) -> bool {
        Self::request_fits(request, self.receive_updates)
    }

    fn service_deadline(&self) -> Option<std::time::Instant> {
        self.timer_driven
            .store(true, std::sync::atomic::Ordering::Relaxed);
        self.pending_ack_since.map(|since| since + ACK_MAX_DELAY)
    }

    fn on_service_timeout(&mut self) {
        self.ack_flush_due = !self.pending_ack.is_empty();
    }

    fn on_packet_sent(&mut self, at: std::time::Instant) {
        self.last_sent = Some(at);
    }
    /// Pushes a request into the internal buffer.
    ///
    /// Multiple requests will be buffered and sent as a single container to reduce roundtrips.
    /// If the buffer is full (request does not fit in the container), returns `None`.
    ///
    /// [MTProto 2.0 guidelines]: https://core.telegram.org/mtproto/description.
    fn push(&mut self, buffer: &mut DequeBuffer<u8>, request: &[u8]) -> Option<MsgId> {
        // Invalid requests must not consume ACKs, salt state or message IDs.
        if !Self::request_fits(request, self.receive_updates) {
            return None;
        }
        // TODO rather than taking in bytes, take requests, serialize them in place, and if too large drop the last part of the buffer

        // Check to see if the next salt can be used already. If it can, drop the current one and,
        // if the next salt is the last one, fetch more.
        // The time since the salts arrived is measured on the monotonic clock, but
        // only once the system clock (as the last packet read it, for its message
        // ID) has moved since the previous look, in either direction.
        if let Some((start_secs, start_instant)) =
            self.start_salt_time.filter(|_| self.salts.len() > 1)
            && self.clock_secs.abs_diff(self.salt_checked_secs) >= SALT_CHECK_INTERVAL
        {
            self.salt_checked_secs = self.clock_secs;
            let salt = &self.salts[self.salts.len() - 2];
            let now = i64::from(start_secs)
                + start_instant.elapsed().as_secs().min(i32::MAX as u64) as i64;
            if now >= i64::from(salt.valid_since) + i64::from(SALT_USE_DELAY) {
                self.salts.pop();
            }
        }

        let requesting_salt = self.salt_request_msg_id.is_some();
        self.try_request_salts(buffer);
        if !requesting_salt && self.salt_request_msg_id.is_some() {
            // Keep the salt request in its own packet so a bad-message response
            // identifies it. Later user calls need not wait for its response.
            return None;
        }

        // ACKs normally go first. If that prevents a user request from fitting,
        // give the next attempt priority so incoming traffic cannot starve it.
        // Finalize can still append ACKs if they fit.
        let previous_msg_count = self.msg_count;
        if !self.request_deferred_by_ack && self.ack_due() {
            self.serialize_pending_ack(buffer);
        }
        let wrote_ack = self.msg_count != previous_msg_count;

        // Serialize `MAXIMUM_LENGTH` requests at most.
        if self.msg_count == manual_tl::MessageContainer::MAXIMUM_LENGTH {
            self.request_deferred_by_ack |= wrote_ack;
            return None;
        }

        let wrapper =
            tl::functions::InvokeWithoutUpdates::<tl::functions::help::GetConfig>::CONSTRUCTOR_ID
                .to_le_bytes();
        let mut prefix: &[u8] =
            &wrapper[..Self::without_updates_prefix_len(request, self.receive_updates)];

        // Payload provided by the user is always considered to be
        // content-related, which means we can apply compression.
        let mut body = request;
        let compressed;
        if self
            .compression_threshold
            .is_some_and(|threshold| prefix.len() + request.len() >= threshold)
        {
            compressed = manual_tl::GzipPacked::new_prefixed(prefix, request).to_bytes();
            if compressed.len() < prefix.len() + request.len() {
                body = &compressed;
                prefix = &[];
            }
        }

        if !self.message_fits(buffer, prefix.len() + body.len()) {
            // No more messages fit in this container.
            self.request_deferred_by_ack |= wrote_ack;
            return None;
        }

        // This request still fits in the container, so give it a message ID.
        let msg_id = self.serialize_msg_header(buffer, prefix.len() + body.len(), true);
        buffer.extend(prefix);
        buffer.extend(body);
        self.request_deferred_by_ack = false;
        Some(msg_id)
    }

    fn finalize(&mut self, buffer: &mut DequeBuffer<u8>) -> Option<MsgId> {
        let salt_only =
            self.msg_count == 1 && self.salt_request_msg_id == Some(MsgId(self.last_msg_id));
        if !salt_only
            && self.ack_due()
            && (self.ack_flush_due
                || !buffer.is_empty()
                || self.pending_ack.len() >= 8192
                || (self.pending_ack_since.is_some() && self.ack_delay_passed()))
        {
            self.serialize_pending_ack(buffer);
        }
        self.finalize_plain(buffer);
        if buffer.is_empty() {
            None
        } else {
            encrypt_data_v2(buffer, &self.auth_key);
            Some(MsgId(self.last_msg_id))
        }
    }

    /// Processes an encrypted response from the server.
    fn deserialize(
        &mut self,
        payload: &mut [u8],
    ) -> Result<Vec<Deserialization>, DeserializeError> {
        crate::utils::check_message_buffer(payload)?;

        let plaintext = decrypt_data_v2(payload, &self.auth_key)?;

        self.process_plaintext(plaintext)
    }

    fn recycle_deserialization(&mut self, mut results: Vec<Deserialization>) {
        results.clear();
        // Keep ordinary small-packet capacity, not a rare giant container's
        // allocation on every long-lived connection.
        if results.capacity() <= 64 && results.capacity() > self.deserialization.capacity() {
            self.deserialization = results;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    // salt + session_id
    const MESSAGE_PREFIX_LEN: usize = 8 + 8;

    // gzip_packed#3072cfa1 packed_data:string = Object;
    const GZIP_PACKED_HEADER: [u8; 4] = [0xa1, 0xcf, 0x72, 0x30];

    // msg_container#73f1f8dc messages:vector<message> = MessageContainer;
    const MSG_CONTAINER_HEADER: [u8; 4] = [0xdc, 0xf8, 0xf1, 0x73];

    const REQUEST: &[u8] = b"Hey!";
    const REQUEST_B: &[u8] = b"Bye!";

    fn auth_key() -> [u8; 256] {
        [0; 256]
    }

    fn plaintext(session: i64, id: i64, padding: usize) -> Vec<u8> {
        plaintext_body(session, id, 1, test_rpc_body(4, &true.to_bytes()), padding)
    }

    fn plaintext_body(
        session: i64,
        id: i64,
        seq_no: i32,
        body: Vec<u8>,
        padding: usize,
    ) -> Vec<u8> {
        let mut data = 0i64.to_bytes();
        session.serialize(&mut data);
        manual_tl::Message {
            msg_id: id,
            seq_no,
            body,
        }
        .serialize(&mut data);
        data.resize(data.len() + padding, 0);
        data
    }

    #[test]
    fn checked_builder_reports_rng_failure_without_panicking() {
        assert!(matches!(
            Encrypted::build().finish_with_rng(auth_key(), |_| Err(())),
            Err(())
        ));
        let mtp = Encrypted::build()
            .finish_with_rng(auth_key(), |bytes| {
                bytes.fill(7);
                Ok::<(), ()>(())
            })
            .unwrap();
        assert_eq!(mtp.session_id, i64::from_le_bytes([7; 8]));
    }

    #[test]
    fn authenticated_envelope_checks_and_replay_suppression() {
        let mut mtp = Encrypted::build().finish(auth_key());
        let session = mtp.session_id;
        for bytes in [
            plaintext(session.wrapping_add(1), 3, 16),
            plaintext(session, 2, 16),
            plaintext(session, -3, 16),
            plaintext(session, 3, 4),
            plaintext(session, 3, 1028),
        ] {
            assert!(mtp.process_plaintext(&bytes).is_err());
        }
        let first = plaintext(session, 3, 16);
        assert_eq!(mtp.process_plaintext(&first).unwrap().len(), 1);
        assert!(mtp.process_plaintext(&first).unwrap().is_empty());
        assert_eq!(mtp.pending_ack, [3, 3]);
        for index in 1..1100 {
            assert_eq!(
                mtp.process_plaintext(&plaintext(session, index * 4 + 3, 16))
                    .unwrap()
                    .len(),
                1
            );
        }
        assert_eq!(mtp.received_ids.len(), 1024);
        assert_eq!(mtp.received_ids.capacity(), 1024);
        let ack_count = mtp.pending_ack.len();
        for id in [3, 5] {
            let results = mtp.process_plaintext(&plaintext(session, id, 16)).unwrap();
            assert!(matches!(
                results.as_slice(),
                [Deserialization::RpcResult(_)]
            ));
        }
        assert_eq!(mtp.pending_ack.len(), ack_count + 2);
        let retained_id = mtp.received_ids.values()[0];
        assert!(
            mtp.process_plaintext(&plaintext(session, retained_id, 16))
                .unwrap()
                .is_empty()
        );
        assert_eq!(mtp.pending_ack.last(), Some(&retained_id));
    }

    fn container_body(messages: Vec<manual_tl::Message>) -> Vec<u8> {
        let mut body = manual_tl::MessageContainer::CONSTRUCTOR_ID.to_bytes();
        (messages.len() as i32).serialize(&mut body);
        for message in messages {
            message.serialize(&mut body);
        }
        body
    }

    #[test]
    fn authenticated_out_of_order_and_inner_replays_deliver_only_once() {
        let mut mtp = Encrypted::build().finish(auth_key());
        let session = mtp.session_id;
        for id in [11, 3, 7] {
            assert_eq!(
                mtp.process_plaintext(&plaintext(session, id, 16))
                    .unwrap()
                    .len(),
                1
            );
        }
        assert_eq!(mtp.received_ids.values(), [3, 7, 11]);
        let body = container_body(
            [11, 15, 15, 7]
                .into_iter()
                .map(|msg_id| manual_tl::Message {
                    msg_id,
                    seq_no: 1,
                    body: test_rpc_body(msg_id * 4, &true.to_bytes()),
                })
                .collect(),
        );
        let results = mtp
            .process_plaintext(&plaintext_body(session, 19, 2, body, 16))
            .unwrap();
        assert!(matches!(
            results.as_slice(),
            [Deserialization::RpcResult(r)] if r.msg_id == MsgId(60)
        ));
        assert_eq!(mtp.received_ids.values(), [3, 7, 11, 15, 19]);
        assert_eq!(mtp.pending_ack, [11, 3, 7, 11, 15, 15, 7]);
        assert!(
            mtp.process_plaintext(&plaintext(session, 15, 16))
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn authenticated_inner_reply_before_cache_minimum_is_not_assumed_stale() {
        let mut mtp = Encrypted::build().finish(auth_key());
        let session = mtp.session_id;
        for index in 0..1024 {
            mtp.process_plaintext(&plaintext(session, index * 4 + 7, 16))
                .unwrap();
        }
        let ack_count = mtp.pending_ack.len();
        let body = container_body(vec![manual_tl::Message {
            msg_id: 3,
            seq_no: 1,
            body: test_rpc_body(4, &true.to_bytes()),
        }]);
        assert_eq!(mtp.received_floor, 0);
        let results = mtp
            .process_plaintext(&plaintext_body(session, 4103, 2, body, 16))
            .unwrap();
        assert!(matches!(
            results.as_slice(),
            [Deserialization::RpcResult(_)]
        ));
        assert_eq!(mtp.pending_ack.len(), ack_count + 1);
        assert!(mtp.received_ids.contains(&4103));
        assert!(mtp.deserialization.is_empty());
    }

    fn replay_updates() -> Vec<u8> {
        tl::enums::Updates::Updates(tl::types::Updates {
            updates: vec![],
            users: vec![],
            chats: vec![],
            date: 100,
            seq: 1,
        })
        .to_bytes()
    }

    fn fill_replay_cache(mtp: &mut Encrypted, count: i64) {
        for index in 0..count {
            mtp.process_plaintext(&plaintext(mtp.session_id, index * 4 + 11, 16))
                .unwrap();
        }
    }

    #[test]
    fn age_evicted_ids_stay_stale_while_later_messages_remain_usable() {
        const SECOND: i64 = 1 << 32;
        let mut mtp = Encrypted::build().finish(auth_key());
        let session = mtp.session_id;
        let first = 0x6500_0000 * SECOND + 3;
        let update = replay_updates();
        let plain_update = |session, msg_id| plaintext_body(session, msg_id, 1, update.clone(), 16);
        assert_eq!(
            mtp.process_plaintext(&plain_update(session, first))
                .unwrap()
                .len(),
            1
        );
        // A single newer server ID ages the first out of the retained window.
        let later = first + 301 * SECOND;
        assert_eq!(
            mtp.process_plaintext(&plain_update(session, later))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(mtp.received_ids.values(), [later]);
        assert_eq!(mtp.received_floor, first);
        // Replaying it is suppressed although it is no longer stored, but a delayed
        // reply below the floor still completes its pending caller.
        assert!(
            mtp.process_plaintext(&plain_update(session, first))
                .unwrap()
                .is_empty()
        );
        assert!(matches!(
            mtp.process_plaintext(&plaintext(session, first, 16))
                .unwrap()
                .as_slice(),
            [Deserialization::RpcResult(_)]
        ));
        assert_eq!(
            mtp.process_plaintext(&plain_update(session, later + SECOND))
                .unwrap()
                .len(),
            1
        );
    }

    #[test]
    fn container_replay_classification_is_independent_of_sibling_order() {
        for older_id in [9, 11] {
            for reverse in [false, true] {
                let mut mtp = Encrypted::build().finish(auth_key());
                fill_replay_cache(&mut mtp, 1024);
                assert_eq!(mtp.received_floor, 0);
                let mut ids = [5003, older_id];
                if reverse {
                    ids.reverse();
                }
                let update = replay_updates();
                let body = container_body(
                    ids.into_iter()
                        .map(|msg_id| manual_tl::Message {
                            msg_id,
                            seq_no: 1,
                            body: test_rpc_body(msg_id * 4, &update),
                        })
                        .collect(),
                );
                let packet = plaintext_body(mtp.session_id, 6003, 2, body, 16);
                let results = mtp.process_plaintext(&packet).unwrap();
                let expected = if older_id == 9 { 2 } else { 1 };
                assert_eq!(
                    results
                        .iter()
                        .filter(|r| matches!(r, Deserialization::OwnUpdate { .. }))
                        .count(),
                    expected
                );
                assert_eq!(
                    results
                        .iter()
                        .filter(|r| matches!(r, Deserialization::RpcResult(_)))
                        .count(),
                    expected
                );
                assert_eq!(mtp.received_floor, 15);
                assert_eq!(mtp.received_ids.len(), 1024);
            }
        }
    }

    #[test]
    fn stale_rpc_siblings_complete_without_replaying_own_updates() {
        for stale_error in [false, true] {
            for reverse in [false, true] {
                let mut mtp = Encrypted::build().first_salt(999).finish(auth_key());
                fill_replay_cache(&mut mtp, 1026);
                assert_eq!(mtp.received_floor, 15);
                let update = replay_updates();
                let error = tl::enums::RpcError::Error(tl::types::RpcError {
                    error_code: 400,
                    error_message: "STALE_TEST".into(),
                })
                .to_bytes();
                let stale_payload = if stale_error { &error } else { &update };
                let stale_rpc =
                    test_rpc_body(8, &manual_tl::GzipPacked::new(stale_payload).to_bytes());
                let mut children = vec![
                    manual_tl::Message {
                        msg_id: 5003,
                        seq_no: 1,
                        body: test_rpc_body(4, &update),
                    },
                    manual_tl::Message {
                        msg_id: 9,
                        seq_no: 1,
                        body: manual_tl::GzipPacked::new(&stale_rpc).to_bytes(),
                    },
                ];
                if reverse {
                    children.reverse();
                }
                let packet = plaintext_body(mtp.session_id, 6003, 2, container_body(children), 16);
                let results = mtp.process_plaintext(&packet).unwrap();
                assert_eq!(results.len(), 3);
                assert!(results.iter().any(
                    |r| matches!(r, Deserialization::OwnUpdate { msg_id, update: body }
                    if *msg_id == MsgId(4) && *body == update)
                ));
                assert!(!results.iter().any(|r| matches!(r, Deserialization::OwnUpdate { msg_id, .. } if *msg_id == MsgId(8))));
                assert!(results.iter().any(|r| matches!(r, Deserialization::RpcResult(r) if r.msg_id == MsgId(4) && r.body == update)));
                if stale_error {
                    assert!(results.iter().any(|r| matches!(r, Deserialization::RpcError(r) if r.msg_id == MsgId(8) && r.error.error_code == 400)));
                } else {
                    assert!(results.iter().any(|r| matches!(r, Deserialization::RpcResult(r) if r.msg_id == MsgId(8) && r.body == update)));
                }
                let push = plaintext_body(mtp.session_id, 5, 1, update.clone(), 16);
                assert!(mtp.process_plaintext(&push).unwrap().is_empty());
                let salt = tl::enums::BadMsgNotification::BadServerSalt(tl::types::BadServerSalt {
                    bad_msg_id: 4,
                    bad_msg_seqno: 1,
                    error_code: 48,
                    new_server_salt: 888,
                })
                .to_bytes();
                let packet = plaintext_body(mtp.session_id, 7, 1, salt, 16);
                assert!(matches!(mtp.process_plaintext(&packet).unwrap().as_slice(),
                    [Deserialization::BadMessage(bad)] if bad.msg_id == MsgId(4) && bad.code == 48));
                assert_eq!(mtp.get_current_salt(), 999);
                let body = container_body(vec![manual_tl::Message {
                    msg_id: 9,
                    seq_no: 1,
                    body: test_rpc_body(12, &update),
                }]);
                let packet = plaintext_body(mtp.session_id, 13, 2, body, 16);
                let results = mtp.process_plaintext(&packet).unwrap();
                assert!(
                    matches!(results.as_slice(), [Deserialization::RpcResult(r)] if r.msg_id == MsgId(12) && r.body == update)
                );
                assert!(mtp.pending_ack.ends_with(&[5, 7, 9]));
            }
        }
    }

    #[test]
    fn maximum_inbound_container_preserves_all_results_and_bounds_replay_cache() {
        let mut mtp = Encrypted::build().finish(auth_key());
        let body = container_body(
            (0..1024)
                .map(|index| manual_tl::Message {
                    msg_id: index * 4 + 3,
                    seq_no: 1,
                    body: test_rpc_body(index * 4, &true.to_bytes()),
                })
                .collect(),
        );
        let packet = plaintext_body(mtp.session_id, 4099, 2, body, 16);
        let results = mtp.process_plaintext(&packet).unwrap();
        assert_eq!(results.len(), 1024);
        for (index, result) in results.iter().enumerate() {
            assert!(
                matches!(result, Deserialization::RpcResult(r) if r.msg_id == MsgId(index as i64 * 4))
            );
        }
        assert_eq!(mtp.pending_ack.len(), 1024);
        assert_eq!(mtp.received_ids.len(), 1024);
        assert_eq!(mtp.received_ids.capacity(), 1024);
        assert_eq!(mtp.received_floor, 3);
        assert!(mtp.received_ids.contains(&4099));
    }

    #[test]
    fn failed_container_does_not_commit_partial_replay_history() {
        let mut mtp = Encrypted::build().finish(auth_key());
        fill_replay_cache(&mut mtp, 1024);
        let before = mtp.received_ids.clone();
        let body = container_body(vec![
            manual_tl::Message {
                msg_id: 5003,
                seq_no: 1,
                body: test_rpc_body(4, &true.to_bytes()),
            },
            manual_tl::Message {
                msg_id: 5007,
                seq_no: 1,
                body: manual_tl::MessageCopy::CONSTRUCTOR_ID.to_bytes(),
            },
        ]);
        let packet = plaintext_body(mtp.session_id, 6003, 2, body, 16);
        assert!(mtp.process_plaintext(&packet).is_err());
        assert!(mtp.deserialization.is_empty());
        assert_eq!(mtp.received_ids, before);
        assert_eq!(mtp.received_floor, 0);
    }

    #[test]
    fn seeded_authenticated_packet_mutations_are_bounded_and_panic_free() {
        // Deterministic property corpus, including valid packets; no external fuzzer or I/O.
        let mut seed = 0x0d1ff3e7u64;
        for trial in 0..4096 {
            let mut mtp = Encrypted::build().finish(auth_key());
            let mut bytes = plaintext(mtp.session_id, 7, 16);
            seed = seed.wrapping_mul(6364136223846793005).wrapping_add(1);
            match trial % 4 {
                0 => {}
                1 => bytes.truncate(seed as usize % bytes.len()),
                _ => {
                    let at = seed as usize % bytes.len();
                    bytes[at] ^= (seed >> 32) as u8;
                }
            }
            if let Ok(messages) = mtp.process_plaintext(&bytes) {
                assert!(messages.len() <= 1);
                assert!(mtp.received_ids.len() <= 1);
                assert!(mtp.pending_ack.len() <= 1);
            }
        }
    }

    #[test]
    fn fast_clock_bad_message_resets_monotonic_floor() {
        let mut mtp = Encrypted::build().time_offset(3600).finish(auth_key());
        let old = mtp.get_new_msg_id();
        let server_id = ((SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64)
            << 32)
            | 1;
        let body = tl::enums::BadMsgNotification::Notification(tl::types::BadMsgNotification {
            bad_msg_id: old,
            bad_msg_seqno: 1,
            error_code: 17,
        })
        .to_bytes();
        mtp.process_message(manual_tl::MessageView {
            msg_id: server_id,
            seq_no: 0,
            body: Cow::Owned(body),
        })
        .unwrap();
        let corrected = mtp.get_new_msg_id();
        assert!(corrected < old);
        assert_eq!(corrected >> 32, server_id >> 32);
        mtp.correct_time_offset(i64::MAX);
        assert!(mtp.get_new_msg_id() > 0);
    }

    #[test]
    fn time_offset_jitter_preserves_monotonic_message_ids() {
        let mut mtp = Encrypted::build().finish(auth_key());
        let mut previous = mtp.get_new_msg_id();
        let seconds = previous >> 32;
        for delta in [-1, 0, -2, 1, -1, 0] {
            mtp.correct_time_offset(((seconds + delta) << 32) | 1);
            assert_eq!(mtp.last_msg_id, previous);
            let next = mtp.get_new_msg_id();
            assert!(next > previous);
            previous = next;
        }
    }

    #[test]
    fn high_sequence_rejection_preserves_valid_sibling_results() {
        for sequence in 0..16 {
            for reverse in [false, true] {
                let mut mtp = Encrypted::build().first_salt(123).finish(auth_key());
                mtp.sequence = sequence;
                let rejection =
                    tl::enums::BadMsgNotification::Notification(tl::types::BadMsgNotification {
                        bad_msg_id: 4,
                        bad_msg_seqno: sequence,
                        error_code: 33,
                    })
                    .to_bytes();
                let mut children = vec![
                    manual_tl::Message {
                        msg_id: 3,
                        seq_no: 0,
                        body: rejection,
                    },
                    manual_tl::Message {
                        msg_id: 7,
                        seq_no: 0,
                        body: test_rpc_body(8, &true.to_bytes()),
                    },
                ];
                if reverse {
                    children.reverse();
                }
                let packet = plaintext_body(mtp.session_id, 9, 0, container_body(children), 16);
                let results = mtp.process_plaintext(&packet).unwrap();
                assert_eq!(mtp.sequence, 0);
                assert_eq!(results.len(), 2);
                assert!(results.iter().any(|r| matches!(r, Deserialization::BadMessage(b) if b.msg_id == MsgId(4) && b.code == 33)));
                assert!(results.iter().any(|r| matches!(r, Deserialization::RpcResult(r) if r.msg_id == MsgId(8) && r.body == true.to_bytes())));
            }
        }
    }

    #[test]
    fn invalid_push_preserves_services_and_salt_packet_excludes_ack() {
        let mut mtp = Encrypted::build().first_salt(123).finish(auth_key());
        mtp.pending_ack.push(7);
        mtp.pending_ack_since = Some(Instant::now());
        let mut buffer = DequeBuffer::with_capacity(0, 0);
        assert!(mtp.push(&mut buffer, &[1]).is_none());
        assert!(buffer.is_empty());
        assert!(mtp.salt_request_msg_id.is_none());
        assert_eq!(mtp.pending_ack, [7]);
        assert!(mtp.push(&mut buffer, REQUEST).is_none());
        let salt_id = mtp.salt_request_msg_id;
        assert_eq!(mtp.finalize(&mut buffer), salt_id);
        assert_eq!(mtp.pending_ack, [7]);
    }

    /// The header fields of each message in a finalized, unencrypted packet.
    fn packet_message_ids(buffer: &[u8], count: usize) -> Vec<i64> {
        let id = |at: usize| i64::from_le_bytes(buffer[at..at + 8].try_into().unwrap());
        let len = |at: usize| i32::from_le_bytes(buffer[at..at + 4].try_into().unwrap()) as usize;
        if count == 1 {
            return vec![id(MESSAGE_PREFIX_LEN)];
        }
        // Salt and session, then the container's own header and constructor.
        let mut ids = vec![id(MESSAGE_PREFIX_LEN)];
        let mut at = MESSAGE_PREFIX_LEN + 16 + 8;
        for _ in 0..count {
            ids.push(id(at));
            at += 16 + len(at + 12);
        }
        assert_eq!(at, buffer.len());
        ids
    }

    #[test]
    fn one_packet_reads_the_clock_once_for_all_its_message_ids() {
        let mut mtp = Encrypted::build().finish(auth_key());
        let mut buffer = DequeBuffer::with_capacity(1024, 64);
        mtp.queue_ack(5).unwrap();
        // Acknowledgements, two requests and their container: four identifiers.
        let first = mtp.push(&mut buffer, REQUEST).unwrap();
        let second = mtp.push(&mut buffer, REQUEST_B).unwrap();
        mtp.finalize_plain(&mut buffer);
        let ids = packet_message_ids(&buffer[..], 3);
        let ack = ids[1];
        assert_eq!(ids, [ack + 12, ack, first.0, second.0]);
        // Each follows the one before by the smallest step the protocol allows,
        // which separate clock reads a few hundred nanoseconds apart never gave.
        assert_eq!([first.0, second.0], [ack + 4, ack + 8]);
        assert_eq!(mtp.last_msg_id, ack + 12);
        let wall = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        assert!((wall - (ack >> 32)).abs() <= 1, "the first is the time");
        assert_eq!(mtp.clock_secs, ack >> 32);

        // A packet of the same tick of the clock still takes a later identifier.
        buffer.clear();
        let same_tick = mtp.push(&mut buffer, REQUEST).unwrap();
        mtp.finalize_plain(&mut buffer);
        assert!(same_tick.0 > ack + 12);

        // The next packet reads the clock again instead of counting on from the last.
        // That reading is as of the kernel's last tick, at most 10 ms before.
        buffer.clear();
        std::thread::sleep(std::time::Duration::from_millis(25));
        let later = mtp.push(&mut buffer, REQUEST).unwrap();
        mtp.finalize_plain(&mut buffer);
        assert_eq!(packet_message_ids(&buffer[..], 1), [later.0]);
        assert!(
            later.0 - same_tick.0 > 1_000_000 << 2,
            "a later packet must carry its own time"
        );

        // A corrected clock discards the floor, and with it any counting on.
        buffer.clear();
        mtp.time_offset = -1000;
        mtp.last_msg_id = 0;
        let corrected = mtp.push(&mut buffer, REQUEST).unwrap();
        let sibling = mtp.push(&mut buffer, REQUEST_B).unwrap();
        assert!(corrected.0 < later.0);
        assert_eq!(sibling.0, corrected.0 + 4);
    }

    #[test]
    fn the_next_salt_is_examined_when_the_clock_has_moved() {
        let salts = |now: i32| {
            // Sorted as the session keeps them: the one in use last.
            vec![
                tl::types::FutureSalt {
                    valid_since: now - SALT_USE_DELAY,
                    valid_until: now + 3600,
                    salt: 222,
                },
                tl::types::FutureSalt {
                    valid_since: now - 1800,
                    valid_until: now + 1800,
                    salt: 111,
                },
            ]
        };
        let mut mtp = Encrypted::build().finish(auth_key());
        let mut buffer = DequeBuffer::with_capacity(1024, 64);
        // Moving to the last salt asks for more in a packet of its own, so a push
        // may hold no request; either way it examined the salts and read the clock.
        let mut send = |mtp: &mut Encrypted| {
            mtp.salt_request_msg_id = None;
            mtp.push(&mut buffer, REQUEST);
            mtp.finalize_plain(&mut buffer);
            buffer.clear();
        };
        send(&mut mtp);
        let wall = mtp.clock_secs;
        assert_ne!(wall, 0);

        // Looked at a moment ago: a salt that became usable since then waits.
        mtp.salts = salts(1000);
        mtp.start_salt_time = Some((1000, Instant::now()));
        mtp.salt_checked_secs = wall;
        send(&mut mtp);
        assert_eq!(mtp.get_current_salt(), 111);
        // Until the clock has moved by the interval, forwards or backwards.
        for moved in [SALT_CHECK_INTERVAL as i64, -(SALT_CHECK_INTERVAL as i64)] {
            mtp.salts = salts(1000);
            mtp.salt_checked_secs = mtp.clock_secs - moved + moved.signum();
            send(&mut mtp);
            assert_eq!(mtp.get_current_salt(), 111, "moved {moved} less one");
            let looked_at = mtp.clock_secs;
            mtp.salt_checked_secs = looked_at - moved;
            send(&mut mtp);
            assert_eq!(mtp.get_current_salt(), 222, "moved {moved}");
            assert_eq!(mtp.salt_checked_secs, looked_at);
        }
        // A look that finds the next salt too young leaves it, and is not repeated
        // by the message after.
        mtp.salts = salts(1001);
        mtp.salt_checked_secs = 0;
        let looked_at = mtp.clock_secs;
        send(&mut mtp);
        assert_eq!(mtp.get_current_salt(), 111);
        assert_eq!(mtp.salt_checked_secs, looked_at);
    }

    #[test]
    fn newly_received_salts_are_examined_by_the_next_request() {
        let mut mtp = Encrypted::build().first_salt(123).finish(auth_key());
        let mut buffer = DequeBuffer::with_capacity(1024, 64);
        assert!(mtp.push(&mut buffer, REQUEST).is_none());
        let id = mtp.salt_request_msg_id.unwrap();
        mtp.finalize_plain(&mut buffer);
        buffer.clear();
        // As if the salts had been examined this very second.
        mtp.salt_checked_secs = mtp.clock_secs;
        // The server time `future_salts_body` reports.
        let now = 100;
        mtp.process_message(manual_tl::MessageView {
            msg_id: 9,
            seq_no: 1,
            body: Cow::Owned(future_salts_body(
                id.0,
                vec![
                    tl::types::FutureSalt {
                        valid_since: now - 1800,
                        valid_until: now + 1800,
                        salt: 111,
                    },
                    tl::types::FutureSalt {
                        valid_since: now - SALT_USE_DELAY,
                        valid_until: now + 3600,
                        salt: 222,
                    },
                ],
            )),
        })
        .unwrap();
        assert_eq!(mtp.salt_checked_secs, 0);
        assert_eq!(mtp.get_current_salt(), 111);
        mtp.push(&mut buffer, REQUEST);
        assert_eq!(mtp.get_current_salt(), 222);
    }

    #[test]
    fn failed_optional_salt_prefetch_keeps_requests_and_salt_recovery_working() {
        let mut mtp = Encrypted::build().first_salt(123).finish(auth_key());
        let mut buffer = DequeBuffer::with_capacity(1024, 64);
        for _ in 0..4 {
            assert!(mtp.push(&mut buffer, REQUEST).is_none());
            let id = mtp.salt_request_msg_id.unwrap();
            mtp.finalize_plain(&mut buffer);
            buffer.clear();
            mtp.process_message(manual_tl::MessageView {
                msg_id: 7,
                seq_no: 0,
                body: Cow::Owned(
                    tl::enums::BadMsgNotification::Notification(tl::types::BadMsgNotification {
                        bad_msg_id: id.0,
                        bad_msg_seqno: 0,
                        error_code: 64,
                    })
                    .to_bytes(),
                ),
            })
            .unwrap();
            assert!(mtp.deserialization.is_empty());
        }
        assert_eq!(mtp.get_current_salt(), 123);
        let id = mtp
            .push(&mut buffer, REQUEST)
            .expect("user RPC must still progress");
        assert!(mtp.salt_request_msg_id.is_none());
        mtp.process_message(manual_tl::MessageView {
            msg_id: 11,
            seq_no: 0,
            body: Cow::Owned(
                tl::enums::BadMsgNotification::BadServerSalt(tl::types::BadServerSalt {
                    bad_msg_id: id.0,
                    bad_msg_seqno: 0,
                    error_code: 48,
                    new_server_salt: 456,
                })
                .to_bytes(),
            ),
        })
        .unwrap();
        assert_eq!(mtp.get_current_salt(), 456);
        assert!(matches!(
            mtp.deserialization.as_slice(),
            [Deserialization::BadMessage(_)]
        ));
    }

    #[test]
    fn authenticated_container_envelopes_validate_before_delivery() {
        for (inner_id, inner_seq, accept) in [
            (3, 1, true),
            (3, 2, true),
            (3, 3, true),
            (7, 1, false),
            (2, 1, false),
            (-1, 1, false),
            (3, -1, false),
        ] {
            let mut mtp = Encrypted::build().finish(auth_key());
            let mut body = manual_tl::MessageContainer::CONSTRUCTOR_ID.to_bytes();
            1i32.serialize(&mut body);
            manual_tl::Message {
                msg_id: inner_id,
                seq_no: inner_seq,
                body: test_rpc_body(4, &true.to_bytes()),
            }
            .serialize(&mut body);
            let mut data = 0i64.to_bytes();
            mtp.session_id.serialize(&mut data);
            manual_tl::Message {
                msg_id: 7,
                seq_no: 2,
                body,
            }
            .serialize(&mut data);
            data.resize(data.len() + 16, 0);
            let result = mtp.process_plaintext(&data);
            if accept {
                assert_eq!(result.unwrap().len(), 1);
            } else {
                assert!(matches!(
                    result,
                    Err(DeserializeError::InvalidEnvelope { .. })
                ));
                assert!(mtp.pending_ack.is_empty());
            }
        }
    }

    #[test]
    fn salt_prefetch_does_not_block_user_requests_waiting_for_response() {
        let mut mtp = Encrypted::build().first_salt(123).finish(auth_key());
        let mut buffer = DequeBuffer::with_capacity(0, 0);
        assert!(mtp.push(&mut buffer, REQUEST).is_none());
        assert!(mtp.salt_request_msg_id.is_some());
        mtp.finalize(&mut buffer).unwrap();
        buffer.clear();
        assert!(mtp.push(&mut buffer, REQUEST).is_some());
    }

    #[test]
    fn service_messages_respect_packet_capacity_and_preserve_deferred_acks() {
        let mut mtp = Encrypted::build()
            .compression_threshold(None)
            .finish(auth_key());
        let mut buffer = DequeBuffer::with_capacity(0, 0);
        let request = vec![0; 1_044_372];
        assert!(mtp.push(&mut buffer, &request).is_some());
        for id in 0..9000 {
            mtp.queue_ack(id).unwrap();
        }
        let oldest = mtp.pending_ack_since;
        mtp.serialize_pending_ack(&mut buffer);
        assert_eq!(mtp.pending_ack.len(), 9000);
        assert_eq!(mtp.pending_ack_since, oldest);
        mtp.finalize(&mut buffer).unwrap();
        assert!(buffer.len() <= 0xff000);
        buffer.clear();
        mtp.serialize_pending_ack(&mut buffer);
        assert_eq!(mtp.pending_ack, (8192..9000).collect::<Vec<_>>());
        assert_eq!(mtp.pending_ack_since, oldest);
        mtp.finalize(&mut buffer).unwrap();
        assert!(buffer.len() <= 0xff000);
        assert!(mtp.pending_ack.is_empty());
    }

    #[test]
    fn idle_ack_only_packet_and_gzip_ack_once() {
        let mut mtp = Encrypted::build().finish(auth_key());
        let body = manual_tl::GzipPacked::new(&test_rpc_body(4, &true.to_bytes())).to_bytes();
        mtp.process_message(manual_tl::MessageView {
            msg_id: 7,
            seq_no: 1,
            body: Cow::Owned(body),
        })
        .unwrap();
        assert_eq!(mtp.pending_ack, [7]);
        mtp.pending_ack_since = Instant::now().checked_sub(ACK_MAX_DELAY);
        let mut buffer = DequeBuffer::with_capacity(0, 0);
        assert!(mtp.finalize(&mut buffer).is_some());
        assert!(mtp.pending_ack.is_empty());
    }

    #[test]
    fn a_driver_given_the_deadline_reports_it_instead_of_the_clock_being_read() {
        let mut mtp = Encrypted::build()
            .compression_threshold(None)
            .ack_batch(3)
            .finish(auth_key());
        let mut buffer = DequeBuffer::with_capacity(1024, 64);
        mtp.queue_ack(7).unwrap();
        let queued = mtp.pending_ack_since.unwrap();
        // The driver asks, as the sender does before waiting with nothing to write.
        assert_eq!(mtp.service_deadline(), Some(queued + ACK_MAX_DELAY));
        // From then on the delay is the driver's to report. An old acknowledgement
        // neither leaves alone nor joins a request below the batch on the clock.
        mtp.pending_ack_since = Instant::now().checked_sub(ACK_MAX_DELAY);
        assert!(mtp.finalize(&mut buffer).is_none());
        assert!(mtp.push(&mut buffer, REQUEST).is_some());
        mtp.finalize_plain(&mut buffer);
        assert_eq!(mtp.pending_ack, [7]);
        assert_eq!(packet_message_ids(&buffer[..], 1).len(), 1);
        buffer.clear();
        // The deadline it was given is unchanged by that, and reporting it works.
        assert!(mtp.service_deadline().unwrap() <= Instant::now());
        mtp.on_service_timeout();
        assert!(mtp.finalize(&mut buffer).is_some());
        assert!(mtp.pending_ack.is_empty());
        assert_eq!(mtp.service_deadline(), None);

        // A full batch still leaves with the next request without any report.
        buffer.clear();
        for id in [9, 11, 13] {
            mtp.queue_ack(id).unwrap();
        }
        assert!(mtp.push(&mut buffer, REQUEST).is_some());
        assert!(mtp.pending_ack.is_empty());
    }

    #[test]
    fn acknowledgements_are_timed_from_the_last_packet_the_driver_reported() {
        let mut mtp = Encrypted::build().finish(auth_key());
        // Without a report the clock is read, as before.
        let before = Instant::now();
        mtp.queue_ack(3).unwrap();
        assert!(mtp.pending_ack_since.unwrap() >= before);
        mtp.pending_ack.clear();

        // With one, the delay counts from the write: never later than the truth.
        let written = Instant::now() - std::time::Duration::from_secs(2);
        mtp.on_packet_sent(written);
        mtp.queue_ack(5).unwrap();
        assert_eq!(mtp.pending_ack_since, Some(written));
        assert_eq!(mtp.service_deadline(), Some(written + ACK_MAX_DELAY));
        // Later ones join the oldest, whose deadline is the one that counts.
        mtp.on_packet_sent(Instant::now());
        mtp.queue_ack(7).unwrap();
        assert_eq!(mtp.pending_ack_since, Some(written));

        // After a silence longer than the delay, the deadline has already passed:
        // the driver's timer fires at once and the acknowledgement leaves alone.
        mtp.pending_ack.clear();
        let long_ago = Instant::now() - ACK_MAX_DELAY * 3;
        mtp.on_packet_sent(long_ago);
        mtp.queue_ack(9).unwrap();
        assert!(mtp.service_deadline().unwrap() <= Instant::now());
        mtp.on_service_timeout();
        let mut buffer = DequeBuffer::with_capacity(0, 0);
        assert!(mtp.finalize(&mut buffer).is_some());
        assert!(mtp.pending_ack.is_empty());
    }

    #[test]
    fn explicit_service_deadline_flushes_small_batch_and_backlog_is_bounded() {
        let mut mtp = Encrypted::build().ack_batch(100).finish(auth_key());
        mtp.queue_ack(3).unwrap();
        let mut buffer = DequeBuffer::with_capacity(0, 0);
        assert!(mtp.finalize(&mut buffer).is_none());
        mtp.on_service_timeout();
        assert!(mtp.finalize(&mut buffer).is_some());
        assert!(mtp.pending_ack.is_empty());
        mtp.pending_ack.resize(65_536, 7);
        assert!(matches!(
            mtp.queue_ack(9),
            Err(DeserializeError::ResourceLimit { .. })
        ));
        assert_eq!(mtp.pending_ack.len(), 65_536);
    }

    #[test]
    fn gzip_depth_cumulative_budget_and_hidden_container_nesting() {
        let mut body = test_rpc_body(4, &true.to_bytes());
        for _ in 0..33 {
            body = manual_tl::GzipPacked::new(&body).to_bytes();
        }
        let mut mtp = Encrypted::build().finish(auth_key());
        assert!(matches!(
            mtp.process_message(manual_tl::MessageView {
                msg_id: 7,
                seq_no: 1,
                body: Cow::Owned(body),
            }),
            Err(DeserializeError::ResourceLimit { .. })
        ));
        let mut budget = DecodeBudget::new(DecodeLimits {
            gzip_output: 128,
            expanded_bytes: 192,
            ..Default::default()
        });
        let body = manual_tl::GzipPacked::new(&[0; 128]).to_bytes();
        assert_eq!(budget.decompress(&body, 1).unwrap().len(), 128);
        assert!(budget.decompress(&body, 1).is_err());
        assert_eq!(budget.remaining, 0);

        let mut container = manual_tl::MessageContainer::CONSTRUCTOR_ID.to_bytes();
        0i32.serialize(&mut container);
        let gzip = manual_tl::GzipPacked::new(&container).to_bytes();
        let mut outer = manual_tl::MessageContainer::CONSTRUCTOR_ID.to_bytes();
        1i32.serialize(&mut outer);
        manual_tl::Message {
            msg_id: 3,
            seq_no: 0,
            body: gzip,
        }
        .serialize(&mut outer);
        assert!(matches!(
            mtp.process_message(manual_tl::MessageView {
                msg_id: 7,
                seq_no: 0,
                body: Cow::Owned(outer),
            }),
            Err(DeserializeError::InvalidEnvelope { .. })
        ));
    }

    fn future_salts_body(req_msg_id: i64, salts: Vec<tl::types::FutureSalt>) -> Vec<u8> {
        tl::enums::FutureSalts::Salts(tl::types::FutureSalts {
            req_msg_id,
            now: 100,
            salts: tl::RawVec(salts),
        })
        .to_bytes()
    }

    #[test]
    fn stale_direct_pong_remains_an_rpc_completion() {
        for compressed in [false, true] {
            let mut mtp = Encrypted::build().finish(auth_key());
            fill_replay_cache(&mut mtp, 1026);
            let ack_count = mtp.pending_ack.len();
            let body = tl::enums::Pong::Pong(tl::types::Pong {
                msg_id: 12,
                ping_id: 34,
            })
            .to_bytes();
            let wire_body = if compressed {
                manual_tl::GzipPacked::new(&body).to_bytes()
            } else {
                body.clone()
            };
            let packet = plaintext_body(mtp.session_id, 9, 0, wire_body, 16);
            let results = mtp.process_plaintext(&packet).unwrap();
            assert!(
                matches!(results.as_slice(), [Deserialization::RpcResult(r)] if r.msg_id == MsgId(12) && r.body == body)
            );
            assert_eq!(mtp.pending_ack.len(), ack_count);
            assert_eq!(mtp.received_floor, 15);
            assert!(!mtp.received_ids.contains(&9));
        }
    }

    #[test]
    fn stale_external_future_salts_complete_without_changing_service_state() {
        for compressed in [false, true] {
            let mut mtp = Encrypted::build().first_salt(999).finish(auth_key());
            fill_replay_cache(&mut mtp, 1026);
            let start = (90, Instant::now());
            mtp.start_salt_time = Some(start);
            mtp.salt_request_msg_id = Some(MsgId(4));
            mtp.salt_request_failures = 3;
            let ack_count = mtp.pending_ack.len();
            let body = future_salts_body(
                12,
                vec![tl::types::FutureSalt {
                    valid_since: 100,
                    valid_until: 200,
                    salt: 777,
                }],
            );
            let wire_body = if compressed {
                manual_tl::GzipPacked::new(&body).to_bytes()
            } else {
                body.clone()
            };
            let packet = plaintext_body(mtp.session_id, 9, 0, wire_body, 16);
            let results = mtp.process_plaintext(&packet).unwrap();
            assert!(
                matches!(results.as_slice(), [Deserialization::RpcResult(r)] if r.msg_id == MsgId(12) && r.body == body)
            );
            assert_eq!(mtp.salt_request_msg_id, Some(MsgId(4)));
            assert_eq!(mtp.salt_request_failures, 3);
            assert_eq!(mtp.start_salt_time, Some(start));
            assert_eq!(mtp.salts.len(), 1);
            assert_eq!(mtp.get_current_salt(), 999);
            assert_eq!(mtp.pending_ack.len(), ack_count);
            assert_eq!(mtp.received_floor, 15);
        }
    }

    #[test]
    fn stale_internal_future_salts_retire_prefetch_without_installing_salts() {
        for empty in [false, true] {
            for failures in [0u8, 3, u8::MAX] {
                let mut mtp = Encrypted::build().first_salt(999).finish(auth_key());
                fill_replay_cache(&mut mtp, 1026);
                let start = (90, Instant::now());
                mtp.start_salt_time = Some(start);
                mtp.salt_request_msg_id = Some(MsgId(4));
                mtp.salt_request_failures = failures;
                let salts = if empty {
                    vec![]
                } else {
                    vec![tl::types::FutureSalt {
                        valid_since: 100,
                        valid_until: 200,
                        salt: 777,
                    }]
                };
                let packet = plaintext_body(mtp.session_id, 9, 0, future_salts_body(4, salts), 16);
                assert!(mtp.process_plaintext(&packet).unwrap().is_empty());
                assert_eq!(mtp.salt_request_msg_id, None);
                assert_eq!(mtp.salt_request_failures, failures.saturating_add(1));
                assert_eq!(mtp.start_salt_time, Some(start));
                assert_eq!(mtp.salts.len(), 1);
                assert_eq!(mtp.get_current_salt(), 999);
                if failures >= 3 {
                    let mut buffer = DequeBuffer::with_capacity(0, 0);
                    assert!(mtp.push(&mut buffer, REQUEST).is_some());
                    assert_eq!(mtp.salt_request_msg_id, None);
                }
            }
        }
    }

    #[test]
    fn stale_future_salts_are_validated_before_internal_retirement() {
        let mut mtp = Encrypted::build().first_salt(999).finish(auth_key());
        fill_replay_cache(&mut mtp, 1026);
        let start = (90, Instant::now());
        mtp.start_salt_time = Some(start);
        mtp.salt_request_msg_id = Some(MsgId(4));
        mtp.salt_request_failures = 3;
        let body = future_salts_body(
            4,
            vec![tl::types::FutureSalt {
                valid_since: 100,
                valid_until: 100,
                salt: 777,
            }],
        );
        let packet = plaintext_body(mtp.session_id, 9, 0, body, 16);
        assert!(matches!(
            mtp.process_plaintext(&packet),
            Err(DeserializeError::InvalidEnvelope {
                reason: "invalid future salts"
            })
        ));
        assert_eq!(mtp.salt_request_msg_id, Some(MsgId(4)));
        assert_eq!(mtp.salt_request_failures, 3);
        assert_eq!(mtp.start_salt_time, Some(start));
        assert_eq!(mtp.get_current_salt(), 999);
        assert!(mtp.deserialization.is_empty());
    }

    #[test]
    fn stale_rejections_preserve_service_state_and_retire_internal_requests() {
        for code in [16, 17, 32, 33, 48] {
            for internal in [false, true] {
                let mut mtp = Encrypted::build().first_salt(999).finish(auth_key());
                fill_replay_cache(&mut mtp, 1026);
                mtp.time_offset = 123;
                mtp.sequence = 42;
                mtp.last_msg_id = 1000;
                mtp.salt_request_msg_id = Some(MsgId(if internal { 4 } else { 8 }));
                let bad = if code == 48 {
                    tl::enums::BadMsgNotification::BadServerSalt(tl::types::BadServerSalt {
                        bad_msg_id: 4,
                        bad_msg_seqno: 1,
                        error_code: code,
                        new_server_salt: 777,
                    })
                } else {
                    tl::enums::BadMsgNotification::Notification(tl::types::BadMsgNotification {
                        bad_msg_id: 4,
                        bad_msg_seqno: 1,
                        error_code: code,
                    })
                };
                let packet = plaintext_body(mtp.session_id, 9, 0, bad.to_bytes(), 16);
                let results = mtp.process_plaintext(&packet).unwrap();
                if internal {
                    assert!(results.is_empty());
                    assert_eq!(mtp.salt_request_msg_id, None);
                    assert_eq!(mtp.salt_request_failures, 1);
                } else {
                    assert!(
                        matches!(results.as_slice(), [Deserialization::BadMessage(bad)] if bad.msg_id == MsgId(4) && bad.code == code)
                    );
                    assert_eq!(mtp.salt_request_msg_id, Some(MsgId(8)));
                }
                assert_eq!(mtp.time_offset, 123);
                assert_eq!(mtp.sequence, 42);
                assert_eq!(mtp.last_msg_id, 1000);
                assert_eq!(mtp.get_current_salt(), 999);
            }
        }
    }

    #[test]
    fn authenticated_future_salts_handles_internal_external_and_empty_replies() {
        for req_msg_id in [4, 8] {
            for count in [0, 3, NUM_FUTURE_SALTS] {
                let mut mtp = Encrypted::build().first_salt(999).finish(auth_key());
                mtp.salt_request_msg_id = Some(MsgId(4));
                mtp.salt_request_failures = 3;
                let before_start = (90, Instant::now());
                mtp.start_salt_time = Some(before_start);
                let body = future_salts_body(
                    req_msg_id,
                    (0..count)
                        .map(|index| tl::types::FutureSalt {
                            valid_since: index * 10,
                            valid_until: index * 10 + 1000,
                            salt: i64::from(index) + 1000,
                        })
                        .collect(),
                );
                let packet = plaintext_body(mtp.session_id, 7, 0, body.clone(), 16);
                let results = mtp.process_plaintext(&packet).unwrap();
                if req_msg_id == 4 {
                    assert!(results.is_empty());
                    assert_eq!(mtp.salt_request_msg_id, None);
                } else {
                    assert!(
                        matches!(results.as_slice(), [Deserialization::RpcResult(r)] if r.msg_id == MsgId(req_msg_id) && r.body == body)
                    );
                    assert_eq!(mtp.salt_request_msg_id, Some(MsgId(4)));
                }
                if count == 0 {
                    assert_eq!(
                        mtp.salt_request_failures,
                        if req_msg_id == 4 { 4 } else { 3 }
                    );
                    assert_eq!(mtp.start_salt_time, Some(before_start));
                    assert_eq!(mtp.salts.len(), 1);
                    assert_eq!(mtp.get_current_salt(), 999);
                } else {
                    assert_eq!(mtp.salt_request_failures, 0);
                    assert_eq!(mtp.start_salt_time.unwrap().0, 100);
                    assert_eq!(mtp.salts.len(), count as usize);
                    assert_eq!(
                        mtp.salts.iter().map(|s| s.valid_since).collect::<Vec<_>>(),
                        (0..count).rev().map(|index| index * 10).collect::<Vec<_>>()
                    );
                    assert_eq!(mtp.get_current_salt(), 1000);
                }
                assert!(mtp.pending_ack.is_empty());
            }
        }
    }

    #[test]
    fn wrapped_salt_prefetch_failures_are_bounded_without_phantom_results() {
        for kind in 0..7 {
            for compressed in [false, true] {
                let mut mtp = Encrypted::build().first_salt(999).finish(auth_key());
                let mut buffer = DequeBuffer::with_capacity(0, 0);
                for index in 0..4 {
                    assert!(mtp.push(&mut buffer, REQUEST).is_none());
                    let request_id = mtp.salt_request_msg_id.unwrap();
                    mtp.finalize_plain(&mut buffer);
                    buffer.clear();
                    let mut result = match kind {
                        0 => tl::enums::RpcError::Error(tl::types::RpcError {
                            error_code: 500,
                            error_message: "INTERNAL".into(),
                        })
                        .to_bytes(),
                        1 => true.to_bytes(),
                        2 => vec![],
                        3 => future_salts_body(request_id.0 + 4, vec![]),
                        4 => future_salts_body(request_id.0, vec![]),
                        5 => future_salts_body(
                            request_id.0,
                            vec![tl::types::FutureSalt {
                                valid_since: 100,
                                valid_until: 100,
                                salt: 456,
                            }],
                        ),
                        _ => GZIP_PACKED_HEADER.to_vec(),
                    };
                    if compressed {
                        result = manual_tl::GzipPacked::new(&result).to_bytes();
                    }
                    let packet = plaintext_body(
                        mtp.session_id,
                        index * 4 + 3,
                        0,
                        test_rpc_body(request_id.0, &result),
                        16,
                    );
                    assert!(
                        mtp.process_plaintext(&packet).unwrap().is_empty(),
                        "kind {kind}, compressed {compressed}"
                    );
                    assert_eq!(mtp.salt_request_msg_id, None);
                    assert_eq!(mtp.salt_request_failures, index as u8 + 1);
                    assert_eq!(mtp.get_current_salt(), 999);
                }
                assert!(mtp.push(&mut buffer, REQUEST).is_some());
                assert_eq!(mtp.salt_request_msg_id, None);
            }
        }
    }

    #[test]
    fn wrapped_salt_prefetch_success_and_stale_reply_follow_direct_semantics() {
        for stale in [false, true] {
            for compressed in [false, true] {
                let mut mtp = Encrypted::build().first_salt(999).finish(auth_key());
                if stale {
                    fill_replay_cache(&mut mtp, 1025);
                }
                mtp.salt_request_msg_id = Some(MsgId(4));
                let mut result = future_salts_body(
                    4,
                    vec![tl::types::FutureSalt {
                        valid_since: 0,
                        valid_until: i32::MAX,
                        salt: 456,
                    }],
                );
                if compressed {
                    result = manual_tl::GzipPacked::new(&result).to_bytes();
                }
                let packet = plaintext_body(mtp.session_id, 7, 0, test_rpc_body(4, &result), 16);
                assert!(mtp.process_plaintext(&packet).unwrap().is_empty());
                assert_eq!(mtp.salt_request_msg_id, None);
                assert_eq!(mtp.get_current_salt(), if stale { 999 } else { 456 });
                assert_eq!(mtp.salt_request_failures, u8::from(stale));
            }
        }
    }

    #[test]
    fn empty_salt_prefetch_replies_preserve_salt_and_eventually_allow_user_request() {
        let mut mtp = Encrypted::build().first_salt(999).finish(auth_key());
        let mut buffer = DequeBuffer::with_capacity(0, 0);
        for index in 0..4 {
            assert!(mtp.push(&mut buffer, REQUEST).is_none());
            let request_id = mtp.salt_request_msg_id.unwrap();
            mtp.finalize_plain(&mut buffer);
            buffer.clear();
            let body = future_salts_body(request_id.0, vec![]);
            let packet = plaintext_body(mtp.session_id, index * 4 + 3, 0, body, 16);
            assert!(mtp.process_plaintext(&packet).unwrap().is_empty());
            assert_eq!(mtp.get_current_salt(), 999);
        }
        assert_eq!(mtp.salt_request_failures, 4);
        assert!(mtp.push(&mut buffer, REQUEST).is_some());
        assert_eq!(mtp.salt_request_msg_id, None);
        assert_eq!(mtp.get_current_salt(), 999);
    }

    #[test]
    fn corrected_clock_resets_floor_only_beyond_three_seconds() {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        for ahead in [2, 3, 4] {
            let mut mtp = Encrypted::build().finish(auth_key());
            let previous = ((seconds + ahead) << 32) | 0xffff_fffc;
            mtp.last_msg_id = previous;
            mtp.correct_time_offset((seconds << 32) | 1);
            if ahead <= 3 {
                assert_eq!(mtp.last_msg_id, previous);
                assert!(mtp.get_new_msg_id() > previous);
            } else {
                assert_eq!(mtp.last_msg_id, 0);
                assert!(mtp.get_new_msg_id() < previous);
            }
        }
    }

    #[test]
    fn continuous_ack_demand_does_not_starve_maximum_user_requests() {
        for receive_updates in [false, true] {
            let mut mtp = Encrypted::build()
                .receive_updates(receive_updates)
                .compression_threshold(None)
                .finish(auth_key());
            let prefix = if receive_updates { 0 } else { 4 };
            let mut request = vec![
                0;
                manual_tl::MessageContainer::MAXIMUM_SIZE
                    - manual_tl::Message::SIZE_OVERHEAD
                    - prefix
            ];
            request[..4]
                .copy_from_slice(&tl::functions::help::GetConfig::CONSTRUCTOR_ID.to_le_bytes());
            let mut buffer = DequeBuffer::with_capacity(0, 0);
            for round in 0..3 {
                mtp.queue_ack(round * 4 + 7).unwrap();
                assert!(mtp.push(&mut buffer, &request).is_none());
                assert!(mtp.request_deferred_by_ack);
                mtp.finalize(&mut buffer).unwrap();
                buffer.clear();
                mtp.queue_ack(round * 4 + 9).unwrap();
                let deadline = mtp.pending_ack_since;
                assert!(mtp.push(&mut buffer, &request).is_some());
                assert!(!mtp.request_deferred_by_ack);
                assert_eq!(buffer.len(), manual_tl::MessageContainer::MAXIMUM_SIZE);
                mtp.finalize(&mut buffer).unwrap();
                assert_eq!(mtp.pending_ack, [round * 4 + 9]);
                assert_eq!(mtp.pending_ack_since, deadline);
                buffer.clear();
            }
            mtp.serialize_pending_ack(&mut buffer);
            assert!(mtp.pending_ack.is_empty());
        }
    }

    #[test]
    fn authenticated_invalid_future_salts_preserve_service_state() {
        let invalid = DeserializeError::InvalidEnvelope {
            reason: "invalid future salts",
        };
        let mut cases = vec![(
            future_salts_body(
                4,
                (0..=NUM_FUTURE_SALTS)
                    .map(|_| tl::types::FutureSalt {
                        valid_since: 0,
                        valid_until: 1000,
                        salt: 1,
                    })
                    .collect(),
            ),
            invalid.clone(),
        )];
        for valid_since in [100, 101] {
            cases.push((
                future_salts_body(
                    4,
                    vec![tl::types::FutureSalt {
                        valid_since,
                        valid_until: 100,
                        salt: 1,
                    }],
                ),
                invalid.clone(),
            ));
        }
        let mut truncated = future_salts_body(
            4,
            vec![tl::types::FutureSalt {
                valid_since: 0,
                valid_until: 1000,
                salt: 1,
            }],
        );
        truncated.truncate(truncated.len() - 4);
        cases.push((truncated, DeserializeError::MessageBufferTooSmall));
        let mut scalar = tl::types::FutureSalt::CONSTRUCTOR_ID.to_bytes();
        0i32.serialize(&mut scalar);
        1000i32.serialize(&mut scalar);
        1i64.serialize(&mut scalar);
        cases.push((
            scalar,
            DeserializeError::InvalidEnvelope {
                reason: "unsolicited future_salt",
            },
        ));
        for (body, expected) in cases {
            let mut mtp = Encrypted::build().first_salt(999).finish(auth_key());
            mtp.salt_request_msg_id = Some(MsgId(4));
            mtp.salt_request_failures = 2;
            let start = (90, Instant::now());
            mtp.start_salt_time = Some(start);
            let packet = plaintext_body(mtp.session_id, 7, 0, body, 16);
            assert_eq!(mtp.process_plaintext(&packet).err(), Some(expected));
            assert_eq!(mtp.salt_request_msg_id, Some(MsgId(4)));
            assert_eq!(mtp.salt_request_failures, 2);
            assert_eq!(mtp.start_salt_time, Some(start));
            assert_eq!(mtp.salts.len(), 1);
            assert_eq!(mtp.get_current_salt(), 999);
            assert!(mtp.deserialization.is_empty());
            assert!(mtp.received_ids.is_empty());
            assert!(mtp.pending_ack.is_empty());
        }
    }

    #[test]
    fn authenticated_sibling_rpc_gzip_shares_expansion_budget() {
        let payload = [0; 128];
        let packed = manual_tl::GzipPacked::new(&payload).to_bytes();
        for expanded_bytes in [192, 256] {
            let mut mtp = Encrypted::build()
                .decode_limits(DecodeLimits {
                    gzip_output: payload.len(),
                    expanded_bytes,
                    ..Default::default()
                })
                .finish(auth_key());
            let body = container_body(
                [(3, 4), (7, 8)]
                    .into_iter()
                    .map(|(msg_id, req_msg_id)| manual_tl::Message {
                        msg_id,
                        seq_no: 1,
                        body: test_rpc_body(req_msg_id, &packed),
                    })
                    .collect(),
            );
            let packet = plaintext_body(mtp.session_id, 11, 2, body, 16);
            let results = mtp.process_plaintext(&packet).unwrap();
            assert_eq!(results.len(), 2);
            assert!(
                matches!(&results[0], Deserialization::RpcResult(r) if r.msg_id == MsgId(4) && r.body == payload)
            );
            if expanded_bytes == 256 {
                assert!(
                    matches!(&results[1], Deserialization::RpcResult(r) if r.msg_id == MsgId(8) && r.body == payload)
                );
            } else {
                assert!(
                    matches!(&results[1], Deserialization::Failure(f) if f.msg_id == MsgId(8) && matches!(f.error, DeserializeError::ResourceLimit { .. }))
                );
            }
            assert_eq!(mtp.pending_ack, [3, 7]);
            let next = plaintext_body(mtp.session_id, 15, 1, test_rpc_body(12, &packed), 16);
            let results = mtp.process_plaintext(&next).unwrap();
            assert!(
                matches!(results.as_slice(), [Deserialization::RpcResult(r)] if r.msg_id == MsgId(12) && r.body == payload)
            );
        }
    }

    #[test]
    fn authenticated_failed_gzip_crc_charges_sibling_budget() {
        let mut bad = manual_tl::GzipPacked::new(&[0; 128]);
        let crc = bad.packed_data.len() - 8;
        bad.packed_data[crc] ^= 1;
        let good = manual_tl::GzipPacked::new(&[0; 128]).to_bytes();
        let mut mtp = Encrypted::build()
            .decode_limits(DecodeLimits {
                gzip_output: 128,
                expanded_bytes: 192,
                ..Default::default()
            })
            .finish(auth_key());
        let body = container_body(vec![
            manual_tl::Message {
                msg_id: 3,
                seq_no: 1,
                body: test_rpc_body(4, &bad.to_bytes()),
            },
            manual_tl::Message {
                msg_id: 7,
                seq_no: 1,
                body: test_rpc_body(8, &good),
            },
        ]);
        let packet = plaintext_body(mtp.session_id, 11, 2, body, 16);
        let results = mtp.process_plaintext(&packet).unwrap();
        assert!(
            matches!(results.as_slice(), [Deserialization::Failure(first), Deserialization::Failure(second)]
            if first.msg_id == MsgId(4) && first.error == DeserializeError::DecompressionFailed && second.msg_id == MsgId(8)
            && matches!(second.error, DeserializeError::ResourceLimit { .. }))
        );
        assert_eq!(mtp.pending_ack, [3, 7]);
    }

    #[test]
    fn authenticated_sibling_gzip_wrappers_share_expansion_budget() {
        for expanded_bytes in [24, 32] {
            let mut mtp = Encrypted::build()
                .decode_limits(DecodeLimits {
                    gzip_output: 16,
                    expanded_bytes,
                    ..Default::default()
                })
                .finish(auth_key());
            let body = container_body(
                [(3, 4), (7, 8)]
                    .into_iter()
                    .map(|(msg_id, req_msg_id)| manual_tl::Message {
                        msg_id,
                        seq_no: 1,
                        body: manual_tl::GzipPacked::new(&test_rpc_body(
                            req_msg_id,
                            &true.to_bytes(),
                        ))
                        .to_bytes(),
                    })
                    .collect(),
            );
            let packet = plaintext_body(mtp.session_id, 11, 2, body, 16);
            let result = mtp.process_plaintext(&packet);
            if expanded_bytes == 32 {
                assert_eq!(result.unwrap().len(), 2);
            } else {
                assert!(matches!(
                    result,
                    Err(DeserializeError::ResourceLimit { .. })
                ));
                assert!(mtp.deserialization.is_empty());
            }
        }
    }

    fn test_rpc_body(id: i64, result: &[u8]) -> Vec<u8> {
        let mut body = manual_tl::RpcResult::CONSTRUCTOR_ID.to_bytes();
        body.extend(id.to_bytes());
        body.extend_from_slice(result);
        body
    }

    #[test]
    fn regression_compressed_errors_and_short_replies_are_not_successes() {
        let error = tl::enums::RpcError::Error(tl::types::RpcError {
            error_code: 420,
            error_message: "FLOOD_WAIT_30".into(),
        })
        .to_bytes();
        for receive_updates in [true, false] {
            for payload in [&error[..], &[][..], &[1, 2, 3][..]] {
                let mut mtp = Encrypted::build()
                    .receive_updates(receive_updates)
                    .finish(auth_key());
                let body = test_rpc_body(42, &manual_tl::GzipPacked::new(payload).to_bytes());
                mtp.process_message(manual_tl::MessageView {
                    msg_id: 3,
                    seq_no: 1,
                    body: Cow::Borrowed(&body),
                })
                .unwrap();
                assert_eq!(mtp.deserialization.len(), 1);
                if payload == error {
                    assert!(
                        matches!(&mtp.deserialization[0], Deserialization::RpcError(e)
                        if e.msg_id == MsgId(42) && e.error.error_code == 420 && e.error.error_message == "FLOOD_WAIT_30")
                    );
                } else {
                    assert!(
                        matches!(&mtp.deserialization[0], Deserialization::Failure(e) if e.msg_id == MsgId(42))
                    );
                }
            }
        }
    }

    #[test]
    fn regression_drop_answer_replies_complete_their_own_request() {
        for payload in [
            tl::types::RpcAnswerUnknown::CONSTRUCTOR_ID.to_bytes(),
            tl::types::RpcAnswerDroppedRunning::CONSTRUCTOR_ID.to_bytes(),
            [
                tl::types::RpcAnswerDropped::CONSTRUCTOR_ID.to_bytes(),
                tl::types::RpcAnswerDropped {
                    msg_id: 7,
                    seq_no: 1,
                    bytes: 4,
                }
                .to_bytes(),
            ]
            .concat(),
        ] {
            let mut mtp = Encrypted::build().finish(auth_key());
            let body = test_rpc_body(42, &payload);
            mtp.process_message(manual_tl::MessageView {
                msg_id: 3,
                seq_no: 1,
                body: Cow::Borrowed(&body),
            })
            .unwrap();
            assert!(
                matches!(mtp.deserialization.as_slice(), [Deserialization::RpcResult(r)]
                if r.msg_id == MsgId(42) && r.body == payload)
            );
        }
    }

    #[test]
    fn borrowed_and_owned_rpc_results_survive_plain_and_gzip_paths() {
        let expected = true.to_bytes();
        let plain = test_rpc_body(42, &expected);
        let zipped = test_rpc_body(42, &manual_tl::GzipPacked::new(&expected).to_bytes());
        let outer_gzip = manual_tl::GzipPacked::new(&plain).to_bytes();
        for receive_updates in [true, false] {
            for wire in [&plain, &zipped, &outer_gzip] {
                for own in [false, true] {
                    let mut mtp = Encrypted::build()
                        .receive_updates(receive_updates)
                        .finish(auth_key());
                    let mut input = wire.to_vec();
                    let body = if own {
                        Cow::Owned(input.clone())
                    } else {
                        Cow::Borrowed(input.as_slice())
                    };
                    mtp.process_message(manual_tl::MessageView {
                        msg_id: 3,
                        seq_no: 1,
                        body,
                    })
                    .unwrap();
                    input.fill(0); // Replies must own their bytes before the receive buffer is reused.
                    assert!(mtp.pending_ack.contains(&3));
                    assert_eq!(mtp.deserialization.len(), 1);
                    let Deserialization::RpcResult(reply) = &mtp.deserialization[0] else {
                        panic!("missing reply");
                    };
                    assert_eq!(reply.msg_id, MsgId(42));
                    assert_eq!(reply.body, expected);
                }
            }
        }
    }

    #[test]
    fn malformed_container_has_no_partial_inner_effects() {
        let first = manual_tl::Message {
            msg_id: 3,
            seq_no: 1,
            body: test_rpc_body(42, &true.to_bytes()),
        }
        .to_bytes();
        let mut wire = manual_tl::MessageContainer::CONSTRUCTOR_ID.to_bytes();
        wire.extend(2i32.to_bytes());
        wire.extend(first);
        wire.extend([0; 16]); // Header claims a body below, which is absent.
        let end = wire.len();
        wire[end - 4..].copy_from_slice(&100i32.to_le_bytes());
        let mut mtp = Encrypted::build().finish(auth_key());
        assert!(
            mtp.process_message(manual_tl::MessageView {
                msg_id: 7,
                seq_no: 0,
                body: Cow::Borrowed(&wire)
            })
            .is_err()
        );
        assert!(mtp.pending_ack.is_empty());
        assert!(mtp.deserialization.is_empty());
    }

    #[test]
    fn disabled_updates_are_acknowledged_but_rpc_updates_are_returned() {
        let update = tl::types::UpdatesTooLong::CONSTRUCTOR_ID.to_bytes();
        for enabled in [true, false] {
            let mut mtp = Encrypted::build()
                .receive_updates(enabled)
                .finish(auth_key());
            mtp.process_message(manual_tl::MessageView {
                msg_id: 3,
                seq_no: 1,
                body: Cow::Borrowed(&update),
            })
            .unwrap();
            assert_eq!(mtp.pending_ack, [3]);
            assert_eq!(mtp.deserialization.len(), usize::from(enabled));
            mtp.deserialization.clear();
            let own_update = UPDATE_IDS[0].to_bytes();
            let rpc = test_rpc_body(42, &own_update);
            mtp.process_message(manual_tl::MessageView {
                msg_id: 5,
                seq_no: 1,
                body: Cow::Borrowed(&rpc),
            })
            .unwrap();
            assert_eq!(mtp.deserialization.len(), if enabled { 2 } else { 1 });
            if enabled {
                assert!(matches!(
                    mtp.deserialization[0],
                    Deserialization::OwnUpdate { .. }
                ));
            }
            let Some(Deserialization::RpcResult(reply)) = mtp.deserialization.last() else {
                panic!("RPC reply was dropped");
            };
            assert_eq!(reply.body, own_update);
        }
    }

    #[test]
    fn disabled_updates_keep_compressed_containers_errors_and_service_messages() {
        let mut mtp = Encrypted::build().receive_updates(false).finish(auth_key());
        let error = tl::enums::RpcError::Error(tl::types::RpcError {
            error_code: 420,
            error_message: "FLOOD_WAIT_3".into(),
        })
        .to_bytes();
        let bodies = [
            tl::types::UpdatesTooLong::CONSTRUCTOR_ID.to_bytes(),
            test_rpc_body(42, &true.to_bytes()),
            test_rpc_body(43, &error),
        ];
        let mut container = manual_tl::MessageContainer::CONSTRUCTOR_ID.to_bytes();
        container.extend((bodies.len() as i32).to_bytes());
        for (i, body) in bodies.into_iter().enumerate() {
            container.extend(
                manual_tl::Message {
                    msg_id: 3 + i as i64 * 2,
                    seq_no: 1,
                    body,
                }
                .to_bytes(),
            );
        }
        let zipped = manual_tl::GzipPacked::new(&container).to_bytes();
        mtp.process_message(manual_tl::MessageView {
            msg_id: 11,
            seq_no: 0,
            body: Cow::Borrowed(&zipped),
        })
        .unwrap();
        assert_eq!(mtp.pending_ack, [3, 5, 7]);
        assert_eq!(mtp.deserialization.len(), 2);
        assert!(
            matches!(&mtp.deserialization[0], Deserialization::RpcResult(r) if r.msg_id == MsgId(42) && r.body == true.to_bytes())
        );
        assert!(
            matches!(&mtp.deserialization[1], Deserialization::RpcError(r) if r.msg_id == MsgId(43) && r.error.error_code == 420)
        );
        mtp.deserialization.clear();
        let created = tl::enums::NewSession::Created(tl::types::NewSessionCreated {
            first_msg_id: 1,
            unique_id: 2,
            server_salt: 123,
        })
        .to_bytes();
        mtp.process_message(manual_tl::MessageView {
            msg_id: 13,
            seq_no: 1,
            body: Cow::Borrowed(&created),
        })
        .unwrap();
        assert_eq!(mtp.get_current_salt(), 123);
        let pong = tl::enums::Pong::Pong(tl::types::Pong {
            msg_id: 44,
            ping_id: 55,
        })
        .to_bytes();
        mtp.process_message(manual_tl::MessageView {
            msg_id: 15,
            seq_no: 0,
            body: Cow::Borrowed(&pong),
        })
        .unwrap();
        assert!(
            matches!(mtp.deserialization.last(), Some(Deserialization::RpcResult(r)) if r.msg_id == MsgId(44) && r.body == pong)
        );
    }

    #[test]
    fn no_updates_wraps_api_bytes_once_and_excludes_protocol_calls() {
        let query = tl::functions::users::GetUsers {
            id: vec![tl::enums::InputUser::UserSelf],
        };
        let request = query.to_bytes();
        let expected = tl::functions::InvokeWithoutUpdates { query }.to_bytes();
        for enabled in [true, false] {
            let mut mtp = Encrypted::build()
                .receive_updates(enabled)
                .finish(auth_key());
            let mut buffer = DequeBuffer::with_capacity(4096, 0);
            mtp.push(&mut buffer, &request).unwrap();
            assert_eq!(&buffer[16..], if enabled { &request } else { &expected });
        }
        let service = [
            tl::functions::Ping { ping_id: 1 }.to_bytes(),
            tl::functions::PingDelayDisconnect {
                ping_id: 1,
                disconnect_delay: 60,
            }
            .to_bytes(),
            tl::functions::GetFutureSalts { num: 1 }.to_bytes(),
            tl::functions::RpcDropAnswer { req_msg_id: 1 }.to_bytes(),
            tl::functions::DestroySession { session_id: 1 }.to_bytes(),
            expected,
        ];
        for body in service {
            let mut mtp = Encrypted::build().receive_updates(false).finish(auth_key());
            let mut buffer = DequeBuffer::with_capacity(4096, 0);
            mtp.push(&mut buffer, &body).unwrap();
            assert_eq!(&buffer[16..], &body);
        }
        let mut mtp = Encrypted::build()
            .receive_updates(false)
            .compression_threshold(Some(1))
            .finish(auth_key());
        let query = tl::functions::users::GetUsers {
            id: vec![tl::enums::InputUser::UserSelf; 200],
        };
        let request = query.to_bytes();
        let expected = tl::functions::InvokeWithoutUpdates { query }.to_bytes();
        let mut buffer = DequeBuffer::with_capacity(4096, 0);
        mtp.push(&mut buffer, &request).unwrap();
        assert_eq!(
            manual_tl::GzipPacked::decompress_from_bytes(&buffer[16..]).unwrap(),
            expected
        );
    }

    #[test]
    fn acknowledgements_match_typed_encoding_and_reuse_ids() {
        let mut mtp = Encrypted::build().finish(auth_key());
        let mut buffer = DequeBuffer::with_capacity(4096, 0);
        for ids in [vec![1], vec![3, 5, 7], (0..128).collect()] {
            mtp.pending_ack.extend_from_slice(&ids);
            let allocation = mtp.pending_ack.as_ptr();
            let capacity = mtp.pending_ack.capacity();
            let sequence = mtp.sequence;
            let expected = tl::enums::MsgsAck::Ack(tl::types::MsgsAck { msg_ids: ids }).to_bytes();
            buffer.clear();
            mtp.serialize_pending_ack(&mut buffer);
            assert_eq!(
                i32::from_le_bytes(buffer[8..12].try_into().unwrap()),
                sequence
            );
            assert_eq!(
                i32::from_le_bytes(buffer[12..16].try_into().unwrap()) as usize,
                expected.len()
            );
            assert_eq!(&buffer[16..], expected.as_slice());
            assert_eq!(mtp.pending_ack.as_ptr(), allocation);
            assert_eq!(mtp.pending_ack.capacity(), capacity);
            assert!(mtp.pending_ack.is_empty());
            assert_eq!(mtp.sequence, sequence);
            let len = buffer.len();
            mtp.serialize_pending_ack(&mut buffer);
            assert_eq!(buffer.len(), len);
        }
    }

    #[test]
    fn response_list_recycling_preserves_small_capacity_only() {
        let mut mtp = Encrypted::build().finish(auth_key());
        let mut results = Vec::with_capacity(4);
        results.push(Deserialization::RpcResult(RpcResult {
            msg_id: MsgId(1),
            body: vec![1; 4],
        }));
        let allocation = results.as_ptr();
        mtp.recycle_deserialization(results);
        assert!(mtp.deserialization.is_empty());
        assert_eq!(mtp.deserialization.as_ptr(), allocation);
        mtp.deserialization
            .push(Deserialization::RpcResult(RpcResult {
                msg_id: MsgId(2),
                body: vec![2; 4],
            }));
        let mut returned = mem::take(&mut mtp.deserialization);
        assert_eq!(returned.len(), 1);
        returned.clear();
        mtp.recycle_deserialization(returned);
        assert_eq!(mtp.deserialization.as_ptr(), allocation);
        mtp.recycle_deserialization(Vec::with_capacity(1024));
        assert_eq!(mtp.deserialization.as_ptr(), allocation);
    }

    #[test]
    #[ignore = "isolated ACK allocation benchmark; run release with nocapture"]
    fn benchmark_ack_serialization() {
        use std::hint::black_box;
        let mut mtp = Encrypted::build().finish(auth_key());
        let mut buffer = DequeBuffer::with_capacity(4096, 0);
        let mut samples = [Vec::new(), Vec::new()];
        for round in 0..8 {
            for reuse in [round % 2 == 0, round % 2 != 0] {
                let started = Instant::now();
                for i in 0..100_000 {
                    mtp.msg_count = 0;
                    mtp.pending_ack.push(black_box(i));
                    buffer.clear();
                    if reuse {
                        mtp.serialize_pending_ack(&mut buffer);
                    } else {
                        let body = tl::enums::MsgsAck::Ack(tl::types::MsgsAck {
                            msg_ids: mem::take(&mut mtp.pending_ack),
                        })
                        .to_bytes();
                        mtp.serialize_msg(&mut buffer, &body, false);
                    }
                    black_box(&buffer);
                }
                samples[usize::from(reuse)].push(started.elapsed().as_nanos() as f64 / 100_000.0);
            }
        }
        for values in &mut samples {
            values.sort_by(f64::total_cmp);
        }
        println!(
            "ACK median: allocate {:.1} ns, reuse {:.1} ns",
            samples[0][4], samples[1][4]
        );
    }

    /// Receive one content-related reply, then serialize one request after it.
    /// Returns whether the request went out inside a container (with acknowledgements).
    fn reply_then_push(mtp: &mut Encrypted, msg_id: i64) -> bool {
        let reply = test_rpc_body(42, &true.to_bytes());
        mtp.process_message(manual_tl::MessageView {
            msg_id,
            seq_no: 1,
            body: Cow::Borrowed(&reply),
        })
        .unwrap();
        mtp.deserialization.clear();
        let mut buffer = DequeBuffer::with_capacity(0, 0);
        assert!(mtp.push(&mut buffer, REQUEST).is_some());
        mtp.finalize_plain(&mut buffer);
        buffer[MESSAGE_PREFIX_LEN + 16..MESSAGE_PREFIX_LEN + 20] == MSG_CONTAINER_HEADER
    }

    #[test]
    fn acknowledgements_ride_on_every_message_by_default() {
        let mut mtp = Encrypted::build()
            .compression_threshold(None)
            .finish(auth_key());
        assert!(reply_then_push(&mut mtp, 3));
        assert!(mtp.pending_ack.is_empty());
        assert!(mtp.pending_ack_since.is_none());
    }

    #[test]
    fn batched_acknowledgements_wait_for_the_batch_or_the_delay() {
        let mut mtp = Encrypted::build()
            .compression_threshold(None)
            .ack_batch(3)
            .finish(auth_key());
        assert!(!reply_then_push(&mut mtp, 3));
        assert!(!reply_then_push(&mut mtp, 5));
        assert!(reply_then_push(&mut mtp, 7));
        assert!(mtp.pending_ack.is_empty());
        assert!(mtp.pending_ack_since.is_none());

        // A lone acknowledgement still leaves once it has waited long enough.
        assert!(!reply_then_push(&mut mtp, 9));
        assert_eq!(mtp.pending_ack, [9]);
        mtp.pending_ack_since = Instant::now().checked_sub(ACK_MAX_DELAY);
        let mut buffer = DequeBuffer::with_capacity(0, 0);
        assert!(mtp.push(&mut buffer, REQUEST).is_some());
        mtp.finalize_plain(&mut buffer);
        assert!(mtp.pending_ack.is_empty());
        assert_eq!(
            &buffer[MESSAGE_PREFIX_LEN + 16..MESSAGE_PREFIX_LEN + 20],
            MSG_CONTAINER_HEADER
        );

        mtp.set_ack_batch(0);
        assert!(reply_then_push(&mut mtp, 11), "0 behaves as 1");
    }

    fn ensure_buffer_is_message(buffer: &[u8], body: &[u8], seq_no: u8) {
        // buffer[0..8] is the msg_id, based on `SystemTime::now()`
        assert_ne!(&buffer[0..8], [0, 0, 0, 0, 0, 0, 0, 0]);
        // buffer[8..12] is the seq_no, ever-increasing odd number (little endian)
        assert_eq!(&buffer[8..12], [seq_no, 0, 0, 0]);
        // buffer[12..16] is the bytes, the len of the body (little endian)
        assert_eq!(&buffer[12..16], [body.len() as u8, 0, 0, 0]);
        // buffer[16..] is the body, which is padded to 4 bytes
        assert_eq!(&buffer[16..], body);
    }

    #[test]
    fn ensure_serialization_has_salt_session_id() {
        let mut buffer = DequeBuffer::with_capacity(0, 0);
        let mut mtproto = Encrypted::build().finish(auth_key());

        mtproto.push(&mut buffer, REQUEST);
        mtproto.finalize_plain(&mut buffer);

        // salt comes first, it's zero by default.
        assert_eq!(&buffer[0..8], [0, 0, 0, 0, 0, 0, 0, 0]);

        // session_id should be random.
        assert_ne!(&buffer[8..16], [0, 0, 0, 0, 0, 0, 0, 0]);

        // Only one message should remain.
        ensure_buffer_is_message(&buffer[MESSAGE_PREFIX_LEN..], REQUEST, 1);
    }

    #[test]
    fn ensure_correct_single_serialization() {
        let mut buffer = DequeBuffer::with_capacity(0, 0);
        let mut mtproto = Encrypted::build().finish(auth_key());

        assert!(mtproto.push(&mut buffer, REQUEST).is_some());
        mtproto.finalize_plain(&mut buffer);

        let buffer = &buffer[MESSAGE_PREFIX_LEN..];
        ensure_buffer_is_message(buffer, REQUEST, 1);
    }

    #[test]
    fn ensure_correct_multi_serialization() {
        let mut buffer = DequeBuffer::with_capacity(0, 0);
        let mut mtproto = Encrypted::build()
            .compression_threshold(None)
            .finish(auth_key());

        assert!(mtproto.push(&mut buffer, REQUEST).is_some());
        assert!(mtproto.push(&mut buffer, REQUEST_B).is_some());
        mtproto.finalize_plain(&mut buffer);
        let buffer = &buffer[MESSAGE_PREFIX_LEN..];

        // buffer[0..8] is the msg_id for the container
        assert_ne!(&buffer[0..8], [0, 0, 0, 0, 0, 0, 0, 0]);
        // buffer[8..12] is the seq_no, maybe-increasing even number.
        // after two messages (1, 3) the next non-content related is 4.
        assert_eq!(&buffer[8..12], [4, 0, 0, 0]);
        // buffer[12..16] is the bytes, the len of the body
        assert_eq!(&buffer[12..16], [48, 0, 0, 0]);

        // buffer[16..20] is the constructor id of the container
        assert_eq!(&buffer[16..20], MSG_CONTAINER_HEADER);
        // buffer[20..24] is how many messages are included
        assert_eq!(&buffer[20..24], [2, 0, 0, 0]);

        // buffer[24..44] is an inner message
        ensure_buffer_is_message(&buffer[24..44], REQUEST, 1);

        // buffer[44..] is the other inner message
        ensure_buffer_is_message(&buffer[44..], REQUEST_B, 3);
    }

    #[test]
    fn ensure_correct_single_large_serialization() {
        let mut buffer = DequeBuffer::with_capacity(0, 0);
        let mut mtproto = Encrypted::build()
            .compression_threshold(None)
            .finish(auth_key());
        let data = vec![0x7f; 768 * 1024];

        assert!(mtproto.push(&mut buffer, &data).is_some());
        mtproto.finalize_plain(&mut buffer);

        let buffer = &buffer[MESSAGE_PREFIX_LEN..];
        assert_eq!(buffer.len(), 16 + data.len());
    }

    #[test]
    fn maximum_request_fits_without_compression_or_indefinite_deferral() {
        for receive_updates in [false, true] {
            for constructor in [
                tl::functions::help::GetConfig::CONSTRUCTOR_ID,
                tl::functions::Ping::CONSTRUCTOR_ID,
                tl::functions::InvokeWithoutUpdates::<tl::functions::help::GetConfig>::CONSTRUCTOR_ID,
            ] {
                let wrapped = !receive_updates && constructor == tl::functions::help::GetConfig::CONSTRUCTOR_ID;
                let limit = 1_044_372 - if wrapped { 4 } else { 0 };
                let mut data = vec![0; limit];
                data[..4].copy_from_slice(&constructor.to_le_bytes());
                assert!(Encrypted::request_fits(&data, receive_updates));
                let mut mtproto = Encrypted::build()
                    .receive_updates(receive_updates)
                    .compression_threshold(None)
                    .finish(auth_key());
                let mut buffer = DequeBuffer::with_capacity(0, 0);
                assert!(mtproto.push(&mut buffer, &data).is_some());
                assert_eq!(buffer.len(), manual_tl::MessageContainer::MAXIMUM_SIZE);
                data.extend_from_slice(&[0; 4]);
                assert!(!Encrypted::request_fits(&data, receive_updates));
            }
        }
    }

    #[test]
    fn maximum_compressible_request_preserves_existing_input_limit() {
        for receive_updates in [false, true] {
            let limit = 1_044_372 - if receive_updates { 0 } else { 4 };
            let mut data = vec![0; limit];
            data[..4]
                .copy_from_slice(&tl::functions::help::GetConfig::CONSTRUCTOR_ID.to_le_bytes());
            let mut mtproto = Encrypted::build()
                .receive_updates(receive_updates)
                .finish(auth_key());
            let mut buffer = DequeBuffer::with_capacity(0, 0);
            assert!(mtproto.push(&mut buffer, &data).is_some());
            assert!(buffer.len() < data.len());
            data.extend_from_slice(&[0; 4]);
            assert!(!Encrypted::request_fits(&data, receive_updates));
        }
    }

    #[test]
    fn ensure_correct_multi_large_serialization() {
        let mut buffer = DequeBuffer::with_capacity(0, 0);
        let mut mtproto = Encrypted::build()
            .compression_threshold(None)
            .finish(auth_key());
        let data = vec![0x7f; 768 * 1024];

        assert!(mtproto.push(&mut buffer, &data).is_some());
        assert!(mtproto.push(&mut buffer, &data).is_none());

        // No container should be used, only the `salt` + `session_id` (16 bytes) should count.
        mtproto.finalize_plain(&mut buffer);
        let buffer = &buffer[MESSAGE_PREFIX_LEN..];
        assert_eq!(buffer.len(), 16 + data.len());
    }

    #[test]
    fn ensure_large_payload_is_rejected() {
        let mut buffer = DequeBuffer::with_capacity(0, 0);
        let mut mtproto = Encrypted::build().finish(auth_key());

        assert!(
            mtproto
                .push(&mut buffer, &vec![0; 2 * 1024 * 1024])
                .is_none()
        );
    }

    #[test]
    fn ensure_non_padded_payload_is_rejected() {
        let mut buffer = DequeBuffer::with_capacity(0, 0);
        let mut mtproto = Encrypted::build().finish(auth_key());

        assert!(mtproto.push(&mut buffer, &[1, 2, 3]).is_none());
    }

    #[test]
    fn ensure_no_compression_is_honored() {
        // A large vector of null bytes should compress
        let mut buffer = DequeBuffer::with_capacity(0, 0);
        let mut mtproto = Encrypted::build()
            .compression_threshold(None)
            .finish(auth_key());

        mtproto.push(&mut buffer, &vec![0; 512 * 1024]);
        mtproto.finalize_plain(&mut buffer);
        assert!(!buffer.as_ref().windows(4).any(|w| w == GZIP_PACKED_HEADER));
    }

    #[test]
    fn ensure_some_compression() {
        // A large vector of null bytes should compress
        {
            // High threshold not reached, should not compress
            let mut buffer = DequeBuffer::with_capacity(0, 0);
            let mut mtproto = Encrypted::build()
                .compression_threshold(Some(768 * 1024))
                .finish(auth_key());
            mtproto.push(&mut buffer, &vec![0; 512 * 1024]);
            mtproto.finalize_plain(&mut buffer);
            assert!(!buffer.as_ref().windows(4).any(|w| w == GZIP_PACKED_HEADER));
        }
        {
            // Low threshold is exceeded, should compress
            let mut buffer = DequeBuffer::with_capacity(0, 0);
            let mut mtproto = Encrypted::build()
                .compression_threshold(Some(256 * 1024))
                .finish(auth_key());
            mtproto.push(&mut buffer, &vec![0; 512 * 1024]);
            mtproto.finalize_plain(&mut buffer);
            assert!(buffer.as_ref().windows(4).any(|w| w == GZIP_PACKED_HEADER));
        }
        {
            // The default should compress
            let mut buffer = DequeBuffer::with_capacity(0, 0);
            let mut mtproto = Encrypted::build().finish(auth_key());
            mtproto.push(&mut buffer, &vec![0; 512 * 1024]);
            mtproto.finalize_plain(&mut buffer);
            assert!(buffer.as_ref().windows(4).any(|w| w == GZIP_PACKED_HEADER));
        }
    }
}
