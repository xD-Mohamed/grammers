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
use std::sync::{Arc, Mutex, OnceLock, RwLock};
use std::task::{Context, Poll, Wake, Waker};
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

/// Serialized size reserved for a typed request before it is known.
const INITIAL_REQUEST_CAPACITY: usize = 256;

/// A connection surviving this long ends any run of failed attempts to its datacenter.
const STABLE_CONNECTION: Duration = Duration::from_secs(30);
const CONNECT_RETRY_DELAY_LIMIT: Duration = Duration::from_secs(30);

/// How long another connection waits after `failures` in a row. The first retry is not
/// delayed, since connections do end normally; repeated ones must not hammer the server
/// whatever the caller's own retry loop does. TDLib's `ConnectionCreator` allows about
/// one attempt a second, then spaces them further.
fn connect_retry_delay(failures: u32) -> Duration {
    match failures {
        0 | 1 => Duration::ZERO,
        failures => {
            Duration::from_secs(1u64 << (failures - 2).min(5)).min(CONNECT_RETRY_DELAY_LIMIT)
        }
    }
}

/// Recent connection attempts to one datacenter.
struct ConnectPacing {
    dc_id: i32,
    failures: u32,
    not_before: tokio::time::Instant,
    connected_at: Option<tokio::time::Instant>,
}

/// How long to wait after warning the user that the updates was dropped.
const UPDATES_DROPPED_LOG_COOLDOWN: Duration = Duration::from_secs(300);

/// The lightest framing: one length byte for packets under 508 bytes, four
/// above, and no checksum (TCP already has one). `Full` adds twelve bytes
/// and a CRC32 to every packet in both directions.
#[cfg(not(feature = "proxy"))]
pub(crate) type Transport = transport::Abridged;
#[cfg(feature = "proxy")]
pub(crate) type Transport = crate::ProxyTransport;

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

struct PendingConnection {
    dc_id: i32,
    requests: Vec<Rpc>,
    abort_handle: AbortHandle,
    watched: usize,
    closure: Arc<ClosureWake>,
    #[cfg(test)]
    closure_polls: usize,
}

#[derive(Default)]
struct ClosureWake {
    dirty: AtomicBool,
    runner: Mutex<Option<Waker>>,
}

impl Wake for ClosureWake {
    fn wake(self: Arc<Self>) {
        self.wake_by_ref();
    }

    fn wake_by_ref(self: &Arc<Self>) {
        self.dirty.store(true, Ordering::Release);
        let runner = self.runner.lock().unwrap().take();
        if let Some(runner) = runner {
            runner.wake();
        }
    }
}

impl PendingConnection {
    fn poll_closed(&mut self, cx: &mut Context<'_>) -> bool {
        {
            let mut runner = self.closure.runner.lock().unwrap();
            if runner.as_ref().is_none_or(|w| !w.will_wake(cx.waker())) {
                *runner = Some(cx.waker().clone());
            }
        }
        // Register before checking the flag so a concurrent close cannot be
        // missed. Arrivals only poll the new suffix; a closure wake rescans.
        let start = if self.closure.dirty.swap(false, Ordering::AcqRel) {
            0
        } else {
            self.watched
        };
        let waker = Waker::from(Arc::clone(&self.closure));
        let mut context = Context::from_waker(&waker);
        #[cfg(test)]
        {
            self.closure_polls += self.requests.len() - start;
        }
        self.requests
            .extract_if(start.., |rpc| rpc.tx.poll_closed(&mut context).is_ready())
            .for_each(drop);
        self.watched = self.requests.len();
        self.requests.is_empty()
    }
}

impl Drop for PendingConnection {
    fn drop(&mut self) {
        // Response senders may outlive setup; their old closure wakers must
        // not retain the runner task after this pending group is gone.
        self.closure.runner.lock().unwrap().take();
    }
}

struct ConnectionSetup {
    session: Arc<ErasedSession>,
    state: Arc<PoolState>,
    api_id: i32,
    connection_params: Arc<ConnectionParams>,
}

type SetupResult = (
    i32,
    tokio::task::Id,
    Result<Sender<Transport, mtp::Encrypted>, InvocationError>,
);

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
    config_lock: tokio::sync::Mutex<()>,
    home_failures: AtomicU64,
    home_failure: tokio::sync::Notify,
    capacity: Option<Arc<Semaphore>>,
    byte_capacity: Option<Arc<Semaphore>>,
    byte_limit: usize,
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
            config_lock: tokio::sync::Mutex::new(()),
            home_failures: AtomicU64::new(0),
            home_failure: tokio::sync::Notify::new(),
            capacity: None,
            byte_capacity: None,
            byte_limit: usize::MAX,
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
    connection_params: Arc<ConnectionParams>,
    pending: Vec<PendingConnection>,
    setup_pool: JoinSet<SetupResult>,
    request_rx: queue::UnboundedReceiver<Request>,
    updates_tx: Option<mpsc::Sender<UpdatesLike>>,
    connections: Vec<ConnectionInfo>,
    connection_pool: JoinSet<(i32, Result<(), ReadError>)>,
    connect_pacing: Vec<ConnectPacing>,
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
    pub(crate) fn home_failure_generation(&self) -> u64 {
        self.3.home_failures.load(Ordering::Acquire)
    }

    pub(crate) async fn wait_home_failure(&self, generation: u64) {
        loop {
            let notified = self.3.home_failure.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.home_failure_generation() != generation {
                return;
            }
            notified.await;
        }
    }
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
        // Most requests fit, which avoids several reallocations per call.
        let mut body = Vec::with_capacity(INITIAL_REQUEST_CAPACITY);
        tl::Serializable::serialize(request, &mut body);
        self.do_invoke_in_dc(dc_id, body)
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
        if body.len() > self.3.byte_limit {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "request body exceeds the pool byte admission limit",
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
        let permit = if let Some(capacity) = &self.3.byte_capacity {
            let bytes = Arc::clone(capacity)
                .try_acquire_many_owned(body.len() as u32)
                .map_err(|_| {
                    self.3.rejected.fetch_add(1, Ordering::Relaxed);
                    InvocationError::Io(std::io::Error::new(
                        std::io::ErrorKind::WouldBlock,
                        "pending request byte limit reached",
                    ))
                })?;
            Some(crate::InvocationPermit::with_bytes(permit, bytes))
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
            byte_limit: connection_params
                .max_pending_bytes
                .map_or(usize::MAX, |n| n.get().min(Semaphore::MAX_PERMITS)),
            byte_capacity: connection_params
                .max_pending_bytes
                .map(|n| Arc::new(Semaphore::new(n.get().min(Semaphore::MAX_PERMITS)))),
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
                connection_params: Arc::new(connection_params),
                pending: Vec::new(),
                setup_pool: JoinSet::new(),
                request_rx,
                updates_tx: receive_updates.then_some(updates_tx),
                connections: Vec::new(),
                connection_pool: JoinSet::new(),
                connect_pacing: Vec::new(),
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
            self.pending.clear();
            self.setup_pool.abort_all();
            while let Some(result) = self.setup_pool.join_next().await {
                if result.is_err_and(|e| e.is_panic()) {
                    failure = Some(PoolRunError::TaskPanicked);
                }
            }
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
        let mut work = 0;
        loop {
            work += 1;
            if work == 64 {
                tokio::task::yield_now().await;
                work = 0;
            }
            tokio::select! {
                biased;
                setup = self.setup_pool.join_next(), if !self.setup_pool.is_empty() => {
                    match setup {
                        Some(Ok(result)) => self.finish_setup(result),
                        Some(Err(e)) if e.is_panic() => {
                            self.state.emit(PoolEvent::RunnerFailed(PoolRunError::TaskPanicked));
                            return Err(PoolRunError::TaskPanicked);
                        },
                        _ => {},
                    }
                }
                dc_id = poll_fn(|cx| {
                    for pending in &mut self.pending {
                        if pending.poll_closed(cx) { return Poll::Ready(pending.dc_id); }
                    }
                    Poll::Pending
                }), if !self.pending.is_empty() => {
                    self.cancel_setup(dc_id);
                }
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
                // Closing soon after connecting counts as failing to connect: otherwise a
                // server that accepts and then drops connections would never be backed off.
                if !self.forget_failures_if_stable(dc_id) && result.is_err() {
                    self.note_connect_failure(dc_id);
                }
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
                tx,
                permit,
            } => {
                if tx.is_closed() {
                    return ControlFlow::Continue(());
                }
                let rpc = Rpc { body, tx, permit };
                if let Some(connection) = self
                    .connections
                    .iter()
                    .find(|c| c.dc_id == dc_id && !c.rpc_tx.is_closed())
                {
                    // Only a rejected send returns ownership and permits fallback.
                    if let Err(error) = connection.rpc_tx.send(rpc) {
                        self.start_setup(dc_id, error.0);
                    }
                } else {
                    self.start_setup(dc_id, rpc);
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
                self.cancel_setup(dc_id);
                self.forget_failures_if_stable(dc_id);
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

    fn setup_context(&self) -> ConnectionSetup {
        ConnectionSetup {
            session: Arc::clone(&self.session),
            api_id: self.api_id,
            state: Arc::clone(&self.state),
            connection_params: Arc::clone(&self.connection_params),
        }
    }

    fn start_setup(&mut self, dc_id: i32, rpc: Rpc) {
        if let Some(pending) = self.pending.iter_mut().find(|p| p.dc_id == dc_id) {
            pending.requests.push(rpc);
            return;
        }
        let mut setup = self.setup_context();
        let routes = Arc::clone(&self.routes);
        let not_before = self
            .connect_pacing
            .iter()
            .find(|pacing| pacing.dc_id == dc_id)
            .map(|pacing| pacing.not_before);
        let abort_handle = self.setup_pool.spawn(async move {
            let result = tokio::select! {
                biased;
                _ = wait_for_connection_cancel(routes, dc_id) => Err(InvocationError::Dropped("connection initialization was cancelled")),
                result = Box::pin(async {
                    if let Some(not_before) = not_before {
                        tokio::time::sleep_until(not_before).await;
                    }
                    setup.create_connection(dc_id).await
                }) => result,
            };
            (dc_id, tokio::task::id(), result)
        });
        self.pending.push(PendingConnection {
            dc_id,
            requests: vec![rpc],
            abort_handle,
            watched: 0,
            closure: Arc::default(),
            #[cfg(test)]
            closure_polls: 0,
        });
    }

    fn pacing_mut(&mut self, dc_id: i32) -> &mut ConnectPacing {
        let index = match self.connect_pacing.iter().position(|p| p.dc_id == dc_id) {
            Some(index) => index,
            None => {
                // There is one of these per datacenter, and pools can be many.
                self.connect_pacing.reserve_exact(1);
                self.connect_pacing.push(ConnectPacing {
                    dc_id,
                    failures: 0,
                    not_before: tokio::time::Instant::now(),
                    connected_at: None,
                });
                self.connect_pacing.len() - 1
            }
        };
        &mut self.connect_pacing[index]
    }

    fn forget_failures_if_stable(&mut self, dc_id: i32) -> bool {
        let pacing = self.pacing_mut(dc_id);
        let stable = pacing
            .connected_at
            .is_some_and(|since| since.elapsed() >= STABLE_CONNECTION);
        if stable {
            pacing.failures = 0;
        }
        stable
    }

    fn note_connect_failure(&mut self, dc_id: i32) {
        let pacing = self.pacing_mut(dc_id);
        pacing.failures = pacing.failures.saturating_add(1);
        pacing.connected_at = None;
        pacing.not_before = tokio::time::Instant::now() + connect_retry_delay(pacing.failures);
    }

    fn cancel_setup(&mut self, dc_id: i32) {
        if let Some(index) = self.pending.iter().position(|p| p.dc_id == dc_id) {
            self.pending.swap_remove(index).abort_handle.abort();
        }
    }

    fn finish_setup(&mut self, (dc_id, setup_id, result): SetupResult) {
        let Some(index) = self
            .pending
            .iter()
            .position(|p| p.dc_id == dc_id && p.abort_handle.id() == setup_id)
        else {
            return;
        };
        let mut pending = self.pending.swap_remove(index);
        let requests = std::mem::take(&mut pending.requests);
        let sender = match result {
            Ok(sender) => {
                self.pacing_mut(dc_id).connected_at = Some(tokio::time::Instant::now());
                sender
            }
            Err(error) => {
                let error = error.for_unframed_request();
                // Neither of these reached the server.
                if !matches!(
                    error,
                    InvocationError::Dropped(_) | InvocationError::InvalidDc
                ) {
                    self.note_connect_failure(dc_id);
                }
                self.report_setup_failure(dc_id, &error);
                if requests.len() == 1 {
                    let rpc = requests.into_iter().next().unwrap();
                    drop(rpc.permit);
                    let _ = rpc.tx.send(Err(error));
                    return;
                }
                let error = Arc::new(error);
                for rpc in requests {
                    drop(rpc.permit);
                    let _ = rpc
                        .tx
                        .send(Err(InvocationError::Shared(Arc::clone(&error))));
                }
                return;
            }
        };
        let (rpc_tx, rpc_rx) = queue::unbounded_channel();
        for rpc in requests {
            if !rpc.tx.is_closed() {
                let _ = rpc_tx.send(rpc);
            }
        }
        let updates = self.updates_tx.clone();
        let session = Arc::clone(&self.session);
        let state = Arc::clone(&self.state);
        let abort_handle = self
            .connection_pool
            .spawn(run_sender(sender, rpc_rx, updates, dc_id, session, state));
        self.routes
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .publish(dc_id, &rpc_tx);
        self.connections.push(ConnectionInfo {
            dc_id,
            rpc_tx,
            abort_handle,
        });
        self.state
            .active
            .store(self.connections.len(), Ordering::Relaxed);
        self.state.emit(PoolEvent::Connected { dc_id });
    }

    #[cfg(test)]
    async fn update_dc_options(
        &self,
        options: &[tl::enums::DcOption],
    ) -> Result<(), InvocationError> {
        self.setup_context().update_dc_options(options).await
    }
}

impl ConnectionSetup {
    async fn create_connection(
        &mut self,
        dc_id: i32,
    ) -> Result<Sender<Transport, mtp::Encrypted>, InvocationError> {
        let dc_option = self
            .session
            .dc_option(dc_id)?
            .ok_or(InvocationError::InvalidDc)?;
        let limit = self.connection_params.connection_timeout;
        let connecting = self.connect_sender(&dc_option);
        let (sender, remote_config) = if let Some(limit) = limit {
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
        self.finalize_connection(sender, remote_config, dc_option)
            .await
    }

    async fn finalize_connection(
        &self,
        mut sender: Sender<Transport, mtp::Encrypted>,
        remote_config: tl::types::Config,
        dc_option: DcOption,
    ) -> Result<Sender<Transport, mtp::Encrypted>, InvocationError> {
        // The initialization deadline covers network work. Storage and another
        // DC's config merge may take longer and have their own cancellation rules.
        self.update_config(remote_config).await?;
        self.persist_connection_key(&mut sender, dc_option).await?;
        Ok(sender)
    }

    async fn persist_connection_key(
        &self,
        sender: &mut Sender<Transport, mtp::Encrypted>,
        dc_option: DcOption,
    ) -> Result<(), InvocationError> {
        // Concurrent DC setup may refresh endpoints while this handshake runs.
        // Merge under the same lock as config writes so a stale config cannot
        // erase a freshly generated key or overwrite refreshed routes.
        let _guard = self.state.config_lock.lock().await;
        let mut dc_option = self.session.dc_option(dc_option.id)?.unwrap_or(dc_option);
        dc_option.auth_key = Some(sender.auth_key());
        self.session.set_dc_option(&dc_option).await?;
        // Storage/config-lock latency is not evidence that the peer went silent.
        sender.set_liveness_timeout(self.connection_params.liveness_timeout);
        Ok(())
    }

    async fn connect_sender(
        &mut self,
        dc_option: &DcOption,
    ) -> Result<(Sender<Transport, mtp::Encrypted>, tl::types::Config), InvocationError> {
        #[cfg(not(feature = "proxy"))]
        let transport = || Ok::<_, std::io::Error>(Transport::new());
        #[cfg(feature = "proxy")]
        let transport = || match self.connection_params.proxy_url.as_deref() {
            Some(proxy) => {
                Transport::for_connection(proxy, dc_option, self.connection_params.use_ipv6)
            }
            None => Ok(Transport::new()),
        };

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
            connect_with_auth(transport()?, addr(), auth_key)
                .await
                .map_err(InvocationError::Io)?
        } else {
            connect(transport()?, addr()).await?
        };

        sender.set_receive_updates(self.connection_params.receive_updates);
        sender.set_ack_batch(self.connection_params.ack_batch);
        sender.set_decode_limits(self.connection_params.decode_limits);
        sender.set_liveness_timeout(self.connection_params.liveness_timeout);
        let enums::Config::Config(remote_config) = match sender.invoke(&init_connection).await {
            Ok(config) => config,
            Err(error)
                if matches!(
                    error.transport_error(),
                    Some(transport::Error::BadStatus { status: 404 })
                ) =>
            {
                sender = connect(transport()?, addr()).await?;
                sender.set_receive_updates(self.connection_params.receive_updates);
                sender.set_ack_batch(self.connection_params.ack_batch);
                sender.set_decode_limits(self.connection_params.decode_limits);
                sender.set_liveness_timeout(self.connection_params.liveness_timeout);
                sender.invoke(&init_connection).await?
            }
            Err(e) => return Err(e),
        };

        Ok((sender, remote_config))
    }

    async fn update_config(&self, config: tl::types::Config) -> Result<(), InvocationError> {
        self.update_dc_options(&config.dc_options).await
    }

    async fn update_dc_options(
        &self,
        options: &[tl::enums::DcOption],
    ) -> Result<(), InvocationError> {
        let _guard = self.state.config_lock.lock().await;
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
) -> impl Future<Output = (i32, Result<(), ReadError>)> {
    async move {
        let mut dropped: usize = 0;
        let mut last_update_limit_warn = Instant::now();
        let mut updates_closed = false;

        let result = loop {
            tokio::task::consume_budget().await;
            tokio::select! {
                step = sender.step() => match step {
                    Ok(mut all_new_updates) => {
                        let batch = all_new_updates.len();
                        let mut consumer_stalled = false;
                        if let Some(updates) = updates.as_ref().filter(|_| !updates_closed) {
                        for new_updates in all_new_updates.drain(..) {
                            // A burst decoded in one poll can fill the queue before its
                            // consumer has run. Dropping causes a gap, and so a recovery
                            // request to Telegram: yield to the consumer once first.
                            // Boxed so that this future, which lives as long as the connection,
                            // does not hold room for one across the yield. Only once a batch:
                            // RPCs on this connection must not wait for a stalled consumer.
                            let new_updates = match updates.try_send(new_updates) {
                                Err(mpsc::error::TrySendError::Full(new_updates)) => {
                                    Box::new(new_updates)
                                }
                                Err(mpsc::error::TrySendError::Closed(_)) => {
                                    debug!("updates channel closed, stopping update forwarding");
                                    updates_closed = true;
                                    break;
                                }
                                Ok(()) => continue,
                            };
                            if !consumer_stalled {
                                tokio::task::yield_now().await;
                            }
                            if let Err(e) = forward_update(updates, &state, *new_updates) {
                                match e {
                                    UpdateDeliveryError::Full => {
                                        consumer_stalled = true;
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
                        }
                        sender.recycle_updates(all_new_updates, batch);
                        // After delivery: this can yield, and the task can end while it waits.
                        for _ in 0..sender.take_read_charge() {
                            tokio::task::consume_budget().await;
                        }
                    },
                    Err(err) => {
                        // The sender retires framed requests itself. Close the
                        // route before draining calls still owned by this queue:
                        // none of these could have reached the wire.
                        let shared = match err {
                            ReadError::Shared(error) => error,
                            error => Arc::new(error),
                        };
                        rpc_rx.close();
                        while let Ok(rpc) = rpc_rx.try_recv() {
                            drop(rpc.permit);
                            let _ = rpc.tx.send(Err(InvocationError::Read {
                                error: Arc::clone(&shared),
                                may_have_been_sent: false,
                            }));
                        }
                        // Login migration can promote a connection created while
                        // another DC was home. Classify only at failure, without a
                        // lookup on the RPC path or recreating a live sender.
                        // If storage fails, still wake consumers so they can surface
                        // that failure instead of waiting indefinitely for updates.
                        let home_sender = session.home_dc_id().map_or(true, |home| home == dc_id);
                        if let (true, Some(updates)) = (home_sender, &updates) {
                            state.home_failures.fetch_add(1, Ordering::Release);
                            state.home_failure.notify_waiters();
                            let _ = forward_update(updates, &state, UpdatesLike::ConnectionClosed);
                        }
                        break Err(ReadError::Shared(shared))
                    },
                },
                rpc = rpc_rx.recv() => match rpc {
                    Some(rpc) => {
                        sender.enqueue_tracked_body(rpc.body, rpc.tx, rpc.permit);
                        // Pack what is already queued into the next write instead of
                        // one write per request. Each request is still charged, so a
                        // ready queue yields as often as it would one at a time.
                        for _ in 1..READY_RPC_BATCH {
                            let Ok(rpc) = rpc_rx.try_recv() else {
                                break;
                            };
                            sender.enqueue_tracked_body(rpc.body, rpc.tx, rpc.permit);
                            tokio::task::consume_budget().await;
                        }
                    }
                    None => break Ok(()),
                },
            }
        };
        (dc_id, result)
    }
}

// Measured with the loopback overload scenario; see PERFORMANCE.md.
const READY_RPC_BATCH: usize = 8;

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
    use tl::Serializable;

    #[tokio::test]
    async fn ready_connection_queue_yields_before_draining_its_backlog() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (sender, peer) = tokio::join!(
            connect_with_auth(
                Transport::new(),
                ServerAddr::Tcp {
                    address: listener.local_addr().unwrap()
                },
                [0; 256]
            ),
            listener.accept()
        );
        let _peer = peer.unwrap().0;
        let (tx, rx) = queue::unbounded_channel();
        let mut responses = Vec::new();
        for _ in 0..512 {
            let (tx_result, rx_result) = oneshot::channel();
            // Invalid bodies fail synchronously without socket progress, so
            // the ready queue itself must not monopolize the runtime.
            tx.send(Rpc {
                body: Bytes::from_static(&[0; 3]),
                permit: None,
                tx: tx_result,
            })
            .unwrap();
            responses.push(rx_result);
        }
        let session = erase(Arc::new(
            grammers_session::storages::MemorySession::default(),
        ));
        let runner = run_sender(sender.unwrap(), rx, None, 2, session, Arc::default());
        tokio::pin!(runner);
        tokio::select! { biased;
            _ = &mut runner => panic!("open connection should remain active"),
            _ = std::future::ready(()) => {}
        }
        let mut completed = 0;
        let mut pending = false;
        for result in &mut responses {
            match result.try_recv() {
                Ok(Err(InvocationError::Io(error))) => {
                    assert_eq!(error.kind(), std::io::ErrorKind::InvalidInput);
                    assert!(!pending, "requests must retire in queue order");
                    completed += 1;
                }
                Err(oneshot::error::TryRecvError::Empty) => pending = true,
                other => panic!("unexpected request outcome: {other:?}"),
            }
        }
        // Batching must not stretch the runtime's quantum. Uncharged batches could
        // finish this whole backlog in one poll.
        assert!(
            (1..responses.len()).contains(&completed),
            "completed {completed}"
        );
    }

    #[tokio::test]
    async fn closing_the_queue_retires_everything_the_sender_already_holds() {
        use tokio::io::AsyncReadExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (sender, peer) = tokio::join!(
            connect_with_auth(
                Transport::new(),
                ServerAddr::Tcp {
                    address: listener.local_addr().unwrap()
                },
                [0; 256]
            ),
            listener.accept()
        );
        let peer_socket = peer.unwrap().0;
        let tracker = crate::InvocationTracker::default();
        let (tx, rx) = queue::unbounded_channel();
        let mut responses = Vec::new();
        for ping_id in 0..3 {
            let (tx_result, rx_result) = oneshot::channel();
            tx.send(Rpc {
                body: tl::functions::Ping { ping_id }.to_bytes().into(),
                permit: (ping_id == 0).then(|| tracker.try_acquire().unwrap()),
                tx: tx_result,
            })
            .unwrap();
            responses.push(rx_result);
        }
        let session = erase(Arc::new(
            grammers_session::storages::MemorySession::default(),
        ));
        let runner = run_sender(sender.unwrap(), rx, None, 2, session, Arc::default());
        tokio::pin!(runner);
        // Run until they all reached the peer: then they are in the sender, not the queue.
        let mut peer = peer_socket;
        let mut bytes = vec![0; 4096];
        let received = tokio::select! {
            _ = &mut runner => panic!("open connection should remain active"),
            read = peer.read(&mut bytes) => read.unwrap(),
        };
        assert!(received > 0);
        assert!(tracker.try_acquire().is_none());
        for response in &mut responses {
            assert!(matches!(
                response.try_recv(),
                Err(oneshot::error::TryRecvError::Empty)
            ));
        }
        // Closing now cannot lose them: the connection ends and retires each one.
        drop(tx);
        let (_, result) = runner.await;
        assert!(result.is_ok());
        for response in responses {
            assert!(response.await.is_err());
        }
        assert!(tracker.try_acquire().is_some());
    }

    #[tokio::test(start_paused = true)]
    async fn a_stable_connection_ending_forgets_earlier_connection_failures() {
        let pool = SenderPool::new(
            Arc::new(grammers_session::storages::MemorySession::default()),
            1,
        );
        let mut runner = pool.runner;
        for _ in 0..5 {
            runner.note_connect_failure(2);
        }
        let delay = runner.pacing_mut(2).not_before - tokio::time::Instant::now();
        assert_eq!(delay, connect_retry_delay(5));
        runner.pacing_mut(2).connected_at = Some(tokio::time::Instant::now());

        // A connection failing soon after it came up counts as one more failure.
        let closed = || Err(ReadError::Io(std::io::Error::other("closed")));
        runner.record_completion(Ok((2, closed()))).unwrap();
        assert_eq!(runner.pacing_mut(2).failures, 6);

        // One lasting a while, however it ends, shows that the problem went away.
        runner.pacing_mut(2).connected_at = Some(tokio::time::Instant::now());
        tokio::time::advance(STABLE_CONNECTION).await;
        runner.record_completion(Ok((2, closed()))).unwrap();
        assert_eq!(runner.pacing_mut(2).failures, 0);

        runner.note_connect_failure(2);
        runner.pacing_mut(2).connected_at = Some(tokio::time::Instant::now());
        tokio::time::advance(STABLE_CONNECTION).await;
        assert!(
            runner
                .process_request(Request::Disconnect { dc_id: 2 })
                .await
                .is_continue()
        );
        assert_eq!(runner.pacing_mut(2).failures, 0);
    }

    #[test]
    fn repeated_connection_failures_are_spaced_out() {
        let delays: Vec<u64> = (0..=9)
            .map(|failures| connect_retry_delay(failures).as_secs())
            .collect();
        assert_eq!(delays, [0, 0, 1, 2, 4, 8, 16, 30, 30, 30]);
    }

    #[tokio::test]
    async fn a_caller_retry_loop_cannot_reconnect_as_fast_as_it_fails() {
        // Accepts and drops every connection, as an overloaded front end might.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let attempts = Arc::new(std::sync::Mutex::new(Vec::new()));
        let accepted = Arc::clone(&attempts);
        let server = tokio::spawn(async move {
            loop {
                let (socket, _) = listener.accept().await.unwrap();
                accepted.lock().unwrap().push(tokio::time::Instant::now());
                drop(socket);
            }
        });
        let session = Arc::new(grammers_session::storages::MemorySession::default());
        let std::net::SocketAddr::V4(ipv4) = address else {
            panic!("expected IPv4")
        };
        session
            .set_dc_option(&grammers_session::types::DcOption {
                id: 2,
                ipv4,
                ipv6: std::net::SocketAddrV6::new(ipv4.ip().to_ipv6_mapped(), ipv4.port(), 0, 0),
                auth_key: Some([1; 256]),
            })
            .await
            .unwrap();
        session.set_home_dc_id(2).await.unwrap();
        let pool = SenderPool::new(session, 1);
        let runner = tokio::spawn(pool.runner.run());
        for _ in 0..4 {
            assert!(
                pool.handle
                    .thin
                    .raw_invoke_in_dc(2, tl::functions::Ping { ping_id: 1 }.to_bytes())
                    .await
                    .is_err()
            );
        }
        let attempts = attempts.lock().unwrap().clone();
        assert_eq!(attempts.len(), 4);
        // The first retry is not delayed; the next wait one and then two seconds.
        assert!(attempts[2] - attempts[1] >= Duration::from_secs(1));
        assert!(attempts[3] - attempts[2] >= Duration::from_secs(2));
        pool.handle.thin.quit();
        runner.await.unwrap();
        server.abort();
    }

    // A flood wait above what the retry policy sleeps through returns to the caller, whose
    // loop calls next() again: that must not ask Telegram again before the wait is over.
    #[tokio::test(start_paused = true)]
    async fn recovery_honours_a_flood_wait_longer_than_its_own_delay() {
        let session = Arc::new(RecoverySession::default());
        cache_self_user(session.as_ref()).await;
        let mut pool = SenderPool::new(Arc::clone(&session), 1);
        let mut receiver = crate::UpdatesReceiver::create(
            pool.handle.thin.clone(),
            erase(Arc::clone(&session)),
            pool.updates,
            crate::UpdatesConfiguration { catch_up: true },
        )
        .await
        .unwrap();
        let update_tx = pool.runner.updates_tx.clone().unwrap();
        let (result, ()) = tokio::join!(receiver.next(), async {
            expect_get_state(&mut pool.runner)
                .await
                .send(Err(InvocationError::Rpc(
                    tl::types::RpcError {
                        error_code: 420,
                        error_message: "FLOOD_WAIT_120".into(),
                    }
                    .into(),
                )))
                .unwrap();
            let Request::CheckRetry { error, tx, .. } =
                pool.runner.request_rx.recv().await.unwrap()
            else {
                panic!("expected retry inspection")
            };
            tx.send(ControlFlow::Break(*error)).unwrap();
        });
        assert!(result.unwrap_err().is("FLOOD_WAIT"));
        let failed_at = tokio::time::Instant::now();
        tokio::time::timeout(Duration::from_secs(119), receiver.next())
            .await
            .unwrap_err();
        assert!(pool.runner.request_rx.try_recv().is_err());
        let (result, ()) = tokio::join!(receiver.next(), async {
            expect_get_state(&mut pool.runner)
                .await
                .send(Ok(state_reply()))
                .unwrap();
            update_tx.send(config_update()).await.unwrap();
        });
        assert_eq!(result.unwrap().0[0].0, tl::enums::Update::Config);
        assert_eq!(failed_at.elapsed(), Duration::from_secs(120));
    }

    // Telegram gives no delay for a failing recovery request, so it must not stay at one
    // request a second for as long as the failure lasts.
    #[tokio::test(start_paused = true)]
    async fn repeated_recovery_failures_back_off_to_a_minute_and_success_resets() {
        let session = Arc::new(RecoverySession::default());
        cache_self_user(session.as_ref()).await;
        let mut pool = SenderPool::new(Arc::clone(&session), 1);
        let mut receiver = crate::UpdatesReceiver::create(
            pool.handle.thin.clone(),
            erase(Arc::clone(&session)),
            pool.updates,
            crate::UpdatesConfiguration { catch_up: true },
        )
        .await
        .unwrap();
        let update_tx = pool.runner.updates_tx.clone().unwrap();
        let mut spacing = Vec::new();
        let mut last = tokio::time::Instant::now();
        for _ in 0..9 {
            let (result, ()) = tokio::join!(receiver.next(), async {
                expect_get_state(&mut pool.runner)
                    .await
                    .send(Err(InvocationError::Rpc(
                        tl::types::RpcError {
                            error_code: 500,
                            error_message: "INTERNAL".into(),
                        }
                        .into(),
                    )))
                    .unwrap();
                let Request::CheckRetry { error, tx, .. } =
                    pool.runner.request_rx.recv().await.unwrap()
                else {
                    panic!("expected retry inspection")
                };
                tx.send(ControlFlow::Break(*error)).unwrap();
            });
            assert!(result.is_err());
            spacing.push(last.elapsed().as_secs());
            last = tokio::time::Instant::now();
        }
        assert_eq!(spacing, [0, 1, 2, 4, 8, 16, 32, 60, 60]);
        let (result, ()) = tokio::join!(receiver.next(), async {
            expect_get_state(&mut pool.runner)
                .await
                .send(Ok(state_reply()))
                .unwrap();
            update_tx.send(config_update()).await.unwrap();
        });
        assert!(result.is_ok());
        // Success forgets the failures: the next recovery failing waits one second again.
        // ConnectionClosed forgets the account position, so that it asks for state again.
        update_tx.send(UpdatesLike::ConnectionClosed).await.unwrap();
        let (result, ()) = tokio::join!(receiver.next(), async {
            let Request::Invoke { tx, .. } = pool.runner.request_rx.recv().await.unwrap() else {
                panic!("expected a recovery request")
            };
            tx.send(Err(InvocationError::Rpc(
                tl::types::RpcError {
                    error_code: 500,
                    error_message: "INTERNAL".into(),
                }
                .into(),
            )))
            .unwrap();
            let Request::CheckRetry { error, tx, .. } =
                pool.runner.request_rx.recv().await.unwrap()
            else {
                panic!("expected retry inspection")
            };
            tx.send(ControlFlow::Break(*error)).unwrap();
        });
        assert!(result.is_err());
        let failed_at = tokio::time::Instant::now();
        let (result, ()) = tokio::join!(receiver.next(), async {
            let Request::Invoke { tx, .. } = pool.runner.request_rx.recv().await.unwrap() else {
                panic!("expected a recovery request")
            };
            assert_eq!(failed_at.elapsed(), Duration::from_secs(1));
            tx.send(Err(InvocationError::Dropped("end of test")))
                .unwrap();
            let Request::CheckRetry { error, tx, .. } =
                pool.runner.request_rx.recv().await.unwrap()
            else {
                panic!("expected retry inspection")
            };
            tx.send(ControlFlow::Break(*error)).unwrap();
        });
        assert!(result.is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn a_stalled_consumer_costs_one_yield_per_batch() {
        use crate::sender::optimization_tests::{zero_key_aes, zero_key_server_packet};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        const BURST: usize = 64;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (sender, peer) = tokio::join!(
            connect_with_auth(
                Transport::new(),
                ServerAddr::Tcp {
                    address: listener.local_addr().unwrap()
                },
                [0; 256]
            ),
            listener.accept()
        );
        let mut peer = peer.unwrap().0;
        // No one reads this queue: every update beyond the first has nowhere to go.
        let (updates_tx, _updates_rx) = mpsc::channel(1);
        let state: Arc<PoolState> = Arc::default();
        let (tx, rx) = queue::unbounded_channel();
        let (tx_result, _rx_result) = oneshot::channel();
        tx.send(Rpc {
            body: tl::functions::Ping { ping_id: 1 }.to_bytes().into(),
            permit: None,
            tx: tx_result,
        })
        .unwrap();
        let session = erase(Arc::new(
            grammers_session::storages::MemorySession::default(),
        ));
        let mut connection = std::pin::pin!(run_sender(
            sender.unwrap(),
            rx,
            Some(updates_tx),
            2,
            session,
            Arc::clone(&state),
        ));
        let mut bytes = vec![0; 4096];
        let read = tokio::select! {
            _ = &mut connection => panic!("open connection should remain active"),
            read = peer.read(&mut bytes) => read.unwrap(),
        };
        let packet = &bytes[2..read];
        let (key, iv) = zero_key_aes(&packet[8..24].try_into().unwrap());
        let mut plain = packet[24..].to_vec();
        grammers_crypto::aes::ige_decrypt(&mut plain, &key, &iv);
        let session_id = i64::from_le_bytes(plain[8..16].try_into().unwrap());
        let mut burst = Vec::new();
        let body = tl::enums::Updates::TooLong.to_bytes();
        for index in 0..BURST as i64 {
            let id = (1_700_000_000i64 << 32) + 2 * index + 1;
            let packet = zero_key_server_packet(session_id, id, 1, &body);
            burst.push((packet.len() / 4) as u8);
            burst.extend(packet);
        }
        peer.write_all(&burst).await.unwrap();
        // Each yield is one more poll, so a yield per update would take at least BURST.
        let mut polls = 0;
        tokio::time::timeout(
            Duration::from_secs(5),
            std::future::poll_fn(|cx| {
                if state.dropped.load(Ordering::Relaxed) == BURST as u64 - 1 {
                    return Poll::Ready(());
                }
                polls += 1;
                assert!(connection.as_mut().poll(cx).is_pending());
                Poll::Pending
            }),
        )
        .await
        .unwrap();
        assert!(polls < BURST / 2, "{polls} polls");
    }

    // A connection decoding more updates at once than the queue holds used to drop the
    // rest, and each drop costs a recovery request to Telegram.
    #[tokio::test(flavor = "current_thread")]
    async fn update_burst_beyond_the_queue_reaches_a_consumer_on_the_same_thread() {
        use crate::sender::optimization_tests::{zero_key_aes, zero_key_server_packet};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        const BURST: usize = 64;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (sender, peer) = tokio::join!(
            connect_with_auth(
                Transport::new(),
                ServerAddr::Tcp {
                    address: listener.local_addr().unwrap()
                },
                [0; 256]
            ),
            listener.accept()
        );
        let mut peer = peer.unwrap().0;
        let (updates_tx, mut updates_rx) = mpsc::channel(4);
        let state: Arc<PoolState> = Arc::default();
        let (tx, rx) = queue::unbounded_channel();
        let (tx_result, _rx_result) = oneshot::channel();
        tx.send(Rpc {
            body: tl::functions::Ping { ping_id: 1 }.to_bytes().into(),
            permit: None,
            tx: tx_result,
        })
        .unwrap();
        let session = erase(Arc::new(
            grammers_session::storages::MemorySession::default(),
        ));
        let connection = tokio::spawn(run_sender(
            sender.unwrap(),
            rx,
            Some(updates_tx),
            2,
            session,
            Arc::clone(&state),
        ));

        // The client's first packet tells its session: a marker, a length, the key's ID,
        // the message key and then the encrypted contents, all under the zero test key.
        let mut bytes = vec![0; 4096];
        let read = peer.read(&mut bytes).await.unwrap();
        assert_eq!(bytes[0], 0xef);
        let packet = &bytes[2..read];
        let (key, iv) = zero_key_aes(&packet[8..24].try_into().unwrap());
        let mut plain = packet[24..].to_vec();
        grammers_crypto::aes::ige_decrypt(&mut plain, &key, &iv);
        let session_id = i64::from_le_bytes(plain[8..16].try_into().unwrap());

        let mut burst = Vec::new();
        let body = tl::enums::Updates::TooLong.to_bytes();
        for index in 0..BURST as i64 {
            let id = (1_700_000_000i64 << 32) + 2 * index + 1;
            let packet = zero_key_server_packet(session_id, id, 1, &body);
            burst.push((packet.len() / 4) as u8);
            burst.extend(packet);
        }
        peer.write_all(&burst).await.unwrap();

        let mut seen = 0;
        while seen < BURST {
            tokio::time::timeout(Duration::from_secs(5), updates_rx.recv())
                .await
                .unwrap()
                .unwrap();
            seen += 1;
        }
        assert_eq!(state.dropped.load(Ordering::Relaxed), 0);
        connection.abort();
    }

    // A pts update can create a provisional account position during the pause after
    // a failed GetState. The reply must replace it without corrupting recovery.
    #[tokio::test(start_paused = true)]
    async fn get_state_after_a_pause_supersedes_a_provisional_account_position() {
        let session = Arc::new(RecoverySession::default());
        cache_self_user(session.as_ref()).await;
        let mut pool = SenderPool::new(Arc::clone(&session), 1);
        let mut receiver = crate::UpdatesReceiver::create(
            pool.handle.thin.clone(),
            erase(Arc::clone(&session)),
            pool.updates,
            crate::UpdatesConfiguration { catch_up: true },
        )
        .await
        .unwrap();
        let update_tx = pool.runner.updates_tx.clone().unwrap();
        let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(receiver.next(), async {
                expect_get_state(&mut pool.runner)
                    .await
                    .send(Err(InvocationError::Rpc(
                        tl::types::RpcError {
                            error_code: 401,
                            error_message: "AUTH_KEY_UNREGISTERED".into(),
                        }
                        .into(),
                    )))
                    .unwrap();
                let Request::CheckRetry { error, tx, .. } =
                    pool.runner.request_rx.recv().await.unwrap()
                else {
                    panic!("expected retry inspection")
                };
                tx.send(ControlFlow::Break(*error)).unwrap();
            })
        })
        .await
        .unwrap();
        assert!(result.unwrap_err().is("AUTH_KEY_UNREGISTERED"));
        // Inside the pause: an account update, then a reconnect marks it pending.
        update_tx
            .send(UpdatesLike::Updates(
                tl::types::UpdateShort {
                    update: tl::types::UpdateDeleteMessages {
                        messages: vec![1],
                        pts: 40,
                        pts_count: 1,
                    }
                    .into(),
                    date: 1,
                }
                .into(),
            ))
            .await
            .unwrap();
        assert!(matches!(
            receiver.next().await.unwrap().0[0].0,
            tl::enums::Update::DeleteMessages(_)
        ));
        update_tx.send(UpdatesLike::ConnectionClosed).await.unwrap();
        let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(receiver.next(), async {
                expect_get_state(&mut pool.runner)
                    .await
                    .send(Ok(state_reply()))
                    .unwrap();
                update_tx.send(config_update()).await.unwrap();
            })
        })
        .await
        .unwrap();
        assert_eq!(result.unwrap().0[0].0, tl::enums::Update::Config);
        // The account state came from the reply, and recovery is idle: a stale queue
        // entry would wake immediately instead of letting the paused clock advance.
        tokio::time::timeout(Duration::from_secs(60), receiver.next())
            .await
            .unwrap_err();
        assert!(pool.runner.request_rx.try_recv().is_err());
        receiver.sync_update_state().await.unwrap();
        assert_eq!(session.updates_state().await.unwrap().pts, 100);
    }

    #[tokio::test]
    async fn queued_requests_share_writes_instead_of_one_per_request() {
        use tokio::io::AsyncReadExt;
        const REQUESTS: usize = 32;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (sender, peer) = tokio::join!(
            connect_with_auth(
                Transport::new(),
                ServerAddr::Tcp {
                    address: listener.local_addr().unwrap()
                },
                [0; 256]
            ),
            listener.accept()
        );
        let mut peer = peer.unwrap().0;
        let (tx, rx) = queue::unbounded_channel();
        let mut responses = Vec::new();
        for ping_id in 0..REQUESTS as i64 {
            let (tx_result, rx_result) = oneshot::channel();
            tx.send(Rpc {
                body: tl::functions::Ping { ping_id }.to_bytes().into(),
                permit: None,
                tx: tx_result,
            })
            .unwrap();
            responses.push(rx_result);
        }
        let session = erase(Arc::new(
            grammers_session::storages::MemorySession::default(),
        ));
        let runner = run_sender(sender.unwrap(), rx, None, 2, session, Arc::default());
        tokio::pin!(runner);
        // One poll frames everything; give the kernel a moment to deliver it.
        tokio::select! { biased;
            _ = &mut runner => panic!("open connection should remain active"),
            _ = std::future::ready(()) => {}
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        let mut bytes = vec![0; 1 << 20];
        let read = peer.read(&mut bytes).await.unwrap();
        // Abridged transport: a marker byte, then frames sized in 4-byte words.
        assert_eq!(bytes[0], 0xef);
        let (mut frames, mut at) = (0, 1);
        while at < read {
            at += match bytes[at] {
                0x7f => {
                    4 + 4 * u32::from_le_bytes([bytes[at + 1], bytes[at + 2], bytes[at + 3], 0])
                        as usize
                }
                words => 1 + 4 * words as usize,
            };
            frames += 1;
        }
        assert_eq!(at, read, "complete frames only");
        // Eight per batch needs at most five frames, plus one for a salt request.
        assert!((1..=6).contains(&frames), "{frames} frames for {REQUESTS}");
    }

    #[tokio::test]
    async fn socket_failure_drains_unframed_queue_with_original_error_and_permits() {
        use crate::configuration::{AutoSleep, RetryPolicy};
        use tokio::io::AsyncWriteExt;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (sender, peer) = tokio::join!(
            connect_with_auth(
                Transport::new(),
                ServerAddr::Tcp {
                    address: listener.local_addr().unwrap()
                },
                [0; 256]
            ),
            listener.accept()
        );
        let mut peer = peer.unwrap().0;
        peer.shutdown().await.unwrap();
        let (tx, rx) = queue::unbounded_channel();
        let capacity = Arc::new(Semaphore::new(512));
        let mut receivers = Vec::new();
        for _ in 0..512 {
            let (response, result) = oneshot::channel();
            let permit = crate::InvocationPermit::with_capacity(
                None,
                Arc::clone(&capacity).try_acquire_owned().unwrap(),
            );
            tx.send(Rpc {
                body: Bytes::from(tl::functions::Ping { ping_id: 1 }.to_bytes()),
                permit: Some(permit),
                tx: response,
            })
            .unwrap();
            receivers.push(result);
        }
        let session = erase(Arc::new(
            grammers_session::storages::MemorySession::default(),
        ));
        let (_, error) = tokio::time::timeout(
            Duration::from_secs(1),
            run_sender(sender.unwrap(), rx, None, 2, session, Arc::default()),
        )
        .await
        .unwrap();
        let ReadError::Shared(source) = error.unwrap_err() else {
            panic!("shared socket failure")
        };
        assert_eq!(
            source.io_error().unwrap().kind(),
            std::io::ErrorKind::ConnectionReset
        );
        let mut unframed = 0;
        for result in receivers {
            let error = result.await.unwrap().unwrap_err();
            let InvocationError::Read {
                error: ref inner,
                may_have_been_sent,
            } = error
            else {
                panic!("lost delivery certainty: {error}")
            };
            assert!(Arc::ptr_eq(inner, &source));
            let retry = AutoSleep::default().should_retry(&RetryContext {
                fail_count: NonZeroU32::new(1).unwrap(),
                slept_so_far: Duration::ZERO,
                error,
            });
            if !may_have_been_sent {
                unframed += 1;
                assert!(retry.is_continue());
            } else {
                assert!(retry.is_break());
            }
        }
        assert!(
            unframed > 128,
            "fixture must leave a real queue behind the socket failure"
        );
        assert_eq!(capacity.available_permits(), 512);
        let body = Bytes::from_static(&[1; 4]);
        let (response, _result) = oneshot::channel();
        let rejected = tx
            .send(Rpc {
                body: body.clone(),
                permit: None,
                tx: response,
            })
            .err()
            .unwrap()
            .0;
        assert_eq!(rejected.body.as_ptr(), body.as_ptr());
    }

    async fn cache_self_user<S: Session>(session: &S)
    where
        S::Error: std::fmt::Debug,
    {
        session
            .cache_peer(grammers_session::types::PeerInfo::User {
                id: 1,
                auth: None,
                bot: Some(false),
                is_self: Some(true),
            })
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn cold_arrivals_poll_only_new_waiters_until_a_closure_wake() {
        let mut pool = SenderPool::new(
            Arc::new(grammers_session::storages::MemorySession::default()),
            1,
        );
        let mut receivers = Vec::new();
        let mut cx = Context::from_waker(Waker::noop());
        for _ in 0..20_000 {
            let (tx, rx) = oneshot::channel();
            receivers.push(rx);
            pool.runner.start_setup(
                999,
                Rpc {
                    body: Bytes::from_static(&[1; 4]),
                    tx,
                    permit: None,
                },
            );
            assert!(!pool.runner.pending[0].poll_closed(&mut cx));
        }
        let pending = &mut pool.runner.pending[0];
        assert_eq!(pending.closure_polls, 20_000);
        assert!(!pending.poll_closed(&mut cx));
        assert_eq!(pending.closure_polls, 20_000);
        drop(receivers.swap_remove(123));
        assert!(pending.closure.dirty.load(Ordering::Acquire));
        assert!(!pending.poll_closed(&mut cx));
        assert_eq!(pending.requests.len(), 19_999);
        assert_eq!(pending.closure_polls, 40_000);
        assert!(!pending.poll_closed(&mut cx));
        assert_eq!(pending.closure_polls, 40_000);
    }

    #[test]
    #[ignore = "local cold-waiter scan comparison; no network connection"]
    fn benchmark_cold_waiter_scan() {
        tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
            for count in [1024, 20_000] {
                for round in 0..3 {
                    for incremental in [round % 2 == 0, round % 2 != 0] {
                        let mut pool = SenderPool::new(Arc::new(grammers_session::storages::MemorySession::default()), 1);
                        let mut receivers = Vec::with_capacity(count);
                        let mut cx = Context::from_waker(Waker::noop());
                        let began = Instant::now();
                        for _ in 0..count {
                            let (tx, rx) = oneshot::channel();
                            receivers.push(rx);
                            pool.runner.start_setup(999, Rpc { body: Bytes::from_static(&[1; 4]), tx, permit: None });
                            let pending = &mut pool.runner.pending[0];
                            if incremental {
                                assert!(!pending.poll_closed(&mut cx));
                            } else {
                                pending.requests.retain_mut(|rpc| rpc.tx.poll_closed(&mut cx).is_pending());
                            }
                        }
                        println!("cold_waiters={count} round={round} incremental={incremental} elapsed_us={}", began.elapsed().as_micros());
                        // Invalid DC 999 never opens a socket, and dropping the
                        // runner owns and cancels its single setup task.
                        drop(pool);
                        drop(receivers);
                    }
                }
            }
        });
    }

    #[derive(Default)]
    struct RecoverySession {
        memory: grammers_session::storages::MemorySession,
        fail_state: AtomicBool,
        dc_write_delay: Duration,
        self_reads: std::sync::atomic::AtomicUsize,
        fail_self_reads: AtomicBool,
    }

    impl Session for RecoverySession {
        type Error = std::io::Error;
        fn home_dc_id(&self) -> Result<i32, Self::Error> {
            self.memory.home_dc_id().map_err(std::io::Error::other)
        }
        fn set_home_dc_id(
            &self,
            id: i32,
        ) -> grammers_session::BoxFuture<'_, Result<(), Self::Error>> {
            Box::pin(async move {
                self.memory
                    .set_home_dc_id(id)
                    .await
                    .map_err(std::io::Error::other)
            })
        }
        fn dc_option(&self, id: i32) -> Result<Option<DcOption>, Self::Error> {
            self.memory.dc_option(id).map_err(std::io::Error::other)
        }
        fn set_dc_option(
            &self,
            option: &DcOption,
        ) -> grammers_session::BoxFuture<'_, Result<(), Self::Error>> {
            let option = option.clone();
            Box::pin(async move {
                tokio::time::sleep(self.dc_write_delay).await;
                self.memory
                    .set_dc_option(&option)
                    .await
                    .map_err(std::io::Error::other)
            })
        }
        fn peer(
            &self,
            id: grammers_session::types::PeerId,
        ) -> grammers_session::BoxFuture<
            '_,
            Result<Option<grammers_session::types::PeerInfo>, Self::Error>,
        > {
            Box::pin(async move {
                if id == grammers_session::types::PeerId::self_user() {
                    self.self_reads.fetch_add(1, Ordering::Relaxed);
                    if self.fail_self_reads.load(Ordering::Relaxed) {
                        return Err(std::io::Error::other("self lookup fixture failure"));
                    }
                }
                self.memory.peer(id).await.map_err(std::io::Error::other)
            })
        }
        fn cache_peer(
            &self,
            peer: grammers_session::types::PeerInfo,
        ) -> grammers_session::BoxFuture<'_, Result<(), Self::Error>> {
            Box::pin(async move {
                self.memory
                    .cache_peer(peer)
                    .await
                    .map_err(std::io::Error::other)
            })
        }
        fn updates_state(
            &self,
        ) -> grammers_session::BoxFuture<
            '_,
            Result<grammers_session::types::UpdatesState, Self::Error>,
        > {
            Box::pin(async move {
                self.memory
                    .updates_state()
                    .await
                    .map_err(std::io::Error::other)
            })
        }
        fn set_update_state(
            &self,
            state: grammers_session::types::UpdateState,
        ) -> grammers_session::BoxFuture<'_, Result<(), Self::Error>> {
            Box::pin(async move {
                if self.fail_state.swap(false, Ordering::Relaxed) {
                    return Err(std::io::Error::other("state fixture failure"));
                }
                self.memory
                    .set_update_state(state)
                    .await
                    .map_err(std::io::Error::other)
            })
        }
    }

    fn setup_config(dc_options: Vec<tl::enums::DcOption>) -> tl::types::Config {
        tl::types::Config {
            default_p2p_contacts: false,
            preload_featured_stickers: false,
            revoke_pm_inbox: false,
            blocked_mode: false,
            force_try_ipv6: false,
            date: 1_700_000_000,
            expires: 1_700_003_600,
            test_mode: false,
            this_dc: 2,
            dc_options,
            dc_txt_domain_name: String::new(),
            chat_size_max: 200,
            megagroup_size_max: 200_000,
            forwarded_count_max: 100,
            online_update_period_ms: 210_000,
            offline_blur_timeout_ms: 5_000,
            offline_idle_timeout_ms: 30_000,
            online_cloud_timeout_ms: 300_000,
            notify_cloud_delay_ms: 30_000,
            notify_default_delay_ms: 1_500,
            push_chat_period_ms: 60_000,
            push_chat_limit: 2,
            edit_time_limit: 172_800,
            revoke_time_limit: i32::MAX,
            revoke_pm_time_limit: i32::MAX,
            rating_e_decay: 2_419_200,
            stickers_recent_limit: 200,
            channels_read_media_period: 604_800,
            tmp_sessions: None,
            call_receive_timeout_ms: 20_000,
            call_ring_timeout_ms: 90_000,
            call_connect_timeout_ms: 30_000,
            call_packet_timeout_ms: 10_000,
            me_url_prefix: String::new(),
            autoupdate_url_prefix: None,
            gif_search_username: None,
            venue_search_username: None,
            img_search_username: None,
            static_maps_provider: None,
            caption_length_max: 1_024,
            message_length_max: 4_096,
            webfile_dc_id: 4,
            suggested_lang_code: None,
            lang_pack_version: None,
            base_lang_pack_version: None,
            reactions_default: None,
            autologin_token: None,
        }
    }

    #[tokio::test(start_paused = true)]
    async fn slow_setup_storage_rearms_connection_liveness_after_key_persistence() {
        let session = Arc::new(RecoverySession {
            memory: Default::default(),
            fail_state: AtomicBool::new(false),
            dc_write_delay: Duration::from_secs(10),
            ..Default::default()
        });
        let pool = SenderPool::with_configuration(
            Arc::clone(&session),
            1,
            ConnectionParams {
                liveness_timeout: Some(Duration::from_secs(4)),
                ..Default::default()
            },
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (sender, _peer) = tokio::join!(
            connect_with_auth(
                Transport::new(),
                ServerAddr::Tcp {
                    address: listener.local_addr().unwrap()
                },
                [7; 256]
            ),
            listener.accept(),
        );
        let mut sender = sender.unwrap();
        sender.set_liveness_timeout(Some(Duration::from_secs(4)));
        let dc_option = session.dc_option(2).unwrap().unwrap();
        let started = tokio::time::Instant::now();
        let remote_config = setup_config(vec![
            tl::types::DcOption {
                ipv6: false,
                media_only: false,
                tcpo_only: false,
                cdn: false,
                r#static: true,
                this_port_only: false,
                id: 2,
                ip_address: "127.0.0.2".into(),
                port: 444,
                secret: None,
            }
            .into(),
        ]);
        let mut sender = pool
            .runner
            .setup_context()
            .finalize_connection(sender, remote_config, dc_option)
            .await
            .unwrap();
        assert!(started.elapsed() >= Duration::from_secs(20));
        assert_eq!(
            session.dc_option(2).unwrap().unwrap().ipv4,
            "127.0.0.2:444".parse().unwrap()
        );
        assert_eq!(
            session.dc_option(2).unwrap().unwrap().auth_key,
            Some([7; 256])
        );
        sender.step().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn setup_task_panic_retires_live_waiters_and_their_permits() {
        let (mut pool, _listener) = silent_pool(ConnectionParams {
            max_pending_requests: std::num::NonZeroUsize::new(2),
            ..Default::default()
        })
        .await;
        let trackers = [
            crate::InvocationTracker::default(),
            crate::InvocationTracker::default(),
        ];
        let mut waiters = Vec::new();
        for tracker in &trackers {
            waiters.push(
                pool.handle
                    .start_invoke_shared(4, Bytes::from_static(&[1; 4]), tracker.try_acquire())
                    .unwrap(),
            );
            let request = pool.runner.request_rx.recv().await.unwrap();
            assert!(pool.runner.process_request(request).await.is_continue());
        }
        assert_eq!(pool.runner.pending.len(), 1);
        assert_eq!(pool.runner.pending[0].requests.len(), 2);
        let capacity = Arc::clone(pool.runner.state.capacity.as_ref().unwrap());
        assert_eq!(capacity.available_permits(), 0);
        pool.runner.pending[0].abort_handle.abort();
        pool.runner.pending[0].abort_handle = pool.runner.setup_pool.spawn(async {
            panic!("setup panic fixture");
        });
        let result = tokio::time::timeout(Duration::from_secs(1), pool.runner.run_checked())
            .await
            .unwrap();
        assert_eq!(result, Err(PoolRunError::TaskPanicked));
        for (waiter, tracker) in waiters.into_iter().zip(trackers) {
            assert!(waiter.await.is_err());
            assert_eq!(tracker.stage(), crate::InvocationStage::Idle);
        }
        assert_eq!(capacity.available_permits(), 2);
        assert!(pool.handle.status().stopped);
        assert!(pool.handle.thin.1.read().unwrap().entries.is_empty());
    }

    #[tokio::test]
    async fn receiver_startup_repairs_bad_saved_channels_and_keeps_valid_rows() {
        use grammers_session::types::{ChannelState, UpdateState, UpdatesState};
        let session = Arc::new(grammers_session::storages::MemorySession::default());
        let saved = UpdatesState {
            pts: 100,
            qts: 0,
            date: 1,
            seq: 0,
            channels: vec![
                ChannelState { id: 7, pts: 10 },
                ChannelState { id: -1, pts: 10 },
                ChannelState { id: 8, pts: -1 },
                ChannelState { id: 20, pts: 11 },
            ],
        };
        assert!(grammers_session::updates::MessageBoxes::try_load(saved.clone()).is_err());
        session
            .set_update_state(UpdateState::All(saved))
            .await
            .unwrap();
        let pool = SenderPool::new(Arc::clone(&session), 1);
        let receiver = crate::UpdatesReceiver::create(
            pool.handle.thin.clone(),
            erase(Arc::clone(&session)),
            pool.updates,
            crate::UpdatesConfiguration { catch_up: true },
        )
        .await
        .unwrap();
        receiver.sync_update_state().await.unwrap();
        let mut repaired = session.updates_state().await.unwrap();
        repaired.channels.sort_by_key(|channel| channel.id);
        assert_eq!(
            repaired.channels,
            vec![
                ChannelState { id: 7, pts: 10 },
                ChannelState { id: 20, pts: 11 }
            ]
        );
        assert_eq!((repaired.pts, repaired.date, repaired.seq), (100, 1, 0));
    }

    #[tokio::test(start_paused = true)]
    async fn receiver_created_before_login_rechecks_self_while_next_is_waiting() {
        for catch_up in [false, true] {
            let session = Arc::new(grammers_session::storages::MemorySession::default());
            let mut pool = SenderPool::new(Arc::clone(&session), 1);
            let mut receiver = crate::UpdatesReceiver::create(
                pool.handle.thin.clone(),
                erase(Arc::clone(&session)),
                pool.updates,
                crate::UpdatesConfiguration { catch_up },
            )
            .await
            .unwrap();
            let update_tx = pool.runner.updates_tx.clone().unwrap();
            let responder = async {
                tokio::task::yield_now().await;
                assert!(pool.runner.request_rx.try_recv().is_err());
                cache_self_user(session.as_ref()).await;
                expect_get_state(&mut pool.runner)
                    .await
                    .send(Ok(state_reply()))
                    .unwrap();
                update_tx.send(config_update()).await.unwrap();
            };
            let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
                tokio::join!(receiver.next(), responder)
            })
            .await
            .unwrap();
            assert_eq!(result.unwrap().0[0].0, tl::enums::Update::Config);
            assert_eq!(session.updates_state().await.unwrap().pts, 100);
            assert_eq!(pool.handle.home_failure_generation(), 0);
        }
    }

    async fn expect_get_state(
        runner: &mut SenderPoolRunner,
    ) -> oneshot::Sender<Result<Vec<u8>, InvocationError>> {
        use tl::Identifiable;
        let Request::Invoke { body, tx, .. } = runner.request_rx.recv().await.unwrap() else {
            panic!("expected GetState")
        };
        assert_eq!(
            u32::from_le_bytes(body[..4].try_into().unwrap()),
            tl::functions::updates::GetState::CONSTRUCTOR_ID
        );
        tx
    }

    fn state_reply() -> Vec<u8> {
        tl::enums::updates::State::State(tl::types::updates::State {
            pts: 100,
            qts: 0,
            date: 1,
            seq: 1,
            unread_count: 0,
        })
        .to_bytes()
    }

    fn config_update() -> UpdatesLike {
        UpdatesLike::Updates(
            tl::types::UpdateShort {
                update: tl::enums::Update::Config,
                date: 1,
            }
            .into(),
        )
    }

    fn inline_query_update() -> UpdatesLike {
        UpdatesLike::Updates(
            tl::types::UpdateShort {
                update: tl::types::UpdateBotInlineQuery {
                    query_id: 1,
                    user_id: 7,
                    query: String::new(),
                    geo: None,
                    peer_type: None,
                    offset: String::new(),
                }
                .into(),
                date: 1,
            }
            .into(),
        )
    }

    #[tokio::test(start_paused = true)]
    async fn prelogin_service_updates_deliver_without_arming_get_state() {
        let services = vec![
            tl::enums::Update::LoginToken,
            tl::enums::Update::Config,
            tl::types::UpdateDcOptions { dc_options: vec![] }.into(),
            tl::types::UpdateLangPackTooLong {
                lang_code: "en".into(),
            }
            .into(),
            tl::types::UpdateLangPack {
                difference: tl::types::LangPackDifference {
                    lang_code: "en".into(),
                    from_version: 0,
                    version: 1,
                    strings: vec![],
                }
                .into(),
            }
            .into(),
            tl::types::UpdateSentPhoneCode {
                sent_code: tl::types::auth::SentCode {
                    r#type: tl::types::auth::SentCodeTypeApp { length: 5 }.into(),
                    phone_code_hash: String::new(),
                    next_type: None,
                    timeout: None,
                }
                .into(),
            }
            .into(),
            tl::types::UpdateServiceNotification {
                popup: false,
                invert_media: false,
                inbox_date: None,
                r#type: String::new(),
                message: String::new(),
                media: tl::enums::MessageMedia::Empty,
                entities: vec![],
            }
            .into(),
        ];
        for service in services {
            for envelope in 0..3 {
                let session = Arc::new(RecoverySession::default());
                let mut pool = SenderPool::new(Arc::clone(&session), 1);
                let mut receiver = crate::UpdatesReceiver::create(
                    pool.handle.thin.clone(),
                    erase(Arc::clone(&session)),
                    pool.updates,
                    crate::UpdatesConfiguration { catch_up: true },
                )
                .await
                .unwrap();
                let updates = vec![service.clone()];
                let users = vec![tl::types::UserEmpty { id: 7 }.into()];
                let batch = match envelope {
                    0 => tl::types::UpdateShort {
                        update: service.clone(),
                        date: 1,
                    }
                    .into(),
                    1 => tl::types::Updates {
                        updates,
                        users,
                        chats: vec![],
                        date: 1,
                        seq: 0,
                    }
                    .into(),
                    _ => tl::types::UpdatesCombined {
                        updates,
                        users,
                        chats: vec![],
                        date: 1,
                        seq_start: 0,
                        seq: 0,
                    }
                    .into(),
                };
                let update_tx = pool.runner.updates_tx.clone().unwrap();
                update_tx.send(UpdatesLike::Updates(batch)).await.unwrap();
                assert_eq!(receiver.next().await.unwrap().0[0].0, service);
                assert_eq!(session.self_reads.load(Ordering::Relaxed), 0);
                assert!(
                    tokio::time::timeout(Duration::from_millis(900), receiver.next())
                        .await
                        .is_err()
                );
                assert!(pool.runner.request_rx.try_recv().is_err());
                assert_eq!(session.self_reads.load(Ordering::Relaxed), 1);
                cache_self_user(session.as_ref()).await;
                let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
                    tokio::join!(receiver.next(), async {
                        expect_get_state(&mut pool.runner)
                            .await
                            .send(Ok(state_reply()))
                            .unwrap();
                        update_tx.send(config_update()).await.unwrap();
                    })
                })
                .await
                .unwrap();
                assert_eq!(result.unwrap().0[0].0, tl::enums::Update::Config);
                assert!(pool.runner.request_rx.try_recv().is_err());
                assert_eq!(session.self_reads.load(Ordering::Relaxed), 2);
            }
        }
    }

    #[tokio::test(start_paused = true)]
    async fn service_backlog_does_not_starve_cached_login_discovery() {
        let session = Arc::new(RecoverySession::default());
        let mut pool = SenderPool::new(Arc::clone(&session), 1);
        let mut receiver = crate::UpdatesReceiver::create(
            pool.handle.thin.clone(),
            erase(Arc::clone(&session)),
            pool.updates,
            crate::UpdatesConfiguration { catch_up: true },
        )
        .await
        .unwrap();
        let update_tx = pool.runner.updates_tx.clone().unwrap();
        for _ in 0..16 {
            update_tx.send(config_update()).await.unwrap();
        }
        receiver.next().await.unwrap();
        receiver.next().await.unwrap();
        assert_eq!(session.self_reads.load(Ordering::Relaxed), 1);
        cache_self_user(session.as_ref()).await;
        tokio::time::advance(Duration::from_secs(1)).await;
        let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(receiver.next(), async {
                expect_get_state(&mut pool.runner)
                    .await
                    .send(Ok(state_reply()))
                    .unwrap();
            })
        })
        .await
        .unwrap();
        assert_eq!(result.unwrap().0[0].0, tl::enums::Update::Config);
        assert_eq!(session.self_reads.load(Ordering::Relaxed), 2);
    }

    #[tokio::test(start_paused = true)]
    async fn mixed_service_and_account_updates_establish_provisional_login() {
        for combined in [false, true] {
            let session = Arc::new(RecoverySession::default());
            session.fail_self_reads.store(true, Ordering::Relaxed);
            let mut pool = SenderPool::new(Arc::clone(&session), 1);
            let mut receiver = crate::UpdatesReceiver::create(
                pool.handle.thin.clone(),
                erase(Arc::clone(&session)),
                pool.updates,
                crate::UpdatesConfiguration { catch_up: true },
            )
            .await
            .unwrap();
            let UpdatesLike::Updates(tl::enums::Updates::UpdateShort(account)) =
                inline_query_update()
            else {
                unreachable!()
            };
            let updates = vec![tl::enums::Update::LoginToken, account.update];
            let batch = if combined {
                tl::types::UpdatesCombined {
                    updates,
                    users: vec![],
                    chats: vec![],
                    date: 1,
                    seq_start: 0,
                    seq: 0,
                }
                .into()
            } else {
                tl::types::Updates {
                    updates,
                    users: vec![],
                    chats: vec![],
                    date: 1,
                    seq: 0,
                }
                .into()
            };
            let update_tx = pool.runner.updates_tx.clone().unwrap();
            update_tx.send(UpdatesLike::Updates(batch)).await.unwrap();
            let first = receiver.next().await.unwrap();
            assert_eq!(first.0.len(), 2);
            assert_eq!(first.0[0].0, tl::enums::Update::LoginToken);
            assert!(matches!(first.0[1].0, tl::enums::Update::BotInlineQuery(_)));
            let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
                tokio::join!(receiver.next(), async {
                    expect_get_state(&mut pool.runner)
                        .await
                        .send(Ok(state_reply()))
                        .unwrap();
                    update_tx.send(config_update()).await.unwrap();
                })
            })
            .await
            .unwrap();
            assert_eq!(result.unwrap().0[0].0, tl::enums::Update::Config);
            assert_eq!(session.self_reads.load(Ordering::Relaxed), 0);
            assert!(pool.runner.request_rx.try_recv().is_err());
        }
    }

    #[tokio::test(start_paused = true)]
    async fn inferred_login_rejection_waits_for_self_without_rpc_storm() {
        for error_name in ["AUTH_KEY_UNREGISTERED", "SESSION_PASSWORD_NEEDED"] {
            let session = Arc::new(RecoverySession::default());
            let mut pool = SenderPool::new(Arc::clone(&session), 1);
            let mut receiver = crate::UpdatesReceiver::create(
                pool.handle.thin.clone(),
                erase(Arc::clone(&session)),
                pool.updates,
                crate::UpdatesConfiguration { catch_up: true },
            )
            .await
            .unwrap();
            let update_tx = pool.runner.updates_tx.clone().unwrap();
            update_tx.send(inline_query_update()).await.unwrap();
            assert!(matches!(
                receiver.next().await.unwrap().0[0].0,
                tl::enums::Update::BotInlineQuery(_)
            ));
            let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
                tokio::join!(receiver.next(), async {
                    expect_get_state(&mut pool.runner)
                        .await
                        .send(Err(InvocationError::Rpc(
                            tl::types::RpcError {
                                error_code: 401,
                                error_message: error_name.into(),
                            }
                            .into(),
                        )))
                        .unwrap();
                    let Request::CheckRetry { error, tx, .. } =
                        pool.runner.request_rx.recv().await.unwrap()
                    else {
                        panic!("expected retry inspection")
                    };
                    tx.send(ControlFlow::Break(*error)).unwrap();
                    for _ in 0..8 {
                        update_tx.send(inline_query_update()).await.unwrap();
                    }
                })
            })
            .await
            .unwrap();
            assert!(matches!(
                result.unwrap().0[0].0,
                tl::enums::Update::BotInlineQuery(_)
            ));
            for _ in 0..7 {
                receiver.next().await.unwrap();
            }
            assert!(
                tokio::time::timeout(Duration::from_millis(900), receiver.next())
                    .await
                    .is_err()
            );
            assert!(pool.runner.request_rx.try_recv().is_err());
            assert_eq!(session.self_reads.load(Ordering::Relaxed), 0);
            // Reconnect does not revive a rejected inference either.
            pool.runner
                .state
                .home_failures
                .fetch_add(1, Ordering::Release);
            cache_self_user(session.as_ref()).await;
            // Self metadata confirms the account after a rejected inference.
            // A second 401 must now reach the caller, never silently reject it again.
            let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
                tokio::join!(receiver.next(), async {
                    expect_get_state(&mut pool.runner)
                        .await
                        .send(Err(InvocationError::Rpc(
                            tl::types::RpcError {
                                error_code: 401,
                                error_message: error_name.into(),
                            }
                            .into(),
                        )))
                        .unwrap();
                    let Request::CheckRetry { error, tx, .. } =
                        pool.runner.request_rx.recv().await.unwrap()
                    else {
                        panic!("expected retry inspection")
                    };
                    tx.send(ControlFlow::Break(*error)).unwrap();
                })
            })
            .await
            .unwrap();
            assert!(
                matches!(result.unwrap_err().root_cause(), InvocationError::Rpc(rpc)
                if rpc.code == 401 && rpc.name == error_name)
            );
            assert_eq!(session.self_reads.load(Ordering::Relaxed), 1);
            assert!(pool.runner.request_rx.try_recv().is_err());
            let paused_at = tokio::time::Instant::now();
            let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
                tokio::join!(receiver.next(), async {
                    expect_get_state(&mut pool.runner)
                        .await
                        .send(Ok(state_reply()))
                        .unwrap();
                    update_tx.send(config_update()).await.unwrap();
                })
            })
            .await
            .unwrap();
            assert_eq!(result.unwrap().0[0].0, tl::enums::Update::Config);
            assert!(paused_at.elapsed() >= Duration::from_secs(1));
            assert_eq!(session.self_reads.load(Ordering::Relaxed), 1);
            assert!(pool.runner.request_rx.try_recv().is_err());
        }
    }

    // A stream used through logout and re-login: its self metadata is stale.
    #[tokio::test(start_paused = true)]
    async fn confirmed_login_failures_are_paced_and_live_updates_keep_flowing() {
        let session = Arc::new(RecoverySession::default());
        cache_self_user(session.as_ref()).await;
        let mut pool = SenderPool::new(Arc::clone(&session), 1);
        let mut receiver = crate::UpdatesReceiver::create(
            pool.handle.thin.clone(),
            erase(Arc::clone(&session)),
            pool.updates,
            crate::UpdatesConfiguration { catch_up: true },
        )
        .await
        .unwrap();
        let update_tx = pool.runner.updates_tx.clone().unwrap();
        for attempt in 0..3 {
            let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
                tokio::join!(receiver.next(), async {
                    expect_get_state(&mut pool.runner)
                        .await
                        .send(Err(InvocationError::Rpc(
                            tl::types::RpcError {
                                error_code: 401,
                                error_message: "AUTH_KEY_UNREGISTERED".into(),
                            }
                            .into(),
                        )))
                        .unwrap();
                    let Request::CheckRetry { error, tx, .. } =
                        pool.runner.request_rx.recv().await.unwrap()
                    else {
                        panic!("expected retry inspection")
                    };
                    tx.send(ControlFlow::Break(*error)).unwrap();
                })
            })
            .await
            .unwrap();
            assert!(result.unwrap_err().is("AUTH_KEY_UNREGISTERED"), "{attempt}");
            let failed_at = tokio::time::Instant::now();
            // The pause must not hold back the push a re-login is waiting for.
            update_tx
                .send(UpdatesLike::Updates(tl::enums::Updates::UpdateShort(
                    tl::types::UpdateShort {
                        update: tl::enums::Update::LoginToken,
                        date: 1,
                    },
                )))
                .await
                .unwrap();
            let (updates, _, _) = receiver.next().await.unwrap();
            assert_eq!(updates[0].0, tl::enums::Update::LoginToken);
            assert_eq!(failed_at.elapsed(), Duration::ZERO);
            assert!(pool.runner.request_rx.try_recv().is_err());
            // Cancelling inside the pause neither issues an RPC nor restarts it.
            tokio::time::timeout(Duration::from_millis(400), receiver.next())
                .await
                .unwrap_err();
            assert!(pool.runner.request_rx.try_recv().is_err());
        }
        // Each failure doubled the pause; this is the rest of the third, four second one.
        let retried_from = tokio::time::Instant::now();
        let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
            tokio::join!(receiver.next(), async {
                expect_get_state(&mut pool.runner)
                    .await
                    .send(Ok(state_reply()))
                    .unwrap();
                update_tx.send(config_update()).await.unwrap();
            })
        })
        .await
        .unwrap();
        assert_eq!(result.unwrap().0[0].0, tl::enums::Update::Config);
        assert_eq!(retried_from.elapsed(), Duration::from_millis(3600));
    }

    // Beyond the generated TL decoder's nesting limit, including its 2-frame story cycle.
    fn too_deep_message() -> Vec<u8> {
        use tl::{Identifiable, Serializable};
        let mut bytes = tl::types::Message::CONSTRUCTOR_ID.to_bytes();
        bytes.extend((1u32 << 9).to_bytes());
        bytes.extend(0u32.to_bytes());
        bytes.extend(1i32.to_bytes());
        bytes.extend(tl::types::PeerUser::CONSTRUCTOR_ID.to_bytes());
        bytes.extend(1i64.to_bytes());
        bytes.extend(1i32.to_bytes());
        b"owned".as_slice().serialize(&mut bytes);
        for _ in 0..64 {
            bytes.extend(tl::types::MessageMediaStory::CONSTRUCTOR_ID.to_bytes());
            bytes.extend(1u32.to_bytes());
            bytes.extend(tl::types::PeerUser::CONSTRUCTOR_ID.to_bytes());
            bytes.extend(1i64.to_bytes());
            bytes.extend(1i32.to_bytes());
            bytes.extend(tl::types::StoryItem::CONSTRUCTOR_ID.to_bytes());
            bytes.extend(0u32.to_bytes());
            bytes.extend(1i32.to_bytes());
            bytes.extend(1i32.to_bytes());
            bytes.extend(2i32.to_bytes());
        }
        bytes.extend(tl::types::MessageMediaEmpty::CONSTRUCTOR_ID.to_bytes());
        bytes
    }

    fn too_deep_vector() -> Vec<u8> {
        let mut bytes = 0x1cb5c415u32.to_bytes();
        bytes.extend(1u32.to_bytes());
        bytes.extend(too_deep_message());
        bytes
    }

    fn assert_nesting_error(error: &InvocationError) {
        assert!(
            matches!(
                error.root_cause(),
                InvocationError::Deserialize(
                    grammers_mtproto::mtp::DeserializeError::ResourceLimit {
                        resource: "TL nesting depth"
                    }
                )
            ),
            "{error:?}"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn unreadable_account_difference_is_paced_and_keeps_its_position() {
        use grammers_session::types::{UpdateState, UpdatesState};
        use tl::{Deserializable, Identifiable, Serializable};
        let session = Arc::new(grammers_session::storages::MemorySession::default());
        cache_self_user(session.as_ref()).await;
        session
            .set_update_state(UpdateState::All(UpdatesState {
                pts: 100,
                date: 1,
                ..Default::default()
            }))
            .await
            .unwrap();
        let mut pool = SenderPool::new(Arc::clone(&session), 1);
        let mut receiver = crate::UpdatesReceiver::create(
            pool.handle.thin.clone(),
            erase(Arc::clone(&session)),
            pool.updates,
            crate::UpdatesConfiguration { catch_up: true },
        )
        .await
        .unwrap();
        let update_tx = pool.runner.updates_tx.clone().unwrap();
        update_tx
            .send(UpdatesLike::Updates(tl::enums::Updates::TooLong))
            .await
            .unwrap();
        let mut unreadable = tl::types::updates::Difference::CONSTRUCTOR_ID.to_bytes();
        unreadable.extend(too_deep_vector());
        let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(receiver.next(), async {
                let Request::Invoke { body, tx, .. } = pool.runner.request_rx.recv().await.unwrap()
                else {
                    panic!("expected account difference");
                };
                assert_eq!(
                    tl::functions::updates::GetDifference::from_bytes(&body[4..])
                        .unwrap()
                        .pts,
                    100
                );
                tx.send(Ok(unreadable.clone())).unwrap();
            })
        })
        .await
        .unwrap();
        assert_nesting_error(&result.unwrap_err());
        let failed_at = tokio::time::Instant::now();
        // Nothing is retried immediately, and unrelated pushes are still delivered.
        update_tx.send(config_update()).await.unwrap();
        let (updates, _, _) = receiver.next().await.unwrap();
        assert_eq!(updates[0].0, tl::enums::Update::Config);
        assert!(pool.runner.request_rx.try_recv().is_err());
        let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(receiver.next(), async {
                let Request::Invoke { body, tx, .. } = pool.runner.request_rx.recv().await.unwrap()
                else {
                    panic!("expected account difference retry");
                };
                assert!(failed_at.elapsed() >= Duration::from_secs(1));
                assert_eq!(
                    tl::functions::updates::GetDifference::from_bytes(&body[4..])
                        .unwrap()
                        .pts,
                    100,
                    "the unreadable backlog is never skipped"
                );
                tx.send(Ok(tl::enums::updates::Difference::Empty(
                    tl::types::updates::DifferenceEmpty { date: 2, seq: 0 },
                )
                .to_bytes()))
                    .unwrap();
                update_tx.send(config_update()).await.unwrap();
            })
        })
        .await
        .unwrap();
        assert_eq!(result.unwrap().0[0].0, tl::enums::Update::Config);
    }

    #[tokio::test(start_paused = true)]
    async fn unreadable_channel_difference_does_not_block_its_sibling() {
        use grammers_session::types::{
            ChannelState, PeerAuth, PeerInfo, UpdateState, UpdatesState,
        };
        use tl::{Deserializable, Identifiable, Serializable};
        let session = Arc::new(grammers_session::storages::MemorySession::default());
        cache_self_user(session.as_ref()).await;
        session
            .set_update_state(UpdateState::All(UpdatesState {
                pts: 100,
                date: 1,
                channels: vec![
                    ChannelState { id: 7, pts: 10 },
                    ChannelState { id: 8, pts: 20 },
                ],
                ..Default::default()
            }))
            .await
            .unwrap();
        for id in [7, 8] {
            session
                .cache_peer(PeerInfo::Channel {
                    id,
                    auth: Some(PeerAuth::from_hash(123)),
                    kind: None,
                })
                .await
                .unwrap();
        }
        let mut pool = SenderPool::new(Arc::clone(&session), 1);
        let mut receiver = crate::UpdatesReceiver::create(
            pool.handle.thin.clone(),
            erase(Arc::clone(&session)),
            pool.updates,
            crate::UpdatesConfiguration { catch_up: true },
        )
        .await
        .unwrap();
        // final, pts 15, and then the unreadable new_messages.
        let mut unreadable = tl::types::updates::ChannelDifference::CONSTRUCTOR_ID.to_bytes();
        unreadable.extend(1u32.to_bytes());
        unreadable.extend(15i32.to_bytes());
        unreadable.extend(too_deep_vector());
        // The caller still learns about the failure, once, without losing the position.
        let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(receiver.next(), async {
                answer_restored_account_difference(&mut pool.runner).await;
                let Request::Invoke { body, tx, .. } = pool.runner.request_rx.recv().await.unwrap()
                else {
                    panic!("expected channel difference");
                };
                let request =
                    tl::functions::updates::GetChannelDifference::from_bytes(&body[4..]).unwrap();
                assert_eq!(request.pts, 10);
                tx.send(Ok(unreadable.clone())).unwrap();
            })
        })
        .await
        .unwrap();
        assert_nesting_error(&result.unwrap_err());
        let failed_at = tokio::time::Instant::now();
        let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(receiver.next(), async {
                let Request::Invoke { body, tx, .. } = pool.runner.request_rx.recv().await.unwrap()
                else {
                    panic!("expected sibling channel difference");
                };
                assert!(failed_at.elapsed() >= Duration::from_secs(1));
                let request =
                    tl::functions::updates::GetChannelDifference::from_bytes(&body[4..]).unwrap();
                assert_eq!(request.pts, 20);
                tx.send(Ok(tl::enums::updates::ChannelDifference::Difference(
                    tl::types::updates::ChannelDifference {
                        r#final: true,
                        pts: 25,
                        timeout: None,
                        new_messages: vec![],
                        other_updates: vec![tl::enums::Update::Config],
                        chats: vec![],
                        users: vec![],
                    },
                )
                .to_bytes()))
                    .unwrap();
            })
        })
        .await
        .unwrap();
        assert_eq!(result.unwrap().0[0].0, tl::enums::Update::Config);
        receiver.sync_update_state().await.unwrap();
        let mut channels = session.updates_state().await.unwrap().channels;
        channels.sort_by_key(|channel| channel.id);
        assert_eq!(
            channels,
            vec![
                ChannelState { id: 7, pts: 10 },
                ChannelState { id: 8, pts: 25 }
            ]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn repeatedly_unreadable_differences_abandon_only_their_own_backlog() {
        use grammers_session::types::{
            ChannelState, PeerAuth, PeerInfo, UpdateState, UpdatesState,
        };
        use tl::{Deserializable, Identifiable, Serializable};
        let session = Arc::new(grammers_session::storages::MemorySession::default());
        cache_self_user(session.as_ref()).await;
        session
            .set_update_state(UpdateState::All(UpdatesState {
                pts: 100,
                date: 1,
                channels: vec![ChannelState { id: 7, pts: 10 }],
                ..Default::default()
            }))
            .await
            .unwrap();
        session
            .cache_peer(PeerInfo::Channel {
                id: 7,
                auth: Some(PeerAuth::from_hash(123)),
                kind: None,
            })
            .await
            .unwrap();
        let mut pool = SenderPool::new(Arc::clone(&session), 1);
        let mut receiver = crate::UpdatesReceiver::create(
            pool.handle.thin.clone(),
            erase(Arc::clone(&session)),
            pool.updates,
            crate::UpdatesConfiguration { catch_up: true },
        )
        .await
        .unwrap();
        let mut unreadable = tl::types::updates::Difference::CONSTRUCTOR_ID.to_bytes();
        unreadable.extend(too_deep_vector());
        // Only the account sequence is unreadable. Each failure is reported and its
        // position holds until the retries are spent. Recovery alternates, so the
        // healthy channel completes without waiting for the account.
        for attempt in 0..30 {
            let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
                tokio::join!(receiver.next(), async {
                    let Request::Invoke { body, tx, .. } =
                        pool.runner.request_rx.recv().await.unwrap()
                    else {
                        panic!("expected account difference");
                    };
                    assert_eq!(
                        tl::functions::updates::GetDifference::from_bytes(&body[4..])
                            .unwrap()
                            .pts,
                        100,
                        "attempt {attempt}"
                    );
                    tx.send(Ok(unreadable.clone())).unwrap();
                })
            })
            .await
            .unwrap();
            assert_nesting_error(&result.unwrap_err());
            if attempt == 0 {
                let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
                    tokio::join!(receiver.next(), async {
                        let Request::Invoke { body, tx, .. } =
                            pool.runner.request_rx.recv().await.unwrap()
                        else {
                            panic!("expected channel difference");
                        };
                        let request =
                            tl::functions::updates::GetChannelDifference::from_bytes(&body[4..])
                                .unwrap();
                        assert_eq!(request.pts, 10);
                        tx.send(Ok(tl::enums::updates::ChannelDifference::Difference(
                            tl::types::updates::ChannelDifference {
                                r#final: true,
                                pts: 12,
                                timeout: None,
                                new_messages: vec![],
                                other_updates: vec![tl::enums::Update::Config],
                                chats: vec![],
                                users: vec![],
                            },
                        )
                        .to_bytes()))
                            .unwrap();
                    })
                })
                .await
                .unwrap();
                assert_eq!(result.unwrap().0[0].0, tl::enums::Update::Config);
            }
        }
        // The account now starts over from fresh state; its channel position stays.
        let update_tx = pool.runner.updates_tx.clone().unwrap();
        let (result, ()) = tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(receiver.next(), async {
                expect_get_state(&mut pool.runner)
                    .await
                    .send(Ok(tl::enums::updates::State::State(
                        tl::types::updates::State {
                            pts: 500,
                            qts: 0,
                            date: 3,
                            seq: 0,
                            unread_count: 0,
                        },
                    )
                    .to_bytes()))
                    .unwrap();
                update_tx.send(config_update()).await.unwrap();
            })
        })
        .await
        .unwrap();
        assert_eq!(result.unwrap().0[0].0, tl::enums::Update::Config);
        receiver.sync_update_state().await.unwrap();
        let state = session.updates_state().await.unwrap();
        assert_eq!(state.channels, vec![ChannelState { id: 7, pts: 12 }]);
        assert_eq!(
            state.pts, 500,
            "fresh account state replaced the abandoned one"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn failing_to_store_fetched_state_is_paced_like_other_recovery_failures() {
        let session = Arc::new(RecoverySession::default());
        cache_self_user(session.as_ref()).await;
        session.fail_state.store(true, Ordering::Relaxed);
        let mut pool = SenderPool::new(Arc::clone(&session), 1);
        let mut receiver = crate::UpdatesReceiver::create(
            pool.handle.thin.clone(),
            erase(Arc::clone(&session)),
            pool.updates,
            crate::UpdatesConfiguration { catch_up: true },
        )
        .await
        .unwrap();
        let update_tx = pool.runner.updates_tx.clone().unwrap();
        let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(receiver.next(), async {
                expect_get_state(&mut pool.runner)
                    .await
                    .send(Ok(state_reply()))
                    .unwrap();
            })
        })
        .await
        .unwrap();
        assert!(result.is_err());
        let failed_at = tokio::time::Instant::now();
        // A caller retrying at once costs no further RPC inside the pause.
        tokio::time::timeout(Duration::from_millis(400), receiver.next())
            .await
            .unwrap_err();
        assert!(pool.runner.request_rx.try_recv().is_err());
        session.fail_state.store(false, Ordering::Relaxed);
        let (result, ()) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(receiver.next(), async {
                expect_get_state(&mut pool.runner)
                    .await
                    .send(Ok(state_reply()))
                    .unwrap();
                assert!(failed_at.elapsed() >= Duration::from_secs(1));
                update_tx.send(config_update()).await.unwrap();
            })
        })
        .await
        .unwrap();
        assert_eq!(result.unwrap().0[0].0, tl::enums::Update::Config);
    }

    // A receiver restored from saved state first catches up on its account.
    async fn answer_restored_account_difference(runner: &mut SenderPoolRunner) {
        use tl::{Identifiable, Serializable};
        let Request::Invoke { body, tx, .. } = runner.request_rx.recv().await.unwrap() else {
            panic!("expected account difference");
        };
        assert_eq!(
            u32::from_le_bytes(body[..4].try_into().unwrap()),
            tl::functions::updates::GetDifference::CONSTRUCTOR_ID
        );
        tx.send(Ok(tl::enums::updates::Difference::Empty(
            tl::types::updates::DifferenceEmpty { date: 2, seq: 0 },
        )
        .to_bytes()))
            .unwrap();
    }

    fn delete_channel_message(channel_id: i64, pts: i32) -> tl::enums::Update {
        tl::types::UpdateDeleteChannelMessages {
            channel_id,
            messages: vec![pts],
            pts,
            pts_count: 1,
        }
        .into()
    }

    fn short_update(update: tl::enums::Update) -> UpdatesLike {
        UpdatesLike::Updates(tl::types::UpdateShort { update, date: 2 }.into())
    }

    // Message-box deadlines follow the real clock, as in the other grace fixtures.
    #[tokio::test]
    async fn exhausted_channel_delivers_updates_received_during_its_grace() {
        use grammers_session::types::{ChannelState, UpdateState, UpdatesState};
        let session = Arc::new(grammers_session::storages::MemorySession::default());
        cache_self_user(session.as_ref()).await;
        session
            .set_update_state(UpdateState::All(UpdatesState {
                pts: 100,
                date: 1,
                channels: vec![ChannelState { id: 7, pts: 10 }],
                ..Default::default()
            }))
            .await
            .unwrap();
        let mut pool = SenderPool::new(Arc::clone(&session), 1);
        let mut receiver = crate::UpdatesReceiver::create(
            pool.handle.thin.clone(),
            erase(Arc::clone(&session)),
            pool.updates,
            crate::UpdatesConfiguration { catch_up: true },
        )
        .await
        .unwrap();
        let update_tx = pool.runner.updates_tx.clone().unwrap();
        // Saved channels recover on start. Their backlog at 11 and 12 is unreachable.
        let started = tokio::time::Instant::now();
        let (result, ()) = tokio::time::timeout(Duration::from_secs(60), async {
            tokio::join!(receiver.next(), async {
                answer_restored_account_difference(&mut pool.runner).await;
                for pts in 13..=15 {
                    tokio::time::sleep(Duration::from_secs(1)).await;
                    assert!(pool.runner.request_rx.try_recv().is_err());
                    update_tx
                        .send(short_update(delete_channel_message(7, pts)))
                        .await
                        .unwrap();
                }
            })
        })
        .await
        .unwrap();
        let (updates, _, _) = result.unwrap();
        assert!(started.elapsed() >= Duration::from_secs(30));
        assert_eq!(
            updates
                .iter()
                .map(|(update, _)| update.clone())
                .collect::<Vec<_>>(),
            (13..=15)
                .map(|pts| delete_channel_message(7, pts))
                .collect::<Vec<_>>()
        );
        assert!(pool.runner.request_rx.try_recv().is_err());
        receiver.sync_update_state().await.unwrap();
        assert_eq!(
            session.updates_state().await.unwrap().channels,
            vec![ChannelState { id: 7, pts: 15 }]
        );
    }

    #[tokio::test]
    async fn bot_tries_zero_hash_before_discarding_an_unavailable_channel() {
        use grammers_session::types::{ChannelState, PeerInfo, UpdateState, UpdatesState};
        use tl::{Deserializable, Serializable};
        // Concurrent cases: each spends the complete real-time grace.
        let case = async |accepted: bool| {
            let session = Arc::new(grammers_session::storages::MemorySession::default());
            session
                .cache_peer(PeerInfo::User {
                    id: 1,
                    auth: None,
                    bot: Some(true),
                    is_self: Some(true),
                })
                .await
                .unwrap();
            session
                .set_update_state(UpdateState::All(UpdatesState {
                    pts: 100,
                    date: 1,
                    channels: vec![ChannelState { id: 7, pts: 10 }],
                    ..Default::default()
                }))
                .await
                .unwrap();
            let mut pool = SenderPool::new(Arc::clone(&session), 1);
            let mut receiver = crate::UpdatesReceiver::create(
                pool.handle.thin.clone(),
                erase(Arc::clone(&session)),
                pool.updates,
                crate::UpdatesConfiguration { catch_up: true },
            )
            .await
            .unwrap();
            let update_tx = pool.runner.updates_tx.clone().unwrap();
            let too_long = || {
                short_update(
                    tl::types::UpdateChannelTooLong {
                        channel_id: 7,
                        pts: None,
                    }
                    .into(),
                )
            };
            let started = tokio::time::Instant::now();
            let responder = async {
                answer_restored_account_difference(&mut pool.runner).await;
                let Request::Invoke { body, tx, .. } = pool.runner.request_rx.recv().await.unwrap()
                else {
                    panic!("expected the last-resort channel difference");
                };
                assert!(
                    started.elapsed() >= Duration::from_secs(30),
                    "usable access can still appear during the normal grace"
                );
                let request =
                    tl::functions::updates::GetChannelDifference::from_bytes(&body[4..]).unwrap();
                assert_eq!((request.pts, request.limit), (10, 100_000));
                assert!(matches!(
                    request.channel,
                    tl::enums::InputChannel::Channel(input)
                        if input.channel_id == 7 && input.access_hash == 0
                ));
                if accepted {
                    tx.send(Ok(tl::enums::updates::ChannelDifference::Difference(
                        tl::types::updates::ChannelDifference {
                            r#final: true,
                            pts: 15,
                            timeout: None,
                            new_messages: vec![],
                            other_updates: vec![tl::enums::Update::Config],
                            chats: vec![],
                            users: vec![],
                        },
                    )
                    .to_bytes()))
                        .unwrap();
                    return;
                }
                tx.send(Err(InvocationError::Rpc(
                    tl::types::RpcError {
                        error_code: 400,
                        error_message: "CHANNEL_INVALID".into(),
                    }
                    .into(),
                )))
                .unwrap();
                let Request::CheckRetry { error, tx, .. } =
                    pool.runner.request_rx.recv().await.unwrap()
                else {
                    panic!("expected retry inspection")
                };
                tx.send(ControlFlow::Break(*error)).unwrap();
                update_tx
                    .send(short_update(delete_channel_message(7, 30)))
                    .await
                    .unwrap();
            };
            let (result, ()) = tokio::time::timeout(Duration::from_secs(60), async {
                tokio::join!(receiver.next(), responder)
            })
            .await
            .unwrap();
            let (updates, _, _) = result.unwrap();
            receiver.sync_update_state().await.unwrap();
            let channels = session.updates_state().await.unwrap().channels;
            if accepted {
                assert_eq!(updates[0].0, tl::enums::Update::Config);
                assert_eq!(channels, vec![ChannelState { id: 7, pts: 15 }]);
                return;
            }
            // Rejected: the position is gone, live delivery resumed, and another
            // episode retries the zero hash once rather than a further 30 seconds.
            assert_eq!(updates[0].0, delete_channel_message(7, 30));
            assert_eq!(channels, vec![ChannelState { id: 7, pts: 30 }]);
            update_tx.send(too_long()).await.unwrap();
            let retried_from = tokio::time::Instant::now();
            let (result, ()) = tokio::time::timeout(Duration::from_secs(25), async {
                tokio::join!(receiver.next(), async {
                    let Request::Invoke { tx, .. } = pool.runner.request_rx.recv().await.unwrap()
                    else {
                        panic!("expected an immediate last-resort attempt");
                    };
                    assert!(retried_from.elapsed() < Duration::from_secs(20));
                    tx.send(Ok(tl::enums::updates::ChannelDifference::Difference(
                        tl::types::updates::ChannelDifference {
                            r#final: true,
                            pts: 31,
                            timeout: None,
                            new_messages: vec![],
                            other_updates: vec![tl::enums::Update::Config],
                            chats: vec![],
                            users: vec![],
                        },
                    )
                    .to_bytes()))
                        .unwrap();
                })
            })
            .await
            .unwrap();
            assert_eq!(result.unwrap().0[0].0, tl::enums::Update::Config);
        };
        tokio::join!(case(true), case(false));
    }

    #[tokio::test]
    async fn imported_too_long_without_self_still_initializes_state() {
        let session = Arc::new(RecoverySession::default());
        session.fail_self_reads.store(true, Ordering::Relaxed);
        let mut pool = SenderPool::new(Arc::clone(&session), 1);
        let mut receiver = crate::UpdatesReceiver::create(
            pool.handle.thin.clone(),
            erase(Arc::clone(&session)),
            pool.updates,
            crate::UpdatesConfiguration { catch_up: true },
        )
        .await
        .unwrap();
        let update_tx = pool.runner.updates_tx.clone().unwrap();
        update_tx
            .send(UpdatesLike::Updates(tl::enums::Updates::TooLong))
            .await
            .unwrap();
        let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(receiver.next(), async {
                expect_get_state(&mut pool.runner)
                    .await
                    .send(Ok(state_reply()))
                    .unwrap();
                update_tx.send(config_update()).await.unwrap();
            })
        })
        .await
        .unwrap();
        assert_eq!(result.unwrap().0[0].0, tl::enums::Update::Config);
        assert_eq!(session.self_reads.load(Ordering::Relaxed), 0);
        assert_eq!(session.updates_state().await.unwrap().pts, 100);
    }

    #[tokio::test(start_paused = true)]
    async fn unproven_receiver_self_lookup_is_paced_across_cancelled_next() {
        let session = Arc::new(RecoverySession::default());
        let mut pool = SenderPool::new(Arc::clone(&session), 1);
        let mut receiver = crate::UpdatesReceiver::create(
            pool.handle.thin.clone(),
            erase(Arc::clone(&session)),
            pool.updates,
            crate::UpdatesConfiguration { catch_up: true },
        )
        .await
        .unwrap();
        assert_eq!(session.self_reads.load(Ordering::Relaxed), 0);
        for count in 1..=3 {
            assert!(
                tokio::time::timeout(Duration::from_millis(900), receiver.next())
                    .await
                    .is_err()
            );
            assert_eq!(session.self_reads.load(Ordering::Relaxed), count);
            assert!(pool.runner.request_rx.try_recv().is_err());
            tokio::time::advance(Duration::from_millis(100)).await;
        }
    }

    #[tokio::test]
    async fn live_peer_only_batch_is_preserved_without_self_lookup() {
        let session = Arc::new(RecoverySession::default());
        session.fail_self_reads.store(true, Ordering::Relaxed);
        let mut pool = SenderPool::new(Arc::clone(&session), 1);
        let mut receiver = crate::UpdatesReceiver::create(
            pool.handle.thin.clone(),
            erase(Arc::clone(&session)),
            pool.updates,
            crate::UpdatesConfiguration { catch_up: true },
        )
        .await
        .unwrap();
        let user: tl::enums::User = tl::types::UserEmpty { id: 7 }.into();
        pool.runner
            .updates_tx
            .as_ref()
            .unwrap()
            .send(UpdatesLike::Updates(
                tl::types::Updates {
                    updates: vec![],
                    users: vec![user.clone()],
                    chats: vec![],
                    date: 1,
                    seq: 0,
                }
                .into(),
            ))
            .await
            .unwrap();
        let (updates, users, chats) = tokio::time::timeout(Duration::from_secs(1), receiver.next())
            .await
            .unwrap()
            .unwrap();
        assert!(updates.is_empty());
        assert_eq!(users, vec![user]);
        assert!(chats.is_empty());
        assert_eq!(session.self_reads.load(Ordering::Relaxed), 0);
        assert!(pool.runner.request_rx.try_recv().is_err());
    }

    fn common_delete_update(pts: i32) -> UpdatesLike {
        UpdatesLike::Updates(
            tl::types::UpdateShort {
                date: 1,
                update: tl::types::UpdateDeleteMessages {
                    messages: vec![pts],
                    pts,
                    pts_count: 1,
                }
                .into(),
            }
            .into(),
        )
    }

    #[tokio::test]
    async fn live_common_and_cached_authorization_survive_reconnect_without_self_reads() {
        use grammers_session::types::PeerId;
        use tl::{Deserializable, Identifiable};
        for cached_self in [false, true] {
            let session = Arc::new(RecoverySession::default());
            let mut dc = session
                .dc_option(session.home_dc_id().unwrap())
                .unwrap()
                .unwrap();
            dc.auth_key = Some([7; 256]);
            session.memory.set_dc_option(&dc).await.unwrap();
            if cached_self {
                cache_self_user(session.as_ref()).await;
            }
            let mut pool = SenderPool::new(Arc::clone(&session), 1);
            let mut receiver = crate::UpdatesReceiver::create(
                pool.handle.thin.clone(),
                erase(Arc::clone(&session)),
                pool.updates,
                crate::UpdatesConfiguration { catch_up: true },
            )
            .await
            .unwrap();
            assert_eq!(session.self_reads.load(Ordering::Relaxed), 0);
            let update_tx = pool.runner.updates_tx.clone().unwrap();
            if cached_self {
                let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
                    tokio::join!(receiver.next(), async {
                        expect_get_state(&mut pool.runner)
                            .await
                            .send(Ok(state_reply()))
                            .unwrap();
                        update_tx.send(config_update()).await.unwrap();
                    })
                })
                .await
                .unwrap();
                assert_eq!(result.unwrap().0[0].0, tl::enums::Update::Config);
            } else {
                // Importers preserve the key, but need not cache the self user.
                session.fail_self_reads.store(true, Ordering::Relaxed);
                update_tx.send(common_delete_update(100)).await.unwrap();
                let result = tokio::time::timeout(Duration::from_secs(1), receiver.next())
                    .await
                    .unwrap()
                    .unwrap();
                assert!(
                    matches!(&result.0[0].0, tl::enums::Update::DeleteMessages(u) if u.pts == 100)
                );
            }
            let expected_reads = usize::from(cached_self);
            assert_eq!(session.self_reads.load(Ordering::Relaxed), expected_reads);
            session.fail_self_reads.store(true, Ordering::Relaxed);
            for pts in 101..=104 {
                let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
                    tokio::join!(receiver.next(), async {
                        tokio::task::yield_now().await;
                        update_tx.send(common_delete_update(pts)).await.unwrap();
                    })
                })
                .await
                .unwrap();
                assert!(
                    matches!(&result.unwrap().0[0].0, tl::enums::Update::DeleteMessages(u) if u.pts == pts)
                );
            }
            pool.runner
                .state
                .home_failures
                .fetch_add(1, Ordering::Release);
            let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
                tokio::join!(receiver.next(), async {
                    let Request::Invoke { body, tx, .. } =
                        pool.runner.request_rx.recv().await.unwrap()
                    else {
                        panic!("expected account recovery after reconnect");
                    };
                    assert_eq!(
                        u32::from_le_bytes(body[..4].try_into().unwrap()),
                        tl::functions::updates::GetDifference::CONSTRUCTOR_ID
                    );
                    assert_eq!(
                        tl::functions::updates::GetDifference::from_bytes(&body[4..])
                            .unwrap()
                            .pts,
                        104
                    );
                    tx.send(Ok(tl::enums::updates::Difference::Empty(
                        tl::types::updates::DifferenceEmpty { date: 2, seq: 1 },
                    )
                    .to_bytes()))
                        .unwrap();
                    update_tx.send(config_update()).await.unwrap();
                })
            })
            .await
            .unwrap();
            assert_eq!(result.unwrap().0[0].0, tl::enums::Update::Config);
            assert_eq!(session.self_reads.load(Ordering::Relaxed), expected_reads);
            assert_eq!(
                session
                    .memory
                    .peer(PeerId::self_user())
                    .await
                    .unwrap()
                    .is_some(),
                cached_self
            );
            receiver.sync_update_state().await.unwrap();
            assert_eq!(session.updates_state().await.unwrap().pts, 104);
        }
    }

    #[tokio::test]
    async fn imported_live_channel_recovers_initial_state_and_deadline_without_self_cache() {
        live_channel_recovery_resolves_kind(false).await;
    }

    #[tokio::test]
    async fn live_first_bot_resolves_kind_once_on_channel_recovery() {
        live_channel_recovery_resolves_kind(true).await;
    }

    async fn live_channel_recovery_resolves_kind(cached_bot: bool) {
        use grammers_session::types::{ChannelState, PeerAuth, PeerId, PeerInfo};
        use tl::{Deserializable, Identifiable};
        let session = Arc::new(RecoverySession::default());
        let mut dc = session
            .dc_option(session.home_dc_id().unwrap())
            .unwrap()
            .unwrap();
        dc.auth_key = Some([7; 256]);
        session.memory.set_dc_option(&dc).await.unwrap();
        session.fail_self_reads.store(true, Ordering::Relaxed);
        if cached_bot {
            session
                .cache_peer(PeerInfo::User {
                    id: 1,
                    auth: None,
                    bot: Some(true),
                    is_self: Some(true),
                })
                .await
                .unwrap();
        }
        let mut pool = SenderPool::new(Arc::clone(&session), 1);
        let mut receiver = crate::UpdatesReceiver::create(
            pool.handle.thin.clone(),
            erase(Arc::clone(&session)),
            pool.updates,
            crate::UpdatesConfiguration { catch_up: true },
        )
        .await
        .unwrap();
        let update_tx = pool.runner.updates_tx.clone().unwrap();
        update_tx
            .send(UpdatesLike::Updates(
                tl::types::UpdateShort {
                    date: 1,
                    update: tl::types::UpdateDeleteChannelMessages {
                        channel_id: 7,
                        messages: vec![1],
                        pts: 10,
                        pts_count: 1,
                    }
                    .into(),
                }
                .into(),
            ))
            .await
            .unwrap();
        let live = tokio::time::timeout(Duration::from_secs(1), receiver.next())
            .await
            .unwrap()
            .unwrap();
        assert!(
            matches!(&live.0[0].0, tl::enums::Update::DeleteChannelMessages(u) if u.channel_id == 7 && u.pts == 10)
        );
        // Channel-only traffic needs Common state, using the live login proof.
        let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(receiver.next(), async {
                expect_get_state(&mut pool.runner)
                    .await
                    .send(Ok(state_reply()))
                    .unwrap();
                update_tx.send(config_update()).await.unwrap();
            })
        })
        .await
        .unwrap();
        assert_eq!(result.unwrap().0[0].0, tl::enums::Update::Config);
        assert_eq!(
            session.self_reads.load(Ordering::Relaxed),
            0,
            "ordinary live delivery does not read self metadata"
        );
        session
            .cache_peer(PeerInfo::Channel {
                id: 7,
                auth: Some(PeerAuth::from_hash(123)),
                kind: None,
            })
            .await
            .unwrap();
        update_tx
            .send(UpdatesLike::Updates(
                tl::types::UpdateShort {
                    date: 1,
                    update: tl::types::UpdateChannelTooLong {
                        channel_id: 7,
                        pts: Some(999),
                    }
                    .into(),
                }
                .into(),
            ))
            .await
            .unwrap();
        // Exercise a real storage failure before kind discovery succeeds. It
        // must not cache a conservative user limit or clear the pending channel.
        let error = tokio::time::timeout(Duration::from_secs(1), receiver.next())
            .await
            .unwrap()
            .unwrap_err();
        assert!(matches!(error.root_cause(), InvocationError::Session(_)));
        assert_eq!(session.self_reads.load(Ordering::Relaxed), 1);
        assert!(pool.runner.request_rx.try_recv().is_err());
        session.fail_self_reads.store(false, Ordering::Relaxed);
        // Cancel after kind discovery and request issuance. The retry must keep
        // both the pending position and the successfully read account kind.
        {
            let next = receiver.next();
            tokio::pin!(next);
            tokio::select! {
                result = &mut next => panic!("unexpected completion: {result:?}"),
                request = pool.runner.request_rx.recv() => {
                    let Request::Invoke { body, .. } = request.unwrap() else { panic!("expected channel recovery") };
                    assert_eq!(u32::from_le_bytes(body[..4].try_into().unwrap()), tl::functions::updates::GetChannelDifference::CONSTRUCTOR_ID);
                    let request = tl::functions::updates::GetChannelDifference::from_bytes(&body[4..]).unwrap();
                    assert_eq!(request.limit, if cached_bot { 100000 } else { 100 });
                }
            }
        }
        assert_eq!(session.self_reads.load(Ordering::Relaxed), 2);
        session.fail_self_reads.store(true, Ordering::Relaxed);
        for pts in [10, 11] {
            let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
                tokio::join!(receiver.next(), async {
                    let Request::Invoke { body, tx, .. } = pool.runner.request_rx.recv().await.unwrap() else { panic!("expected channel recovery"); };
                    assert_eq!(u32::from_le_bytes(body[..4].try_into().unwrap()), tl::functions::updates::GetChannelDifference::CONSTRUCTOR_ID);
                    let request = tl::functions::updates::GetChannelDifference::from_bytes(&body[4..]).unwrap();
                    assert_eq!(request.pts, pts, "preserve the learned local position");
                    assert_eq!(request.limit, if cached_bot { 100000 } else { 100 });
                    assert!(matches!(request.channel, tl::enums::InputChannel::Channel(c) if c.channel_id == 7 && c.access_hash == 123));
                    tx.send(Ok(tl::enums::updates::ChannelDifference::Difference(tl::types::updates::ChannelDifference {
                        r#final: true, pts: pts + 1, timeout: (pts == 10).then_some(0),
                        new_messages: vec![], other_updates: vec![tl::enums::Update::Config], chats: vec![], users: vec![],
                    }).to_bytes())).unwrap();
                })
            }).await.unwrap();
            assert_eq!(result.unwrap().0[0].0, tl::enums::Update::Config);
        }
        assert_eq!(session.self_reads.load(Ordering::Relaxed), 2);
        assert_eq!(
            session
                .memory
                .peer(PeerId::self_user())
                .await
                .unwrap()
                .is_some(),
            cached_bot
        );
        receiver.sync_update_state().await.unwrap();
        let saved = session.updates_state().await.unwrap();
        assert_eq!(saved.pts, 100);
        assert_eq!(saved.channels, vec![ChannelState { id: 7, pts: 12 }]);
    }

    #[tokio::test]
    async fn deliverable_live_backlog_cannot_starve_recovery_across_cancellation() {
        use grammers_session::types::{UpdateState, UpdatesState};
        use tl::{Deserializable, Identifiable};
        for peer_only in [false, true] {
            let session = Arc::new(RecoverySession::default());
            session
                .set_update_state(UpdateState::All(UpdatesState {
                    pts: 100,
                    date: 1,
                    ..Default::default()
                }))
                .await
                .unwrap();
            session.fail_self_reads.store(true, Ordering::Relaxed);
            let mut pool = SenderPool::new(Arc::clone(&session), 1);
            let mut receiver = crate::UpdatesReceiver::create(
                pool.handle.thin.clone(),
                erase(Arc::clone(&session)),
                pool.updates,
                crate::UpdatesConfiguration { catch_up: true },
            )
            .await
            .unwrap();
            let update_tx = pool.runner.updates_tx.clone().unwrap();
            let capacity = update_tx.capacity();
            let mut updates = vec![
                tl::types::UpdateDeleteMessages {
                    messages: vec![1],
                    pts: 101,
                    pts_count: 1,
                }
                .into(),
            ];
            let users = if peer_only {
                vec![tl::types::UserEmpty { id: 7 }.into()]
            } else {
                updates.push(tl::enums::Update::Config);
                vec![]
            };
            update_tx
                .send(UpdatesLike::Updates(
                    tl::types::Updates {
                        updates,
                        users,
                        chats: vec![],
                        date: 1,
                        seq: 0,
                    }
                    .into(),
                ))
                .await
                .unwrap();
            for _ in 0..31 {
                update_tx.send(config_update()).await.unwrap();
            }
            let first = receiver.next().await.unwrap();
            if peer_only {
                assert!(first.0.is_empty());
                assert_eq!(first.1.len(), 1);
            } else {
                assert_eq!(first.0[0].0, tl::enums::Update::Config);
            }
            for _ in 0..2 {
                let response = tokio::time::timeout(Duration::from_secs(1), async {
                    let next = receiver.next(); tokio::pin!(next);
                    tokio::select! {
                        biased;
                        result = &mut next => panic!("live delivery bypassed ready recovery: {result:?}"),
                        request = pool.runner.request_rx.recv() => {
                            let Request::Invoke { body, tx, .. } = request.unwrap() else { panic!("expected account recovery"); };
                            assert_eq!(u32::from_le_bytes(body[..4].try_into().unwrap()), tl::functions::updates::GetDifference::CONSTRUCTOR_ID);
                            assert_eq!(tl::functions::updates::GetDifference::from_bytes(&body[4..]).unwrap().pts, 100);
                            tx
                        }
                    }
                }).await.unwrap();
                assert!(response.is_closed());
                assert_eq!(
                    update_tx.capacity(),
                    capacity - 31,
                    "cancelled recovery retains its turn"
                );
            }
            let (result, ()) = tokio::time::timeout(Duration::from_secs(1), async {
                tokio::join!(receiver.next(), async {
                    let Request::Invoke { body, tx, .. } =
                        pool.runner.request_rx.recv().await.unwrap()
                    else {
                        panic!("expected account recovery");
                    };
                    assert_eq!(
                        u32::from_le_bytes(body[..4].try_into().unwrap()),
                        tl::functions::updates::GetDifference::CONSTRUCTOR_ID
                    );
                    tx.send(Ok(tl::enums::updates::Difference::Empty(
                        tl::types::updates::DifferenceEmpty { date: 2, seq: 0 },
                    )
                    .to_bytes()))
                        .unwrap();
                })
            })
            .await
            .unwrap();
            assert_eq!(result.unwrap().0[0].0, tl::enums::Update::Config);
            assert_eq!(
                update_tx.capacity(),
                capacity - 30,
                "live delivery resumes after recovery"
            );
            assert_eq!(session.self_reads.load(Ordering::Relaxed), 0);
            receiver.sync_update_state().await.unwrap();
            assert_eq!(session.updates_state().await.unwrap().pts, 100);
        }
    }

    #[tokio::test]
    async fn live_authorization_with_pending_backlog_starts_recovery_without_draining_socket() {
        use grammers_session::types::{UpdateState, UpdatesState};
        use tl::{Deserializable, Identifiable};
        let session = Arc::new(RecoverySession::default());
        session
            .set_update_state(UpdateState::All(UpdatesState {
                pts: 100,
                date: 1,
                ..Default::default()
            }))
            .await
            .unwrap();
        session.fail_self_reads.store(true, Ordering::Relaxed);
        let mut pool = SenderPool::new(Arc::clone(&session), 1);
        let mut receiver = crate::UpdatesReceiver::create(
            pool.handle.thin.clone(),
            erase(Arc::clone(&session)),
            pool.updates,
            crate::UpdatesConfiguration { catch_up: true },
        )
        .await
        .unwrap();
        let update_tx = pool.runner.updates_tx.clone().unwrap();
        let capacity = update_tx.capacity();
        assert!(capacity >= 32);
        for pts in 101..=132 {
            update_tx.send(common_delete_update(pts)).await.unwrap();
        }
        let response = tokio::time::timeout(Duration::from_secs(1), async {
            let next = receiver.next(); tokio::pin!(next);
            tokio::select! {
                biased;
                _ = &mut next => panic!("pending PTS batches cannot be delivered before recovery"),
                request = pool.runner.request_rx.recv() => {
                    let Request::Invoke { body, tx, .. } = request.unwrap() else { panic!("expected account difference"); };
                    assert_eq!(u32::from_le_bytes(body[..4].try_into().unwrap()), tl::functions::updates::GetDifference::CONSTRUCTOR_ID);
                    assert_eq!(tl::functions::updates::GetDifference::from_bytes(&body[4..]).unwrap().pts, 100);
                    tx
                }
            }
        }).await.unwrap();
        assert!(
            response.is_closed(),
            "cancelling next must cancel its RPC waiter"
        );
        assert!(
            update_tx.capacity() <= capacity - 30,
            "recovery must not drain ready pending updates"
        );
        assert_eq!(session.self_reads.load(Ordering::Relaxed), 0);
        receiver.sync_update_state().await.unwrap();
        assert_eq!(session.updates_state().await.unwrap().pts, 100);
    }

    #[tokio::test]
    async fn get_state_survives_cancelled_next_and_failed_storage_with_saved_channels() {
        use grammers_session::types::{ChannelState, UpdateState, UpdatesState};
        let session = Arc::new(RecoverySession {
            memory: Default::default(),
            fail_state: AtomicBool::new(false),
            dc_write_delay: Duration::ZERO,
            ..Default::default()
        });
        cache_self_user(session.as_ref()).await;
        let channels = vec![ChannelState { id: 7, pts: 10 }];
        session
            .set_update_state(UpdateState::All(UpdatesState {
                pts: 0,
                qts: 5,
                channels: channels.clone(),
                ..Default::default()
            }))
            .await
            .unwrap();
        let mut pool = SenderPool::new(Arc::clone(&session), 1);
        let mut receiver = crate::UpdatesReceiver::create(
            pool.handle.thin.clone(),
            erase(Arc::clone(&session)),
            pool.updates,
            crate::UpdatesConfiguration { catch_up: true },
        )
        .await
        .unwrap();
        // Cancel next only after GetState has actually entered the request queue.
        let cancelled = {
            let next = receiver.next();
            tokio::pin!(next);
            tokio::select! { biased; _ = &mut next => panic!("no response"), tx = expect_get_state(&mut pool.runner) => tx }
        };
        assert!(cancelled.is_closed());
        drop(cancelled);
        session.fail_state.store(true, Ordering::Relaxed);
        let (result, ()) = tokio::join!(receiver.next(), async {
            expect_get_state(&mut pool.runner)
                .await
                .send(Ok(state_reply()))
                .unwrap();
        });
        assert!(matches!(result, Err(InvocationError::Session(_))));
        assert_eq!(session.updates_state().await.unwrap().pts, 0);
        let update_tx = pool.runner.updates_tx.clone().unwrap();
        let (result, ()) = tokio::join!(receiver.next(), async {
            expect_get_state(&mut pool.runner)
                .await
                .send(Ok(state_reply()))
                .unwrap();
            update_tx.send(config_update()).await.unwrap();
        });
        assert_eq!(result.unwrap().0[0].0, tl::enums::Update::Config);
        let saved = session.updates_state().await.unwrap();
        assert_eq!(saved.pts, 100);
        assert_eq!(saved.qts, 0);
        assert_eq!(saved.channels, channels);
    }

    #[tokio::test]
    async fn invalid_socket_channel_does_not_start_recovery_or_block_updates() {
        let session = Arc::new(grammers_session::storages::MemorySession::default());
        let mut pool = SenderPool::new(Arc::clone(&session), 1);
        let mut receiver = crate::UpdatesReceiver::create(
            pool.handle.thin.clone(),
            erase(Arc::clone(&session)),
            pool.updates,
            crate::UpdatesConfiguration { catch_up: true },
        )
        .await
        .unwrap();
        let update_tx = pool.runner.updates_tx.clone().unwrap();
        update_tx
            .send(UpdatesLike::Updates(
                tl::types::UpdateShort {
                    date: 1,
                    update: tl::types::UpdateChannelTooLong {
                        channel_id: i64::MAX,
                        pts: Some(1),
                    }
                    .into(),
                }
                .into(),
            ))
            .await
            .unwrap();
        update_tx.send(config_update()).await.unwrap();
        let (update, ()) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(receiver.next(), async {
                expect_get_state(&mut pool.runner)
                    .await
                    .send(Ok(state_reply()))
                    .unwrap();
            })
        })
        .await
        .unwrap();
        let update = update.unwrap();
        assert_eq!(update.0[0].0, tl::enums::Update::Config);
        receiver.sync_update_state().await.unwrap();
        assert!(session.updates_state().await.unwrap().channels.is_empty());
    }

    #[tokio::test]
    async fn invalid_get_state_preserves_storage_and_retries_recovery() {
        use grammers_session::types::{ChannelState, UpdateState, UpdatesState};
        let session = Arc::new(grammers_session::storages::MemorySession::default());
        cache_self_user(session.as_ref()).await;
        let saved = UpdatesState {
            channels: vec![ChannelState { id: 7, pts: 10 }],
            ..Default::default()
        };
        session
            .set_update_state(UpdateState::All(saved.clone()))
            .await
            .unwrap();
        let mut pool = SenderPool::new(Arc::clone(&session), 1);
        let mut receiver = crate::UpdatesReceiver::create(
            pool.handle.thin.clone(),
            erase(Arc::clone(&session)),
            pool.updates,
            crate::UpdatesConfiguration { catch_up: true },
        )
        .await
        .unwrap();
        for field in 0..4 {
            let mut state = tl::types::updates::State {
                pts: 100,
                qts: 0,
                date: 1,
                seq: 1,
                unread_count: 0,
            };
            *match field {
                0 => &mut state.pts,
                1 => &mut state.qts,
                2 => &mut state.date,
                _ => &mut state.seq,
            } = -1;
            let (result, ()) = tokio::join!(receiver.next(), async {
                expect_get_state(&mut pool.runner)
                    .await
                    .send(Ok(tl::enums::updates::State::State(state).to_bytes()))
                    .unwrap();
            });
            assert!(matches!(
                result,
                Err(InvocationError::Deserialize(
                    grammers_mtproto::mtp::DeserializeError::InvalidEnvelope { .. }
                ))
            ));
            assert_eq!(session.updates_state().await.unwrap(), saved);
        }
        let update_tx = pool.runner.updates_tx.clone().unwrap();
        let (result, ()) = tokio::join!(receiver.next(), async {
            expect_get_state(&mut pool.runner)
                .await
                .send(Ok(state_reply()))
                .unwrap();
            update_tx.send(config_update()).await.unwrap();
        });
        assert_eq!(result.unwrap().0[0].0, tl::enums::Update::Config);
        let recovered = session.updates_state().await.unwrap();
        assert_eq!(recovered.pts, 100);
        assert_eq!(recovered.channels, saved.channels);
    }

    #[tokio::test]
    async fn logged_out_home_failure_does_not_request_account_state() {
        let session = Arc::new(grammers_session::storages::MemorySession::default());
        let mut pool = SenderPool::new(Arc::clone(&session), 1);
        let mut receiver = crate::UpdatesReceiver::create(
            pool.handle.thin.clone(),
            erase(session),
            pool.updates,
            crate::UpdatesConfiguration { catch_up: true },
        )
        .await
        .unwrap();
        pool.runner
            .state
            .home_failures
            .fetch_add(1, Ordering::Release);
        pool.runner
            .updates_tx
            .as_ref()
            .unwrap()
            .send(config_update())
            .await
            .unwrap();
        assert_eq!(
            receiver.next().await.unwrap().0[0].0,
            tl::enums::Update::Config
        );
        assert!(matches!(
            pool.runner.request_rx.try_recv(),
            Err(queue::TryRecvError::Empty)
        ));
    }

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
        rpc.permit.as_ref().unwrap().mark_sent(&mut None);
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

    #[tokio::test(flavor = "current_thread")]
    async fn control_backlog_yields_before_draining_ready_work() {
        use std::sync::atomic::AtomicUsize;
        struct Count(Arc<AtomicUsize>);
        impl crate::RetryPolicy for Count {
            fn should_retry(&self, _: &RetryContext) -> ControlFlow<(), Duration> {
                self.0.fetch_add(1, Ordering::Relaxed);
                ControlFlow::Break(())
            }
        }
        let count = Arc::new(AtomicUsize::new(0));
        let pool = SenderPool::with_configuration(
            Arc::new(grammers_session::storages::MemorySession::default()),
            1,
            ConnectionParams {
                retry_policy: Box::new(Count(Arc::clone(&count))),
                ..Default::default()
            },
        );
        for _ in 0..1000 {
            let (tx, _) = oneshot::channel();
            pool.handle
                .thin
                .0
                .send(Request::CheckRetry {
                    error: Box::new(InvocationError::InvalidDc),
                    fail_count: NonZeroU32::MIN,
                    slept_so_far: Duration::ZERO,
                    tx,
                })
                .unwrap();
        }
        let runner = tokio::spawn(pool.runner.run_checked());
        tokio::task::yield_now().await;
        let processed = count.load(Ordering::Relaxed);
        assert!(
            processed > 0 && processed < 1000,
            "runner processed {processed} before yielding"
        );
        pool.handle.quit();
        runner.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn concurrent_configuration_merges_survive_yielding_storage() {
        use grammers_session::storages::MemorySession;
        use grammers_session::{
            BoxFuture,
            types::{PeerId, PeerInfo, UpdateState, UpdatesState},
        };
        struct YieldingSession(MemorySession);
        impl Session for YieldingSession {
            type Error = <MemorySession as Session>::Error;
            fn home_dc_id(&self) -> Result<i32, Self::Error> {
                self.0.home_dc_id()
            }
            fn set_home_dc_id(&self, id: i32) -> BoxFuture<'_, Result<(), Self::Error>> {
                self.0.set_home_dc_id(id)
            }
            fn dc_option(&self, id: i32) -> Result<Option<DcOption>, Self::Error> {
                self.0.dc_option(id)
            }
            fn set_dc_option(&self, option: &DcOption) -> BoxFuture<'_, Result<(), Self::Error>> {
                let option = option.clone();
                Box::pin(async move {
                    tokio::task::yield_now().await;
                    self.0.set_dc_option(&option).await
                })
            }
            fn peer(&self, id: PeerId) -> BoxFuture<'_, Result<Option<PeerInfo>, Self::Error>> {
                self.0.peer(id)
            }
            fn cache_peer(&self, peer: PeerInfo) -> BoxFuture<'_, Result<(), Self::Error>> {
                self.0.cache_peer(peer)
            }
            fn updates_state(&self) -> BoxFuture<'_, Result<UpdatesState, Self::Error>> {
                self.0.updates_state()
            }
            fn set_update_state(
                &self,
                state: UpdateState,
            ) -> BoxFuture<'_, Result<(), Self::Error>> {
                self.0.set_update_state(state)
            }
        }
        let session = Arc::new(YieldingSession(MemorySession::default()));
        let pool = SenderPool::new(Arc::clone(&session), 1);
        let option = tl::types::DcOption {
            ipv6: false,
            media_only: false,
            tcpo_only: false,
            cdn: false,
            r#static: true,
            this_port_only: false,
            id: 90,
            ip_address: "127.0.0.1".into(),
            port: 444,
            secret: None,
        };
        let ipv4 = [option.clone().into()];
        let ipv6 = [tl::types::DcOption {
            ipv6: true,
            ip_address: "::1".into(),
            port: 445,
            ..option
        }
        .into()];
        let (a, b) = tokio::join!(
            pool.runner.update_dc_options(&ipv4),
            pool.runner.update_dc_options(&ipv6)
        );
        a.unwrap();
        b.unwrap();
        let merged = session.dc_option(90).unwrap().unwrap();
        assert_eq!(merged.ipv4, "127.0.0.1:444".parse().unwrap());
        assert_eq!(merged.ipv6, "[::1]:445".parse().unwrap());
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
    async fn byte_admission_is_released_with_sdk_request_not_waiter() {
        let mut pool = SenderPool::with_configuration(
            Arc::new(grammers_session::storages::MemorySession::default()),
            1,
            ConnectionParams {
                max_pending_bytes: std::num::NonZeroUsize::new(8),
                ..Default::default()
            },
        );
        let first = pool
            .handle
            .start_invoke_shared(4, Bytes::from_static(&[1; 8]), None)
            .unwrap();
        let request = pool.runner.request_rx.recv().await.unwrap();
        drop(first);
        assert!(
            matches!(pool.handle.start_invoke_shared(4, Bytes::from_static(&[1; 4]), None),
            Err(InvocationError::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock)
        );
        drop(request);
        assert!(
            pool.handle
                .start_invoke_shared(4, Bytes::from_static(&[1; 8]), None)
                .is_ok()
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn full_update_queue_does_not_hide_home_failure_notification() {
        let pool = SenderPool::new(
            Arc::new(grammers_session::storages::MemorySession::default()),
            1,
        );
        let handle = &pool.handle.thin;
        let state = &pool.runner.state;
        let tx = pool.runner.updates_tx.as_ref().unwrap();
        while tx.try_send(UpdatesLike::ConnectionClosed).is_ok() {}
        let old = handle.home_failure_generation();
        let before = handle.status().dropped_updates;
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let (sender, peer) = tokio::join!(
            connect_with_auth(
                Transport::new(),
                ServerAddr::Tcp {
                    address: listener.local_addr().unwrap()
                },
                [0; 256]
            ),
            listener.accept(),
        );
        let (_rpc_tx, rpc_rx) = queue::unbounded_channel();
        drop(peer.unwrap());
        let (outcome, ()) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(
                run_sender(
                    sender.unwrap(),
                    rpc_rx,
                    Some(tx.clone()),
                    pool.runner.session.home_dc_id().unwrap(),
                    Arc::clone(&pool.runner.session),
                    Arc::clone(state)
                ),
                handle.wait_home_failure(old),
            )
        })
        .await
        .unwrap();
        assert!(outcome.1.is_err());
        assert_eq!(handle.home_failure_generation(), old + 1);
        assert_eq!(handle.status().dropped_updates, before + 1);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn same_dc_initialization_is_shared_and_other_dcs_remain_responsive() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let session = Arc::new(grammers_session::storages::MemorySession::default());
        let std::net::SocketAddr::V4(ipv4) = address else {
            unreachable!()
        };
        session
            .set_dc_option(&DcOption {
                id: 4,
                ipv4,
                ipv6: SocketAddrV6::new(Ipv6Addr::LOCALHOST, address.port(), 0, 0),
                auth_key: Some([0; 256]),
            })
            .await
            .unwrap();
        let mut pool = SenderPool::with_configuration(session, 1, ConnectionParams::default());
        let first = pool
            .handle
            .start_invoke_shared(4, Bytes::from_static(&[1; 4]), None)
            .unwrap();
        let second = pool
            .handle
            .start_invoke_shared(4, Bytes::from_static(&[1; 4]), None)
            .unwrap();
        for _ in 0..2 {
            let request = pool.runner.request_rx.recv().await.unwrap();
            assert!(pool.runner.process_request(request).await.is_continue());
        }
        assert_eq!(pool.runner.pending.len(), 1);
        assert_eq!(pool.runner.pending[0].requests.len(), 2);
        let runner = tokio::spawn(pool.runner.run_checked());
        let (peer, _) = listener.accept().await.unwrap();
        let other = pool
            .handle
            .start_invoke_shared(999, Bytes::from_static(&[1; 4]), None)
            .unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(1), other)
                .await
                .unwrap(),
            Err(InvocationError::InvalidDc)
        ));
        drop(peer);
        let first = first.await.unwrap_err();
        let second = second.await.unwrap_err();
        let (InvocationError::Shared(a), InvocationError::Shared(b)) = (first, second) else {
            panic!("failure not shared");
        };
        assert!(Arc::ptr_eq(&a, &b));
        pool.handle.quit();
        runner.await.unwrap().unwrap();
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
    async fn update_receiver_skips_missing_and_invalid_channels_without_stalling() {
        use grammers_session::types::{
            ChannelState, PeerAuth, PeerInfo, UpdateState, UpdatesState,
        };
        use tl::Identifiable;
        let session = Arc::new(grammers_session::storages::MemorySession::default());
        cache_self_user(session.as_ref()).await;
        session
            .set_update_state(UpdateState::All(UpdatesState {
                pts: 100,
                qts: 0,
                date: 1,
                seq: 0,
                channels: (1..=13).map(|id| ChannelState { id, pts: 10 }).collect(),
            }))
            .await
            .unwrap();
        for id in 2..=13 {
            session
                .cache_peer(PeerInfo::Channel {
                    id,
                    auth: Some(PeerAuth::from_hash(123)),
                    kind: None,
                })
                .await
                .unwrap();
        }
        let mut pool = SenderPool::new(Arc::clone(&session), 1);
        let mut receiver = crate::UpdatesReceiver::create(
            pool.handle.thin.clone(),
            erase(Arc::clone(&session)),
            pool.updates,
            crate::UpdatesConfiguration { catch_up: true },
        )
        .await
        .unwrap();
        let consumer = async {
            loop {
                let result = receiver.next().await.unwrap();
                if !result.0.is_empty() {
                    assert_eq!(result.0[0].0, tl::enums::Update::Config);
                    break;
                }
            }
        };
        let responder = async {
            for expected in [0, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13] {
                let (body, tx) = loop {
                    match pool.runner.request_rx.recv().await.unwrap() {
                        Request::Invoke { body, tx, .. } => break (body, tx),
                        Request::CheckRetry { error, tx, .. } => {
                            let _ = tx.send(ControlFlow::Break(*error));
                        }
                        _ => panic!("expected RPC"),
                    }
                };
                let constructor = u32::from_le_bytes(body[..4].try_into().unwrap());
                let response =
                    if expected == 0 {
                        assert_eq!(
                            constructor,
                            tl::functions::updates::GetDifference::CONSTRUCTOR_ID
                        );
                        Ok(tl::enums::updates::Difference::Empty(
                            tl::types::updates::DifferenceEmpty { date: 2, seq: 0 },
                        )
                        .to_bytes())
                    } else {
                        assert_eq!(
                            constructor,
                            tl::functions::updates::GetChannelDifference::CONSTRUCTOR_ID
                        );
                        // constructor + flags + inputChannel constructor precede its ID.
                        assert_eq!(
                            i64::from_le_bytes(body[12..20].try_into().unwrap()),
                            expected
                        );
                        if expected == 2 {
                            Err(InvocationError::Rpc(
                                tl::types::RpcError {
                                    error_code: 400,
                                    error_message: "CHANNEL_INVALID".into(),
                                }
                                .into(),
                            ))
                        } else if expected == 3 {
                            Err(InvocationError::Shared(Arc::new(InvocationError::Rpc(
                                tl::types::RpcError {
                                    error_code: 500,
                                    error_message: "INTERNAL".into(),
                                }
                                .into(),
                            ))))
                        } else if expected < 13 {
                            let (code, name) = match expected {
                                4 => (400, "PERSISTENT_TIMESTAMP_EMPTY"),
                                5 => (400, "PERSISTENT_TIMESTAMP_INVALID"),
                                6 => (400, "FROZEN_PARTICIPANT_MISSING"),
                                7 => (403, "CHAT_WRITE_FORBIDDEN"),
                                8 => (406, "CHANNEL_PRIVATE"),
                                9 => (400, "UNKNOWN_CHANNEL_ERROR"),
                                10 => (403, "UNKNOWN_CHANNEL_ERROR"),
                                11 => (406, "UNKNOWN_CHANNEL_ERROR"),
                                12 => (500, "PERSISTENT_TIMESTAMP_OUTDATED"),
                                _ => unreachable!(),
                            };
                            Err(InvocationError::Shared(Arc::new(InvocationError::Rpc(
                                tl::types::RpcError {
                                    error_code: code,
                                    error_message: name.into(),
                                }
                                .into(),
                            ))))
                        } else {
                            Ok(tl::enums::updates::ChannelDifference::Difference(
                                tl::types::updates::ChannelDifference {
                                    r#final: true,
                                    pts: 11,
                                    timeout: None,
                                    new_messages: vec![],
                                    other_updates: vec![tl::enums::Update::Config],
                                    chats: vec![],
                                    users: vec![],
                                },
                            )
                            .to_bytes())
                        }
                    };
                tx.send(response).unwrap();
            }
        };
        tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(consumer, responder);
        })
        .await
        .unwrap();
        receiver.sync_update_state().await.unwrap();
        let saved = session.updates_state().await.unwrap();
        assert!(
            saved
                .channels
                .iter()
                .any(|channel| channel.id == 1 && channel.pts == 10)
        );
        assert!(
            saved
                .channels
                .iter()
                .any(|channel| channel.id == 3 && channel.pts == 10)
        );
        for id in 9..=12 {
            assert!(
                saved
                    .channels
                    .iter()
                    .any(|channel| channel.id == id && channel.pts == 10)
            );
        }
        assert!(
            saved
                .channels
                .iter()
                .any(|channel| channel.id == 13 && channel.pts == 11)
        );
        assert!(
            saved
                .channels
                .iter()
                .all(|channel| !matches!(channel.id, 2 | 4..=8))
        );
    }

    #[tokio::test]
    async fn missing_hash_and_uncached_channels_eventually_resume_socket_delivery() {
        use grammers_session::types::{ChannelState, PeerInfo, UpdateState, UpdatesState};
        use tl::Identifiable;
        let session = Arc::new(grammers_session::storages::MemorySession::default());
        cache_self_user(session.as_ref()).await;
        session
            .set_update_state(UpdateState::All(UpdatesState {
                pts: 100,
                qts: 0,
                date: 1,
                seq: 0,
                channels: vec![
                    ChannelState { id: 7, pts: 10 },
                    ChannelState { id: 8, pts: 20 },
                ],
            }))
            .await
            .unwrap();
        session
            .cache_peer(PeerInfo::Channel {
                id: 7,
                auth: None,
                kind: None,
            })
            .await
            .unwrap();
        let mut pool = SenderPool::new(Arc::clone(&session), 1);
        let mut receiver = crate::UpdatesReceiver::create(
            pool.handle.thin.clone(),
            erase(Arc::clone(&session)),
            pool.updates,
            crate::UpdatesConfiguration { catch_up: true },
        )
        .await
        .unwrap();
        let update_tx = pool.runner.updates_tx.clone().unwrap();
        let (finished, mut done) = oneshot::channel();
        let consumer = async {
            let mut received = [false; 2];
            while !received.into_iter().all(|seen| seen) {
                for (update, _) in receiver.next().await.unwrap().0 {
                    if let tl::enums::Update::DeleteChannelMessages(update) = update {
                        match update.channel_id {
                            7 => received[0] = true,
                            8 => received[1] = true,
                            _ => panic!("unexpected channel"),
                        }
                    }
                }
            }
            finished.send(()).unwrap();
        };
        let producer = async {
            let Request::Invoke { body, tx, .. } = pool.runner.request_rx.recv().await.unwrap()
            else {
                panic!("expected account difference");
            };
            assert_eq!(
                u32::from_le_bytes(body[..4].try_into().unwrap()),
                tl::functions::updates::GetDifference::CONSTRUCTOR_ID
            );
            tx.send(Ok(tl::enums::updates::Difference::Empty(
                tl::types::updates::DifferenceEmpty { date: 2, seq: 0 },
            )
            .to_bytes()))
                .unwrap();
            for tick in 1..=40 {
                tokio::select! {
                    _ = &mut done => return,
                    _ = tokio::time::sleep(Duration::from_secs(1)) => {},
                }
                assert!(
                    pool.runner.request_rx.try_recv().is_err(),
                    "missing hashes cannot issue channel differences"
                );
                update_tx
                    .send(UpdatesLike::Updates(tl::enums::Updates::Updates(
                        tl::types::Updates {
                            updates: [(7, 10), (8, 20)]
                                .into_iter()
                                .map(|(channel_id, pts)| {
                                    tl::types::UpdateDeleteChannelMessages {
                                        channel_id,
                                        messages: vec![tick],
                                        pts: pts + tick,
                                        pts_count: 1,
                                    }
                                    .into()
                                })
                                .collect(),
                            users: vec![],
                            chats: vec![],
                            date: 2,
                            seq: 0,
                        },
                    )))
                    .await
                    .unwrap();
            }
            panic!("bounded missing-hash recovery never resumed socket updates");
        };
        tokio::time::timeout(Duration::from_secs(45), async {
            tokio::join!(consumer, producer);
        })
        .await
        .unwrap();
        receiver.sync_update_state().await.unwrap();
        let saved = session.updates_state().await.unwrap();
        assert!(
            saved
                .channels
                .iter()
                .any(|channel| channel.id == 7 && channel.pts > 10)
        );
        assert!(
            saved
                .channels
                .iter()
                .any(|channel| channel.id == 8 && channel.pts > 20)
        );
        update_tx
            .send(UpdatesLike::Updates(
                tl::types::Updates {
                    updates: [7, 8]
                        .into_iter()
                        .map(|channel_id| {
                            tl::types::UpdateChannelTooLong {
                                channel_id,
                                pts: Some(1000),
                            }
                            .into()
                        })
                        .chain(std::iter::once(tl::enums::Update::Config))
                        .collect(),
                    users: vec![],
                    chats: vec![],
                    date: 2,
                    seq: 0,
                }
                .into(),
            ))
            .await
            .unwrap();
        // Live updates retained through the grace may still be queued ahead of it.
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                let (updates, _, _) = receiver.next().await.unwrap();
                if updates[0].0 == tl::enums::Update::Config {
                    break;
                }
                assert!(updates.iter().all(|(update, _)| matches!(
                    update,
                    tl::enums::Update::DeleteChannelMessages(_)
                )));
            }
        })
        .await
        .unwrap();
        let producer = async {
            tokio::task::yield_now().await;
            update_tx
                .send(UpdatesLike::Updates(
                    tl::types::Updates {
                        updates: saved
                            .channels
                            .iter()
                            .map(|channel| {
                                tl::types::UpdateDeleteChannelMessages {
                                    channel_id: channel.id,
                                    messages: vec![100],
                                    pts: channel.pts + 1,
                                    pts_count: 1,
                                }
                                .into()
                            })
                            .collect(),
                        users: vec![],
                        chats: vec![],
                        date: 2,
                        seq: 0,
                    }
                    .into(),
                ))
                .await
                .unwrap();
        };
        let (resumed, ()) = tokio::time::timeout(Duration::from_secs(1), async {
            tokio::join!(receiver.next(), producer)
        })
        .await
        .unwrap();
        assert_eq!(
            resumed.unwrap().0.len(),
            2,
            "second episode must not start another grace"
        );
        assert!(pool.runner.request_rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn channel_hash_arriving_mid_grace_recovers_from_saved_pts() {
        use grammers_session::types::{
            ChannelState, PeerAuth, PeerInfo, UpdateState, UpdatesState,
        };
        use tl::{Deserializable, Identifiable};
        let session = Arc::new(grammers_session::storages::MemorySession::default());
        cache_self_user(session.as_ref()).await;
        session
            .set_update_state(UpdateState::All(UpdatesState {
                pts: 100,
                date: 1,
                channels: vec![ChannelState { id: 7, pts: 10 }],
                ..Default::default()
            }))
            .await
            .unwrap();
        let mut pool = SenderPool::new(Arc::clone(&session), 1);
        let mut receiver = crate::UpdatesReceiver::create(
            pool.handle.thin.clone(),
            erase(Arc::clone(&session)),
            pool.updates,
            crate::UpdatesConfiguration { catch_up: true },
        )
        .await
        .unwrap();
        let update_tx = pool.runner.updates_tx.clone().unwrap();
        let started = std::time::Instant::now();
        let consumer = async {
            loop {
                let result = receiver.next().await.unwrap();
                if !result.0.is_empty() {
                    assert_eq!(result.0[0].0, tl::enums::Update::Config);
                    break;
                }
            }
        };
        let responder = async {
            let Request::Invoke { body, tx, .. } = pool.runner.request_rx.recv().await.unwrap()
            else {
                panic!("expected account difference");
            };
            assert_eq!(
                u32::from_le_bytes(body[..4].try_into().unwrap()),
                tl::functions::updates::GetDifference::CONSTRUCTOR_ID
            );
            tx.send(Ok(tl::enums::updates::Difference::Empty(
                tl::types::updates::DifferenceEmpty { date: 2, seq: 0 },
            )
            .to_bytes()))
                .unwrap();
            tokio::time::sleep(Duration::from_millis(100)).await;
            assert!(pool.runner.request_rx.try_recv().is_err());
            update_tx
                .send(UpdatesLike::Updates(
                    tl::types::UpdateShort {
                        date: 2,
                        update: tl::types::UpdateDeleteChannelMessages {
                            channel_id: 7,
                            messages: vec![1],
                            pts: 11,
                            pts_count: 1,
                        }
                        .into(),
                    }
                    .into(),
                ))
                .await
                .unwrap();
            session
                .cache_peer(PeerInfo::Channel {
                    id: 7,
                    auth: Some(PeerAuth::from_hash(123)),
                    kind: None,
                })
                .await
                .unwrap();
            let Request::Invoke { body, tx, .. } = pool.runner.request_rx.recv().await.unwrap()
            else {
                panic!("expected channel difference");
            };
            assert!(
                started.elapsed() >= Duration::from_millis(900),
                "honor one-second hash retry"
            );
            assert_eq!(
                u32::from_le_bytes(body[..4].try_into().unwrap()),
                tl::functions::updates::GetChannelDifference::CONSTRUCTOR_ID
            );
            let request =
                tl::functions::updates::GetChannelDifference::from_bytes(&body[4..]).unwrap();
            assert_eq!(request.pts, 10);
            assert!(
                matches!(request.channel, tl::enums::InputChannel::Channel(channel) if channel.channel_id == 7 && channel.access_hash == 123)
            );
            tx.send(Ok(tl::enums::updates::ChannelDifference::Difference(
                tl::types::updates::ChannelDifference {
                    r#final: true,
                    pts: 15,
                    timeout: None,
                    new_messages: vec![],
                    other_updates: vec![tl::enums::Update::Config],
                    chats: vec![],
                    users: vec![],
                },
            )
            .to_bytes()))
                .unwrap();
        };
        tokio::time::timeout(Duration::from_secs(3), async {
            tokio::join!(consumer, responder);
        })
        .await
        .unwrap();
        receiver.sync_update_state().await.unwrap();
        assert_eq!(
            session.updates_state().await.unwrap().channels,
            vec![ChannelState { id: 7, pts: 15 }]
        );
    }

    #[tokio::test(start_paused = true)]
    async fn saved_channels_wait_for_cached_self_before_any_recovery_rpc() {
        use grammers_session::types::{
            ChannelState, PeerAuth, PeerInfo, UpdateState, UpdatesState,
        };
        use tl::{Deserializable, Identifiable};
        for pts in [0, 100] {
            let session = Arc::new(grammers_session::storages::MemorySession::default());
            session
                .set_update_state(UpdateState::All(UpdatesState {
                    pts,
                    date: 1,
                    channels: vec![ChannelState { id: 7, pts: 10 }],
                    ..Default::default()
                }))
                .await
                .unwrap();
            session
                .cache_peer(PeerInfo::Channel {
                    id: 7,
                    auth: Some(PeerAuth::from_hash(123)),
                    kind: None,
                })
                .await
                .unwrap();
            let mut pool = SenderPool::new(Arc::clone(&session), 1);
            let mut receiver = crate::UpdatesReceiver::create(
                pool.handle.thin.clone(),
                erase(Arc::clone(&session)),
                pool.updates,
                crate::UpdatesConfiguration { catch_up: true },
            )
            .await
            .unwrap();
            let consumer = async {
                loop {
                    let result = receiver.next().await.unwrap();
                    if !result.0.is_empty() {
                        assert_eq!(result.0[0].0, tl::enums::Update::Config);
                        break;
                    }
                }
            };
            let responder = async {
                tokio::time::sleep(Duration::from_secs(3)).await;
                assert!(
                    pool.runner.request_rx.try_recv().is_err(),
                    "saved state cannot authorize recovery"
                );
                cache_self_user(session.as_ref()).await;
                let Request::Invoke { body, tx, .. } = pool.runner.request_rx.recv().await.unwrap()
                else {
                    panic!("expected account recovery");
                };
                let constructor = u32::from_le_bytes(body[..4].try_into().unwrap());
                if pts == 0 {
                    assert_eq!(
                        constructor,
                        tl::functions::updates::GetState::CONSTRUCTOR_ID
                    );
                    tx.send(Ok(state_reply())).unwrap();
                } else {
                    assert_eq!(
                        constructor,
                        tl::functions::updates::GetDifference::CONSTRUCTOR_ID
                    );
                    tx.send(Ok(tl::enums::updates::Difference::Empty(
                        tl::types::updates::DifferenceEmpty { date: 2, seq: 0 },
                    )
                    .to_bytes()))
                        .unwrap();
                }
                let Request::Invoke { body, tx, .. } = pool.runner.request_rx.recv().await.unwrap()
                else {
                    panic!("expected channel recovery");
                };
                assert_eq!(
                    u32::from_le_bytes(body[..4].try_into().unwrap()),
                    tl::functions::updates::GetChannelDifference::CONSTRUCTOR_ID
                );
                let request =
                    tl::functions::updates::GetChannelDifference::from_bytes(&body[4..]).unwrap();
                assert_eq!(request.pts, 10);
                assert!(
                    matches!(request.channel, tl::enums::InputChannel::Channel(channel) if channel.channel_id == 7 && channel.access_hash == 123)
                );
                tx.send(Ok(tl::enums::updates::ChannelDifference::Difference(
                    tl::types::updates::ChannelDifference {
                        r#final: true,
                        pts: 11,
                        timeout: None,
                        new_messages: vec![],
                        other_updates: vec![tl::enums::Update::Config],
                        chats: vec![],
                        users: vec![],
                    },
                )
                .to_bytes()))
                    .unwrap();
            };
            tokio::time::timeout(Duration::from_secs(6), async {
                tokio::join!(consumer, responder);
            })
            .await
            .unwrap();
            receiver.sync_update_state().await.unwrap();
            assert_eq!(
                session.updates_state().await.unwrap().channels,
                vec![ChannelState { id: 7, pts: 11 }]
            );
        }
    }

    #[tokio::test(start_paused = true)]
    async fn channel_recovery_propagates_auth_flood_and_io_without_losing_state() {
        use grammers_session::types::{
            ChannelState, PeerAuth, PeerInfo, UpdateState, UpdatesState,
        };
        use tl::Identifiable;
        let errors = [
            (401, "AUTH_KEY_UNREGISTERED"),
            (406, "AUTH_KEY_DUPLICATED"),
            (400, "AUTH_KEY_INVALID"),
            (403, "SESSION_REVOKED"),
            (400, "CONNECTION_NOT_INITED"),
            (400, "API_ID_INVALID"),
            (401, "CHANNEL_PRIVATE"),
            (420, "FLOOD_WAIT_10"),
            (400, "FLOOD_WAIT_10"),
            (403, "SLOWMODE_WAIT_2"),
        ]
        .into_iter()
        .map(|(error_code, error_message)| {
            InvocationError::Rpc(
                tl::types::RpcError {
                    error_code,
                    error_message: error_message.into(),
                }
                .into(),
            )
        })
        .chain(std::iter::once(InvocationError::Io(std::io::Error::new(
            std::io::ErrorKind::ConnectionReset,
            "channel recovery transport fixture",
        ))));
        for error in errors {
            let original = Arc::new(error);
            let session = Arc::new(grammers_session::storages::MemorySession::default());
            cache_self_user(session.as_ref()).await;
            let channels = vec![
                ChannelState { id: 7, pts: 10 },
                ChannelState { id: 8, pts: 20 },
            ];
            session
                .set_update_state(UpdateState::All(UpdatesState {
                    pts: 100,
                    date: 1,
                    channels: channels.clone(),
                    ..Default::default()
                }))
                .await
                .unwrap();
            for id in [7, 8] {
                session
                    .cache_peer(PeerInfo::Channel {
                        id,
                        auth: Some(PeerAuth::from_hash(123)),
                        kind: None,
                    })
                    .await
                    .unwrap();
            }
            let mut pool = SenderPool::new(Arc::clone(&session), 1);
            let mut receiver = crate::UpdatesReceiver::create(
                pool.handle.thin.clone(),
                erase(Arc::clone(&session)),
                pool.updates,
                crate::UpdatesConfiguration { catch_up: true },
            )
            .await
            .unwrap();
            let consumer = async {
                loop {
                    match receiver.next().await {
                        Ok(result) => assert!(result.0.is_empty()),
                        Err(error) => break error,
                    }
                }
            };
            let responder = async {
                let Request::Invoke { body, tx, .. } = pool.runner.request_rx.recv().await.unwrap()
                else {
                    panic!("expected account difference");
                };
                assert_eq!(
                    u32::from_le_bytes(body[..4].try_into().unwrap()),
                    tl::functions::updates::GetDifference::CONSTRUCTOR_ID
                );
                tx.send(Ok(tl::enums::updates::Difference::Empty(
                    tl::types::updates::DifferenceEmpty { date: 2, seq: 0 },
                )
                .to_bytes()))
                    .unwrap();
                let Request::Invoke { body, tx, .. } = pool.runner.request_rx.recv().await.unwrap()
                else {
                    panic!("expected first channel difference");
                };
                assert_eq!(
                    u32::from_le_bytes(body[..4].try_into().unwrap()),
                    tl::functions::updates::GetChannelDifference::CONSTRUCTOR_ID
                );
                assert_eq!(i64::from_le_bytes(body[12..20].try_into().unwrap()), 7);
                tx.send(Err(InvocationError::Shared(Arc::clone(&original))))
                    .unwrap();
                let Request::CheckRetry { error, tx, .. } =
                    pool.runner.request_rx.recv().await.unwrap()
                else {
                    panic!("expected retry-policy query");
                };
                tx.send(ControlFlow::Break(*error)).unwrap();
            };
            let (error, ()) = tokio::time::timeout(Duration::from_secs(2), async {
                tokio::join!(consumer, responder)
            })
            .await
            .unwrap();
            let InvocationError::Shared(actual) = error else {
                panic!("original error must survive")
            };
            assert!(Arc::ptr_eq(&actual, &original));
            receiver.sync_update_state().await.unwrap();
            assert_eq!(session.updates_state().await.unwrap().channels, channels);
            assert!(pool.runner.request_rx.try_recv().is_err());
        }
    }

    #[tokio::test]
    async fn empty_update_receiver_rearms_get_state_after_home_failure() {
        use tl::Identifiable;
        let session = Arc::new(grammers_session::storages::MemorySession::default());
        let mut pool = SenderPool::new(Arc::clone(&session), 1);
        let mut receiver = crate::UpdatesReceiver::create(
            pool.handle.thin.clone(),
            erase(Arc::clone(&session)),
            pool.updates,
            crate::UpdatesConfiguration { catch_up: false },
        )
        .await
        .unwrap();
        cache_self_user(session.as_ref()).await;
        pool.runner
            .state
            .home_failures
            .fetch_add(1, Ordering::Release);
        let tx = pool.runner.updates_tx.clone().unwrap();
        for fail in [true, false] {
            let responder = async {
                let Request::Invoke {
                    body, tx: response, ..
                } = pool.runner.request_rx.recv().await.unwrap()
                else {
                    panic!("expected GetState")
                };
                assert_eq!(
                    u32::from_le_bytes(body[..4].try_into().unwrap()),
                    tl::functions::updates::GetState::CONSTRUCTOR_ID
                );
                if fail {
                    response
                        .send(Err(InvocationError::Rpc(
                            tl::types::RpcError {
                                error_code: 401,
                                error_message: "AUTH_KEY_UNREGISTERED".into(),
                            }
                            .into(),
                        )))
                        .unwrap();
                    let Request::CheckRetry { error, tx, .. } =
                        pool.runner.request_rx.recv().await.unwrap()
                    else {
                        panic!("expected retry inspection")
                    };
                    tx.send(ControlFlow::Break(*error)).unwrap();
                } else {
                    response
                        .send(Ok(tl::enums::updates::State::State(
                            tl::types::updates::State {
                                pts: 1,
                                qts: 0,
                                date: 1,
                                seq: 0,
                                unread_count: 0,
                            },
                        )
                        .to_bytes()))
                        .unwrap();
                    tx.send(UpdatesLike::Updates(
                        tl::types::UpdateShort {
                            update: tl::enums::Update::Config,
                            date: 1,
                        }
                        .into(),
                    ))
                    .await
                    .unwrap();
                }
            };
            // The retry following a 401 is deliberately paced by about one second.
            tokio::time::timeout(Duration::from_secs(3), async {
                let (result, ()) = tokio::join!(receiver.next(), responder);
                if fail {
                    assert!(result.unwrap_err().is("AUTH_KEY_UNREGISTERED"));
                } else {
                    assert_eq!(result.unwrap().0[0].0, tl::enums::Update::Config);
                }
            })
            .await
            .unwrap();
        }
    }

    async fn silent_pool(params: ConnectionParams) -> (SenderPool, tokio::net::TcpListener) {
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
        (SenderPool::with_configuration(session, 1, params), listener)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn two_hundred_cold_cancellations_close_peer_without_quit() {
        use tokio::io::AsyncReadExt;
        let (pool, listener) = silent_pool(ConnectionParams::default()).await;
        let calls: Vec<_> = (0..200)
            .map(|_| {
                pool.handle
                    .start_invoke_shared(4, Bytes::from_static(&[1; 4]), None)
                    .unwrap()
            })
            .collect();
        let runner = tokio::spawn(pool.runner.run_checked());
        let (mut peer, _) = listener.accept().await.unwrap();
        drop(calls);
        tokio::time::timeout(Duration::from_secs(1), peer.read_to_end(&mut Vec::new()))
            .await
            .expect("caller cancellation must close initialization socket")
            .unwrap();
        assert!(!runner.is_finished());
        pool.handle.quit();
        runner.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn partial_cold_cancellation_releases_admission_for_other_dc() {
        let (pool, listener) = silent_pool(ConnectionParams {
            max_pending_requests: std::num::NonZeroUsize::new(200),
            max_pending_bytes: std::num::NonZeroUsize::new(800),
            ..Default::default()
        })
        .await;
        let mut calls: Vec<_> = (0..200)
            .map(|_| {
                pool.handle
                    .start_invoke_shared(4, Bytes::from_static(&[1; 4]), None)
                    .unwrap()
            })
            .collect();
        let live = calls.pop().unwrap();
        let runner = tokio::spawn(pool.runner.run_checked());
        let (_peer, _) = listener.accept().await.unwrap();
        drop(calls);
        let other = tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                match pool
                    .handle
                    .start_invoke_shared(999, Bytes::from_static(&[1; 4]), None)
                {
                    Ok(call) => break call.await,
                    Err(e) if e.category() == crate::InvocationErrorCategory::Overloaded => {
                        tokio::task::yield_now().await
                    }
                    Err(e) => panic!("{e}"),
                }
            }
        })
        .await
        .expect("cancelled cold waiters retained permits");
        assert!(matches!(other, Err(InvocationError::InvalidDc)));
        assert!(pool.handle.disconnect_from_dc(4));
        assert!(
            tokio::time::timeout(Duration::from_secs(1), live)
                .await
                .unwrap()
                .is_err()
        );
        pool.handle.quit();
        runner.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn stale_setup_completion_cannot_retire_replacement_waiter() {
        let (mut pool, _listener) = silent_pool(ConnectionParams::default()).await;
        let first = pool
            .handle
            .start_invoke_shared(4, Bytes::from_static(&[1; 4]), None)
            .unwrap();
        let request = pool.runner.request_rx.recv().await.unwrap();
        assert!(pool.runner.process_request(request).await.is_continue());
        let old_id = pool.runner.pending[0].abort_handle.id();
        pool.runner.cancel_setup(4);
        assert!(first.await.is_err());
        let second = pool
            .handle
            .start_invoke_shared(4, Bytes::from_static(&[1; 4]), None)
            .unwrap();
        let request = pool.runner.request_rx.recv().await.unwrap();
        assert!(pool.runner.process_request(request).await.is_continue());
        pool.runner
            .finish_setup((4, old_id, Err(InvocationError::InvalidDc)));
        assert_eq!(pool.runner.pending.len(), 1);
        assert_eq!(pool.runner.pending[0].requests.len(), 1);
        drop(second);
    }

    #[test]
    fn request_exceeding_entire_byte_budget_is_permanently_invalid() {
        let pool = SenderPool::with_configuration(
            Arc::new(grammers_session::storages::MemorySession::default()),
            1,
            ConnectionParams {
                max_pending_bytes: std::num::NonZeroUsize::new(4),
                ..Default::default()
            },
        );
        let error = pool
            .handle
            .start_invoke_shared(4, Bytes::from_static(&[1; 8]), None)
            .err()
            .unwrap();
        assert!(
            matches!(error, InvocationError::Io(e) if e.kind() == std::io::ErrorKind::InvalidInput)
        );
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
    async fn rejected_cached_key_starts_a_fresh_plain_handshake() {
        use tl::Identifiable;
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let std::net::SocketAddr::V4(address) = listener.local_addr().unwrap() else {
            unreachable!()
        };
        let session = Arc::new(grammers_session::storages::MemorySession::default());
        session
            .set_dc_option(&DcOption {
                id: 4,
                ipv4: address,
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
                connection_timeout: Some(Duration::from_secs(5)),
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
        tokio::time::timeout(Duration::from_secs(2), async {
            let (mut first, _) = listener.accept().await.unwrap();
            assert_eq!(first.read_u8().await.unwrap(), 0xef);
            let mut rejection = vec![1]; // abridged one-word status packet
            rejection.extend((-404i32).to_le_bytes());
            first.write_all(&rejection).await.unwrap();
            // Keep the first socket alive with unread request bytes, avoiding
            // a TCP reset that could race the definitive status response.
            let (mut fresh, _) = listener.accept().await.unwrap();
            assert_eq!(fresh.read_u8().await.unwrap(), 0xef);
            let words = fresh.read_u8().await.unwrap();
            assert!(words < 127);
            let mut body = vec![0; usize::from(words) * 4];
            fresh.read_exact(&mut body).await.unwrap();
            assert_eq!(&body[..8], &[0; 8]); // unencrypted auth_key_id
            assert_eq!(
                &body[20..24],
                &tl::functions::ReqPqMulti::CONSTRUCTOR_ID.to_le_bytes()
            );
        })
        .await
        .unwrap();
        drop(call);
        handle.quit();
        tokio::time::timeout(Duration::from_secs(2), runner)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(tracker.stage(), crate::InvocationStage::Idle);
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

    #[cfg(feature = "proxy")]
    #[tokio::test]
    async fn http_proxy_covers_fresh_keys_cached_keys_and_recreated_dc_connections() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        tokio::time::timeout(Duration::from_secs(5), async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let session = Arc::new(grammers_session::storages::MemorySession::default());
            for id in [1, 2] {
                session.set_dc_option(&DcOption {
                    id,
                    ipv4: format!("192.0.2.{id}:443").parse().unwrap(),
                    ipv6: format!("[2001:db8::{id}]:443").parse().unwrap(),
                    auth_key: (id == 1).then_some([0; 256]),
                }).await.unwrap();
            }
            let pool = SenderPool::with_configuration(session, 1, ConnectionParams {
                proxy_url: Some(format!("http://{}", listener.local_addr().unwrap())),
                connection_timeout: Some(Duration::from_secs(1)),
                ..Default::default()
            });
            let mut setup = pool.runner.setup_context();
            let server = async {
                for id in [1, 2, 1] {
                    let (mut socket, _) = listener.accept().await.unwrap();
                    let mut request = Vec::new();
                    while !request.ends_with(b"\r\n\r\n") { request.push(socket.read_u8().await.unwrap()); }
                    assert_eq!(request, format!("CONNECT 192.0.2.{id}:443 HTTP/1.1\r\nHost: 192.0.2.{id}:443\r\n\r\n").as_bytes());
                    socket.write_all(b"HTTP/1.1 403 Denied\r\n\r\n").await.unwrap();
                }
            };
            let client = async {
                for id in [1, 2, 1] {
                    assert!(matches!(setup.create_connection(id).await, Err(InvocationError::Io(error)) if error.kind() == std::io::ErrorKind::ConnectionRefused));
                }
            };
            tokio::join!(client, server);
        }).await.unwrap();
    }

    #[cfg(feature = "proxy")]
    #[tokio::test]
    async fn connection_initialization_deadline_includes_stalled_http_proxy_handshake() {
        use tokio::io::AsyncReadExt;
        tokio::time::timeout(Duration::from_secs(5), async {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let session = Arc::new(grammers_session::storages::MemorySession::default());
            let pool = SenderPool::with_configuration(session, 1, ConnectionParams {
                proxy_url: Some(format!("http://{}", listener.local_addr().unwrap())),
                connection_timeout: Some(Duration::from_millis(30)),
                ..Default::default()
            });
            let mut setup = pool.runner.setup_context();
            let server = async {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut bytes = Vec::new();
                socket.read_to_end(&mut bytes).await.unwrap();
                assert!(bytes.starts_with(b"CONNECT "));
            };
            let client = async {
                assert!(matches!(setup.create_connection(2).await, Err(InvocationError::Io(error)) if error.kind() == std::io::ErrorKind::TimedOut));
            };
            tokio::join!(client, server);
        }).await.unwrap();
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
            assert!(result.1.is_err());
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
                    .1
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
                    let limit = 1_044_372 - if wrapped { 4 } else { 0 };
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
        println!(
            "spawned_connection_future={} sender={} bytes",
            size_of_val(&connection),
            sender_size
        );
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
