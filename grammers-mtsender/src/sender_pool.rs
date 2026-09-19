// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::net::{Ipv4Addr, Ipv6Addr, SocketAddrV4, SocketAddrV6};
use std::num::NonZeroU32;
use std::ops::{ControlFlow, Deref};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use std::{fmt, panic};

use bytes::Bytes;
use grammers_mtproto::{mtp, transport};
use grammers_session::Session;
use grammers_session::storages::{ErasedSession, erase};
use grammers_session::types::DcOption;
use grammers_session::updates::UpdatesLike;
use grammers_tl_types::{self as tl, Deserializable, enums};
use log::{debug, info, warn};
use tokio::task::AbortHandle;
use tokio::{
    sync::{mpsc, oneshot},
    task::JoinSet,
    time::sleep,
};

use crate::configuration::{ConnectionParams, RetryContext};
use crate::errors::{InvocationError, ReadError};
use crate::{Sender, ServerAddr, connect, connect_with_auth};

/// How long to wait after warning the user that the updates was dropped.
const UPDATES_DROPPED_LOG_COOLDOWN: Duration = Duration::from_secs(300);

pub(crate) type Transport = transport::Full;

type InvokeResponse = Vec<u8>;

enum Request {
    Invoke {
        dc_id: i32,
        body: Bytes,
        tx: oneshot::Sender<Result<InvokeResponse, InvocationError>>,
    },
    CheckRetry {
        error: InvocationError,
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
    tx: oneshot::Sender<Result<InvokeResponse, InvocationError>>,
}

struct ConnectionInfo {
    dc_id: i32,
    rpc_tx: mpsc::UnboundedSender<Rpc>,
    abort_handle: AbortHandle,
}

// One small list per pool (usually one DC). Written only on connection changes;
// the read guard is released before queueing or awaiting a response.
#[derive(Default)]
struct RouteCache {
    entries: Vec<(i32, mpsc::UnboundedSender<Rpc>)>,
    disconnecting: Vec<(i32, usize)>,
    stopping: bool,
}
type Routes = Arc<RwLock<RouteCache>>;

impl RouteCache {
    fn publish(&mut self, dc_id: i32, tx: &mpsc::UnboundedSender<Rpc>) {
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
pub struct SenderPoolHandle(mpsc::UnboundedSender<Request>, Routes);

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
    request_rx: mpsc::UnboundedReceiver<Request>,
    updates_tx: mpsc::Sender<UpdatesLike>,
    connections: Vec<ConnectionInfo>,
    connection_pool: JoinSet<Result<(), ReadError>>,
    routes: Routes,
}

impl Deref for SenderPoolFatHandle {
    type Target = SenderPoolHandle;

    fn deref(&self) -> &Self::Target {
        &self.thin
    }
}

impl SenderPoolHandle {
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
    pub async fn raw_invoke_shared_in_dc(
        &self,
        dc_id: i32,
        body: Bytes,
    ) -> Result<InvokeResponse, InvocationError> {
        if body.len() < 4 {
            return Err(tl::deserialize::Error::UnexpectedEof.into());
        }
        let (tx, rx) = oneshot::channel();
        let rpc = Rpc { body, tx };
        let rpc = match self.try_route(dc_id, rpc)? {
            None => return rx.await.map_err(|_| InvocationError::Dropped)?,
            Some(rpc) => rpc,
        };
        self.0
            .send(Request::Invoke {
                dc_id,
                body: rpc.body,
                tx: rpc.tx,
            })
            .map_err(|_| InvocationError::Dropped)?;
        rx.await.map_err(|_| InvocationError::Dropped)?
    }

    fn try_route(&self, dc_id: i32, rpc: Rpc) -> Result<Option<Rpc>, InvocationError> {
        if self.0.is_closed() {
            return Err(InvocationError::Dropped);
        }
        let route = {
            let cache = self.1.read().unwrap_or_else(|e| e.into_inner());
            if cache.stopping {
                return Err(InvocationError::Dropped);
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
    /// Has no effect if there was no connection to the datacenter.
    ///
    /// This is useful after datacenter migrations during sign in,
    /// when the old connection is known to not be needed anymore.
    pub fn disconnect_from_dc(&self, dc_id: i32) -> bool {
        {
            let mut cache = self.1.write().unwrap_or_else(|e| e.into_inner());
            cache.begin_disconnect(dc_id);
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
                Err(e) => {
                    let error_info = format!("{}", e);
                    match self.check_retry(e, fail_count, slept_so_far).await {
                        ControlFlow::Continue(delay) => {
                            info!("sleeping on {} for {:?} before retrying", error_info, delay,);
                            sleep(delay).await;
                            fail_count = fail_count.saturating_add(1);
                            slept_so_far += delay;
                            continue;
                        }
                        ControlFlow::Break(final_error) => break Err(final_error),
                    }
                }
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
        let (tx, rx) = oneshot::channel();
        if self
            .0
            .send(Request::CheckRetry {
                error,
                fail_count,
                slept_so_far,
                tx,
            })
            .is_err()
        {
            return ControlFlow::Break(InvocationError::Dropped);
        }

        match rx.await {
            Ok(flow) => flow,
            Err(_) => ControlFlow::Break(InvocationError::Dropped),
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
        let session = erase(session);
        let (request_tx, request_rx) = mpsc::unbounded_channel();
        let routes = Routes::default();
        let (updates_tx, updates_rx) =
            mpsc::channel(connection_params.updates_channel_capacity.get());

        Self {
            runner: SenderPoolRunner {
                session: Arc::clone(&session),
                api_id,
                connection_params,
                request_rx,
                updates_tx,
                connections: Vec::new(),
                connection_pool: JoinSet::new(),
                routes: Arc::clone(&routes),
            },
            handle: SenderPoolFatHandle {
                thin: SenderPoolHandle(request_tx, routes),
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
    pub async fn run(mut self) {
        loop {
            tokio::select! {
                biased;
                completion = self.connection_pool.join_next(), if !self.connection_pool.is_empty() => {
                    if let Err(err) = completion.unwrap() {
                        if let Ok(reason) = err.try_into_panic() {
                            panic::resume_unwind(reason);
                        }
                    }
                    self.connections
                        .retain(|connection| !connection.abort_handle.is_finished());
                    self.routes.write().unwrap_or_else(|e| e.into_inner())
                        .entries.retain(|(_, tx)| !tx.is_closed());
                }
                request = self.request_rx.recv() => {
                    let flow = if let Some(request) = request {
                        self.process_request(request).await
                    } else {
                        ControlFlow::Break(())
                    };
                    match flow {
                        ControlFlow::Continue(_) => continue,
                        ControlFlow::Break(_) => break,
                    }
                }
            }
        }

        {
            let mut cache = self.routes.write().unwrap_or_else(|e| e.into_inner());
            cache.stopping = true;
            cache.entries.clear();
        }
        self.connections.clear(); // drop all channels to cause the `run_sender` loops to stop
        self.connection_pool.join_all().await;
    }

    async fn process_request(&mut self, request: Request) -> ControlFlow<()> {
        match request {
            Request::Invoke { dc_id, body, tx } => {
                if tx.is_closed() {
                    return ControlFlow::Continue(());
                }
                let connection =
                    match self.connections.iter().find(|connection| {
                        connection.dc_id == dc_id && !connection.rpc_tx.is_closed()
                    }) {
                        Some(connection) => connection,
                        None => match self.create_connection(dc_id).await {
                            Ok(x) => x,
                            Err(e) => {
                                let _ = tx.send(Err(e));
                                return ControlFlow::Continue(());
                            }
                        },
                    };
                if !tx.is_closed() {
                    let _ = connection.rpc_tx.send(Rpc { body, tx });
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
                    error,
                };
                let flow = match self
                    .connection_params
                    .retry_policy
                    .should_retry(&retry_context)
                {
                    ControlFlow::Continue(delay) => ControlFlow::Continue(delay),
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
                        false
                    } else {
                        true
                    }
                });
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

        let sender = self.connect_sender(&dc_option).await?;

        dc_option.auth_key = Some(sender.auth_key());
        self.session.set_dc_option(&dc_option).await?;

        let (rpc_tx, rpc_rx) = mpsc::unbounded_channel();
        let abort_handle = self.connection_pool.spawn(run_sender(
            sender,
            rpc_rx,
            self.updates_tx.clone(),
            dc_option.id == self.session.home_dc_id()?,
        ));
        {
            let mut cache = self.routes.write().unwrap_or_else(|e| e.into_inner());
            cache.publish(dc_id, &rpc_tx);
        }
        self.connections.push(ConnectionInfo {
            dc_id,
            rpc_tx,
            abort_handle,
        });
        Ok(self.connections.last().unwrap())
    }

    async fn connect_sender(
        &mut self,
        dc_option: &DcOption,
    ) -> Result<Sender<transport::Full, mtp::Encrypted>, InvocationError> {
        let transport = transport::Full::new;

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

        let enums::Config::Config(remote_config) = match sender.invoke(&init_connection).await {
            Ok(config) => config,
            Err(InvocationError::Transport(transport::Error::BadStatus { status: 404 })) => {
                sender = connect(transport(), addr()).await?;
                sender.invoke(&init_connection).await?
            }
            Err(e) => return Err(e),
        };

        self.update_config(remote_config).await?;

        Ok(sender)
    }

    async fn update_config(
        &self,
        config: tl::types::Config,
    ) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        for option in config
            .dc_options
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
                    option
                        .ip_address
                        .parse()
                        .expect("Telegram to return a valid IPv6 address"),
                    option.port as _,
                    0,
                    0,
                );
            } else {
                dc_option.ipv4 = SocketAddrV4::new(
                    option
                        .ip_address
                        .parse()
                        .expect("Telegram to return a valid IPv4 address"),
                    option.port as _,
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

async fn run_sender(
    mut sender: Sender<Transport, grammers_mtproto::mtp::Encrypted>,
    mut rpc_rx: mpsc::UnboundedReceiver<Rpc>,
    updates: mpsc::Sender<UpdatesLike>,
    home_sender: bool,
) -> Result<(), ReadError> {
    let mut dropped: usize = 0;
    let mut last_update_limit_warn = Instant::now();
    let mut updates_closed = false;

    loop {
        tokio::select! {
            step = sender.step() => match step {
                Ok(all_new_updates) => {
                    if updates_closed {
                        continue;
                    }
                    for new_updates in all_new_updates {
                        if let Err(e) = updates.try_send(new_updates) {
                            match e {
                                mpsc::error::TrySendError::Full(_) => {
                                    dropped += 1;
                                    if last_update_limit_warn.elapsed() >= UPDATES_DROPPED_LOG_COOLDOWN {
                                        warn!(
                                          "updates channel full (cap {}), dropped {} updates in the last {:?}",
                                          updates.max_capacity(), dropped, last_update_limit_warn.elapsed()
                                        );
                                        dropped = 0;
                                        last_update_limit_warn = Instant::now();
                                    }
                                }
                                mpsc::error::TrySendError::Closed(_) => {
                                    debug!("updates channel closed, stopping update forwarding");
                                    updates_closed = true;
                                    break;
                                }
                            }
                        }
                    }
                },
                Err(err) => {
                    if home_sender {
                        let _ = updates.try_send(UpdatesLike::ConnectionClosed);
                    }
                    break Err(err)
                },
            },
            rpc = rpc_rx.recv() => match rpc {
                Some(rpc) => sender.enqueue_body(rpc.body, rpc.tx),
                None => break Ok(()),
            },
        }
    }
}

impl fmt::Debug for Request {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Invoke { dc_id, body, tx } => f
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

    fn fake_handle() -> (SenderPoolHandle, mpsc::UnboundedReceiver<Request>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (SenderPoolHandle(tx, Routes::default()), rx)
    }

    #[tokio::test]
    async fn warm_route_bypasses_pool_and_keeps_shared_payload() {
        let (handle, mut pool_rx) = fake_handle();
        let (tx, mut rx) = mpsc::unbounded_channel();
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
            Err(mpsc::error::TryRecvError::Empty)
        ));
        rpc.tx.send(Ok(vec![7; 4])).unwrap();
        assert_eq!(call.await.unwrap().unwrap(), [7; 4]);
    }

    #[tokio::test]
    async fn rejected_queue_send_falls_back_once_but_lost_reply_does_not() {
        let (handle, mut pool_rx) = fake_handle();
        let (tx, rx) = mpsc::unbounded_channel();
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
        let (new_tx, mut new_rx) = mpsc::unbounded_channel();
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
        assert!(matches!(call.await.unwrap(), Err(InvocationError::Dropped)));
        assert!(matches!(
            pool_rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[test]
    fn pending_disconnects_block_stale_route_publication() {
        let (handle, mut rx) = fake_handle();
        let (tx, _rpc_rx) = mpsc::unbounded_channel();
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
        let (tx, mut rx) = mpsc::unbounded_channel();
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
        let (tx, mut rx) = mpsc::unbounded_channel::<Rpc>();
        runner.routes.write().unwrap().publish(4, &tx);
        let abort_handle = runner.connection_pool.spawn(async move {
            while let Some(rpc) = rx.recv().await {
                let _ = rpc.tx.send(Ok(vec![1; 4]));
            }
            Ok(())
        });
        runner.connections.push(ConnectionInfo {
            dc_id: 4,
            rpc_tx: tx,
            abort_handle,
        });
        let task = tokio::spawn(runner.run());
        assert!(handle.quit());
        // Even if connection creation completes during quit, it cannot republish.
        let (late_tx, _late_rx) = mpsc::unbounded_channel();
        handle.1.write().unwrap().publish(4, &late_tx);
        assert!(handle.1.read().unwrap().entries.is_empty());
        assert!(matches!(
            handle
                .raw_invoke_shared_in_dc(4, Bytes::from_static(&[1; 4]))
                .await,
            Err(InvocationError::Dropped)
        ));
        tokio::time::timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
    }

    #[tokio::test]
    async fn stopped_pool_rejects_even_a_still_open_cached_route() {
        let (handle, pool_rx) = fake_handle();
        let (tx, mut rx) = mpsc::unbounded_channel();
        handle.1.write().unwrap().publish(4, &tx);
        drop(pool_rx);
        assert!(matches!(
            handle
                .raw_invoke_shared_in_dc(4, Bytes::from_static(&[1; 4]))
                .await,
            Err(InvocationError::Dropped)
        ));
        assert!(matches!(
            rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
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
                let (rpc_tx, mut rpc_rx) = mpsc::unbounded_channel::<Rpc>();
                let forwarding_tx = rpc_tx.clone();
                let forwarder = tokio::spawn(async move {
                    while let Some(Request::Invoke { body, tx, .. }) = pool_rx.recv().await {
                        assert!(forwarding_tx.send(Rpc { body, tx }).is_ok());
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
        let (tx, mut rx) = mpsc::unbounded_channel();
        let handle = SenderPoolHandle(tx, Routes::default());
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
        assert!(matches!(
            rx.try_recv(),
            Err(mpsc::error::TryRecvError::Empty)
        ));
    }

    #[tokio::test]
    async fn shared_and_owned_request_apis_preserve_the_payload_allocation() {
        for shared in [false, true] {
            let (tx, mut rx) = mpsc::unbounded_channel();
            let handle = SenderPoolHandle(tx, Routes::default());
            let body = vec![1, 0, 0, 0];
            let address = body.as_ptr() as usize;
            let call = tokio::spawn(async move {
                if shared {
                    handle.raw_invoke_shared_in_dc(4, body.into()).await
                } else {
                    handle.raw_invoke_in_dc(4, body).await
                }
            });
            let Request::Invoke { body, tx, dc_id } = rx.recv().await.unwrap() else {
                panic!("expected invocation");
            };
            assert_eq!(dc_id, 4);
            assert_eq!(body.as_ptr() as usize, address);
            tx.send(Ok(vec![2, 0, 0, 0])).unwrap();
            assert_eq!(call.await.unwrap().unwrap(), [2, 0, 0, 0]);
        }
    }

    #[tokio::test]
    async fn typed_retry_reuses_request_storage_and_keeps_retry_policy() {
        let (tx, mut rx) = mpsc::unbounded_channel();
        let handle = SenderPoolHandle(tx, Routes::default());
        let call = tokio::spawn(async move { handle.do_invoke_in_dc(4, vec![1, 0, 0, 0]).await });
        let Request::Invoke {
            body: first, tx, ..
        } = rx.recv().await.unwrap()
        else {
            panic!("invoke");
        };
        tx.send(Err(InvocationError::Dropped)).unwrap();
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
