// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::pin::Pin;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{Duration, SystemTime};
use std::{io, thread};

use bytes::Bytes;
use grammers_crypto::DequeBuffer;
use grammers_mtproto::mtp::{
    self, BadMessage, Deserialization, DeserializationFailure, Mtp, RpcResult, RpcResultError,
};
use grammers_mtproto::transport::{self, Transport};
use grammers_mtproto::{MsgId, authentication};
use grammers_session::updates::UpdatesLike;
use grammers_tl_types::{self as tl, Deserializable, RemoteCall};
use tl::{Cursor, Identifiable, Serializable};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;
use tokio::sync::oneshot::error::TryRecvError;
use tokio::time::{Instant, Sleep, sleep_until};
use tracing::{debug, error, info, trace, warn};

use crate::errors::{InvocationError, ReadError, RpcError};
use crate::net::{NetStream, ServerAddr};

/// The maximum data that we're willing to send or receive at once.
///
/// By having a fixed-size buffer, we can avoid unnecessary allocations
/// and trivially prevent allocating more than this limit if we ever
/// received invalid data.
///
/// Telegram will close the connection with roughly a megabyte of data,
/// so to account for the transports' own overhead, we add a few extra
/// kilobytes to the maximum data size.
const MAXIMUM_DATA: usize = (1024 * 1024) + (8 * 1024);
/// Initial socket buffers. Typical requests and replies fit; larger packets grow
/// them (receive up to [`MAXIMUM_DATA`]) and the grown capacity is kept.
const INITIAL_READ_BUFFER_SIZE: usize = 1024;
const INITIAL_WRITE_BUFFER_SIZE: usize = 1024;

/// How much leading space should be reserved in a buffer to avoid moving memory.
const LEADING_BUFFER_SPACE: usize = mtp::MAX_TRANSPORT_HEADER_LEN
    + mtp::ENCRYPTED_PACKET_HEADER_LEN
    + mtp::PLAIN_PACKET_HEADER_LEN
    + mtp::MESSAGE_CONTAINER_HEADER_LEN;

/// Every how often are pings sent?
const PING_DELAY: Duration = Duration::from_secs(60);

/// After how many seconds should the server close the connection when we send a ping?
///
/// What this value essentially means is that we have `NO_PING_DISCONNECT - PING_DELAY` seconds
/// to keep sending pings, or the server will close the connection.
///
/// Pings ensure the connection is kept active, and the delayed disconnect ensures the messages
/// are getting through consistently enough.
const NO_PING_DISCONNECT: i32 = 75;

/// Generate a "random" ping ID.
pub(crate) fn generate_random_id() -> i64 {
    static LAST_ID: AtomicI64 = AtomicI64::new(0);

    while LAST_ID.load(Ordering::SeqCst) == 0 {
        let now = SystemTime::now()
            .duration_since(SystemTime::UNIX_EPOCH)
            .expect("system time is before epoch")
            .as_nanos() as i64;

        if LAST_ID
            .compare_exchange(0, now, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            thread::yield_now();
        }
    }

    LAST_ID.fetch_add(1, Ordering::SeqCst)
}

fn deserialize_updates_like(update: Vec<u8>) -> tl::deserialize::Result<UpdatesLike> {
    match tl::enums::Updates::from_bytes(&update) {
        Ok(u) => Ok(UpdatesLike::Updates(u)),
        Err(e) => {
            if let Ok(tl::enums::messages::AffectedMessages::Messages(u)) =
                tl::enums::messages::AffectedMessages::from_bytes(&update)
            {
                Ok(UpdatesLike::AffectedMessages(u))
            } else if let Ok(tl::enums::messages::InvitedUsers::Users(u)) =
                tl::enums::messages::InvitedUsers::from_bytes(&update)
            {
                Ok(UpdatesLike::InvitedUsers(u))
            } else if let Ok(tl::enums::messages::ChatInviteJoinResult::Ok(u)) =
                tl::enums::messages::ChatInviteJoinResult::from_bytes(&update)
            {
                Ok(UpdatesLike::ChatInviteJoinResult(u))
            } else {
                Err(e)
            }
        }
    }
}

fn deserialize_request<T>(body: &[u8]) -> Option<T>
where
    T: Identifiable + Deserializable,
{
    let buf = &mut Cursor::from_slice(body);
    match u32::deserialize(buf) {
        Ok(id) if id == T::CONSTRUCTOR_ID => T::deserialize(buf).ok(),
        _ => None,
    }
}

fn deserialize_own_update<'a>(
    update: Vec<u8>,
    request: impl FnOnce() -> Option<&'a [u8]>,
) -> tl::deserialize::Result<UpdatesLike> {
    Ok(match deserialize_updates_like(update)? {
        UpdatesLike::Updates(tl::enums::Updates::UpdateShortSentMessage(update)) => {
            // Short sent messages need their original text and peer; full updates do not.
            match request().and_then(deserialize_request::<tl::functions::messages::SendMessage>) {
                Some(request) => UpdatesLike::ShortSentMessage { request, update },
                None => UpdatesLike::Updates(tl::enums::Updates::UpdateShortSentMessage(update)),
            }
        }
        UpdatesLike::AffectedMessages(affected) => {
            // Channel deletions advance the channel's pts, not the common sequence.
            let channel = request()
                .and_then(deserialize_request::<tl::functions::channels::DeleteMessages>)
                .and_then(|request| match request.channel {
                    tl::enums::InputChannel::Channel(channel) => {
                        Some((channel.channel_id, request.id))
                    }
                    _ => None,
                });
            if let Some((channel_id, message_ids)) = channel {
                UpdatesLike::AffectedChannelMessages {
                    affected,
                    channel_id,
                    message_ids,
                }
            } else {
                UpdatesLike::AffectedMessages(affected)
            }
        }
        update => update,
    })
}

/// Manages enqueuing requests, matching them to their response, and IO.
pub struct Sender<T: Transport, M: Mtp> {
    stream: NetStream,
    transport: T,
    mtp: M,
    addr: ServerAddr,
    requests: Vec<Request>,
    ping_timer: Pin<Box<Sleep>>,
    next_ping: Instant,
    liveness: Option<(Duration, Instant)>,

    // Transport-level buffers and positions
    read_buffer: Vec<u8>,
    read_tail: usize,
    write_buffer: DequeBuffer<u8>,
    write_head: usize,
}

struct Request {
    protocol_retries: u8,
    body: Bytes,
    state: RequestState,
    // Fields drop in declaration order, including when a connection is dropped.
    // Release admission before dropping the response sender wakes its receiver.
    permit: Option<crate::InvocationPermit>,
    // None is a protocol-owned keepalive, which must survive caller cancellation.
    result: Option<oneshot::Sender<Result<Vec<u8>, InvocationError>>>,
}

impl Request {
    /// Retirement precedes notification on every terminal path. A receiver's
    /// waker may run immediately inside send and reserve its next invocation.
    fn complete(self, outcome: Result<Vec<u8>, InvocationError>) {
        let Self { result, permit, .. } = self;
        drop(permit);
        if let Some(tx) = result {
            let _ = tx.send(outcome);
        }
    }
}

#[derive(Clone, Debug)]
struct MsgIdPair {
    msg_id: MsgId,
    container_msg_id: MsgId,
}

enum RequestState {
    NotSerialized,
    Serialized(MsgIdPair),
    Sent(MsgIdPair),
}

impl MsgIdPair {
    fn new(msg_id: MsgId) -> Self {
        Self {
            msg_id,
            container_msg_id: msg_id, // by default, no container (so the last msg_id is itself)
        }
    }
}

impl<T: Transport, M: Mtp> Sender<T, M> {
    /// Create a new connection using the specified transport, MTP state and server address.
    ///
    /// Note that this does not attempt to invoke [`tl::functions::InitConnection`].
    /// It will simply open a new socket connection to the provided address.
    pub async fn connect(transport: T, mtp: M, addr: ServerAddr) -> Result<Self, io::Error> {
        let stream = NetStream::connect(&addr).await?;
        let next_ping = Instant::now() + PING_DELAY;
        Ok(Self {
            stream,
            transport,
            mtp,
            addr,
            requests: vec![],
            ping_timer: Box::pin(sleep_until(next_ping)),
            next_ping,
            liveness: None,

            read_buffer: vec![0; INITIAL_READ_BUFFER_SIZE],
            read_tail: 0,
            write_buffer: DequeBuffer::with_capacity(
                INITIAL_WRITE_BUFFER_SIZE,
                LEADING_BUFFER_SPACE,
            ),
            write_head: 0,
        })
    }

    /// Shutdown the connection.
    ///
    /// Any further [`Self::invoke`] or [`Self::step`] will return an error.
    /// This method is useful if one wants to explicitly handle on-close errors.
    pub async fn disconnect(&mut self) -> io::Result<()> {
        self.stream.disconnect().await
    }

    /// Set an optional deadline for receiving another valid packet.
    /// This does not change individual invocation deadlines or replay policy.
    pub fn set_liveness_timeout(&mut self, timeout: Option<Duration>) {
        self.liveness = timeout.map(|limit| (limit, Instant::now()));
        self.next_ping = self.next_ping.min(Instant::now() + self.ping_interval());
    }

    fn ping_interval(&self) -> Duration {
        self.liveness
            .map_or(PING_DELAY, |(limit, _)| PING_DELAY.min(limit / 2))
    }

    /// Serializes the given request, enqueues it to the internal buffer,
    /// and repeatedly calls [`Self::step`] until a response for it is received.
    pub async fn invoke<R: RemoteCall>(
        &mut self,
        request: &R,
    ) -> Result<R::Return, InvocationError> {
        let (tx, rx) = oneshot::channel();
        self.enqueue_body(request.to_bytes().into(), tx);
        self.step_until_receive(rx)
            .await
            .and_then(|vec| R::Return::from_bytes(&vec).map_err(|err| err.into()))
    }

    pub(crate) fn enqueue_body(
        &mut self,
        body: Bytes,
        tx: oneshot::Sender<Result<Vec<u8>, InvocationError>>,
    ) {
        self.enqueue_tracked_body(body, tx, None);
    }

    pub(crate) fn enqueue_tracked_body(
        &mut self,
        body: Bytes,
        tx: oneshot::Sender<Result<Vec<u8>, InvocationError>>,
        permit: Option<crate::InvocationPermit>,
    ) {
        if tx.is_closed() {
            return;
        }
        self.enqueue_request(body, Some(tx), permit);
    }

    fn enqueue_request(
        &mut self,
        body: Bytes,
        result: Option<oneshot::Sender<Result<Vec<u8>, InvocationError>>>,
        permit: Option<crate::InvocationPermit>,
    ) {
        if !self.mtp.request_is_valid(&body) {
            drop(permit);
            if let Some(tx) = result {
                let _ = tx.send(Err(io::Error::new(io::ErrorKind::InvalidInput,
                    "request must contain a constructor, be word-aligned and fit the MTProto packet limit").into()));
            }
            return;
        }
        let req_id = u32::from_le_bytes([body[0], body[1], body[2], body[3]]);
        debug!(
            "enqueueing request {} to be serialized",
            tl::name_for_id(req_id)
        );

        self.requests.push(Request {
            protocol_retries: 0,
            body,
            state: RequestState::NotSerialized,
            result,
            permit,
        });
    }

    async fn step_until_receive(
        &mut self,
        mut rx: oneshot::Receiver<Result<Vec<u8>, InvocationError>>,
    ) -> Result<Vec<u8>, InvocationError> {
        loop {
            match rx.try_recv() {
                Ok(x) => break x,
                Err(TryRecvError::Empty) => {
                    if let Err(error) = self.step().await {
                        // on_error sends the invocation's delivery certainty
                        // before step returns its shared connection error.
                        return rx.try_recv().unwrap_or_else(|_| Err(error.into()));
                    }
                }
                Err(TryRecvError::Closed) => {
                    break Err(InvocationError::Dropped(
                        "request channel closed before response",
                    ));
                }
            }
        }
    }

    /// Step network events, writing and reading at the same time.
    ///
    /// Updates received during this step, if any, are returned.
    ///
    /// If an error is returned, the connection should be treated
    /// as dead and the sender instance recreated.
    pub async fn step(&mut self) -> Result<Vec<UpdatesLike>, ReadError> {
        if self
            .liveness
            .is_some_and(|(limit, since)| since.elapsed() >= limit)
        {
            return Err(self.on_error(ReadError::Io(io::Error::new(
                io::ErrorKind::TimedOut,
                "no valid packet received before liveness deadline",
            ))));
        }
        if let Err(error) = self.prepare_read_buffer() {
            return Err(self.on_error(error));
        }
        self.try_fill_write();
        let write_len = self.write_buffer.len() - self.write_head;
        trace!(
            "reading bytes and sending up to {} bytes via network",
            write_len
        );

        let (mut reader, mut writer) = self.stream.split();
        let protocol_deadline = (write_len == 0)
            .then(|| self.mtp.service_deadline())
            .flatten()
            .map(Instant::from_std);
        let deadline = [
            protocol_deadline,
            self.liveness
                .and_then(|(limit, since)| since.checked_add(limit)),
            self.mtp.supports_keepalive().then_some(self.next_ping),
        ]
        .into_iter()
        .flatten()
        .min();
        if let Some(deadline) = deadline
            && (deadline < self.ping_timer.deadline() || self.ping_timer.is_elapsed())
        {
            self.ping_timer.as_mut().reset(deadline);
        }

        let res = tokio::select! {
            n = reader.read(&mut self.read_buffer[self.read_tail..]) => {
                n.map_err(ReadError::Io).and_then(|n| self.on_net_read(n))
            }
            n = writer.write(&self.write_buffer[self.write_head..]), if !self.write_buffer.is_empty() => {
                n.map_err(ReadError::Io).and_then(|n| {
                    self.on_net_write(n)?;
                    Ok(Vec::new())
                })
            }
            _ = self.ping_timer.as_mut(), if deadline.is_some() => {
                if protocol_deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                    self.mtp.on_service_timeout();
                }
                if self.mtp.supports_keepalive() && Instant::now() >= self.next_ping {
                    self.on_ping_timeout();
                }
                Ok(Vec::new())
            }
        };

        match res {
            Ok(ok) => Ok(ok),
            Err(err) => Err(self.on_error(err)),
        }
    }

    fn prepare_read_buffer(&mut self) -> Result<(), ReadError> {
        if self.read_tail == self.read_buffer.len() {
            if self.read_tail >= MAXIMUM_DATA {
                return Err(ReadError::Io(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "transport packet exceeds receive limit",
                )));
            }
            let next = (self.read_buffer.len() * 2).min(MAXIMUM_DATA);
            self.read_buffer.resize(next, 0);
        }
        Ok(())
    }

    /// Setup the write buffer for the transport, unless a write is already pending.
    fn try_fill_write(&mut self) {
        // Serialized packets may already be partially written; preserve those
        // and their update metadata. Only unsent, unframed calls can be removed.
        self.requests.retain(|r| {
            !matches!(r.state, RequestState::NotSerialized)
                || r.result.as_ref().is_none_or(|tx| !tx.is_closed())
        });
        if !self.write_buffer.is_empty() {
            return;
        }

        // TODO add a test to make sure we only ever send the same request once
        for request in self
            .requests
            .iter_mut()
            .filter(|r| matches!(r.state, RequestState::NotSerialized))
        {
            // TODO make mtp itself use BytesMut to avoid copies
            if let Some(msg_id) = self.mtp.push(&mut self.write_buffer, &request.body) {
                assert!(request.body.len() >= 4);
                let req_id = u32::from_le_bytes([
                    request.body[0],
                    request.body[1],
                    request.body[2],
                    request.body[3],
                ]);
                debug!(
                    req_id = format_args!("{:x}", req_id),
                    req_name = tl::name_for_id(req_id),
                    ?msg_id,
                    "serialized request",
                );
                // Note how only NotSerialized become Serialized.
                // Nasty bugs that take ~2h to find occur otherwise!
                // (e.g. infinite loops leading to transport flood.)
                request.state = RequestState::Serialized(MsgIdPair::new(msg_id));
            } else {
                break;
            }
        }

        if let Some(container_msg_id) = self.mtp.finalize(&mut self.write_buffer) {
            for request in self.requests.iter_mut() {
                match request.state {
                    RequestState::Serialized(ref mut pair) => {
                        pair.container_msg_id = container_msg_id;
                    }
                    RequestState::NotSerialized | RequestState::Sent(..) => {}
                }
            }
            self.transport.pack(&mut self.write_buffer)
        }
    }

    /// Handle `n` more read bytes being ready to process by the transport.
    ///
    /// This won't cause `ReadError::Io`, but yet another enum would be overkill.
    fn on_net_read(&mut self, n: usize) -> Result<Vec<UpdatesLike>, ReadError> {
        if n == 0 {
            return Err(ReadError::Io(io::Error::new(
                io::ErrorKind::ConnectionReset,
                "read 0 bytes",
            )));
        }

        self.read_tail += n;
        trace!("read {} bytes from the network", n);
        trace!("trying to unpack buffer of {} bytes...", self.read_tail);

        // TODO the buffer might have multiple transport packets, what should happen with the
        // updates successfully read if subsequent packets fail to be deserialized properly?
        let mut updates = Vec::new();
        let mut next_offset = 0;
        while next_offset != self.read_tail {
            match self
                .transport
                .unpack(&mut self.read_buffer[next_offset..self.read_tail])
            {
                Ok(offset) => {
                    debug!("deserializing valid transport packet...");
                    let result = self
                        .mtp
                        .deserialize(&mut self.read_buffer[next_offset..][offset.data_range])?;

                    self.process_mtp_buffer(result, &mut updates)?;
                    if let Some((_, since)) = &mut self.liveness {
                        *since = Instant::now();
                    }
                    next_offset += offset.next_offset;
                }
                Err(transport::Error::MissingBytes) => break,
                Err(err) => return Err(err.into()),
            }
        }

        self.read_buffer.copy_within(next_offset..self.read_tail, 0);
        self.read_tail -= next_offset;

        Ok(updates)
    }

    /// Handle `n` more written bytes being ready to process by the transport.
    fn on_net_write(&mut self, n: usize) -> Result<(), ReadError> {
        if n == 0 && !self.write_buffer.is_empty() {
            return Err(ReadError::Io(io::Error::new(
                io::ErrorKind::WriteZero,
                "socket wrote zero bytes",
            )));
        }
        self.write_head += n;
        trace!(
            "written {} bytes to the network ({}/{})",
            n,
            self.write_head,
            self.write_buffer.len()
        );
        assert!(self.write_head <= self.write_buffer.len());
        if self.write_head != self.write_buffer.len() {
            return Ok(());
        }

        self.write_buffer.clear();
        self.write_head = 0;
        for req in self.requests.iter_mut() {
            match &req.state {
                RequestState::NotSerialized | RequestState::Sent(_) => {}
                RequestState::Serialized(pair) => {
                    debug!(?pair, "sent request with");
                    req.state = RequestState::Sent(pair.clone());
                    if let Some(permit) = &req.permit {
                        permit.mark_sent();
                    }
                }
            }
        }
        Ok(())
    }

    /// Handle a ping timeout, meaning we need to enqueue a new ping request.
    fn on_ping_timeout(&mut self) {
        self.next_ping = Instant::now() + self.ping_interval();
        self.ping_timer.as_mut().reset(self.next_ping);
        // Refresh the server's disconnect deadline even if its pong was lost.
        // Retain a partially written ping until its packet finishes writing.
        self.requests.retain(|request| {
            request.result.is_some() || !matches!(request.state, RequestState::Sent(_))
        });
        if self.requests.iter().any(|request| request.result.is_none()) {
            return;
        }
        let ping_id = generate_random_id();
        debug!("enqueueing keepalive ping {}", ping_id);
        self.enqueue_request(
            tl::functions::PingDelayDisconnect {
                ping_id,
                disconnect_delay: NO_PING_DISCONNECT,
            }
            .to_bytes()
            .into(),
            None,
            None,
        );
    }

    /// Handle errors that occured while performing I/O.
    fn on_error(&mut self, error: ReadError) -> ReadError {
        warn!(
            "marking all {} request(s) as failed: {}",
            self.requests.len(),
            &error
        );

        let error = std::sync::Arc::new(error);
        self.requests.drain(..).for_each(|r| {
            if r.result.is_some() {
                let may_have_been_sent = match r.state {
                    RequestState::NotSerialized => false,
                    RequestState::Serialized(_) => self.write_head != 0,
                    RequestState::Sent(_) => true,
                };
                r.complete(Err(InvocationError::Read {
                    error: std::sync::Arc::clone(&error),
                    may_have_been_sent,
                }));
            }
        });
        ReadError::Shared(error)
    }

    /// Process the result of deserializing an MTP buffer.
    fn process_mtp_buffer(
        &mut self,
        mut results: Vec<Deserialization>,
        updates: &mut Vec<UpdatesLike>,
    ) -> Result<(), ReadError> {
        for result in &results {
            let (id, container) = match result {
                Deserialization::RpcResult(r) => (r.msg_id, false),
                Deserialization::RpcError(r) => (r.msg_id, false),
                Deserialization::BadMessage(r) => (r.msg_id, true),
                Deserialization::Failure(r) => (r.msg_id, false),
                _ => continue,
            };
            if self.requests.iter().any(|r| {
                matches!(&r.state, RequestState::Serialized(pair)
                if pair.msg_id == id || (container && pair.container_msg_id == id))
            }) {
                return Err(mtp::DeserializeError::InvalidEnvelope {
                    reason: "response for an unsent request",
                }
                .into());
            }
        }
        for result in results.drain(..) {
            match result {
                Deserialization::OwnUpdate { msg_id, update } => {
                    self.process_own_update(updates, msg_id, update)
                }
                Deserialization::Update(update) => self.process_update(updates, update),
                Deserialization::RpcResult(result) => self.process_result(result),
                Deserialization::RpcError(error) => self.process_error(error),
                Deserialization::BadMessage(bad_msg) => self.process_bad_message(bad_msg),
                Deserialization::Failure(failure) => self.process_deserialize_error(failure),
            }
        }
        self.mtp.recycle_deserialization(results);
        Ok(())
    }

    fn process_own_update(
        &mut self,
        updates: &mut Vec<UpdatesLike>,
        msg_id: MsgId,
        update: Vec<u8>,
    ) {
        match deserialize_own_update(update, || {
            self.peek_request(msg_id)
                .map(|request| request.body.as_ref())
        }) {
            Ok(u) => {
                // In the future, we might want to flag "updates produced by the client" somehow.
                // This would be the starting place to do it.
                updates.push(u);
            }
            Err(e) => {
                // This shouldn't happen. We just made the request, so they shouldn't be stale.
                // If the request failed with a failed-to-deserialize error, callers will handle it.
                warn!("telegram responded with updates that failed to be deserialized: {e}")
            }
        }
    }

    fn process_update(&mut self, updates: &mut Vec<UpdatesLike>, update: Vec<u8>) {
        match deserialize_updates_like(update) {
            Ok(u) => updates.push(u),
            Err(e) => {
                // These are to be treated as a gap.
                // > Manually obtaining updates through [get difference] is required in the following situations:
                // > […] Incorrect update: the client cannot deserialize the received data.
                warn!("telegram sent updates that failed to be deserialized: {e}");
                updates.push(UpdatesLike::MalformedUpdates);
            }
        }
    }

    fn process_result(&mut self, result: RpcResult) {
        if let Some(request) = self.pop_request(result.msg_id) {
            let x = result.body;
            if x.len() < 4 {
                request.complete(Err(mtp::DeserializeError::MessageBufferTooSmall.into()));
                return;
            }
            let res_id = u32::from_le_bytes([x[0], x[1], x[2], x[3]]);
            debug!(
                res_id = format_args!("{:x}", res_id),
                res_name = tl::name_for_id(res_id),
                request = ?result.msg_id,
                "got result",
            );
            request.complete(Ok(x));
        } else {
            info!(
                result = ?result.msg_id,
                "got rpc result but no such request is saved",
            );
        }
    }

    fn process_error(&mut self, error: RpcResultError) {
        if let Some(req) = self.pop_request(error.msg_id) {
            debug!(error = ?error.error, "got rpc error");
            let x = req.body.as_ref();
            let outcome = InvocationError::Rpc(
                RpcError::from(error.error)
                    .with_caused_by(u32::from_le_bytes([x[0], x[1], x[2], x[3]])),
            );
            req.complete(Err(outcome));
        } else {
            info!(
                error = ?error.msg_id,
                "got rpc error but no such request is saved",
            );
        }
    }

    fn process_bad_message(&mut self, bad_msg: BadMessage) {
        for i in (0..self.requests.len()).rev() {
            match &self.requests[i].state {
                RequestState::Serialized(pair)
                    if pair.msg_id == bad_msg.msg_id || pair.container_msg_id == bad_msg.msg_id =>
                {
                    // Leave partially written metadata alive until the packet
                    // completes or the connection is torn down.
                    warn!("ignoring bad message for an unsent request");
                }
                RequestState::Sent(pair)
                    if pair.msg_id == bad_msg.msg_id || pair.container_msg_id == bad_msg.msg_id =>
                {
                    // TODO add a test to make sure we resend the request
                    if self.requests[i]
                        .result
                        .as_ref()
                        .is_some_and(|tx| tx.is_closed())
                    {
                        self.requests.swap_remove(i);
                    } else if bad_msg.retryable() && self.requests[i].protocol_retries < 3 {
                        info!(
                            request = ?pair.msg_id,
                            "{}; re-sending request",
                            bad_msg.description(),
                        );

                        // TODO check if actually retryable first!
                        self.requests[i].state = RequestState::NotSerialized;
                        self.requests[i].protocol_retries += 1;
                    } else {
                        if bad_msg.fatal() {
                            error!(
                                request = ?pair.msg_id,
                                "{}; canont retry request",
                                bad_msg.description(),
                            );
                        } else {
                            warn!(
                                request = ?pair.msg_id,
                                "{}; canont retry request",
                                bad_msg.description(),
                            );
                        }
                        let req = self.requests.swap_remove(i);
                        let retries = req.protocol_retries;
                        req.complete(Err(InvocationError::BadMessage {
                            code: bad_msg.code,
                            retries,
                        }));
                    }
                }
                _ => {}
            }
        }
    }

    fn process_deserialize_error(&mut self, failure: DeserializationFailure) {
        if let Some(req) = self.pop_request(failure.msg_id) {
            debug!(error = ?failure.error, "got deserialization failure");
            req.complete(Err(InvocationError::from(failure.error)));
        } else {
            info!(
                error = ?failure.error,
                "got deserialization failure but no such request is saved",
            );
        }
    }

    fn peek_request(&self, msg_id: MsgId) -> Option<&Request> {
        self.requests.iter().find(|request| match request.state {
            RequestState::NotSerialized => false,
            RequestState::Serialized(MsgIdPair { msg_id: m, .. }) => m == msg_id,
            RequestState::Sent(MsgIdPair { msg_id: m, .. }) => m == msg_id,
        })
    }

    fn pop_request(&mut self, msg_id: MsgId) -> Option<Request> {
        for i in 0..self.requests.len() {
            match &self.requests[i].state {
                RequestState::Serialized(pair) if pair.msg_id == msg_id => {
                    warn!("ignoring response for an unsent request");
                    return None;
                }
                RequestState::Sent(pair) if pair.msg_id == msg_id => {
                    return Some(self.requests.swap_remove(i));
                }
                _ => {}
            }
        }

        None
    }
}

impl<T: Transport> Sender<T, mtp::Encrypted> {
    /// Configure inbound resource limits before initialization.
    pub fn set_decode_limits(&mut self, limits: mtp::DecodeLimits) {
        self.mtp.set_decode_limits(limits);
    }
    /// Configure API update suppression before sending initConnection.
    /// Protocol messages and RPC replies are unaffected.
    pub fn set_receive_updates(&mut self, enabled: bool) {
        self.mtp.set_receive_updates(enabled);
    }
    /// Collect up to `count` acknowledgements before attaching them to a request.
    /// See [`ConnectionParams::ack_batch`](crate::ConnectionParams::ack_batch).
    pub fn set_ack_batch(&mut self, count: usize) {
        self.mtp.set_ack_batch(count);
    }
    pub fn auth_key(&self) -> [u8; 256] {
        self.mtp.auth_key()
    }
}

/// Helper function to [`Sender::connect`] a plain transport and [`generate_auth_key`] on it.
pub async fn connect<T: Transport>(
    transport: T,
    addr: ServerAddr,
) -> Result<Sender<T, mtp::Encrypted>, InvocationError> {
    let sender = Sender::connect(transport, mtp::Plain::new(), addr).await?;
    generate_auth_key(sender).await
}

// CPU-bound primality validation and DH exponentiation must not block I/O.
// Started jobs cannot be preempted; hold their capacity until they finish even
// if the handshake caller cancels. Waiting for capacity is cancel-safe.
async fn run_dh_job<T: Send + 'static>(
    job: impl FnOnce() -> Result<T, authentication::Error> + Send + 'static,
) -> Result<T, InvocationError> {
    static DH_JOBS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(4);
    let permit = DH_JOBS
        .acquire()
        .await
        .expect("DH semaphore is never closed");
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        job()
    })
    .await
    .map_err(|error| InvocationError::Io(io::Error::other(error)))?
    .map_err(Into::into)
}

/// Uses the input plain sender to carry the Authorization Key generation process,
/// and returns an encrypted sender reusing the same connection, transport and buffers.
pub async fn generate_auth_key<T: Transport>(
    mut sender: Sender<T, mtp::Plain>,
) -> Result<Sender<T, mtp::Encrypted>, InvocationError> {
    info!("generating new authorization key...");
    let (request, data) = authentication::step1()?;
    debug!("gen auth key: sending step 1");
    let response = sender.invoke(&request).await?;
    debug!("gen auth key: starting step 2");
    let (request, data) = authentication::step2(data, response)?;
    debug!("gen auth key: sending step 2");
    let response = sender.invoke(&request).await?;
    debug!("gen auth key: starting step 3");
    let (request, data) = run_dh_job(move || authentication::step3(data, response)).await?;
    debug!("gen auth key: sending step 3");
    let response = sender.invoke(&request).await?;
    debug!("gen auth key: completing generation");
    let authentication::Finished {
        auth_key,
        time_offset,
        first_salt,
    } = authentication::create_key(data, response)?;
    info!("authorization key generated successfully");

    let next_ping = Instant::now() + PING_DELAY;
    sender.ping_timer.as_mut().reset(next_ping);
    Ok(Sender {
        liveness: None,
        next_ping,
        stream: sender.stream,
        transport: sender.transport,
        mtp: mtp::Encrypted::build()
            .time_offset(time_offset)
            .first_salt(first_salt)
            .finish(auth_key),
        requests: sender.requests,
        ping_timer: sender.ping_timer,
        read_buffer: sender.read_buffer,
        read_tail: sender.read_tail,
        write_buffer: sender.write_buffer,
        write_head: sender.write_head,
        addr: sender.addr,
    })
}

/// Helper function to [`Sender::connect`] an encrypted transport with a previous Authorization Key.
pub async fn connect_with_auth<T: Transport>(
    transport: T,
    addr: ServerAddr,
    auth_key: [u8; 256],
) -> Result<Sender<T, mtp::Encrypted>, io::Error> {
    Sender::connect(transport, mtp::Encrypted::build().finish(auth_key), addr).await
}

#[cfg(test)]
mod optimization_tests {
    use super::*;

    fn send_message_fixture(message: &str) -> Vec<u8> {
        // Minimal SendMessage wire fixture: silent, self peer, text, random id.
        let mut body = tl::functions::messages::SendMessage::CONSTRUCTOR_ID
            .to_le_bytes()
            .to_vec();
        (1u32 << 5).serialize(&mut body);
        tl::enums::InputPeer::PeerSelf.serialize(&mut body);
        message.to_owned().serialize(&mut body);
        987654321i64.serialize(&mut body);
        body
    }

    fn full_updates_fixture() -> Vec<u8> {
        tl::enums::Updates::Updates(tl::types::Updates {
            updates: vec![tl::enums::Update::MessageId(tl::types::UpdateMessageId {
                id: 73,
                random_id: 987654321,
            })],
            users: Vec::new(),
            chats: Vec::new(),
            date: 1234,
            seq: 19,
        })
        .to_bytes()
    }

    fn short_sent_fixture() -> Vec<u8> {
        tl::enums::Updates::UpdateShortSentMessage(tl::types::UpdateShortSentMessage {
            out: true,
            id: 73,
            pts: 81,
            pts_count: 1,
            date: 1234,
            media: None,
            entities: None,
            ttl_period: Some(60),
        })
        .to_bytes()
    }

    fn fixture_msg_id() -> MsgId {
        mtp::Plain::new()
            .push(&mut DequeBuffer::with_capacity(32, 0), &[1; 4])
            .unwrap()
    }

    fn saved_request(body: Vec<u8>, msg_id: MsgId) -> Request {
        Request {
            protocol_retries: 0,
            body: body.into(),
            state: RequestState::Sent(MsgIdPair::new(msg_id)),
            permit: None,
            result: None,
        }
    }

    // Retain the former eager control flow only for the local comparison benchmark.
    fn deserialize_own_update_eager<'a>(
        update: Vec<u8>,
        mut request: impl FnMut() -> Option<&'a [u8]>,
    ) -> tl::deserialize::Result<UpdatesLike> {
        match (
            deserialize_updates_like(update),
            request().and_then(deserialize_request::<tl::functions::messages::SendMessage>),
        ) {
            (
                Ok(UpdatesLike::Updates(tl::enums::Updates::UpdateShortSentMessage(update))),
                Some(request),
            ) => Ok(UpdatesLike::ShortSentMessage { request, update }),
            (Ok(UpdatesLike::AffectedMessages(affected)), _) => {
                let channel = request()
                    .and_then(deserialize_request::<tl::functions::channels::DeleteMessages>)
                    .and_then(|request| match request.channel {
                        tl::enums::InputChannel::Channel(channel) => {
                            Some((channel.channel_id, request.id))
                        }
                        _ => None,
                    });
                Ok(if let Some((channel_id, message_ids)) = channel {
                    UpdatesLike::AffectedChannelMessages {
                        affected,
                        channel_id,
                        message_ids,
                    }
                } else {
                    UpdatesLike::AffectedMessages(affected)
                })
            }
            (update, _) => update,
        }
    }

    #[test]
    fn own_full_updates_do_not_consult_the_original_request() {
        let bytes = full_updates_fixture();
        let update = deserialize_own_update(bytes.clone(), || {
            panic!("full updates must not look up or decode request bodies")
        })
        .unwrap();
        assert!(matches!(update, UpdatesLike::Updates(update) if update.to_bytes() == bytes));
    }

    #[test]
    fn malformed_own_updates_do_not_consult_the_original_request() {
        assert!(
            deserialize_own_update(vec![1; 4], || {
                panic!("invalid updates must not decode request bodies")
            })
            .is_err()
        );
    }

    #[test]
    #[ignore = "isolated full-update decoding benchmark; synthetic requests, no sockets"]
    fn benchmark_own_full_update_decoding() {
        use std::hint::black_box;
        const ITERATIONS: usize = 25_000;
        fn lookup(requests: &[Request], msg_id: MsgId) -> Option<&[u8]> {
            requests
                .iter()
                .find(|request| match request.state {
                    RequestState::NotSerialized => false,
                    RequestState::Serialized(MsgIdPair { msg_id: id, .. })
                    | RequestState::Sent(MsgIdPair { msg_id: id, .. }) => id == msg_id,
                })
                .map(|request| request.body.as_ref())
        }
        let msg_id = fixture_msg_id();
        let other_msg_id = mtp::Encrypted::build()
            .finish([0; 256])
            .push(&mut DequeBuffer::with_capacity(128, 0), &[1; 4])
            .unwrap();
        assert_ne!(msg_id, other_msg_id);
        let update = full_updates_fixture();
        for message_len in [32, 4096] {
            let body = send_message_fixture(&"x".repeat(message_len));
            for pending in [1, 64, 1024] {
                let mut requests: Vec<_> = (0..pending)
                    .map(|_| saved_request(body.clone(), other_msg_id))
                    .collect();
                requests.last_mut().unwrap().state = RequestState::Sent(MsgIdPair::new(msg_id));
                // Sanity-check the retained baseline before timing either branch.
                for optimized in [false, true] {
                    let result = if optimized {
                        deserialize_own_update(update.clone(), || lookup(&requests, msg_id))
                    } else {
                        deserialize_own_update_eager(update.clone(), || lookup(&requests, msg_id))
                    }
                    .unwrap();
                    assert!(
                        matches!(result, UpdatesLike::Updates(result) if result.to_bytes() == update)
                    );
                }
                let mut samples = [Vec::new(), Vec::new()];
                for round in 0..8 {
                    for optimized in [round % 2 == 0, round % 2 != 0] {
                        let started = std::time::Instant::now();
                        for _ in 0..ITERATIONS {
                            let request = || lookup(black_box(&requests), msg_id);
                            let result = if optimized {
                                deserialize_own_update(black_box(update.clone()), request)
                            } else {
                                deserialize_own_update_eager(black_box(update.clone()), request)
                            };
                            black_box(result.unwrap());
                        }
                        samples[usize::from(optimized)]
                            .push(started.elapsed().as_nanos() as f64 / ITERATIONS as f64);
                    }
                }
                for sample in &mut samples {
                    sample.sort_by(f64::total_cmp);
                }
                println!(
                    "Full own update / SendMessage text {message_len} bytes / pending {pending}, target last: eager {:.1} ns, lazy {:.1} ns median (includes update input clone and decode)",
                    (samples[0][3] + samples[0][4]) / 2.0,
                    (samples[1][3] + samples[1][4]) / 2.0
                );
            }
        }
    }

    #[tokio::test]
    async fn own_full_updates_survive_missing_or_malformed_original_requests() {
        let (mut sender, _peer) = pair().await;
        let msg_id = fixture_msg_id();
        let bytes = full_updates_fixture();
        for body in [
            None,
            Some(send_message_fixture("hello")),
            Some(vec![1; 4]),
            Some(
                tl::functions::messages::SendMessage::CONSTRUCTOR_ID
                    .to_le_bytes()
                    .to_vec(),
            ),
        ] {
            sender.requests = body
                .into_iter()
                .map(|body| saved_request(body, msg_id))
                .collect();
            let mut updates = Vec::new();
            sender.process_own_update(&mut updates, msg_id, bytes.clone());
            assert!(
                matches!(updates.as_slice(), [UpdatesLike::Updates(update)] if update.to_bytes() == bytes)
            );
        }
    }

    #[tokio::test]
    async fn own_short_sent_update_keeps_the_original_message_and_fields() {
        let (mut sender, _peer) = pair().await;
        let msg_id = fixture_msg_id();
        let body = send_message_fixture("hello from a short update");
        sender.requests.push(saved_request(body.clone(), msg_id));
        let mut updates = Vec::new();
        sender.process_own_update(&mut updates, msg_id, short_sent_fixture());
        let [UpdatesLike::ShortSentMessage { request, update }] = updates.as_slice() else {
            panic!("expected an expanded short-sent message: {updates:?}");
        };
        assert_eq!(request.to_bytes(), body);
        assert!(request.silent);
        assert_eq!(
            (
                update.id,
                update.pts,
                update.pts_count,
                update.date,
                update.ttl_period
            ),
            (73, 81, 1, 1234, Some(60))
        );
    }

    #[tokio::test]
    async fn own_short_sent_update_falls_back_without_a_decodable_send_message() {
        let (mut sender, _peer) = pair().await;
        let msg_id = fixture_msg_id();
        let bytes = short_sent_fixture();
        for body in [
            None,
            Some(vec![1; 4]),
            Some(
                tl::functions::messages::SendMessage::CONSTRUCTOR_ID
                    .to_le_bytes()
                    .to_vec(),
            ),
        ] {
            sender.requests = body
                .into_iter()
                .map(|body| saved_request(body, msg_id))
                .collect();
            let mut updates = Vec::new();
            sender.process_own_update(&mut updates, msg_id, bytes.clone());
            assert!(
                matches!(updates.as_slice(), [UpdatesLike::Updates(update)] if update.to_bytes() == bytes)
            );
        }
    }

    #[tokio::test]
    async fn own_affected_messages_preserve_channel_identity_and_common_fallback() {
        let (mut sender, _peer) = pair().await;
        let msg_id = fixture_msg_id();
        let affected = tl::enums::messages::AffectedMessages::Messages(
            tl::types::messages::AffectedMessages {
                pts: 93,
                pts_count: 2,
            },
        )
        .to_bytes();
        let channel_delete = tl::functions::channels::DeleteMessages {
            channel: tl::enums::InputChannel::Channel(tl::types::InputChannel {
                channel_id: 456,
                access_hash: 789,
            }),
            id: vec![21, 22],
        }
        .to_bytes();
        let unsupported_channel_delete = tl::functions::channels::DeleteMessages {
            channel: tl::enums::InputChannel::FromMessage(tl::types::InputChannelFromMessage {
                peer: tl::enums::InputPeer::PeerSelf,
                msg_id: 20,
                channel_id: 456,
            }),
            id: vec![21, 22],
        }
        .to_bytes();
        for (body, is_channel) in [
            (Some(channel_delete), true),
            (Some(unsupported_channel_delete), false),
            (Some(send_message_fixture("unrelated")), false),
            (
                Some(
                    tl::functions::channels::DeleteMessages::CONSTRUCTOR_ID
                        .to_le_bytes()
                        .to_vec(),
                ),
                false,
            ),
            (None, false),
        ] {
            sender.requests = body
                .into_iter()
                .map(|body| saved_request(body, msg_id))
                .collect();
            let mut updates = Vec::new();
            sender.process_own_update(&mut updates, msg_id, affected.clone());
            if is_channel {
                assert!(
                    matches!(updates.as_slice(), [UpdatesLike::AffectedChannelMessages { affected, channel_id: 456, message_ids }]
                    if affected.pts == 93 && affected.pts_count == 2 && message_ids == &[21, 22])
                );
            } else {
                assert!(
                    matches!(updates.as_slice(), [UpdatesLike::AffectedMessages(affected)]
                    if affected.pts == 93 && affected.pts_count == 2)
                );
            }
        }
    }

    #[tokio::test]
    async fn own_update_is_expanded_before_cancelled_request_retirement() {
        let (mut sender, _peer) = pair().await;
        let tracker = crate::InvocationTracker::default();
        let (tx, rx) = oneshot::channel();
        sender.enqueue_tracked_body(
            send_message_fixture("retained request").into(),
            tx,
            tracker.try_acquire(),
        );
        sender.try_fill_write();
        sender.on_net_write(sender.write_buffer.len()).unwrap();
        let RequestState::Sent(pair) = &sender.requests[0].state else {
            panic!("unsent");
        };
        let msg_id = pair.msg_id;
        drop(rx);
        let bytes = short_sent_fixture();
        let mut updates = Vec::new();
        sender.process_own_update(&mut updates, msg_id, bytes.clone());
        assert_eq!(tracker.stage(), crate::InvocationStage::Sent);
        assert_eq!(sender.requests.len(), 1);
        updates.clear();
        sender
            .process_mtp_buffer(
                vec![
                    Deserialization::OwnUpdate {
                        msg_id,
                        update: bytes.clone(),
                    },
                    Deserialization::RpcResult(RpcResult {
                        msg_id,
                        body: bytes,
                    }),
                ],
                &mut updates,
            )
            .unwrap();
        assert!(
            matches!(updates.as_slice(), [UpdatesLike::ShortSentMessage { request, .. }]
            if request.message == "retained request")
        );
        assert_eq!(tracker.stage(), crate::InvocationStage::Idle);
        assert!(sender.requests.is_empty());
    }

    /// The write time is recorded once, survives the reply and is cleared by
    /// the next reservation.
    #[tokio::test]
    async fn tracked_requests_record_when_they_were_written() {
        let (mut sender, _peer) = pair().await;
        let tracker = crate::InvocationTracker::default();
        let (tx, _rx) = oneshot::channel();
        sender.enqueue_tracked_body(Bytes::from_static(&[1; 4]), tx, tracker.try_acquire());
        assert!(tracker.sent_at().is_none());
        sender.try_fill_write();
        let before = std::time::Instant::now();
        sender.on_net_write(sender.write_buffer.len()).unwrap();
        let after = std::time::Instant::now();
        let sent = tracker.sent_at().expect("written");
        assert!(sent >= before && sent <= after);
        let RequestState::Sent(pair) = &sender.requests[0].state else {
            panic!("unsent");
        };
        let msg_id = pair.msg_id;
        sender.process_result(RpcResult {
            msg_id,
            body: vec![1; 4],
        });
        assert_eq!(tracker.sent_at(), Some(sent));
        let _next = tracker.try_acquire().expect("released");
        assert!(tracker.sent_at().is_none());
    }

    #[tokio::test]
    async fn cancelled_sent_request_keeps_reservation_until_reply_or_disconnect() {
        use std::sync::Arc;
        for disconnect in [false, true] {
            let (mut sender, _peer) = pair().await;
            let tracker = crate::InvocationTracker::default();
            let (tx, rx) = oneshot::channel();
            let capacity = Arc::new(tokio::sync::Semaphore::new(1));
            let permit = crate::InvocationPermit::with_capacity(
                tracker.try_acquire(),
                Arc::clone(&capacity).try_acquire_owned().unwrap(),
            );
            sender.enqueue_tracked_body(Bytes::from_static(&[1; 4]), tx, Some(permit));
            sender.try_fill_write();
            sender.on_net_write(sender.write_buffer.len()).unwrap();
            let RequestState::Sent(pair) = &sender.requests[0].state else {
                panic!("unsent");
            };
            let msg_id = pair.msg_id;
            assert_eq!(tracker.stage(), crate::InvocationStage::Sent);
            drop(rx); // Simulate an outer deadline cancelling the response waiter.
            sender.try_fill_write();
            assert!(tracker.try_acquire().is_none());
            assert_eq!(capacity.available_permits(), 0);
            assert_eq!(sender.requests.len(), 1);
            if disconnect {
                drop(sender);
            } else {
                sender.process_result(RpcResult {
                    msg_id,
                    body: vec![1; 4],
                });
            }
            assert_eq!(tracker.stage(), crate::InvocationStage::Idle);
            assert_eq!(capacity.available_permits(), 1);
            assert!(tracker.try_acquire().is_some());
        }
    }

    /// A caller woken by its result may reuse the connection at once: the
    /// reservation is already free when the result becomes observable.
    #[tokio::test]
    async fn result_waiter_finds_the_reservation_already_released() {
        let (mut sender, _peer) = pair().await;
        let tracker = crate::InvocationTracker::default();
        let (tx, rx) = oneshot::channel();
        sender.enqueue_tracked_body(Bytes::from_static(&[1; 4]), tx, tracker.try_acquire());
        sender.try_fill_write();
        sender.on_net_write(sender.write_buffer.len()).unwrap();
        let RequestState::Sent(pair) = &sender.requests[0].state else {
            panic!("unsent");
        };
        let msg_id = pair.msg_id;
        let observer = tracker.clone();
        let waiter = std::thread::spawn(move || {
            let reply = rx.blocking_recv();
            (reply.is_ok(), observer.stage())
        });
        sender.process_result(RpcResult {
            msg_id,
            body: vec![1; 4],
        });
        let (replied, stage) = waiter.join().unwrap();
        assert!(replied);
        assert_eq!(stage, crate::InvocationStage::Idle);
    }

    #[tokio::test]
    async fn terminal_error_waiters_observe_released_reservations_during_wake() {
        use std::{
            future::Future,
            sync::{Arc, atomic::AtomicU8},
            task::{Context, Wake, Waker},
        };
        struct ObserveWake {
            tracker: crate::InvocationTracker,
            seen: AtomicU8,
            capacity: Arc<tokio::sync::Semaphore>,
            free_slots: std::sync::atomic::AtomicUsize,
        }
        impl Wake for ObserveWake {
            fn wake(self: Arc<Self>) {
                self.wake_by_ref();
            }
            fn wake_by_ref(self: &Arc<Self>) {
                self.free_slots
                    .store(self.capacity.available_permits(), Ordering::SeqCst);
                self.seen.store(
                    match self.tracker.stage() {
                        crate::InvocationStage::Idle => 0,
                        crate::InvocationStage::Pending => 1,
                        crate::InvocationStage::Sent => 2,
                    },
                    Ordering::SeqCst,
                );
            }
        }
        for kind in 0..5 {
            let (mut sender, _peer) = pair().await;
            let tracker = crate::InvocationTracker::default();
            let (tx, rx) = oneshot::channel();
            let capacity = Arc::new(tokio::sync::Semaphore::new(1));
            let permit = crate::InvocationPermit::with_capacity(
                tracker.try_acquire(),
                Arc::clone(&capacity).try_acquire_owned().unwrap(),
            );
            sender.enqueue_tracked_body(Bytes::from_static(&[1; 4]), tx, Some(permit));
            sender.try_fill_write();
            sender.on_net_write(sender.write_buffer.len()).unwrap();
            let RequestState::Sent(pair) = &sender.requests[0].state else {
                panic!("unsent")
            };
            let msg_id = pair.msg_id;
            let wake = Arc::new(ObserveWake {
                tracker: tracker.clone(),
                seen: AtomicU8::new(255),
                capacity: Arc::clone(&capacity),
                free_slots: std::sync::atomic::AtomicUsize::new(0),
            });
            let waker = Waker::from(Arc::clone(&wake));
            let mut context = Context::from_waker(&waker);
            let mut rx = Box::pin(rx);
            assert!(rx.as_mut().poll(&mut context).is_pending());
            if kind == 4 {
                drop(sender);
            } else {
                match kind {
                    0 => sender.process_error(RpcResultError {
                        msg_id,
                        error: tl::types::RpcError {
                            error_code: 420,
                            error_message: "FLOOD_WAIT_7".into(),
                        },
                    }),
                    1 => sender.process_deserialize_error(DeserializationFailure {
                        msg_id,
                        error: mtp::DeserializeError::DecompressionFailed,
                    }),
                    2 => {
                        sender.on_error(ReadError::Io(io::Error::new(
                            io::ErrorKind::ConnectionReset,
                            "fixture",
                        )));
                    }
                    _ => sender.process_bad_message(BadMessage { msg_id, code: 32 }),
                }
                assert!(sender.requests.is_empty());
            }
            assert_eq!(
                wake.seen.load(Ordering::SeqCst),
                0,
                "terminal error path {kind} woke a busy reservation"
            );
            assert_eq!(
                wake.free_slots.load(Ordering::SeqCst),
                1,
                "terminal path {kind} woke a busy admission slot"
            );
            let outcome = rx.await;
            if kind == 4 {
                assert!(outcome.is_err());
            } else {
                assert!(outcome.unwrap().is_err());
            }
            assert!(tracker.try_acquire().is_some());
        }
    }

    #[tokio::test]
    async fn cancellation_before_framing_releases_reservation_without_a_send() {
        let (mut sender, _peer) = pair().await;
        let tracker = crate::InvocationTracker::default();
        let (tx, rx) = oneshot::channel();
        sender.enqueue_tracked_body(Bytes::from_static(&[1; 4]), tx, tracker.try_acquire());
        drop(rx);
        sender.try_fill_write();
        assert!(sender.requests.is_empty());
        assert!(sender.write_buffer.is_empty());
        assert_eq!(tracker.stage(), crate::InvocationStage::Idle);
    }

    #[tokio::test(start_paused = true)]
    async fn cancelled_steps_keep_one_ping_timer_and_do_not_postpone_keepalive() {
        let (mut sender, _peer) = encrypted_pair().await;
        let timer = sender.ping_timer.as_ref().get_ref() as *const Sleep;
        let deadline = sender.ping_timer.deadline();
        for _ in 0..64 {
            tokio::select! {
                biased;
                result = sender.step() => panic!("unexpected IO or ping: {result:?}"),
                _ = std::future::ready(()) => {},
            }
        }
        assert_eq!(sender.ping_timer.deadline(), deadline);
        assert_eq!(sender.ping_timer.as_ref().get_ref() as *const Sleep, timer);
        assert!(sender.requests.is_empty());
        tokio::time::advance(PING_DELAY).await;
        sender.step().await.unwrap();
        assert_eq!(sender.requests.len(), 1);
        assert!(sender.requests[0].result.is_none());
        assert_eq!(sender.ping_timer.deadline(), Instant::now() + PING_DELAY);
        assert_eq!(sender.ping_timer.as_ref().get_ref() as *const Sleep, timer);
    }

    #[test]
    #[ignore = "isolated pending timer polling benchmark; no sockets"]
    fn benchmark_ping_timer_reuse() {
        use std::{
            future::{Future, poll_fn},
            hint::black_box,
            task::Poll,
        };
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let deadline = Instant::now() + Duration::from_secs(3600);
            let mut timer = Box::pin(sleep_until(deadline));
            let mut samples = [Vec::new(), Vec::new()];
            for round in 0..8 {
                for reuse in [round % 2 == 0, round % 2 != 0] {
                    let start = std::time::Instant::now();
                    for _ in 0..100_000 {
                        poll_fn(|cx| {
                            if reuse {
                                let _ = black_box(timer.as_mut().poll(cx));
                            } else {
                                let fresh = sleep_until(deadline);
                                tokio::pin!(fresh);
                                let _ = black_box(fresh.as_mut().poll(cx));
                            }
                            Poll::Ready(())
                        }).await;
                    }
                    samples[usize::from(reuse)].push(start.elapsed().as_nanos() as f64 / 100_000.0);
                }
            }
            for sample in &mut samples { sample.sort_by(f64::total_cmp); }
            println!("Pending ping timer median: recreate {:.1} ns, reuse {:.1} ns; retained Sleep={} bytes", samples[0][4], samples[1][4], std::mem::size_of::<Sleep>());
        });
    }

    async fn encrypted_pair() -> (
        Sender<transport::Full, mtp::Encrypted>,
        tokio::net::TcpStream,
    ) {
        pair_with(mtp::Encrypted::build().finish([0; 256])).await
    }

    #[tokio::test(start_paused = true)]
    async fn plain_handshake_never_enqueues_keepalives() {
        let (mut sender, _peer) = pair().await;
        tokio::time::advance(PING_DELAY * 3).await;
        assert!(
            tokio::time::timeout(Duration::from_secs(1), sender.step())
                .await
                .is_err()
        );
        assert!(sender.requests.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn service_deadline_uses_the_retained_timer_across_step_cancellation() {
        struct ServiceClock {
            due: Instant,
            fired: bool,
        }
        impl Mtp for ServiceClock {
            fn push(&mut self, _: &mut DequeBuffer<u8>, _: &[u8]) -> Option<MsgId> {
                None
            }
            fn finalize(&mut self, _: &mut DequeBuffer<u8>) -> Option<MsgId> {
                None
            }
            fn deserialize(
                &mut self,
                _: &mut [u8],
            ) -> Result<Vec<Deserialization>, mtp::DeserializeError> {
                Ok(vec![])
            }
            fn supports_keepalive(&self) -> bool {
                false
            }
            fn service_deadline(&self) -> Option<std::time::Instant> {
                (!self.fired).then_some(self.due.into_std())
            }
            fn on_service_timeout(&mut self) {
                self.fired = true;
            }
        }
        let due = Instant::now() + Duration::from_secs(10);
        let (mut sender, _peer) = pair_with(ServiceClock { due, fired: false }).await;
        let timer = sender.ping_timer.as_ref().get_ref() as *const Sleep;
        for _ in 0..20 {
            tokio::select! {
                biased;
                _ = sender.step() => panic!("timer fired before its deadline"),
                _ = std::future::ready(()) => {},
            }
        }
        assert_eq!(sender.ping_timer.deadline(), due);
        // A later ACK deadline must not re-register the timer on every RPC.
        sender.mtp.due = due + Duration::from_secs(5);
        tokio::select! { biased; _ = sender.step() => panic!("not due"), _ = std::future::ready(()) => {} }
        assert_eq!(sender.ping_timer.deadline(), due);
        tokio::time::advance(Duration::from_secs(10)).await;
        // Poll without allowing the paused runtime to advance to the later
        // deadline merely because this is the only pending future.
        tokio::select! { biased; result = sender.step() => { result.unwrap(); }, _ = std::future::ready(()) => {} }
        assert!(
            !sender.mtp.fired,
            "an old early wake must not flush a newer deadline"
        );
        tokio::select! { biased; _ = sender.step() => panic!("not due"), _ = std::future::ready(()) => {} }
        assert_eq!(sender.ping_timer.deadline(), sender.mtp.due);
        tokio::time::advance(Duration::from_secs(5)).await;
        sender.step().await.unwrap();
        assert!(sender.mtp.fired);
        assert_eq!(sender.ping_timer.as_ref().get_ref() as *const Sleep, timer);
    }

    #[tokio::test]
    async fn keepalive_backlog_is_bounded_and_pong_allows_the_next_ping() {
        let (mut sender, _peer) = encrypted_pair().await;
        sender.on_ping_timeout();
        sender.on_ping_timeout();
        assert_eq!(sender.requests.len(), 1);
        sender.try_fill_write();
        sender.on_net_write(sender.write_buffer.len()).unwrap();
        let RequestState::Sent(pair) = &sender.requests[0].state else {
            panic!("unsent ping");
        };
        sender.process_result(RpcResult {
            msg_id: pair.msg_id,
            body: true.to_bytes(),
        });
        assert!(sender.requests.is_empty());
        sender.on_ping_timeout();
        assert_eq!(sender.requests.len(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn dh_job_does_not_block_single_thread_io_progress() {
        let (tx, rx) = std::sync::mpsc::channel();
        let job = run_dh_job(move || Ok(rx.recv_timeout(Duration::from_secs(2)).is_ok()));
        let peer = async {
            tokio::task::yield_now().await;
            let _ = tx.send(());
        };
        let (result, ()) = tokio::join!(job, peer);
        assert!(result.unwrap(), "DH job blocked the only runtime worker");
    }

    #[tokio::test(start_paused = true)]
    async fn short_liveness_pings_before_deadline_and_huge_timeout_is_safe() {
        let (mut sender, _peer) = encrypted_pair().await;
        sender.set_liveness_timeout(Some(Duration::from_secs(4)));
        assert_eq!(sender.next_ping, Instant::now() + Duration::from_secs(2));
        tokio::time::advance(Duration::from_secs(2)).await;
        sender.step().await.unwrap();
        assert_eq!(sender.requests.len(), 1);
        sender.set_liveness_timeout(Some(Duration::MAX));
        // Poll a real step, including deadline arithmetic, then cancel it.
        tokio::select! { biased; result = sender.step() => { result.unwrap(); }, _ = std::future::ready(()) => {} }
    }

    #[tokio::test]
    async fn sent_ping_is_replaced_when_pong_is_late() {
        let (mut sender, _peer) = encrypted_pair().await;
        sender.on_ping_timeout();
        sender.try_fill_write();
        sender.on_net_write(sender.write_buffer.len()).unwrap();
        assert!(matches!(sender.requests[0].state, RequestState::Sent(_)));
        sender.on_ping_timeout();
        assert_eq!(sender.requests.len(), 1);
        assert!(matches!(
            sender.requests[0].state,
            RequestState::NotSerialized
        ));
    }

    #[tokio::test]
    async fn execute_then_drop_response_reports_uncertain_legacy_invocation() {
        use tokio::io::AsyncReadExt;
        let (mut sender, mut peer) = pair().await;
        let executed = async move {
            let length = peer.read_u32_le().await.unwrap() as usize;
            let mut packet = vec![0; length - 4];
            peer.read_exact(&mut packet).await.unwrap();
            // The whole query has reached the peer; deliberately lose its reply.
            drop(peer);
        };
        let request = tl::functions::Ping { ping_id: 42 };
        let (response, ()) = tokio::join!(sender.invoke(&request), executed);
        assert!(matches!(
            response,
            Err(InvocationError::Read {
                may_have_been_sent: true,
                ..
            })
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn silent_connection_obeys_liveness_deadline() {
        let (mut sender, _peer) = encrypted_pair().await;
        sender.set_liveness_timeout(Some(Duration::from_secs(5)));
        tokio::time::advance(Duration::from_secs(5)).await;
        let error = sender.step().await.unwrap_err();
        assert_eq!(error.io_error().unwrap().kind(), io::ErrorKind::TimedOut);
    }

    #[tokio::test]
    async fn connection_failures_preserve_delivery_certainty_and_error_source() {
        use crate::{AutoSleep, RetryContext, RetryPolicy};
        use std::{num::NonZeroU32, ops::ControlFlow};
        for written in [None, Some(0), Some(1), Some(usize::MAX)] {
            let (mut sender, _peer) = encrypted_pair().await;
            let tracker = crate::InvocationTracker::default();
            let (tx, rx) = oneshot::channel();
            sender.enqueue_tracked_body(Bytes::from_static(&[1; 4]), tx, tracker.try_acquire());
            if let Some(n) = written {
                sender.try_fill_write();
                if n > 0 {
                    sender
                        .on_net_write(n.min(sender.write_buffer.len()))
                        .unwrap();
                }
            }
            let failure = sender.on_error(ReadError::Io(io::Error::from_raw_os_error(104)));
            let error = rx.await.unwrap().unwrap_err();
            assert_eq!(tracker.stage(), crate::InvocationStage::Idle);
            let InvocationError::Read {
                error: original,
                may_have_been_sent,
            } = &error
            else {
                panic!("{error:?}");
            };
            assert_eq!(*may_have_been_sent, written.is_some_and(|n| n > 0));
            assert_eq!(original.io_error().unwrap().raw_os_error(), Some(104));
            let ReadError::Shared(shared) = failure else {
                panic!("missing shared source");
            };
            assert!(std::sync::Arc::ptr_eq(original, &shared));
            let retry = AutoSleep::default().should_retry(&RetryContext {
                error,
                fail_count: NonZeroU32::MIN,
                slept_so_far: Duration::ZERO,
            });
            assert_eq!(retry.is_break(), written.is_some_and(|n| n > 0));
            if written.is_none() {
                assert!(matches!(retry, ControlFlow::Continue(_)));
            }
        }
    }

    #[tokio::test]
    async fn protocol_rejections_have_a_finite_retry_budget() {
        let (mut sender, _peer) = pair().await;
        let (tx, rx) = oneshot::channel();
        sender.enqueue_body(Bytes::from_static(&[1; 4]), tx);
        for attempt in 0..4 {
            sender.try_fill_write();
            sender.on_net_write(sender.write_buffer.len()).unwrap();
            let RequestState::Sent(pair) = &sender.requests[0].state else {
                panic!("not sent");
            };
            sender.process_bad_message(BadMessage {
                msg_id: pair.msg_id,
                code: 48,
            });
            assert_eq!(sender.requests.is_empty(), attempt == 3);
        }
        assert!(matches!(
            rx.await.unwrap(),
            Err(InvocationError::BadMessage {
                code: 48,
                retries: 3
            })
        ));
    }

    #[tokio::test]
    async fn invalid_raw_enqueues_release_permits_before_returning_errors() {
        let (mut sender, _peer) = encrypted_pair().await;
        for body in [vec![], vec![0; 3], vec![0; 5], vec![0; 1_044_376]] {
            let tracker = crate::InvocationTracker::default();
            let (tx, rx) = oneshot::channel();
            sender.enqueue_tracked_body(body.into(), tx, tracker.try_acquire());
            assert!(
                matches!(sender.step_until_receive(rx).await, Err(InvocationError::Io(error)) if error.kind() == io::ErrorKind::InvalidInput)
            );
            assert_eq!(tracker.stage(), crate::InvocationStage::Idle);
            assert!(sender.requests.is_empty());
        }
    }

    async fn pair() -> (Sender<transport::Full, mtp::Plain>, tokio::net::TcpStream) {
        pair_with(mtp::Plain::new()).await
    }

    async fn pair_with<M: Mtp>(mtp: M) -> (Sender<transport::Full, M>, tokio::net::TcpStream) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = ServerAddr::Tcp {
            address: listener.local_addr().unwrap(),
        };
        let (sender, peer) = tokio::join!(
            Sender::connect(transport::Full::new(), mtp, addr),
            listener.accept()
        );
        (sender.unwrap(), peer.unwrap().0)
    }

    #[tokio::test]
    async fn cancelled_unsent_requests_are_removed_but_keepalives_are_sent() {
        let (mut sender, _peer) = pair().await;
        let (tx, rx) = oneshot::channel();
        sender.enqueue_body(Bytes::from_static(&[1, 0, 0, 0]), tx);
        drop(rx);
        sender.try_fill_write();
        assert!(sender.requests.is_empty());
        assert!(sender.write_buffer.is_empty());
        let (tx, rx) = oneshot::channel();
        drop(rx);
        sender.enqueue_body(Bytes::from_static(&[1, 0, 0, 0]), tx);
        assert!(sender.requests.is_empty());
        sender.on_ping_timeout();
        sender.try_fill_write();
        assert_eq!(sender.requests.len(), 1);
        assert!(sender.requests[0].result.is_none());
        assert!(!sender.write_buffer.is_empty());
    }

    #[tokio::test]
    async fn cancellation_preserves_framed_data_but_does_not_resend_rejected_requests() {
        let (mut sender, _peer) = pair().await;
        let (tx, rx) = oneshot::channel();
        sender.enqueue_body(Bytes::from_static(&[1, 0, 0, 0]), tx);
        sender.try_fill_write();
        let size = sender.write_buffer.len();
        drop(rx);
        sender.try_fill_write();
        assert_eq!(sender.requests.len(), 1);
        assert_eq!(sender.write_buffer.len(), size);
        sender.on_net_write(size).unwrap();
        let msg_id = match &sender.requests[0].state {
            RequestState::Sent(pair) => pair.msg_id,
            _ => panic!("not sent"),
        };
        sender.process_bad_message(BadMessage { msg_id, code: 48 });
        assert!(sender.requests.is_empty());
        sender.try_fill_write();
        assert!(sender.write_buffer.is_empty());
    }

    #[tokio::test]
    async fn zero_byte_writes_are_errors_instead_of_busy_loops() {
        let (mut sender, _peer) = pair().await;
        sender.on_ping_timeout();
        sender.try_fill_write();
        assert!(
            matches!(sender.on_net_write(0), Err(ReadError::Io(error)) if error.kind() == io::ErrorKind::WriteZero)
        );
    }

    #[tokio::test]
    async fn receive_buffer_grows_for_fragmented_packets_and_keeps_the_hard_limit() {
        let (mut sender, mut peer) = pair().await;
        assert_eq!(sender.read_buffer.len(), INITIAL_READ_BUFFER_SIZE);
        let body = vec![17u8; INITIAL_READ_BUFFER_SIZE * 5];
        let (tx, mut rx) = oneshot::channel();
        sender.enqueue_body(Bytes::from_static(&[1, 0, 0, 0]), tx);
        sender.try_fill_write();
        sender.on_net_write(sender.write_buffer.len()).unwrap();
        let mut packet = DequeBuffer::with_capacity(body.len() + 32, 0);
        let mut plain = mtp::Plain::new();
        plain.push(&mut packet, &body);
        plain.finalize(&mut packet);
        packet[8..16].copy_from_slice(&1i64.to_le_bytes()); // Valid server response id.
        transport::Full::new().pack(&mut packet);
        let bytes = packet.as_ref().to_vec();
        let writer = tokio::spawn(async move {
            for fragment in bytes.chunks(997) {
                peer.write_all(fragment).await.unwrap();
            }
            peer
        });
        let got = tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(result) = rx.try_recv() {
                    break result.unwrap();
                }
                sender.step().await.unwrap();
            }
        })
        .await
        .unwrap();
        assert_eq!(got, body);
        assert!(sender.read_buffer.len() > INITIAL_READ_BUFFER_SIZE);
        assert!(sender.read_buffer.len() <= MAXIMUM_DATA);
        let _peer = writer.await.unwrap();
        sender.read_buffer.resize(MAXIMUM_DATA, 0);
        sender.read_tail = MAXIMUM_DATA;
        assert!(
            matches!(sender.prepare_read_buffer(), Err(ReadError::Io(error)) if error.kind() == io::ErrorKind::InvalidData)
        );
    }

    #[test]
    #[ignore = "isolated allocation benchmark; run release with nocapture"]
    fn benchmark_initial_receive_allocation() {
        use std::hint::black_box;
        const N: usize = 5_000;
        for size in [MAXIMUM_DATA, INITIAL_READ_BUFFER_SIZE] {
            let started = std::time::Instant::now();
            for _ in 0..N {
                black_box(vec![0u8; black_box(size)]);
            }
            println!(
                "initial receive buffer: {size} bytes, {:.1} ns/construction",
                started.elapsed().as_nanos() as f64 / N as f64
            );
        }
    }
}
