// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::fmt;
use std::future::{Future, poll_fn};
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};
use std::num::NonZeroU32;
use std::ops::{ControlFlow, Deref};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, OnceLock, RwLock};
use std::task::Poll;
use std::time::{Duration, Instant};

use bytes::Bytes;
use grammers_mtproto::{mtp, transport};
use grammers_session::Session;
use grammers_session::storages::{ErasedSession, erase};
use grammers_session::types::DcOption;
use grammers_session::updates::UpdatesLike;
use grammers_tl_types::{self as tl, Deserializable, enums};
use tokio::task::AbortHandle;
use tokio::{
    sync::{Semaphore, broadcast, mpsc, oneshot},
    task::JoinSet,
    time::sleep,
};
use tracing::{debug, info, warn};

use crate::configuration::{ConnectionParams, RetryContext};
use crate::errors::{InvocationError, InvocationErrorCategory, ReadError};
use crate::queue;
use crate::{Sender, ServerAddr, connect, connect_with_auth};

/// How long to wait after warning the user that the updates was dropped.
const UPDATES_DROPPED_LOG_COOLDOWN: Duration = Duration::from_secs(300);

/// The lightest framing: one length byte for packets under 508 bytes, four
/// above, and no checksum (TCP already has one). `Full` adds twelve bytes
/// and a CRC32 to every packet in both directions.
pub(crate) type Transport = transport::Abridged;

type InvokeResponse = Vec<u8>;

enum Request {
    Invoke {
        dc_id: i32,
        body: Bytes,
        // Retire before a queued request's dropped response sender can wake it.
        permit: Option<crate::InvocationPermit>,
        tx: oneshot::Sender<Result<InvokeResponse, InvocationError>>,
    },
    CheckRetry {
        // Boxed so this rare variant does not widen every queue slot; each
        // pool's request queue preallocates a block of them.
        error: Box<InvocationError>,
        fail_count: NonZeroU32,
        slept_so_far: Duration,
        tx: oneshot::Sender<ControlFlow<InvocationError, Duration>>,
    },
    Disconnect {
        dc_id: i32,
    },
    Quit,
}

struct Rpc {
    body: Bytes,
    // Preserve this field order for requests discarded with a closed queue.
    permit: Option<crate::InvocationPermit>,
    tx: oneshot::Sender<Result<InvokeResponse, InvocationError>>,
}

struct ConnectionInfo {
    dc_id: i32,
    rpc_tx: queue::UnboundedSender<Rpc>,
    abort_handle: AbortHandle,
}

// One small list per pool (usually one DC). Written only on connection changes;
// the read guard is released before queueing or awaiting a response.
#[derive(Default)]
struct RouteCache {
    entries: Vec<(i32, queue::UnboundedSender<Rpc>)>,
    disconnecting: Vec<(i32, usize)>,
    stopping: bool,
    parked: bool,
    control_changed: Arc<tokio::sync::Notify>,
}
type Routes = Arc<RwLock<RouteCache>>;

/// Snapshot of local runtime activity, not proof of account authorization.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PoolStatus {
    /// No new invocations can be admitted because shutdown has begun.
    pub stopping: bool,
    /// The runner was dropped or completed.
    pub stopped: bool,
    /// Number of currently established connection tasks.
    pub active_connections: usize,
    /// Cumulative updates dropped when the bounded delivery channel was full.
    pub dropped_updates: u64,
    /// Cumulative requests rejected by the admission limit.
    pub admission_rejections: u64,
}

/// Bounded, credential-free runtime notifications. Slow receivers observe
/// [`broadcast::error::RecvError::Lagged`]; network tasks never await observers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PoolEvent {
    /// A datacenter connection completed initialization.
    Connected { dc_id: i32 },
    /// A connection ended; `None` means orderly shutdown.
    ConnectionClosed {
        dc_id: i32,
        category: Option<InvocationErrorCategory>,
    },
    /// A datacenter connection could not be initialized.
    SetupFailed {
        dc_id: i32,
        category: InvocationErrorCategory,
    },
    /// The delivery channel dropped an update batch.
    UpdateOverflow,
    /// An internal connection task failed unexpectedly.
    RunnerFailed(PoolRunError),
}

/// Failure of an internal runner task. Panic payloads are never exposed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[non_exhaustive]
pub enum PoolRunError {
    /// An internal task panicked.
    TaskPanicked,
}
impl fmt::Display for PoolRunError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("sender task panicked (payload redacted)")
    }
}
impl std::error::Error for PoolRunError {}

struct PoolState {
    capacity: Option<Arc<Semaphore>>,
    stopped: AtomicBool,
    active: AtomicUsize,
    dropped: AtomicU64,
    rejected: AtomicU64,
    events: OnceLock<broadcast::Sender<PoolEvent>>,
    prefer_ipv6: bool,
}
impl Default for PoolState {
    fn default() -> Self {
        Self {
            capacity: None,
            stopped: AtomicBool::new(false),
            active: AtomicUsize::new(0),
            dropped: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            events: OnceLock::new(),
            prefer_ipv6: false,
        }
    }
}
impl PoolState {
    /// Receivers only observe events sent after subscribing, so the buffer is
    /// created by the first subscriber instead of eagerly for every pool.
    fn subscribe(&self) -> broadcast::Receiver<PoolEvent> {
        self.events
            .get_or_init(|| broadcast::channel(64).0)
            .subscribe()
    }

    fn emit(&self, event: PoolEvent) {
        if let Some(events) = self.events.get() {
            let _ = events.send(event);
        }
    }
}

impl RouteCache {
    fn publish(&mut self, dc_id: i32, tx: &queue::UnboundedSender<Rpc>) {
        if !self.stopping && !self.disconnecting.iter().any(|(id, _)| *id == dc_id) {
            self.entries.retain(|(id, _)| *id != dc_id);
            self.entries.push((dc_id, tx.clone()));
        }
    }

    fn begin_disconnect(&mut self, dc_id: i32) {
        self.entries.retain(|(id, _)| *id != dc_id);
        if let Some((_, count)) = self.disconnecting.iter_mut().find(|(id, _)| *id == dc_id) {
            *count += 1;
        } else {
            self.disconnecting.push((dc_id, 1));
        }
    }

    fn finish_disconnect(&mut self, dc_id: i32) {
        self.entries.retain(|(id, _)| *id != dc_id);
        if let Some((_, count)) = self.disconnecting.iter_mut().find(|(id, _)| *id == dc_id) {
            *count -= 1;
        }
        self.disconnecting.retain(|(_, count)| *count != 0);
    }
}

/// A fat [`SenderPoolHandle`] with additional metadata from its attached [`SenderPoolRunner`].
#[derive(Clone)]
pub struct SenderPoolFatHandle {
    /// The inner thin handle that self can be derefed into.
    ///
    /// The rest of fields can be dropped if they are no longer needed.
    pub thin: SenderPoolHandle,
    /// The session in use by the attached [`SenderPoolRunner`].
    ///
    /// The runner will read and persist datacenter options in it.
    pub session: Arc<ErasedSession>,
    /// Developer's [Application Identifier](https://core.telegram.org/myapp).
    ///
    /// The [`SenderPoolRunner`] will make use of this value when it needs
    /// to invoke [`tl::functions::InitConnection`] after creating a new connection.
    pub api_id: i32,
}

/// Cheaply cloneable handle to interact with its [`SenderPoolRunner`].
#[derive(Clone)]
pub struct SenderPoolHandle(
    queue::UnboundedSender<Request>,
    Routes,
    bool,
    Arc<PoolState>,
);

/// Builder to configure the runner to drive I/O and linked handles.
pub struct SenderPool {
    /// The single mutable instance responsible for driving I/O.
    ///
    /// Connections are created on-demand, so any errors while the pool
    /// is running can only be retrieved with one of the [`SenderPool::handle`]s.
    pub runner: SenderPoolRunner,
    /// Starting fat handle attached to the [`SenderPool::runner`].
    ///
    /// Handles are the only way to interact with the runner once it's running.
    pub handle: SenderPoolFatHandle,
    /// The single mutable channel through which updates received
    /// from the network by the [`SenderPool::runner`] are delivered.
    ///
    /// Update handling must be processed in a sequential manner,
    /// so this is a separate instance with no way to clone it.
    pub updates: mpsc::Receiver<UpdatesLike>,
}

/// Manages and runs a pool of zero or more [`Sender`]s.
///
/// Use [`SenderPool::new`] to create an instance of this type and associated channels.
pub struct SenderPoolRunner {
    session: Arc<ErasedSession>,
    api_id: i32,
    connection_params: ConnectionParams,
    request_rx: queue::UnboundedReceiver<Request>,
    updates_tx: Option<mpsc::Sender<UpdatesLike>>,
    connections: Vec<ConnectionInfo>,
    connection_pool: JoinSet<(i32, Result<(), ReadError>)>,
    routes: Routes,
    state: Arc<PoolState>,
}

impl Deref for SenderPoolFatHandle {
    type Target = SenderPoolHandle;

    fn deref(&self) -> &Self::Target {
        &self.thin
    }
}

impl SenderPoolHandle {
    /// Observe runtime activity without holding a lock across I/O.
    pub fn status(&self) -> PoolStatus {
        PoolStatus {
            stopping: self.1.read().unwrap_or_else(|e| e.into_inner()).stopping,
            stopped: self.3.stopped.load(Ordering::Acquire),
            active_connections: self.3.active.load(Ordering::Relaxed),
            dropped_updates: self.3.dropped.load(Ordering::Relaxed),
            admission_rejections: self.3.rejected.load(Ordering::Relaxed),
        }
    }

    /// Subscribe to the next runtime events (64 buffered notifications).
    pub fn events(&self) -> broadcast::Receiver<PoolEvent> {
        self.3.subscribe()
    }

    /// The address-family preference fixed at pool construction.
    pub fn prefers_ipv6(&self) -> bool {
        self.3.prefer_ipv6
    }

    /// Activate a parked pool. Does not reopen a stopping or stopped pool.
    /// Start its runner immediately afterward; this method does not spawn it.
    pub fn activate(&self) -> bool {
        let mut cache = self.1.write().unwrap_or_else(|e| e.into_inner());
        if cache.stopping || self.0.is_closed() {
            return false;
        }
        cache.parked = false;
        true
    }

    /// Whether this pool accepts API update delivery (fixed at construction).
    pub fn updates_enabled(&self) -> bool {
        self.2
    }

    /// Communicate with the running [`SenderPoolRunner`] instance
    /// to invoke the request in the specified datacenter.
    pub async fn invoke_in_dc<R: tl::RemoteCall>(
        &self,
        dc_id: i32,
        request: &R,
    ) -> Result<R::Return, InvocationError> {
        self.do_invoke_in_dc(dc_id, request.to_bytes())
            .await
            .and_then(|body| R::Return::from_bytes(&body).map_err(|e| e.into()))
    }

    /// Communicate with the running [`SenderPoolRunner`] instance
    /// to invoke the serialized request body in the specified datacenter.
    pub async fn raw_invoke_in_dc(
        &self,
        dc_id: i32,
        body: Vec<u8>,
    ) -> Result<InvokeResponse, InvocationError> {
        self.raw_invoke_shared_in_dc(dc_id, body.into()).await
    }

    /// Invoke an immutable prepared body without copying its payload.
    /// Clone a [`crate::RequestBody`] to reuse a serialized request across calls.
    /// Like `raw_invoke_in_dc`, this method does not apply the retry policy.
    /// Dropped callers are discarded before serialization when possible; an
    /// already serialized/sent RPC cannot be revoked by dropping its future.
    /// Established connections receive calls directly, bypassing the pool queue.
    /// Only a call rejected before queueing may fall back to connection routing;
    /// a lost reply is returned as an error and never replayed here.
    /// Raw bodies must contain a constructor, have a length divisible by four,
    /// and fit the encrypted message limit before compression, including any
    /// automatic wrapper. Invalid lengths fail before routing or connecting.
    pub async fn raw_invoke_shared_in_dc(
        &self,
        dc_id: i32,
        body: Bytes,
    ) -> Result<InvokeResponse, InvocationError> {
        self.invoke_shared(dc_id, body, None).await
    }

    /// Invoke with an exclusive reservation held by the SDK until retirement, even
    /// if the waiting caller times out. Does not add an automatic retry.
    pub async fn raw_invoke_shared_in_dc_tracked(
        &self,
        dc_id: i32,
        body: Bytes,
        permit: crate::InvocationPermit,
    ) -> Result<InvokeResponse, InvocationError> {
        self.invoke_shared(dc_id, body, Some(permit)).await
    }

    async fn invoke_shared(
        &self,
        dc_id: i32,
        body: Bytes,
        permit: Option<crate::InvocationPermit>,
    ) -> Result<InvokeResponse, InvocationError> {
        self.start_invoke_shared(dc_id, body, permit)?.await
    }

    /// Enqueue a tracked request immediately, returning a movable response waiter.
    /// The caller supplies its own deadline; no task or timer is spawned here.
    pub fn start_raw_invoke_shared_in_dc_tracked(
        &self,
        dc_id: i32,
        body: Bytes,
        permit: crate::InvocationPermit,
    ) -> Result<crate::PendingInvocation, InvocationError> {
        self.start_invoke_shared(dc_id, body, Some(permit))
    }

    fn start_invoke_shared(
        &self,
        dc_id: i32,
        body: Bytes,
        permit: Option<crate::InvocationPermit>,
    ) -> Result<crate::PendingInvocation, InvocationError> {
        if body.len() < 4 {
            return Err(tl::deserialize::Error::UnexpectedEof.into());
        }
        // Apply the framer's uncompressed limit, including automatic wrappers,
        // before malformed input can panic or stall a shared sender task.
        if !mtp::Encrypted::request_fits(&body, self.2) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "serialized request is unaligned or exceeds the encrypted message size limit",
            )
            .into());
        }
        let permit = if let Some(capacity) = &self.3.capacity {
            let capacity = Arc::clone(capacity).try_acquire_owned().map_err(|_| {
                self.3.rejected.fetch_add(1, Ordering::Relaxed);
                InvocationError::Io(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "pending request limit reached",
                ))
            })?;
            Some(crate::InvocationPermit::with_capacity(permit, capacity))
        } else {
            permit
        };
        let (tx, rx) = oneshot::channel();
        let rpc = Rpc { body, tx, permit };
        let rpc = match self.try_route(dc_id, rpc)? {
            None => return Ok(crate::PendingInvocation { receiver: rx }),
            Some(rpc) => rpc,
        };
        self.0
            .send(Request::Invoke {
                dc_id,
                body: rpc.body,
                tx: rpc.tx,
                permit: rpc.permit,
            })
            .map_err(|_| InvocationError::Dropped("SenderPoolRunner was dropped at send"))?;
        Ok(crate::PendingInvocation { receiver: rx })
    }

    fn try_route(&self, dc_id: i32, rpc: Rpc) -> Result<Option<Rpc>, InvocationError> {
        if self.0.is_closed() {
            return Err(InvocationError::Dropped(
                "SenderPoolRunner is closed or stopping",
            ));
        }
        let route = {
            let cache = self.1.read().unwrap_or_else(|e| e.into_inner());
            if cache.stopping || cache.parked {
                return Err(InvocationError::Dropped(
                    "SenderPoolRunner is closed or stopping",
                ));
            }
            cache
                .entries
                .iter()
                .find(|(id, _)| *id == dc_id)
                .map(|(_, tx)| tx.clone())
        };
        match route {
            // SendError gives back an RPC that was NOT queued. Only this case
            // may fall back to the pool; a lost reply must never trigger replay.
            Some(route) => match route.send(rpc) {
                Ok(()) => Ok(None),
                Err(error) => Ok(Some(error.0)),
            },
            None => Ok(Some(rpc)),
        }
    }

    /// Communicate with the running [`SenderPoolRunner`] instance
    /// to drop any active connections to the given datacenter.
    ///
    /// Also cancels pending initialization for this datacenter.
    /// Has no effect if no established or initializing connection exists.
    ///
    /// This is useful after datacenter migrations during sign in,
    /// when the old connection is known to not be needed anymore.
    pub fn disconnect_from_dc(&self, dc_id: i32) -> bool {
        {
            let mut cache = self.1.write().unwrap_or_else(|e| e.into_inner());
            cache.begin_disconnect(dc_id);
            cache.control_changed.notify_waiters();
        }
        self.0.send(Request::Disconnect { dc_id }).is_ok()
    }

    /// Communicate with the running [`SenderPoolRunner`] instance
    /// to drop all active connections and gracefully stop running.
    pub fn quit(&self) -> bool {
        {
            let mut cache = self.1.write().unwrap_or_else(|e| e.into_inner());
            cache.stopping = true;
            cache.entries.clear();
            cache.control_changed.notify_waiters();
        }
        self.0.send(Request::Quit).is_ok()
    }

    /// do [`Self::raw_invoke_in_dc`] with [`Self::check_retry`]
    pub(crate) async fn do_invoke_in_dc(
        &self,
        dc_id: i32,
        request_body: Vec<u8>,
    ) -> Result<Vec<u8>, InvocationError> {
        let request_body = Bytes::from(request_body);
        let mut fail_count = NonZeroU32::new(1).unwrap();
        let mut slept_so_far = Duration::default();

        loop {
            match self
                .raw_invoke_shared_in_dc(dc_id, request_body.clone())
                .await
            {
                Ok(response) => break Ok(response),
                Err(e) => match self.check_retry(e, fail_count, slept_so_far).await {
                    ControlFlow::Continue(delay) => {
                        sleep(delay).await;
                        fail_count = fail_count.saturating_add(1);
                        slept_so_far += delay;
                        continue;
                    }
                    ControlFlow::Break(final_error) => break Err(final_error),
                },
            }
        }
    }

    /// Communicate with the running [`SenderPoolRunner`] instance
    /// to check whether the request needs to retry.
    async fn check_retry(
        &self,
        error: InvocationError,
        fail_count: NonZeroU32,
        slept_so_far: Duration,
    ) -> ControlFlow<InvocationError, Duration> {
        // Preserve local validation/admission/closure errors without enqueueing
        // policy work into an inactive runner.
        let inactive = {
            let cache = self.1.read().unwrap_or_else(|e| e.into_inner());
            cache.parked || cache.stopping
        };
        if inactive
            || matches!(error, InvocationError::Io(ref e) if e.kind() == std::io::ErrorKind::WouldBlock)
        {
            return ControlFlow::Break(error);
        }
        let (tx, rx) = oneshot::channel();
        if self
            .0
            .send(Request::CheckRetry {
                error: Box::new(error),
                fail_count,
                slept_so_far,
                tx,
            })
            .is_err()
        {
            return ControlFlow::Break(InvocationError::Dropped(
                "SenderPoolRunner was dropped during retry at send",
            ));
        }

        match rx.await {
            Ok(flow) => flow,
            Err(_) => ControlFlow::Break(InvocationError::Dropped(
                "SenderPooLRunner was dropped during retry during receive",
            )),
        }
    }
}

impl SenderPool {
    /// Creates a new sender pool instance with default configuration,
    /// attached to the given session and using the provided
    /// [Application Identifier](https://core.telegram.org/myapp)
    /// belonging to the developer.
    ///
    /// Session instance **should not** be reused by multiple pools at the same time.
    /// The session instance will only be used to query datacenter options and persist
    /// any permanent Authorization Keys generated for previously-unconncected datacenters.
    pub fn new<S>(session: Arc<S>, api_id: i32) -> Self
    where
        S: Session + Sized,
        S::Error: std::error::Error + Send + Sync + 'static,
    {
        Self::with_configuration(session, api_id, Default::default())
    }

    /// Creates a new sender pool with non-[`ConnectionParams::default`] configuration.
    pub fn with_configuration<S>(
        session: Arc<S>,
        api_id: i32,
        connection_params: ConnectionParams,
    ) -> Self
    where
        S: Session + Sized,
        S::Error: std::error::Error + Send + Sync + 'static,
    {
        Self::with_erased_session(erase(session), api_id, connection_params)
    }

    /// Construct a pool around already type-erased session storage.
    pub fn with_erased_session(
        session: Arc<ErasedSession>,
        api_id: i32,
        connection_params: ConnectionParams,
    ) -> Self {
        Self::construct(session, api_id, connection_params, false)
    }

    /// Construct a pool that rejects invocations until [`SenderPoolHandle::activate`].
    /// No tasks, network calls or authorization are started here.
    pub fn parked(
        session: Arc<ErasedSession>,
        api_id: i32,
        connection_params: ConnectionParams,
    ) -> Self {
        Self::construct(session, api_id, connection_params, true)
    }

    fn construct(
        session: Arc<ErasedSession>,
        api_id: i32,
        connection_params: ConnectionParams,
        parked: bool,
    ) -> Self {
        let receive_updates = connection_params.receive_updates;
        let (request_tx, request_rx) = queue::unbounded_channel();
        let routes = Arc::new(RwLock::new(RouteCache {
            parked,
            ..Default::default()
        }));
        let state = Arc::new(PoolState {
            // Oversized low-level limits saturate at Tokio's supported maximum.
            // The managed builder reports these as configuration errors.
            capacity: connection_params
                .max_pending_requests
                .map(|n| Arc::new(Semaphore::new(n.get().min(Semaphore::MAX_PERMITS)))),
            prefer_ipv6: connection_params.use_ipv6,
            ..Default::default()
        });
        let (updates_tx, updates_rx) =
            mpsc::channel(connection_params.updates_channel_capacity.get());

        Self {
            runner: SenderPoolRunner {
                session: Arc::clone(&session),
                api_id,
                connection_params,
                request_rx,
                updates_tx: receive_updates.then_some(updates_tx),
                connections: Vec::new(),
                connection_pool: JoinSet::new(),
                routes: Arc::clone(&routes),
                state: Arc::clone(&state),
            },
            handle: SenderPoolFatHandle {
                thin: SenderPoolHandle(request_tx, routes, receive_updates, state),
                session: Arc::clone(&session),
                api_id,
            },
            updates: updates_rx,
        }
    }
}

impl SenderPoolRunner {
    /// Run the sender pool until [`SenderPoolHandle::quit`] is called or the returned future is dropped.
    ///
    /// Connections will be initiated on-demand whenever the first request to a datacenter is made.
    pub fn run(self) -> impl Future<Output = ()> {
        self.run_then(|outcome| {
            if let Err(error) = outcome {
                warn!(%error, "sender pool stopped unexpectedly");
            }
        })
    }

    /// Run with observable internal task failures. Connection I/O failures are
    /// reported as events and to their callers; a later call may reconnect.
    pub fn run_checked(self) -> impl Future<Output = Result<(), PoolRunError>> {
        self.run_then(|outcome| outcome)
    }

    // Not `async fn`, nor one future awaiting another: either keeps a second
    // copy of the runner in a future that lives as long as the pool.
    #[expect(
        clippy::manual_async_fn,
        reason = "an async fn would store the runner twice"
    )]
    fn run_then<T>(
        mut self,
        finish: impl FnOnce(Result<(), PoolRunError>) -> T,
    ) -> impl Future<Output = T> {
        async move {
            let outcome = {
                let mut driving = std::pin::pin!(self.drive());
                poll_fn(
                    |cx| match catch_unwind(AssertUnwindSafe(|| driving.as_mut().poll(cx))) {
                        Ok(Poll::Pending) => Poll::Pending,
                        Ok(Poll::Ready(result)) => Poll::Ready(Ok(result)),
                        Err(_) => Poll::Ready(Err(())),
                    },
                )
                .await
            };
            let mut failure = match outcome {
                Ok(result) => result.err(),
                Err(()) => {
                    let error = PoolRunError::TaskPanicked;
                    self.state.emit(PoolEvent::RunnerFailed(error));
                    Some(error)
                }
            };
            self.close_routes();
            self.request_rx.close();
            // Retire queued permits before their response channels are dropped.
            while self.request_rx.try_recv().is_ok() {}
            self.connections.clear();
            if failure.is_some() {
                self.connection_pool.abort_all();
            }
            while let Some(completion) = self.connection_pool.join_next().await {
                if let Err(error) = self.record_completion(completion) {
                    failure = Some(error);
                }
            }
            finish(failure.map_or(Ok(()), Err))
        }
    }

    async fn drive(&mut self) -> Result<(), PoolRunError> {
        loop {
            tokio::select! {
                biased;
                completion = self.connection_pool.join_next(), if !self.connection_pool.is_empty() => {
                    if let Some(Err(error)) = completion.map(|completion| self.record_completion(completion)) {
                        return Err(error);
                    }
                }
                request = self.request_rx.recv() => {
                    if let Some(request) = request {
                        if self.process_request(request).await.is_break() { return Ok(()); }
                    } else { return Ok(()); }
                }
            }
        }
    }

    fn record_completion(
        &mut self,
        completion: Result<(i32, Result<(), ReadError>), tokio::task::JoinError>,
    ) -> Result<(), PoolRunError> {
        self.connections
            .retain(|connection| !connection.abort_handle.is_finished());
        self.state
            .active
            .store(self.connections.len(), Ordering::Relaxed);
        self.routes
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .entries
            .retain(|(_, tx)| !tx.is_closed());
        match completion {
            Ok((dc_id, result)) => {
                let category = result
                    .err()
                    .map(|error| InvocationError::from(error).category());
                self.state
                    .emit(PoolEvent::ConnectionClosed { dc_id, category });
                Ok(())
            }
            Err(error) if error.is_panic() => {
                let error = PoolRunError::TaskPanicked;
                self.state.emit(PoolEvent::RunnerFailed(error));
                Err(error)
            }
            Err(_) => Ok(()), // explicit disconnect/termination cancellation
        }
    }

    fn close_routes(&self) {
        let mut cache = self.routes.write().unwrap_or_else(|e| e.into_inner());
        cache.stopping = true;
        cache.entries.clear();
        cache.control_changed.notify_waiters();
    }

    fn report_setup_failure(&self, dc_id: i32, error: &InvocationError) {
        self.state.emit(PoolEvent::SetupFailed {
            dc_id,
            category: error.category(),
        });
    }

    async fn process_request(&mut self, request: Request) -> ControlFlow<()> {
        match request {
            Request::Invoke {
                dc_id,
                body,
                mut tx,
                permit,
            } => {
                if tx.is_closed() {
                    return ControlFlow::Continue(());
                }
                let connection = match self
                    .connections
                    .iter()
                    .find(|connection| connection.dc_id == dc_id && !connection.rpc_tx.is_closed())
                {
                    Some(connection) => connection,
                    // Setup is boxed: inline, its handshake state would size
                    // every idle runner task for the life of the pool.
                    None => match tokio::select! {
                        biased;
                        _ = tx.closed() => return ControlFlow::Continue(()),
                        _ = wait_for_connection_cancel(Arc::clone(&self.routes), dc_id) => Err(InvocationError::Dropped("connection initialization was cancelled")),
                        result = Box::pin(self.create_connection(dc_id)) => result,
                    } {
                        Ok(x) => x,
                        Err(e) => {
                            self.report_setup_failure(dc_id, &e);
                            drop(permit);
                            let _ = tx.send(Err(e));
                            return ControlFlow::Continue(());
                        }
                    },
                };
                if !tx.is_closed() {
                    let _ = connection.rpc_tx.send(Rpc { body, tx, permit });
                }
                ControlFlow::Continue(())
            }
            Request::CheckRetry {
                error,
                fail_count,
                slept_so_far,
                tx,
            } => {
                let retry_context = RetryContext {
                    fail_count,
                    slept_so_far,
                    error: *error,
                };
                let flow = match self
                    .connection_params
                    .retry_policy
                    .should_retry(&retry_context)
                {
                    ControlFlow::Continue(delay) => {
                        info!(
                            "retry approved for {} after {:?}",
                            retry_context.error, delay
                        );
                        ControlFlow::Continue(delay)
                    }
                    ControlFlow::Break(()) => ControlFlow::Break(retry_context.error),
                };
                let _ = tx.send(flow);
                ControlFlow::Continue(())
            }
            Request::Disconnect { dc_id } => {
                {
                    let mut cache = self.routes.write().unwrap_or_else(|e| e.into_inner());
                    cache.finish_disconnect(dc_id);
                }
                self.connections.retain(|connection| {
                    if connection.dc_id == dc_id {
                        connection.abort_handle.abort();
                        self.state.emit(PoolEvent::ConnectionClosed {
                            dc_id,
                            category: None,
                        });
                        false
                    } else {
                        true
                    }
                });
                self.state
                    .active
                    .store(self.connections.len(), Ordering::Relaxed);
                ControlFlow::Continue(())
            }
            Request::Quit => ControlFlow::Break(()),
        }
    }

    async fn create_connection(&mut self, dc_id: i32) -> Result<&ConnectionInfo, InvocationError> {
        let mut dc_option = match self.session.dc_option(dc_id)? {
            Some(x) => x,
            None => return Err(InvocationError::InvalidDc),
        };

        let limit = self.connection_params.connection_timeout;
        let connecting = self.connect_sender(&dc_option);
        let sender = if let Some(limit) = limit {
            tokio::time::timeout(limit, connecting)
                .await
                .map_err(|_| {
                    InvocationError::Io(std::io::Error::new(
                        std::io::ErrorKind::TimedOut,
                        "connection initialization deadline elapsed",
                    ))
                })??
        } else {
            connecting.await?
        };

        dc_option.auth_key = Some(sender.auth_key());
        self.session.set_dc_option(&dc_option).await?;

        let (rpc_tx, rpc_rx) = queue::unbounded_channel();
        let updates = self.updates_tx.clone();
        let session = Arc::clone(&self.session);
        let state = Arc::clone(&self.state);
        let abort_handle = self.connection_pool.spawn(async move {
            (
                dc_id,
                run_sender(sender, rpc_rx, updates, dc_id, session, state).await,
            )
        });
        {
            let mut cache = self.routes.write().unwrap_or_else(|e| e.into_inner());
            cache.publish(dc_id, &rpc_tx);
        }
        self.connections.push(ConnectionInfo {
            dc_id,
            rpc_tx,
            abort_handle,
        });
        self.state
            .active
            .store(self.connections.len(), Ordering::Relaxed);
        self.state.emit(PoolEvent::Connected { dc_id });
        Ok(self.connections.last().unwrap())
    }

    async fn connect_sender(
        &mut self,
        dc_option: &DcOption,
    ) -> Result<Sender<Transport, mtp::Encrypted>, InvocationError> {
        let transport = Transport::new;

        let address = if self.connection_params.use_ipv6 {
            dc_option.ipv6.into()
        } else {
            dc_option.ipv4.into()
        };

        #[cfg(feature = "proxy")]
        let addr = || {
            if let Some(proxy) = self.connection_params.proxy_url.clone() {
                ServerAddr::Proxied { address, proxy }
            } else {
                ServerAddr::Tcp { address }
            }
        };
        #[cfg(not(feature = "proxy"))]
        let addr = || ServerAddr::Tcp { address };

        let init_connection = tl::functions::InvokeWithLayer {
            layer: tl::LAYER,
            query: tl::functions::InitConnection {
                api_id: self.api_id,
                device_model: self.connection_params.device_model.clone(),
                system_version: self.connection_params.system_version.clone(),
                app_version: self.connection_params.app_version.clone(),
                system_lang_code: self.connection_params.system_lang_code.clone(),
                lang_pack: "".into(),
                lang_code: self.connection_params.lang_code.clone(),
                proxy: None,
                params: None,
                query: tl::functions::help::GetConfig {},
            },
        };

        let mut sender = if let Some(auth_key) = dc_option.auth_key {
            connect_with_auth(transport(), addr(), auth_key)
                .await
                .map_err(InvocationError::Io)?
        } else {
            connect(transport(), addr()).await?
        };

        sender.set_receive_updates(self.connection_params.receive_updates);
        sender.set_ack_batch(self.connection_params.ack_batch);
        let enums::Config::Config(remote_config) = match sender.invoke(&init_connection).await {
            Ok(config) => config,
            Err(InvocationError::Transport(transport::Error::BadStatus { status: 404 })) => {
                sender = connect(transport(), addr()).await?;
                sender.set_receive_updates(self.connection_params.receive_updates);
                sender.set_ack_batch(self.connection_params.ack_batch);
                sender.invoke(&init_connection).await?
            }
            Err(e) => return Err(e),
        };

        self.update_config(remote_config).await?;

        Ok(sender)
    }

    async fn update_config(&self, config: tl::types::Config) -> Result<(), InvocationError> {
        self.update_dc_options(&config.dc_options).await
    }

    async fn update_dc_options(
        &self,
        options: &[tl::enums::DcOption],
    ) -> Result<(), InvocationError> {
        for option in options
            .iter()
            .map(|tl::enums::DcOption::Option(o)| o)
            .filter(|o| !o.media_only && !o.tcpo_only && o.r#static)
        {
            validate_dc_endpoint(option)?;
        }
        for option in options
            .iter()
            .map(|tl::enums::DcOption::Option(option)| option)
            .filter(|option| !option.media_only && !option.tcpo_only && option.r#static)
        {
            let mut dc_option = self
                .session
                .dc_option(option.id)?
                .unwrap_or_else(|| DcOption {
                    id: option.id,
                    ipv4: SocketAddrV4::new(Ipv4Addr::from_bits(0), 0),
                    ipv6: SocketAddrV6::new(Ipv6Addr::from_bits(0), 0, 0, 0),
                    auth_key: None,
                });
            if option.ipv6 {
                dc_option.ipv6 = SocketAddrV6::new(
                    option.ip_address.parse().map_err(|_| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "invalid datacenter IPv6 address",
                        )
                    })?,
                    u16::try_from(option.port).map_err(|_| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "invalid datacenter port",
                        )
                    })?,
                    0,
                    0,
                );
            } else {
                dc_option.ipv4 = SocketAddrV4::new(
                    option.ip_address.parse().map_err(|_| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "invalid datacenter IPv4 address",
                        )
                    })?,
                    u16::try_from(option.port).map_err(|_| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            "invalid datacenter port",
                        )
                    })?,
                );
                if dc_option.ipv6.ip().to_bits() == 0 {
                    dc_option.ipv6 = SocketAddrV6::new(
                        dc_option.ipv4.ip().to_ipv6_mapped(),
                        dc_option.ipv4.port(),
                        0,
                        0,
                    )
                }
            }
            self.session.set_dc_option(&dc_option).await?;
        }
        Ok(())
    }
}

fn validate_dc_endpoint(option: &tl::types::DcOption) -> std::io::Result<()> {
    let valid_ip = if option.ipv6 {
        option.ip_address.parse::<Ipv6Addr>().is_ok()
    } else {
        option.ip_address.parse::<Ipv4Addr>().is_ok()
    };
    if !valid_ip || !(1..=65535).contains(&option.port) {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "invalid datacenter endpoint",
        ));
    }
    Ok(())
}

impl Drop for SenderPoolRunner {
    fn drop(&mut self) {
        if std::thread::panicking() {
            self.state
                .emit(PoolEvent::RunnerFailed(PoolRunError::TaskPanicked));
        }
        self.close_routes();
        self.state.active.store(0, Ordering::Relaxed);
        self.state.stopped.store(true, Ordering::Release);
        // JoinSet aborts all children when dropped, including cancellation of run().
    }
}

// Only cold connection setup subscribes. Established requests add no wakeup.
async fn wait_for_connection_cancel(routes: Routes, dc_id: i32) {
    let changed = Arc::clone(
        &routes
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .control_changed,
    );
    loop {
        let notified = changed.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        {
            let cache = routes.read().unwrap_or_else(|e| e.into_inner());
            if cache.stopping || cache.disconnecting.iter().any(|(id, _)| *id == dc_id) {
                return;
            }
        }
        notified.await;
    }
}

#[derive(Debug)]
enum UpdateDeliveryError {
    Full,
    Closed,
}

fn forward_update(
    updates: &mpsc::Sender<UpdatesLike>,
    state: &PoolState,
    update: UpdatesLike,
) -> Result<(), UpdateDeliveryError> {
    match updates.try_send(update) {
        Ok(()) => Ok(()),
        Err(mpsc::error::TrySendError::Full(_)) => {
            state.dropped.fetch_add(1, Ordering::Relaxed);
            state.emit(PoolEvent::UpdateOverflow);
            Err(UpdateDeliveryError::Full)
        }
        Err(mpsc::error::TrySendError::Closed(_)) => Err(UpdateDeliveryError::Closed),
    }
}

// Lives as long as the connection; see `footprint_tests`.
#[expect(
    clippy::manual_async_fn,
    reason = "an async fn would store the sender twice"
)]
fn run_sender(
    mut sender: Sender<Transport, grammers_mtproto::mtp::Encrypted>,
    mut rpc_rx: queue::UnboundedReceiver<Rpc>,
    updates: Option<mpsc::Sender<UpdatesLike>>,
    dc_id: i32,
    session: Arc<ErasedSession>,
    state: Arc<PoolState>,
) -> impl Future<Output = Result<(), ReadError>> {
    async move {
        let mut dropped: usize = 0;
        let mut last_update_limit_warn = Instant::now();
        let mut updates_closed = false;

        loop {
            tokio::select! {
                step = sender.step() => match step {
                    Ok(all_new_updates) => {
                        let Some(updates) = updates.as_ref().filter(|_| !updates_closed) else {
                            continue;
                        };
                        for new_updates in all_new_updates {
                            if let Err(e) = forward_update(updates, &state, new_updates) {
                                match e {
                                    UpdateDeliveryError::Full => {
                                        dropped += 1;
                                        if last_update_limit_warn.elapsed() >= UPDATES_DROPPED_LOG_COOLDOWN {
                                            warn!(
                                                capacity = updates.max_capacity(),
                                                dropped_count = dropped,
                                                last_time = ?last_update_limit_warn.elapsed(),
                                                "updates channel full",
                                            );
                                            dropped = 0;
                                            last_update_limit_warn = Instant::now();
                                        }
                                    }
                                    UpdateDeliveryError::Closed => {
                                        debug!("updates channel closed, stopping update forwarding");
                                        updates_closed = true;
                                        break;
                                    }
                                }
                            }
                        }
                    },
                    Err(err) => {
                        // Login migration can promote a connection created while
                        // another DC was home. Classify only at failure, without a
                        // lookup on the RPC path or recreating a live sender.
                        // If storage fails, still wake consumers so they can surface
                        // that failure instead of waiting indefinitely for updates.
                        let home_sender = session.home_dc_id().map_or(true, |home| home == dc_id);
                        if let (true, Some(updates)) = (home_sender, &updates) {
                            let _ = forward_update(updates, &state, UpdatesLike::ConnectionClosed);
                        }
                        break Err(err)
                    },
                },
                rpc = rpc_rx.recv() => match rpc {
                    Some(rpc) => sender.enqueue_tracked_body(rpc.body, rpc.tx, rpc.permit),
                    None => break Ok(()),
                },
            }
        }
    }
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invoke {
                dc_id, body, tx, ..
            } => f
                .debug_struct("Invoke")
                .field("dc_id", dc_id)
                .field(
                    "request",
                    &body[..4]
                        .try_into()
                        .map(|constructor_id| tl::name_for_id(u32::from_le_bytes(constructor_id)))
                        .unwrap_or("?"),
                )
                .field("tx", tx)
                .finish(),
            Self::CheckRetry {
                error,
                fail_count,
                slept_so_far,
                tx,
            } => f
                .debug_struct("CheckRetry")
                .field("error", error)
                .field("fail_count", fail_count)
                .field("slept_so_far", slept_so_far)
                .field("tx", tx)
                .finish(),
            Self::Disconnect { dc_id } => {
                f.debug_struct("Disconnect").field("dc_id", dc_id).finish()
            }
            Self::Quit => write!(f, "Quit"),
        }
    }
}

#[cfg(test)]
mod optimization_tests {
    use super::*;

    #[tokio::test]
    async fn public_checked_and_legacy_runners_contain_policy_panics() {
        struct PanicPolicy;
        impl crate::RetryPolicy for PanicPolicy {
            fn should_retry(&self, _: &RetryContext) -> ControlFlow<(), Duration> {
                panic!("private policy fixture")
            }
        }
        for checked in [true, false] {
            let pool = SenderPool::with_configuration(
                Arc::new(grammers_session::storages::MemorySession::default()),
                1,
                ConnectionParams {
                    retry_policy: Box::new(PanicPolicy),
                    ..Default::default()
                },
            );
            let handle = pool.handle.thin.clone();
            let call = tokio::spawn(async move {
                handle
                    .invoke_in_dc(999, &tl::functions::Ping { ping_id: 0 })
                    .await
            });
            let runner = tokio::spawn(async move {
                if checked {
                    pool.runner.run_checked().await
                } else {
                    pool.runner.run().await;
                    Ok(())
                }
            });
            let outcome = runner.await;
            assert!(call.await.unwrap().is_err());
            assert!(pool.handle.status().stopped);
            if checked {
                assert!(
                    matches!(outcome, Ok(Err(PoolRunError::TaskPanicked))),
                    "{outcome:?}"
                );
            } else {
                assert!(matches!(outcome, Ok(Ok(()))), "{outcome:?}");
            }
        }
    }

    #[tokio::test]
    async fn invalid_remote_config_reports_protocol_category_to_observers() {
        fn as_invocation(error: impl Into<InvocationError>) -> InvocationError {
            error.into()
        }
        let pool = SenderPool::new(
            Arc::new(grammers_session::storages::MemorySession::default()),
            1,
        );
        let mut events = pool.handle.events();
        let option = tl::types::DcOption {
            ipv6: false,
            media_only: false,
            tcpo_only: false,
            cdn: false,
            r#static: true,
            this_port_only: false,
            id: 90,
            ip_address: "invalid".into(),
            port: 443,
            secret: None,
        };
        let error = as_invocation(
            pool.runner
                .update_dc_options(&[option.into()])
                .await
                .unwrap_err(),
        );
        pool.runner.report_setup_failure(90, &error);
        assert_eq!(
            events.recv().await.unwrap(),
            PoolEvent::SetupFailed {
                dc_id: 90,
                category: InvocationErrorCategory::Protocol
            }
        );
        assert!(
            matches!(error, InvocationError::Io(e) if e.kind() == std::io::ErrorKind::InvalidData)
        );
    }

    #[tokio::test]
    async fn admission_covers_warm_raw_and_tracked_entry_points() {
        let pool = SenderPool::with_configuration(
            Arc::new(grammers_session::storages::MemorySession::default()),
            1,
            ConnectionParams {
                max_pending_requests: std::num::NonZeroUsize::new(1),
                ..Default::default()
            },
        );
        let (tx, mut rx) = queue::unbounded_channel();
        pool.handle.1.write().unwrap().publish(4, &tx);
        let held = pool
            .handle
            .start_invoke_shared(4, Bytes::from_static(&[1; 4]), None)
            .unwrap();
        let rpc = rx.recv().await.unwrap();
        rpc.permit.as_ref().unwrap().mark_sent();
        drop(held);
        let tracker = crate::InvocationTracker::default();
        let result = pool
            .handle
            .raw_invoke_shared_in_dc_tracked(
                4,
                Bytes::from_static(&[1; 4]),
                tracker.try_acquire().unwrap(),
            )
            .await;
        assert!(
            matches!(result, Err(InvocationError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock)
        );
        assert_eq!(tracker.stage(), crate::InvocationStage::Idle);
        assert!(
            matches!(pool.handle.raw_invoke_in_dc(4, vec![1; 4]).await, Err(InvocationError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock)
        );
        assert!(
            matches!(pool.handle.raw_invoke_shared_in_dc(4, Bytes::from_static(&[1; 4])).await, Err(InvocationError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock)
        );
        assert!(
            pool.handle
                .start_raw_invoke_shared_in_dc_tracked(
                    4,
                    Bytes::from_static(&[1; 4]),
                    tracker.try_acquire().unwrap()
                )
                .is_err()
        );
        let rejected = pool.handle.status().admission_rejections;
        assert!(matches!(
            pool.handle.raw_invoke_in_dc(4, vec![1; 3]).await,
            Err(InvocationError::Deserialize(_))
        ));
        assert_eq!(pool.handle.status().admission_rejections, rejected);
        drop(rpc);
        assert!(
            pool.handle
                .start_raw_invoke_shared_in_dc_tracked(
                    4,
                    Bytes::from_static(&[1; 4]),
                    tracker.try_acquire().unwrap()
                )
                .is_ok()
        );
    }

    #[tokio::test]
    async fn invalid_dc_config_is_rejected_before_any_storage_write() {
        let session = Arc::new(grammers_session::storages::MemorySession::default());
        let pool = SenderPool::new(Arc::clone(&session), 1);
        let valid = tl::types::DcOption {
            ipv6: false,
            media_only: false,
            tcpo_only: false,
            cdn: false,
            r#static: true,
            this_port_only: false,
            id: 90,
            ip_address: "127.0.0.1".into(),
            port: 443,
            secret: None,
        };
        for (ip, port) in [
            ("invalid", 443),
            ("127.0.0.1", 0),
            ("127.0.0.1", -1),
            ("127.0.0.1", 65536),
        ] {
            let invalid = tl::types::DcOption {
                id: 91,
                ip_address: ip.into(),
                port,
                ..valid.clone()
            };
            let error = pool
                .runner
                .update_dc_options(&[valid.clone().into(), invalid.into()])
                .await
                .unwrap_err();
            assert_eq!(
                match error {
                    InvocationError::Io(e) => e.kind(),
                    other => panic!("unexpected error: {other:?}"),
                },
                std::io::ErrorKind::InvalidData
            );
            assert!(session.dc_option(90).unwrap().is_none());
        }
    }

    #[tokio::test]
    async fn overflow_is_counted_and_observer_lag_is_explicit() {
        let state = PoolState::default();
        let mut events = state.subscribe();
        let (tx, _rx) = mpsc::channel(1);
        forward_update(&tx, &state, UpdatesLike::ConnectionClosed).unwrap();
        for _ in 0..65 {
            assert!(forward_update(&tx, &state, UpdatesLike::ConnectionClosed).is_err());
        }
        assert_eq!(state.dropped.load(Ordering::Relaxed), 65);
        assert!(matches!(
            events.recv().await,
            Err(broadcast::error::RecvError::Lagged(1))
        ));
        assert_eq!(events.recv().await.unwrap(), PoolEvent::UpdateOverflow);
    }

    #[tokio::test]
    async fn bounded_admission_survives_waiter_cancellation() {
        let mut pool = SenderPool::with_configuration(
            Arc::new(grammers_session::storages::MemorySession::default()),
            1,
            ConnectionParams {
                max_pending_requests: std::num::NonZeroUsize::new(1),
                ..Default::default()
            },
        );
        let first = pool
            .handle
            .start_invoke_shared(4, Bytes::from_static(&[1; 4]), None)
            .unwrap();
        let request = pool.runner.request_rx.recv().await.unwrap();
        drop(first);
        assert!(
            matches!(pool.handle.start_invoke_shared(4, Bytes::from_static(&[1; 4]), None), Err(InvocationError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock)
        );
        assert_eq!(pool.handle.status().admission_rejections, 1);
        drop(request);
        assert!(
            pool.handle
                .start_invoke_shared(4, Bytes::from_static(&[1; 4]), None)
                .is_ok()
        );
    }

    #[tokio::test]
    async fn parked_pool_rejects_requests_before_activation() {
        let pool = SenderPool::parked(
            erase(Arc::new(
                grammers_session::storages::MemorySession::default(),
            )),
            1,
            ConnectionParams::default(),
        );
        assert!(matches!(
            pool.handle
                .start_invoke_shared(4, Bytes::from_static(&[1; 4]), None),
            Err(InvocationError::Dropped(_))
        ));
        assert!(pool.handle.activate());
        assert!(
            pool.handle
                .start_invoke_shared(4, Bytes::from_static(&[1; 4]), None)
                .is_ok()
        );
        pool.handle.quit();
        assert!(!pool.handle.activate());
    }

    #[tokio::test]
    async fn checked_runner_classifies_child_panic_and_clears_routes() {
        let mut pool = SenderPool::new(
            Arc::new(grammers_session::storages::MemorySession::default()),
            1,
        );
        pool.runner
            .connection_pool
            .spawn(async { panic!("private panic data") });
        let result = pool.runner.run_checked().await;
        assert_eq!(result, Err(PoolRunError::TaskPanicked));
        assert!(pool.handle.status().stopped);
    }

    #[tokio::test]
    async fn regression_cancelled_cold_call_does_not_block_pool_shutdown() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let session = Arc::new(grammers_session::storages::MemorySession::default());
        session
            .set_dc_option(&DcOption {
                id: 4,
                ipv4: match address {
                    std::net::SocketAddr::V4(a) => a,
                    _ => unreachable!(),
                },
                ipv6: SocketAddrV6::new(Ipv6Addr::LOCALHOST, address.port(), 0, 0),
                auth_key: Some([0; 256]),
            })
            .await
            .unwrap();
        let pool = SenderPool::with_configuration(session, 1, ConnectionParams::default());
        let handle = pool.handle.thin;
        let mut runner = tokio::spawn(pool.runner.run());
        let tracker = crate::InvocationTracker::default();
        let call = handle
            .start_raw_invoke_shared_in_dc_tracked(
                4,
                Bytes::from_static(&[1; 4]),
                tracker.try_acquire().unwrap(),
            )
            .unwrap();
        let _peer = tokio::time::timeout(Duration::from_secs(1), listener.accept())
            .await
            .unwrap()
            .unwrap();
        drop(call);
        assert!(handle.quit());
        let stopped = tokio::time::timeout(Duration::from_secs(1), &mut runner).await;
        if stopped.is_err() {
            runner.abort();
        }
        assert!(
            stopped.is_ok(),
            "cancelled initialization kept the pool blocked"
        );
        assert_eq!(tracker.stage(), crate::InvocationStage::Idle);
    }

    #[tokio::test]
    async fn regression_quit_interrupts_cold_initialization_with_a_live_caller() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let session = Arc::new(grammers_session::storages::MemorySession::default());
        session
            .set_dc_option(&DcOption {
                id: 4,
                ipv4: match address {
                    std::net::SocketAddr::V4(a) => a,
                    _ => unreachable!(),
                },
                ipv6: SocketAddrV6::new(Ipv6Addr::LOCALHOST, address.port(), 0, 0),
                auth_key: Some([0; 256]),
            })
            .await
            .unwrap();
        let pool = SenderPool::with_configuration(session, 1, ConnectionParams::default());
        let handle = pool.handle.thin;
        let mut runner = tokio::spawn(pool.runner.run());
        let tracker = crate::InvocationTracker::default();
        let call = handle
            .start_raw_invoke_shared_in_dc_tracked(
                4,
                Bytes::from_static(&[1; 4]),
                tracker.try_acquire().unwrap(),
            )
            .unwrap();
        let _peer = tokio::time::timeout(Duration::from_secs(1), listener.accept())
            .await
            .unwrap()
            .unwrap();
        assert!(handle.quit());
        let stopped = tokio::time::timeout(Duration::from_secs(1), &mut runner).await;
        if stopped.is_err() {
            runner.abort();
        }
        assert!(
            stopped.is_ok(),
            "cancelled initialization kept the pool blocked"
        );
        assert!(matches!(call.await, Err(InvocationError::Dropped(_))));
        assert_eq!(tracker.stage(), crate::InvocationStage::Idle);
    }

    #[tokio::test]
    async fn connection_cancel_wakeup_is_scoped_and_cannot_be_missed() {
        let routes = Routes::default();
        let first = tokio::spawn(wait_for_connection_cancel(Arc::clone(&routes), 4));
        tokio::task::yield_now().await;
        {
            let mut cache = routes.write().unwrap();
            cache.begin_disconnect(2);
            cache.control_changed.notify_waiters();
        }
        tokio::task::yield_now().await;
        assert!(!first.is_finished());
        {
            let mut cache = routes.write().unwrap();
            cache.begin_disconnect(4);
            cache.control_changed.notify_waiters();
        }
        tokio::time::timeout(Duration::from_secs(1), first)
            .await
            .unwrap()
            .unwrap();
        // Registration after the wake still sees the published state.
        tokio::time::timeout(
            Duration::from_secs(1),
            wait_for_connection_cancel(routes, 4),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn connection_initialization_deadline_releases_tracked_request() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let session = Arc::new(grammers_session::storages::MemorySession::default());
        session
            .set_dc_option(&DcOption {
                id: 4,
                ipv4: match address {
                    std::net::SocketAddr::V4(a) => a,
                    _ => unreachable!(),
                },
                ipv6: SocketAddrV6::new(Ipv6Addr::LOCALHOST, address.port(), 0, 0),
                auth_key: Some([0; 256]),
            })
            .await
            .unwrap();
        let pool = SenderPool::with_configuration(
            session,
            1,
            ConnectionParams {
                receive_updates: false,
                connection_timeout: Some(Duration::from_millis(100)),
                ..ConnectionParams::default()
            },
        );
        let handle = pool.handle.thin;
        let runner = tokio::spawn(pool.runner.run());
        let tracker = crate::InvocationTracker::default();
        let call = handle
            .start_raw_invoke_shared_in_dc_tracked(
                4,
                Bytes::from_static(&[1; 4]),
                tracker.try_acquire().unwrap(),
            )
            .unwrap();
        let _peer = tokio::time::timeout(Duration::from_secs(1), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let result = tokio::time::timeout(Duration::from_secs(1), call)
            .await
            .unwrap();
        assert!(
            matches!(result, Err(InvocationError::Io(e)) if e.kind() == std::io::ErrorKind::TimedOut)
        );
        assert_eq!(tracker.stage(), crate::InvocationStage::Idle);
        assert!(handle.quit());
        tokio::time::timeout(Duration::from_secs(1), runner)
            .await
            .unwrap()
            .unwrap();
    }

    fn fake_handle() -> (SenderPoolHandle, queue::UnboundedReceiver<Request>) {
        let (tx, rx) = queue::unbounded_channel();
        (
            SenderPoolHandle(tx, Routes::default(), true, Arc::default()),
            rx,
        )
    }

    #[test]
    fn dropped_queued_requests_release_reservations_before_waking_waiters() {
        use std::{
            future::Future,
            sync::atomic::{AtomicU8, Ordering},
            task::{Context, Wake, Waker},
        };
        struct ObserveWake(crate::InvocationTracker, AtomicU8);
        impl Wake for ObserveWake {
            fn wake(self: Arc<Self>) {
                self.wake_by_ref();
            }
            fn wake_by_ref(self: &Arc<Self>) {
                self.1.store(
                    if self.0.stage() == crate::InvocationStage::Idle {
                        1
                    } else {
                        2
                    },
                    Ordering::SeqCst,
                );
            }
        }
        for pooled in [false, true] {
            let tracker = crate::InvocationTracker::default();
            let permit = tracker.try_acquire();
            let (tx, rx) = oneshot::channel();
            let wake = Arc::new(ObserveWake(tracker.clone(), AtomicU8::new(0)));
            let waker = Waker::from(Arc::clone(&wake));
            let mut cx = Context::from_waker(&waker);
            let mut rx = Box::pin(rx);
            assert!(rx.as_mut().poll(&mut cx).is_pending());
            if pooled {
                drop(Request::Invoke {
                    dc_id: 4,
                    body: Bytes::from_static(&[1; 4]),
                    tx,
                    permit,
                });
            } else {
                drop(Rpc {
                    body: Bytes::from_static(&[1; 4]),
                    tx,
                    permit,
                });
            }
            assert_eq!(
                wake.1.load(Ordering::SeqCst),
                1,
                "queued receiver woke before retirement"
            );
            assert!(rx.as_mut().poll(&mut cx).is_ready());
            assert!(tracker.try_acquire().is_some());
        }
    }

    #[tokio::test]
    async fn disabled_update_configuration_closes_channel_and_rejects_stream_creation() {
        let session = Arc::new(grammers_session::storages::MemorySession::default());
        let config = ConnectionParams {
            receive_updates: false,
            ..ConnectionParams::default()
        };
        let mut pool = SenderPool::with_configuration(session, 1, config);
        assert!(!pool.handle.updates_enabled());
        assert!(pool.updates.recv().await.is_none());
        let stream = crate::UpdatesReceiver::create(
            pool.handle.thin,
            pool.handle.session,
            pool.updates,
            crate::UpdatesConfiguration { catch_up: true },
        )
        .await;
        assert!(stream.is_err());
        assert!(matches!(
            pool.runner.request_rx.try_recv(),
            Err(queue::TryRecvError::Disconnected)
        ));
        assert!(ConnectionParams::default().receive_updates);
    }

    #[tokio::test]
    async fn connection_errors_still_return_without_emitting_disabled_update_events() {
        for enabled in [true, false] {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let (sender, peer) = tokio::join!(
                connect_with_auth(Transport::new(), ServerAddr::Tcp { address }, [0; 256]),
                listener.accept(),
            );
            let mut sender = sender.unwrap();
            sender.set_receive_updates(enabled);
            let (_rpc_tx, rpc_rx) = queue::unbounded_channel();
            let (updates_tx, mut updates_rx) = mpsc::channel(1);
            let updates = enabled.then_some(updates_tx);
            drop(peer.unwrap());
            let result = tokio::time::timeout(
                Duration::from_secs(1),
                run_sender(
                    sender,
                    rpc_rx,
                    updates,
                    2,
                    erase(Arc::new(
                        grammers_session::storages::MemorySession::default(),
                    )),
                    Arc::default(),
                ),
            )
            .await
            .unwrap();
            assert!(result.is_err());
            if enabled {
                assert!(matches!(
                    updates_rx.recv().await,
                    Some(UpdatesLike::ConnectionClosed)
                ));
            } else {
                assert!(updates_rx.recv().await.is_none());
            }
        }
    }

    #[tokio::test]
    async fn preexisting_connections_classify_failures_using_the_current_home_dc() {
        // QR imports open the destination before committing it as home. Keep a
        // real established local transport alive across the session transition,
        // then fail it without issuing another RPC to wake the update stream.
        for connection_dc in [4, 2] {
            let session = Arc::new(grammers_session::storages::MemorySession::default());
            session.set_home_dc_id(2).await.unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let (sender, peer) = tokio::join!(
                connect_with_auth(Transport::new(), ServerAddr::Tcp { address }, [0; 256]),
                listener.accept(),
            );
            let sender = sender.unwrap();
            let peer = peer.unwrap();
            let (_rpc_tx, rpc_rx) = queue::unbounded_channel();
            let (updates_tx, mut updates_rx) = mpsc::channel(1);
            let shared_session = erase(Arc::clone(&session));
            let running = tokio::spawn(run_sender(
                sender,
                rpc_rx,
                Some(updates_tx),
                connection_dc,
                shared_session,
                Arc::default(),
            ));
            tokio::task::yield_now().await;
            assert!(!running.is_finished());

            session.set_home_dc_id(4).await.unwrap();
            drop(peer);
            assert!(
                tokio::time::timeout(Duration::from_secs(1), running)
                    .await
                    .unwrap()
                    .unwrap()
                    .is_err()
            );
            if connection_dc == 4 {
                assert!(
                    matches!(updates_rx.recv().await, Some(UpdatesLike::ConnectionClosed)),
                    "a promoted destination failure must wake passive update consumers"
                );
            } else {
                assert!(
                    updates_rx.recv().await.is_none(),
                    "a former home must no longer be classified as the current home"
                );
            }
        }
    }

    #[tokio::test]
    async fn warm_route_bypasses_pool_and_keeps_shared_payload() {
        let (handle, mut pool_rx) = fake_handle();
        let (tx, mut rx) = queue::unbounded_channel();
        handle.1.write().unwrap().publish(4, &tx);
        let body = Bytes::from(vec![1; 152]);
        let ptr = body.as_ptr() as usize;
        let call = tokio::spawn(async move { handle.raw_invoke_shared_in_dc(4, body).await });
        let rpc = tokio::time::timeout(Duration::from_secs(1), rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(rpc.body.as_ptr() as usize, ptr);
        assert!(matches!(
            pool_rx.try_recv(),
            Err(queue::TryRecvError::Empty)
        ));
        rpc.tx.send(Ok(vec![7; 4])).unwrap();
        assert_eq!(call.await.unwrap().unwrap(), [7; 4]);
    }

    #[tokio::test]
    async fn rejected_queue_send_falls_back_once_but_lost_reply_does_not() {
        let (handle, mut pool_rx) = fake_handle();
        let (tx, rx) = queue::unbounded_channel();
        handle.1.write().unwrap().publish(4, &tx);
        drop(rx);
        let task_handle = handle.clone();
        let call = tokio::spawn(async move {
            task_handle
                .raw_invoke_shared_in_dc(4, Bytes::from_static(&[1; 4]))
                .await
        });
        let Request::Invoke { dc_id, tx, .. } = pool_rx.recv().await.unwrap() else {
            panic!("fallback");
        };
        assert_eq!(dc_id, 4);
        let (new_tx, mut new_rx) = queue::unbounded_channel();
        handle.1.write().unwrap().publish(4, &new_tx);
        tx.send(Ok(vec![2; 4])).unwrap();
        assert_eq!(call.await.unwrap().unwrap(), [2; 4]);

        let task_handle = handle.clone();
        let call = tokio::spawn(async move {
            task_handle
                .raw_invoke_shared_in_dc(4, Bytes::from_static(&[1; 4]))
                .await
        });
        let accepted = new_rx.recv().await.unwrap();
        drop(accepted.tx);
        assert!(matches!(
            call.await.unwrap(),
            Err(InvocationError::Dropped(_))
        ));
        assert!(matches!(
            pool_rx.try_recv(),
            Err(queue::TryRecvError::Empty)
        ));
    }

    #[test]
    fn pending_disconnects_block_stale_route_publication() {
        let (handle, mut rx) = fake_handle();
        let (tx, _rpc_rx) = queue::unbounded_channel();
        {
            let mut cache = handle.1.write().unwrap();
            cache.publish(2, &tx);
            cache.publish(4, &tx);
        }
        assert!(handle.disconnect_from_dc(4));
        assert!(handle.disconnect_from_dc(4));
        assert!(matches!(
            rx.try_recv(),
            Ok(Request::Disconnect { dc_id: 4 })
        ));
        assert!(matches!(
            rx.try_recv(),
            Ok(Request::Disconnect { dc_id: 4 })
        ));
        let mut cache = handle.1.write().unwrap();
        cache.publish(4, &tx);
        assert_eq!(cache.entries.len(), 1);
        assert_eq!(cache.entries[0].0, 2);
        cache.finish_disconnect(4);
        cache.publish(4, &tx);
        assert_eq!(cache.entries.len(), 1);
        cache.finish_disconnect(4);
        cache.publish(4, &tx);
        assert_eq!(cache.entries.len(), 2);
    }

    #[tokio::test]
    async fn warm_route_cancellation_keeps_the_response_channel_cancelled() {
        let (handle, _pool_rx) = fake_handle();
        let (tx, mut rx) = queue::unbounded_channel();
        handle.1.write().unwrap().publish(4, &tx);
        let call = tokio::spawn(async move {
            handle
                .raw_invoke_shared_in_dc(4, Bytes::from_static(&[1; 4]))
                .await
        });
        let rpc = rx.recv().await.unwrap();
        call.abort();
        assert!(call.await.unwrap_err().is_cancelled());
        assert!(rpc.tx.is_closed());
    }

    #[tokio::test]
    async fn quit_releases_cached_channels_and_stops_connection_tasks() {
        let session = Arc::new(grammers_session::storages::MemorySession::default());
        let pool = SenderPool::with_configuration(session, 1, ConnectionParams::default());
        let handle = pool.handle.thin;
        let mut runner = pool.runner;
        let (tx, mut rx) = queue::unbounded_channel::<Rpc>();
        runner.routes.write().unwrap().publish(4, &tx);
        let abort_handle = runner.connection_pool.spawn(async move {
            while let Some(rpc) = rx.recv().await {
                let _ = rpc.tx.send(Ok(vec![1; 4]));
            }
            (4, Ok(()))
        });
        runner.connections.push(ConnectionInfo {
            dc_id: 4,
            rpc_tx: tx,
            abort_handle,
        });
        let task = tokio::spawn(runner.run());
        assert!(handle.quit());
        // Even if connection creation completes during quit, it cannot republish.
        let (late_tx, _late_rx) = queue::unbounded_channel();
        handle.1.write().unwrap().publish(4, &late_tx);
        assert!(handle.1.read().unwrap().entries.is_empty());
        assert!(matches!(
            handle
                .raw_invoke_shared_in_dc(4, Bytes::from_static(&[1; 4]))
                .await,
            Err(InvocationError::Dropped(_))
        ));
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn stopped_pool_rejects_even_a_still_open_cached_route() {
        let (handle, pool_rx) = fake_handle();
        let (tx, mut rx) = queue::unbounded_channel();
        handle.1.write().unwrap().publish(4, &tx);
        drop(pool_rx);
        assert!(matches!(
            handle
                .raw_invoke_shared_in_dc(4, Bytes::from_static(&[1; 4]))
                .await,
            Err(InvocationError::Dropped(_))
        ));
        assert!(matches!(rx.try_recv(), Err(queue::TryRecvError::Empty)));
    }

    #[test]
    #[ignore = "isolated routing benchmark; no sockets or Telegram RPCs"]
    fn benchmark_warm_connection_route() {
        for threads in [1, 4] {
            let runtime = if threads == 1 {
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap()
            } else {
                tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(threads)
                    .enable_all()
                    .build()
                    .unwrap()
            };
            runtime.block_on(async {
                let (handle, mut pool_rx) = fake_handle();
                let (rpc_tx, mut rpc_rx) = queue::unbounded_channel::<Rpc>();
                let forwarding_tx = rpc_tx.clone();
                let forwarder = tokio::spawn(async move {
                    while let Some(Request::Invoke { body, tx, .. }) = pool_rx.recv().await {
                        assert!(
                            forwarding_tx
                                .send(Rpc {
                                    body,
                                    tx,
                                    permit: None
                                })
                                .is_ok()
                        );
                    }
                });
                let responder = tokio::spawn(async move {
                    while let Some(rpc) = rpc_rx.recv().await {
                        let _ = rpc.tx.send(Ok(vec![1; 4]));
                    }
                });
                let mut samples = [Vec::new(), Vec::new()];
                for round in 0..8 {
                    for direct in [round % 2 == 0, round % 2 != 0] {
                        if direct {
                            handle.1.write().unwrap().publish(4, &rpc_tx);
                        } else {
                            handle.1.write().unwrap().entries.clear();
                        }
                        let started = Instant::now();
                        for _ in 0..20_000 {
                            std::hint::black_box(
                                handle
                                    .raw_invoke_shared_in_dc(4, Bytes::from_static(&[1; 4]))
                                    .await
                                    .unwrap(),
                            );
                        }
                        samples[usize::from(direct)]
                            .push(started.elapsed().as_nanos() as f64 / 20_000.0);
                    }
                }
                for values in &mut samples {
                    values.sort_by(f64::total_cmp);
                }
                println!(
                    "Routing threads={threads} median: pool {:.1} ns, direct {:.1} ns",
                    samples[0][4], samples[1][4]
                );
                forwarder.abort();
                responder.abort();
                let _ = forwarder.await;
                let _ = responder.await;
            });
        }
    }

    #[tokio::test]
    async fn malformed_raw_bodies_fail_before_reaching_the_runner() {
        let (tx, mut rx) = queue::unbounded_channel();
        let handle = SenderPoolHandle(tx, Routes::default(), true, Arc::default());
        for len in 0..4 {
            assert!(matches!(
                handle.raw_invoke_in_dc(4, vec![0; len]).await,
                Err(InvocationError::Deserialize(_))
            ));
            assert!(matches!(
                handle
                    .raw_invoke_shared_in_dc(4, Bytes::from(vec![0; len]))
                    .await,
                Err(InvocationError::Deserialize(_))
            ));
        }
        assert!(matches!(rx.try_recv(), Err(queue::TryRecvError::Empty)));
    }

    #[tokio::test]
    async fn unaligned_raw_bodies_do_not_reach_cold_or_warm_senders() {
        for warm in [false, true] {
            let (handle, mut pool_rx) = fake_handle();
            let (tx, mut direct_rx) = queue::unbounded_channel();
            if warm {
                handle.1.write().unwrap().publish(4, &tx);
            }
            for len in [5, 6, 7, 513, 514, 515] {
                let is_invalid = |outcome: Result<_, InvocationError>| {
                    assert!(matches!(
                        outcome,
                        Err(InvocationError::Io(error))
                            if error.kind() == std::io::ErrorKind::InvalidInput
                    ));
                };
                let tracker = crate::InvocationTracker::default();
                let result = handle.start_raw_invoke_shared_in_dc_tracked(
                    4,
                    Bytes::from(vec![0; len]),
                    tracker.try_acquire().unwrap(),
                );
                assert!(matches!(
                    result,
                    Err(InvocationError::Io(error))
                        if error.kind() == std::io::ErrorKind::InvalidInput
                ));
                assert_eq!(tracker.stage(), crate::InvocationStage::Idle);
                is_invalid(handle.raw_invoke_in_dc(4, vec![0; len]).await);
                is_invalid(
                    handle
                        .raw_invoke_shared_in_dc(4, Bytes::from(vec![0; len]))
                        .await,
                );
                is_invalid(
                    handle
                        .raw_invoke_shared_in_dc_tracked(
                            4,
                            Bytes::from(vec![0; len]),
                            tracker.try_acquire().unwrap(),
                        )
                        .await,
                );
                assert_eq!(tracker.stage(), crate::InvocationStage::Idle);
            }
            assert!(matches!(
                pool_rx.try_recv(),
                Err(queue::TryRecvError::Empty)
            ));
            assert!(matches!(
                direct_rx.try_recv(),
                Err(queue::TryRecvError::Empty)
            ));
            assert!(!handle.0.is_closed());
        }
    }

    #[tokio::test]
    async fn oversized_raw_bodies_fail_before_routing_and_exact_limits_are_accepted() {
        use tl::Identifiable;
        for receive_updates in [false, true] {
            for warm in [false, true] {
                let (mut handle, mut pool_rx) = fake_handle();
                handle.2 = receive_updates;
                let (tx, mut direct_rx) = queue::unbounded_channel();
                if warm {
                    handle.1.write().unwrap().publish(4, &tx);
                }
                for constructor in [
                    tl::functions::help::GetConfig::CONSTRUCTOR_ID,
                    tl::functions::Ping::CONSTRUCTOR_ID,
                    tl::functions::InvokeWithoutUpdates::<tl::functions::help::GetConfig>::CONSTRUCTOR_ID,
                ] {
                    let wrapped = !receive_updates && constructor == tl::functions::help::GetConfig::CONSTRUCTOR_ID;
                    let limit = 1_044_432 - if wrapped { 4 } else { 0 };
                    let mut body = vec![0; limit];
                    body[..4].copy_from_slice(&constructor.to_le_bytes());
                    let tracker = crate::InvocationTracker::default();
                    let pending = handle.start_raw_invoke_shared_in_dc_tracked(
                        4, Bytes::copy_from_slice(&body), tracker.try_acquire().unwrap(),
                    ).expect("exact maximum must enter the queue");
                    if warm {
                        let rpc = direct_rx.try_recv().unwrap();
                        assert_eq!(rpc.body.len(), limit);
                        drop(rpc);
                    } else {
                        let request = pool_rx.try_recv().unwrap();
                        assert!(matches!(&request, Request::Invoke { body, .. } if body.len() == limit));
                        drop(request);
                    }
                    assert_eq!(tracker.stage(), crate::InvocationStage::Idle);
                    drop(pending);
                    body.extend_from_slice(&[0; 4]);
                    let result = handle.start_raw_invoke_shared_in_dc_tracked(
                        4, Bytes::copy_from_slice(&body), tracker.try_acquire().unwrap(),
                    );
                    assert!(matches!(result, Err(InvocationError::Io(error)) if error.kind() == std::io::ErrorKind::InvalidInput));
                    assert_eq!(tracker.stage(), crate::InvocationStage::Idle);
                    for result in [
                        handle.raw_invoke_in_dc(4, body.clone()).await,
                        handle.raw_invoke_shared_in_dc(4, Bytes::copy_from_slice(&body)).await,
                        handle.raw_invoke_shared_in_dc_tracked(4, Bytes::from(body), tracker.try_acquire().unwrap()).await,
                    ] {
                        assert!(matches!(result, Err(InvocationError::Io(error)) if error.kind() == std::io::ErrorKind::InvalidInput));
                    }
                    assert_eq!(tracker.stage(), crate::InvocationStage::Idle);
                }
                assert!(matches!(
                    pool_rx.try_recv(),
                    Err(queue::TryRecvError::Empty)
                ));
                assert!(matches!(
                    direct_rx.try_recv(),
                    Err(queue::TryRecvError::Empty)
                ));
            }
        }
    }

    #[tokio::test]
    async fn shared_and_owned_request_apis_preserve_the_payload_allocation() {
        for shared in [false, true] {
            let (tx, mut rx) = queue::unbounded_channel();
            let handle = SenderPoolHandle(tx, Routes::default(), true, Arc::default());
            let body = vec![1, 0, 0, 0];
            let address = body.as_ptr() as usize;
            let call = tokio::spawn(async move {
                if shared {
                    handle.raw_invoke_shared_in_dc(4, body.into()).await
                } else {
                    handle.raw_invoke_in_dc(4, body).await
                }
            });
            let Request::Invoke {
                body, tx, dc_id, ..
            } = rx.recv().await.unwrap()
            else {
                panic!("expected invocation");
            };
            assert_eq!(dc_id, 4);
            assert_eq!(body.as_ptr() as usize, address);
            tx.send(Ok(vec![2, 0, 0, 0])).unwrap();
            assert_eq!(call.await.unwrap().unwrap(), [2, 0, 0, 0]);
        }
    }

    #[tokio::test]
    async fn rejected_retry_does_not_format_the_original_error() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        #[derive(Debug)]
        struct CountDisplay(Arc<AtomicUsize>);
        impl fmt::Display for CountDisplay {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                self.0.fetch_add(1, Ordering::Relaxed);
                f.write_str("fixture error")
            }
        }
        impl std::error::Error for CountDisplay {}
        let (handle, mut rx) = fake_handle();
        let formatted = Arc::new(AtomicUsize::new(0));
        let call = tokio::spawn(async move { handle.do_invoke_in_dc(4, vec![1; 4]).await });
        let Request::Invoke { tx, .. } = rx.recv().await.unwrap() else {
            panic!("invoke");
        };
        tx.send(Err(InvocationError::Io(std::io::Error::other(
            CountDisplay(Arc::clone(&formatted)),
        ))))
        .unwrap();
        let Request::CheckRetry { error, tx, .. } = rx.recv().await.unwrap() else {
            panic!("retry");
        };
        tx.send(ControlFlow::Break(*error)).unwrap();
        assert!(matches!(call.await.unwrap(), Err(InvocationError::Io(_))));
        assert_eq!(formatted.load(Ordering::Relaxed), 0);
    }

    #[tokio::test]
    async fn typed_retry_reuses_request_storage_and_keeps_retry_policy() {
        let (tx, mut rx) = queue::unbounded_channel();
        let handle = SenderPoolHandle(tx, Routes::default(), true, Arc::default());
        let call = tokio::spawn(async move { handle.do_invoke_in_dc(4, vec![1, 0, 0, 0]).await });
        let Request::Invoke {
            body: first, tx, ..
        } = rx.recv().await.unwrap()
        else {
            panic!("invoke");
        };
        tx.send(Err(InvocationError::Dropped("test cancellation")))
            .unwrap();
        let Request::CheckRetry { tx, fail_count, .. } = rx.recv().await.unwrap() else {
            panic!("policy");
        };
        assert_eq!(fail_count.get(), 1);
        tx.send(ControlFlow::Continue(Duration::ZERO)).unwrap();
        let Request::Invoke {
            body: second, tx, ..
        } = rx.recv().await.unwrap()
        else {
            panic!("retry");
        };
        assert_eq!(first.as_ptr(), second.as_ptr());
        assert_eq!(first, second);
        tx.send(Ok(vec![3, 0, 0, 0])).unwrap();
        assert_eq!(call.await.unwrap().unwrap(), [3, 0, 0, 0]);
    }

    #[test]
    #[ignore = "isolated payload clone benchmark; run release with nocapture"]
    fn benchmark_prepared_request_clones() {
        use std::hint::black_box;
        const N: usize = 500_000;
        for size in [152, 1024, 8192] {
            let owned = vec![7u8; size];
            let shared = Bytes::from(owned.clone());
            let mut owned_ns = Vec::new();
            let mut shared_ns = Vec::new();
            for round in 0..10 {
                for share in [round % 2 == 0, round % 2 != 0] {
                    let started = Instant::now();
                    for _ in 0..N {
                        if share {
                            black_box(black_box(&shared).clone());
                        } else {
                            black_box(black_box(&owned).clone());
                        }
                    }
                    let elapsed = started.elapsed().as_nanos() as f64 / N as f64;
                    if share {
                        shared_ns.push(elapsed);
                    } else {
                        owned_ns.push(elapsed);
                    }
                }
            }
            owned_ns.sort_by(f64::total_cmp);
            shared_ns.sort_by(f64::total_cmp);
            println!(
                "{size}-byte request clone median: Vec {:.1} ns, shared {:.1} ns",
                owned_ns[5], shared_ns[5]
            );
        }
    }
}

#[cfg(test)]
mod footprint_tests {
    use super::*;

    /// Connection and runner futures live as long as their pool, once per
    /// session. Guard against storing owned state twice (`async fn`
    /// arguments) and against inlining the connection handshake.
    #[tokio::test]
    async fn long_lived_futures_hold_their_state_once() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, _peer) = tokio::join!(
            connect_with_auth(Transport::new(), ServerAddr::Tcp { address }, [0; 256]),
            listener.accept()
        );
        let sender = sender.unwrap();
        let sender_size = size_of_val(&sender);
        let (_tx, rx) = queue::unbounded_channel();
        let session = erase(Arc::new(
            grammers_session::storages::MemorySession::default(),
        ));
        let connection = run_sender(sender, rx, None, 2, session, Arc::default());
        assert!(
            size_of_val(&connection) < 2 * sender_size,
            "connection future {} B for a {sender_size} B sender",
            size_of_val(&connection)
        );

        let pool = SenderPool::new(
            Arc::new(grammers_session::storages::MemorySession::default()),
            1,
        );
        let runner = pool.runner.run();
        assert!(
            size_of_val(&runner) < 2048,
            "runner future {} B; connection setup must stay boxed",
            size_of_val(&runner)
        );
        // Queue slots preallocated by Tokio channels were sized by these.
        assert!(size_of::<Request>() <= size_of::<Rpc>() + 8);
    }
}
