//! Managed, opt-in lifecycle for the ordinary client.
use std::fmt;
use std::future::Future;
use std::num::NonZeroUsize;
use std::panic::AssertUnwindSafe;
#[cfg(any(feature = "session-file", feature = "session-import-files"))]
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::future::{BoxFuture, FutureExt, Shared};
use grammers_mtsender::{
    AutoSleep, ConnectionParams, InvocationError, PoolEvent, PoolStatus, RetryPolicy, SenderPool,
    SenderPoolHandle, SenderPoolRunner,
};
use grammers_session::Session;
use grammers_session::storages::{ErasedSession, MemorySession, erase};
use grammers_session::updates::UpdatesLike;
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::AbortHandle;

use super::{Client, ClientConfiguration, ClientInner};

/// An explicitly selected local source. Session strings and paths are redacted
/// in `Debug`. Building a client never authorizes an account or contacts Telegram.
pub struct SessionSource(Source);

enum Source {
    Memory,
    Storage(Arc<ErasedSession>),
    #[cfg(feature = "session-file")]
    File(PathBuf),
    #[cfg(feature = "session-string")]
    Native(String),
    #[cfg(feature = "session-import")]
    Pyrogram(String),
    #[cfg(feature = "session-import")]
    Telethon(String),
    #[cfg(feature = "session-import")]
    Gotd(String),
    #[cfg(feature = "session-import-files")]
    PyrogramFile(PathBuf),
    #[cfg(feature = "session-import-files")]
    TelethonFile(PathBuf),
    #[cfg(feature = "session-import-files")]
    GotdFile(PathBuf),
}

impl fmt::Debug for SessionSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("SessionSource([redacted])")
    }
}

impl SessionSource {
    /// Fresh, memory-only session (the default).
    pub fn memory() -> Self {
        Self(Source::Memory)
    }

    /// Use existing session storage. Do not share it with another active client.
    pub fn storage<S>(session: Arc<S>) -> Self
    where
        S: Session + Sized,
        S::Error: std::error::Error + Send + Sync + 'static,
    {
        Self(Source::Storage(erase(session)))
    }

    /// Use storage whose error type is already erased.
    pub fn erased_storage(session: Arc<ErasedSession>) -> Self {
        Self(Source::Storage(session))
    }

    /// Open a native grammers SQLite session, creating it if missing.
    #[cfg(feature = "session-file")]
    pub fn file(path: impl Into<PathBuf>) -> Self {
        Self(Source::File(path.into()))
    }

    /// Decode a native `grammers:1:` session string.
    #[cfg(feature = "session-string")]
    pub fn native(text: impl Into<String>) -> Self {
        Self(Source::Native(text.into()))
    }

    /// Import an exported Pyrogram session string into memory.
    #[cfg(feature = "session-import")]
    pub fn pyrogram(text: impl Into<String>) -> Self {
        Self(Source::Pyrogram(text.into()))
    }

    /// Import an exported Telethon v1 string into memory.
    #[cfg(feature = "session-import")]
    pub fn telethon(text: impl Into<String>) -> Self {
        Self(Source::Telethon(text.into()))
    }

    /// Import gotd Loader v1 JSON into memory.
    #[cfg(feature = "session-import")]
    pub fn gotd(text: impl Into<String>) -> Self {
        Self(Source::Gotd(text.into()))
    }

    /// Read a supported Pyrogram SQLite file into memory; never modify the source.
    #[cfg(feature = "session-import-files")]
    pub fn pyrogram_file(path: impl Into<PathBuf>) -> Self {
        Self(Source::PyrogramFile(path.into()))
    }

    /// Read a supported Telethon SQLite file into memory; never modify the source.
    #[cfg(feature = "session-import-files")]
    pub fn telethon_file(path: impl Into<PathBuf>) -> Self {
        Self(Source::TelethonFile(path.into()))
    }

    /// Read a gotd Loader v1 JSON file into memory.
    #[cfg(feature = "session-import-files")]
    pub fn gotd_file(path: impl Into<PathBuf>) -> Self {
        Self(Source::GotdFile(path.into()))
    }

    async fn open(
        self,
        ipv6: Option<bool>,
    ) -> Result<(Arc<ErasedSession>, Option<bool>), ClientBuildError> {
        let _ = ipv6; // used when a string/file importer feature is enabled
        match self.0 {
            Source::Memory => Ok((erase(Arc::new(MemorySession::default())), None)),
            Source::Storage(session) => Ok((session, None)),
            #[cfg(feature = "session-file")]
            Source::File(path) => {
                let session = grammers_session::storages::SqliteSession::open(path)
                    .await
                    .map_err(|e| ClientBuildError::Storage(Box::new(e)))?;
                Ok((erase(Arc::new(session)), None))
            }
            #[cfg(feature = "session-string")]
            Source::Native(text) => {
                let session = grammers_session::native::NativeSession::parse(&text)
                    .map_err(ClientBuildError::NativeSession)?;
                let preference = session.prefer_ipv6();
                if ipv6.is_some_and(|explicit| explicit != preference) {
                    return Err(ClientBuildError::AddressPreference);
                }
                Ok((
                    erase(Arc::new(session.into_memory_session())),
                    Some(preference),
                ))
            }
            #[cfg(feature = "session-import")]
            Source::Pyrogram(text) => imported(match ipv6 {
                Some(prefer_ipv6) => grammers_session::import::parse_pyrogram_string(
                    &text,
                    grammers_session::import::ImportOptions { prefer_ipv6 },
                ),
                None => grammers_session::import::ImportedSession::from_pyrogram(&text),
            }),
            #[cfg(feature = "session-import")]
            Source::Telethon(text) => imported(match ipv6 {
                Some(prefer_ipv6) => grammers_session::import::parse_telethon_string(
                    &text,
                    grammers_session::import::ImportOptions { prefer_ipv6 },
                ),
                None => grammers_session::import::ImportedSession::from_telethon(&text),
            }),
            #[cfg(feature = "session-import")]
            Source::Gotd(text) => imported(match ipv6 {
                Some(prefer_ipv6) => grammers_session::import::parse_gotd_json(
                    &text,
                    grammers_session::import::ImportOptions { prefer_ipv6 },
                ),
                None => grammers_session::import::ImportedSession::from_gotd_json(&text),
            }),
            #[cfg(feature = "session-import-files")]
            Source::PyrogramFile(path) => imported_file(
                grammers_session::import_files::import_pyrogram_file(
                    path,
                    grammers_session::import::ImportOptions {
                        prefer_ipv6: ipv6.unwrap_or(false),
                    },
                )
                .await,
            ),
            #[cfg(feature = "session-import-files")]
            Source::TelethonFile(path) => imported_file(
                grammers_session::import_files::import_telethon_file(
                    path,
                    grammers_session::import::ImportOptions {
                        prefer_ipv6: ipv6.unwrap_or(false),
                    },
                )
                .await,
            ),
            #[cfg(feature = "session-import-files")]
            Source::GotdFile(path) => imported_file(
                grammers_session::import_files::import_gotd_file(
                    path,
                    grammers_session::import::ImportOptions {
                        prefer_ipv6: ipv6.unwrap_or(false),
                    },
                )
                .await,
            ),
        }
    }
}

#[cfg(feature = "session-import")]
fn imported(
    result: Result<
        grammers_session::import::ImportedSession,
        grammers_session::import::ImportError,
    >,
) -> Result<(Arc<ErasedSession>, Option<bool>), ClientBuildError> {
    let session = result.map_err(ClientBuildError::ImportedSession)?;
    let preference = session.prefer_ipv6();
    Ok((
        erase(Arc::new(session.into_memory_session())),
        Some(preference),
    ))
}

#[cfg(feature = "session-import-files")]
fn imported_file(
    result: Result<
        grammers_session::import::ImportedSession,
        grammers_session::import_files::ImportFileError,
    >,
) -> Result<(Arc<ErasedSession>, Option<bool>), ClientBuildError> {
    let session = result.map_err(ClientBuildError::ImportedFile)?;
    let preference = session.prefer_ipv6();
    Ok((
        erase(Arc::new(session.into_memory_session())),
        Some(preference),
    ))
}

/// Local configuration or session-source failure. Formatting never includes
/// session text or paths. Inspect `source()` explicitly when diagnosing storage.
#[non_exhaustive]
pub enum ClientBuildError {
    /// API ID must be positive; source metadata never replaces it.
    InvalidApiId,
    /// Capacity must be positive and within Tokio's supported limit.
    InvalidCapacity,
    /// A timeout must be positive.
    InvalidTimeout,
    /// Explicit IPv6 preference contradicts the session's saved route.
    AddressPreference,
    /// Local session storage could not be opened.
    Storage(Box<dyn std::error::Error + Send + Sync>),
    /// Native string failed strict format validation.
    #[cfg(feature = "session-string")]
    NativeSession(grammers_session::native::SessionStringError),
    /// Foreign string failed strict format validation.
    #[cfg(feature = "session-import")]
    ImportedSession(grammers_session::import::ImportError),
    /// Foreign session file could not be imported.
    #[cfg(feature = "session-import-files")]
    ImportedFile(grammers_session::import_files::ImportFileError),
}

impl fmt::Display for ClientBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidApiId => "API ID must be positive",
            Self::InvalidCapacity => "request/update capacity is outside the supported range",
            Self::InvalidTimeout => "timeout must be positive",
            Self::AddressPreference => "address preference contradicts the session route",
            Self::Storage(_) => "session storage could not be opened",
            #[cfg(feature = "session-string")]
            Self::NativeSession(_) => "native session string is invalid",
            #[cfg(feature = "session-import")]
            Self::ImportedSession(_) => "foreign session string is invalid",
            #[cfg(feature = "session-import-files")]
            Self::ImportedFile(_) => "foreign session file could not be imported",
        })
    }
}
impl fmt::Debug for ClientBuildError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
impl std::error::Error for ClientBuildError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Storage(e) => Some(e.as_ref()),
            #[cfg(feature = "session-string")]
            Self::NativeSession(e) => Some(e),
            #[cfg(feature = "session-import")]
            Self::ImportedSession(e) => Some(e),
            #[cfg(feature = "session-import-files")]
            Self::ImportedFile(e) => Some(e),
            _ => None,
        }
    }
}

/// Convenient configuration for a client with an owned runner. Defaults to memory
/// storage, 1024 pending requests, a 30-second connection deadline, and a 10-second
/// shutdown deadline. Explicit flood waits retain the default `AutoSleep` limits;
/// I/O errors are returned instead of being treated as flood waits.
pub struct ClientBuilder {
    api_id: i32,
    source: SessionSource,
    params: ConnectionParams,
    configuration: ClientConfiguration,
    ipv6: Option<bool>,
    invalid_capacity: bool,
    shutdown_timeout: Duration,
}

impl ClientBuilder {
    fn new(api_id: i32) -> Self {
        Self {
            api_id,
            source: SessionSource::memory(),
            params: ConnectionParams {
                max_pending_requests: NonZeroUsize::new(1024),
                connection_timeout: Some(Duration::from_secs(30)),
                retry_policy: Box::new(AutoSleep {
                    io_errors_as_flood_of: None,
                    ..Default::default()
                }),
                ..Default::default()
            },
            configuration: ClientConfiguration::default(),
            ipv6: None,
            invalid_capacity: false,
            shutdown_timeout: Duration::from_secs(10),
        }
    }
    /// Select an explicit local session source.
    pub fn session(mut self, source: SessionSource) -> Self {
        self.source = source;
        self
    }
    /// Replace all advanced connection settings, including conservative defaults.
    pub fn connection_params(mut self, params: ConnectionParams) -> Self {
        self.ipv6 = Some(params.use_ipv6);
        self.params = params;
        self.invalid_capacity = false;
        self
    }
    /// Replace high-level client settings.
    pub fn client_configuration(mut self, configuration: ClientConfiguration) -> Self {
        self.configuration = configuration;
        self
    }
    /// Device name supplied to Telegram.
    pub fn device_model(mut self, value: impl Into<String>) -> Self {
        self.params.device_model = value.into();
        self
    }
    /// Operating system version supplied to Telegram.
    pub fn system_version(mut self, value: impl Into<String>) -> Self {
        self.params.system_version = value.into();
        self
    }
    /// Application version supplied to Telegram.
    pub fn app_version(mut self, value: impl Into<String>) -> Self {
        self.params.app_version = value.into();
        self
    }
    /// Application language code.
    pub fn language(mut self, value: impl Into<String>) -> Self {
        self.params.lang_code = value.into();
        self
    }
    /// Operating system language code.
    pub fn system_language(mut self, value: impl Into<String>) -> Self {
        self.params.system_lang_code = value.into();
        self
    }
    /// Explicit address family; saved native preferences must agree.
    pub fn ipv6(mut self, value: bool) -> Self {
        self.ipv6 = Some(value);
        self.params.use_ipv6 = value;
        self
    }
    /// Enable or disable update delivery (enabled by default).
    pub fn receive_updates(mut self, value: bool) -> Self {
        self.params.receive_updates = value;
        self
    }
    /// Deadline for connection initialization, not an ordinary RPC deadline.
    pub fn connection_timeout(mut self, value: Duration) -> Self {
        self.params.connection_timeout = Some(value);
        self
    }
    /// Limit queued plus in-flight requests; invalid values fail during build.
    pub fn max_pending_requests(mut self, value: usize) -> Self {
        self.invalid_capacity = value == 0 || value > tokio::sync::Semaphore::MAX_PERMITS;
        self.params.max_pending_requests = NonZeroUsize::new(value);
        self
    }
    /// Set the typed RPC retry policy. Retrying I/O can duplicate side effects.
    pub fn retry_policy(mut self, value: impl RetryPolicy + 'static) -> Self {
        self.params.retry_policy = Box::new(value);
        self
    }
    /// Deadline used by `stop()` (10 seconds by default).
    pub fn shutdown_timeout(mut self, value: Duration) -> Self {
        self.shutdown_timeout = value;
        self
    }
    /// SOCKS5 proxy URL; credentials are never included in builder errors.
    #[cfg(feature = "proxy")]
    pub fn proxy(mut self, value: impl Into<String>) -> Self {
        self.params.proxy_url = Some(value.into());
        self
    }

    /// Open/decode the selected local session without spawning tasks or doing
    /// network I/O. Call [`Client::start`] before invoking requests.
    pub async fn build(mut self) -> Result<Client, ClientBuildError> {
        if self.api_id <= 0 {
            return Err(ClientBuildError::InvalidApiId);
        }
        if self.invalid_capacity
            || self
                .params
                .max_pending_requests
                .is_some_and(|n| n.get() > tokio::sync::Semaphore::MAX_PERMITS)
            || self.params.updates_channel_capacity.get() > tokio::sync::Semaphore::MAX_PERMITS
        {
            return Err(ClientBuildError::InvalidCapacity);
        }
        if self.shutdown_timeout.is_zero()
            || self.params.connection_timeout.is_some_and(|t| t.is_zero())
        {
            return Err(ClientBuildError::InvalidTimeout);
        }
        let (session, preference) = self.source.open(self.ipv6).await?;
        if let Some(preference) = preference {
            self.params.use_ipv6 = preference;
        }
        let pool = SenderPool::parked(Arc::clone(&session), self.api_id, self.params);
        let (completion, _) = watch::channel(None);
        let lifecycle = ManagedLifecycle {
            handle: pool.handle.thin.clone(),
            state: Mutex::new(LifecycleState {
                runner: Some(pool.runner),
                task: None,
                updates: Some(pool.updates),
                stopping: false,
            }),
            completion,
            shutdown_timeout: self.shutdown_timeout,
        };
        Ok(Client(Arc::new(ClientInner {
            session,
            api_id: self.api_id,
            handle: pool.handle.thin,
            configuration: self.configuration,
            auth_copied_to_dcs: tokio::sync::Mutex::new(Vec::new()),
            lifecycle: Some(lifecycle),
        })))
    }
}

/// Managed lifecycle failures. Formatting redacts panic payloads and nested
/// request errors; the original connectivity error is available via `source()`.
#[derive(Clone)]
#[non_exhaustive]
pub enum LifecycleError {
    /// `start` needs an entered Tokio runtime.
    MissingRuntime,
    /// A stopped managed client cannot restart; rebuild from its session storage.
    AlreadyStopped,
    /// The client was built with a manually managed sender pool.
    ExternallyManaged,
    /// Update reception was disabled in configuration.
    UpdatesDisabled,
    /// The single update receiver was already taken.
    UpdatesTaken,
    /// An internal task panicked (payload redacted).
    RunnerPanicked,
    /// The owning runtime cancelled the supervised task.
    RunnerCancelled,
    /// Shutdown exceeded its deadline; the runner was aborted and joined.
    ShutdownTimeout,
    /// Explicit startup connectivity check failed. No login prompt is attempted.
    Connectivity(Arc<InvocationError>),
}
impl fmt::Display for LifecycleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::MissingRuntime => "start requires a Tokio runtime",
            Self::AlreadyStopped => "client has stopped; rebuild it to restart",
            Self::ExternallyManaged => "client uses an externally managed sender pool",
            Self::UpdatesDisabled => "update delivery is disabled",
            Self::UpdatesTaken => "update receiver was already taken",
            Self::RunnerPanicked => "runner task panicked (payload redacted)",
            Self::RunnerCancelled => "runner task was cancelled",
            Self::ShutdownTimeout => "shutdown timed out; runner aborted and joined",
            Self::Connectivity(_) => "startup connectivity check failed",
        })
    }
}
impl fmt::Debug for LifecycleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
impl std::error::Error for LifecycleError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Connectivity(e) => Some(e.as_ref()),
            _ => None,
        }
    }
}

type Completion = Result<(), LifecycleError>;
type SharedJoin = Shared<BoxFuture<'static, Completion>>;
struct RunningTask {
    abort: AbortHandle,
    join: SharedJoin,
}
struct LifecycleState {
    runner: Option<SenderPoolRunner>,
    task: Option<RunningTask>,
    updates: Option<mpsc::Receiver<UpdatesLike>>,
    stopping: bool,
}
pub(crate) struct ManagedLifecycle {
    handle: SenderPoolHandle,
    state: Mutex<LifecycleState>,
    completion: watch::Sender<Option<Completion>>,
    shutdown_timeout: Duration,
}
impl Drop for ManagedLifecycle {
    fn drop(&mut self) {
        self.handle.quit();
        let state = self.state.get_mut().unwrap_or_else(|e| e.into_inner());
        if let Some(task) = &state.task {
            task.abort.abort();
        }
        // No async cleanup is promised on Drop; stop().await is the supported path.
    }
}

impl Client {
    /// Configure a managed client without manually constructing a sender pool.
    pub fn builder(api_id: i32) -> ClientBuilder {
        ClientBuilder::new(api_id)
    }

    fn managed(&self) -> Result<&ManagedLifecycle, LifecycleError> {
        self.0
            .lifecycle
            .as_ref()
            .ok_or(LifecycleError::ExternallyManaged)
    }

    /// Start exactly one supervised runner, shared by all client clones.
    /// Connections remain lazy. This does not establish account authorization,
    /// prompt for credentials, or install process-global signal handlers.
    pub async fn start(&self) -> Result<(), LifecycleError> {
        let managed = self.managed()?;
        let mut state = managed.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.stopping || self.0.handle.status().stopped {
            return Err(LifecycleError::AlreadyStopped);
        }
        if state.task.is_some() {
            return Ok(());
        }
        let runtime =
            tokio::runtime::Handle::try_current().map_err(|_| LifecycleError::MissingRuntime)?;
        let runner = state.runner.take().ok_or(LifecycleError::AlreadyStopped)?;
        if !self.0.handle.activate() {
            return Err(LifecycleError::AlreadyStopped);
        }
        let completion = managed.completion.clone();
        let task = runtime.spawn(async move {
            let result = match AssertUnwindSafe(runner.run_checked()).catch_unwind().await {
                Ok(Ok(())) => Ok(()),
                Ok(Err(_)) | Err(_) => Err(LifecycleError::RunnerPanicked),
            };
            completion.send_replace(Some(result.clone()));
            result
        });
        let abort = task.abort_handle();
        let join = async move {
            task.await.unwrap_or_else(|e| {
                Err(if e.is_panic() {
                    LifecycleError::RunnerPanicked
                } else {
                    LifecycleError::RunnerCancelled
                })
            })
        }
        .boxed()
        .shared();
        state.task = Some(RunningTask { abort, join });
        Ok(())
    }

    /// Start the runner and check home-DC connectivity with an MTProto ping.
    /// A successful ping is not proof that the session is logged into an account.
    pub async fn connect(&self) -> Result<(), LifecycleError> {
        self.start().await?;
        self.invoke(&grammers_tl_types::functions::Ping { ping_id: 0 })
            .await
            .map(|_| ())
            .map_err(|e| LifecycleError::Connectivity(Arc::new(e)))
    }

    /// Connect, then wait until another clone stops the client or its runner fails.
    /// Connectivity failures stop the client. Cancelling this waiter leaves a
    /// runner owned by surviving client clones; the last clone's drop aborts it.
    pub async fn run(&self) -> Result<(), LifecycleError> {
        self.start().await?;
        if let Err(error) = self.connect().await {
            let was_stopping = self
                .managed()?
                .state
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .stopping;
            self.stop().await?;
            return if was_stopping { Ok(()) } else { Err(error) };
        }
        let join = self
            .managed()?
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .task
            .as_ref()
            .map(|task| task.join.clone());
        match join {
            Some(join) => join.await,
            None => Ok(()),
        }
    }

    /// Run until shutdown resolves, or until the runner ends. Always await stop
    /// before returning. Pass `tokio::signal::ctrl_c()` or your own shutdown future.
    pub async fn run_until(&self, shutdown: impl Future) -> Result<(), LifecycleError> {
        let result = tokio::select! {
            biased;
            _ = shutdown => Ok(()),
            result = self.run() => result,
        };
        let stopped = self.stop().await;
        result.and(stopped)
    }

    /// Terminal, idempotent shutdown; no new invocation can be queued afterward.
    /// Sync externally owned update streams first with `sync_update_state()`.
    /// The default deadline is 10 seconds; expiration aborts and joins the runner.
    pub async fn stop(&self) -> Result<(), LifecycleError> {
        self.stop_with_timeout(self.managed()?.shutdown_timeout)
            .await
    }

    /// Stop with an explicit deadline. Cancellation of this waiter does not
    /// detach the runner: the client continues to own its supervision handle.
    pub async fn stop_with_timeout(&self, timeout: Duration) -> Result<(), LifecycleError> {
        let managed = self.managed()?;
        let task = {
            let mut state = managed.state.lock().unwrap_or_else(|e| e.into_inner());
            state.stopping = true;
            managed.handle.quit();
            drop(state.runner.take());
            state
                .task
                .as_ref()
                .map(|task| (task.abort.clone(), task.join.clone()))
        };
        let Some((abort, join)) = task else {
            managed.completion.send_replace(Some(Ok(())));
            return Ok(());
        };
        match tokio::time::timeout(timeout, join.clone()).await {
            Ok(result) => managed.completion.borrow().clone().unwrap_or(result),
            Err(_) => {
                abort.abort();
                let _ = join.await;
                managed
                    .completion
                    .send_replace(Some(Err(LifecycleError::ShutdownTimeout)));
                Err(LifecycleError::ShutdownTimeout)
            }
        }
    }

    /// Take the sole update receiver. Use `stream_updates(receiver, config).await`
    /// after start to initialize update state and establish an authorized session's
    /// home connection. Sync the stream's state before stopping the client.
    pub fn take_updates(&self) -> Result<mpsc::Receiver<UpdatesLike>, LifecycleError> {
        let managed = self.managed()?;
        if !self.0.handle.updates_enabled() {
            return Err(LifecycleError::UpdatesDisabled);
        }
        managed
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .updates
            .take()
            .ok_or(LifecycleError::UpdatesTaken)
    }

    /// Local pool status, also available on externally managed clients.
    pub fn status(&self) -> PoolStatus {
        self.0.handle.status()
    }
    /// Bounded runtime events; observer lag is explicit and never blocks I/O.
    pub fn events(&self) -> broadcast::Receiver<PoolEvent> {
        self.0.handle.events()
    }

    /// Export the home-DC authorization and exact endpoint as a credential.
    /// This synchronous operation does not include peer cache or update state.
    #[cfg(feature = "session-string")]
    pub fn export_session_string(
        &self,
    ) -> Result<grammers_session::native::SessionString, grammers_session::native::SessionStringError>
    {
        grammers_session::native::NativeSession::from_session(
            self.0.session.as_ref(),
            self.0.handle.prefers_ipv6(),
        )
        .map(|session| session.encode())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_build_is_offline_and_start_reports_missing_runtime() {
        use std::task::{Context, Poll, Waker};
        let mut context = Context::from_waker(Waker::noop());
        let mut build = std::pin::pin!(Client::builder(1).build());
        let Poll::Ready(Ok(client)) = build.as_mut().poll(&mut context) else {
            panic!("memory build must be ready without a runtime")
        };
        let mut start = std::pin::pin!(client.start());
        assert!(matches!(
            start.as_mut().poll(&mut context),
            Poll::Ready(Err(LifecycleError::MissingRuntime))
        ));
    }

    #[test]
    fn managed_defaults_do_not_retry_ambiguous_io() {
        let builder = Client::builder(1);
        assert_eq!(builder.params.max_pending_requests.unwrap().get(), 1024);
        assert_eq!(
            builder.params.connection_timeout,
            Some(Duration::from_secs(30))
        );
        let context = grammers_mtsender::RetryContext {
            fail_count: std::num::NonZeroU32::MIN,
            slept_so_far: Duration::ZERO,
            error: InvocationError::Io(std::io::ErrorKind::ConnectionReset.into()),
        };
        assert!(
            builder
                .params
                .retry_policy
                .should_retry(&context)
                .is_break()
        );
        assert!(ConnectionParams::default().max_pending_requests.is_none());
    }

    #[tokio::test]
    async fn supervised_runner_redacts_own_panic_and_stops_clones() {
        struct PanicPolicy;
        impl RetryPolicy for PanicPolicy {
            fn should_retry(
                &self,
                _: &grammers_mtsender::RetryContext,
            ) -> std::ops::ControlFlow<(), Duration> {
                panic!("sensitive panic fixture")
            }
        }
        let client = Client::builder(1)
            .retry_policy(PanicPolicy)
            .build()
            .await
            .unwrap();
        client.start().await.unwrap();
        let clone = client.clone();
        assert!(
            client
                .invoke_in_dc(999, &grammers_tl_types::functions::Ping { ping_id: 0 })
                .await
                .is_err()
        );
        let error = clone.stop().await.unwrap_err();
        assert!(matches!(error, LifecycleError::RunnerPanicked));
        assert!(!format!("{error:?}").contains("sensitive"));
        assert!(client.status().stopped);
    }

    #[tokio::test]
    async fn run_reports_runner_panic_instead_of_secondary_closed_reply() {
        struct PanicPolicy;
        impl RetryPolicy for PanicPolicy {
            fn should_retry(
                &self,
                _: &grammers_mtsender::RetryContext,
            ) -> std::ops::ControlFlow<(), Duration> {
                panic!("private fixture")
            }
        }
        let session = Arc::new(MemorySession::default());
        session.set_home_dc_id(999).await.unwrap();
        let client = Client::builder(1)
            .session(SessionSource::storage(session))
            .retry_policy(PanicPolicy)
            .build()
            .await
            .unwrap();
        assert!(matches!(
            client.run().await,
            Err(LifecycleError::RunnerPanicked)
        ));
    }

    #[tokio::test]
    async fn shutdown_timeout_aborts_and_joins_owned_task() {
        struct Dropped(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for Dropped {
            fn drop(&mut self) {
                self.0.store(true, std::sync::atomic::Ordering::Release);
            }
        }
        let client = Client::builder(1).build().await.unwrap();
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let guard = Dropped(Arc::clone(&dropped));
        let task = tokio::spawn(async move {
            let _guard = guard;
            std::future::pending::<()>().await;
            Ok(())
        });
        let abort = task.abort_handle();
        let join = async move { task.await.unwrap_or(Err(LifecycleError::RunnerCancelled)) }
            .boxed()
            .shared();
        client.managed().unwrap().state.lock().unwrap().task = Some(RunningTask { abort, join });
        assert!(matches!(
            client.stop_with_timeout(Duration::from_millis(1)).await,
            Err(LifecycleError::ShutdownTimeout)
        ));
        assert!(dropped.load(std::sync::atomic::Ordering::Acquire));
        assert!(matches!(
            client.stop().await,
            Err(LifecycleError::ShutdownTimeout)
        ));
    }

    #[tokio::test]
    async fn run_connectivity_failure_stops_without_login_prompt() {
        let session = Arc::new(MemorySession::default());
        session.set_home_dc_id(999).await.unwrap();
        let client = Client::builder(1)
            .session(SessionSource::storage(session))
            .build()
            .await
            .unwrap();
        let error = client.run().await.unwrap_err();
        assert!(
            matches!(error, LifecycleError::Connectivity(ref e) if matches!(e.as_ref(), InvocationError::InvalidDc))
        );
        assert!(client.status().stopped);
    }

    #[tokio::test]
    async fn ready_shutdown_stops_without_opening_a_connection() {
        let client = Client::builder(1).build().await.unwrap();
        client.run_until(std::future::ready(())).await.unwrap();
        assert!(client.status().stopped);
        assert_eq!(client.status().active_connections, 0);
    }

    #[cfg(feature = "session-string")]
    #[tokio::test]
    async fn native_builder_preserves_preference_and_legacy_export() {
        let session = Arc::new(MemorySession::default());
        let dc_id = session.home_dc_id().unwrap();
        let mut dc = session.dc_option(dc_id).unwrap().unwrap();
        dc.auth_key = Some([7; 256]);
        session.set_dc_option(&dc).await.unwrap();
        let text = grammers_session::native::NativeSession::from_session(session.as_ref(), true)
            .unwrap()
            .encode();
        let client = Client::builder(1)
            .session(SessionSource::native(text.expose_secret()))
            .build()
            .await
            .unwrap();
        assert!(client.0.handle.prefers_ipv6());
        assert_eq!(
            client.export_session_string().unwrap().expose_secret(),
            text.expose_secret()
        );
        assert!(matches!(
            Client::builder(1)
                .session(SessionSource::native(text.expose_secret()))
                .ipv6(false)
                .build()
                .await,
            Err(ClientBuildError::AddressPreference)
        ));
        let pool = SenderPool::with_configuration(
            session,
            1,
            ConnectionParams {
                use_ipv6: true,
                ..Default::default()
            },
        );
        let legacy = Client::new(pool.handle);
        assert_eq!(
            legacy.export_session_string().unwrap().expose_secret(),
            text.expose_secret()
        );
        assert!(matches!(
            legacy.start().await,
            Err(LifecycleError::ExternallyManaged)
        ));
    }

    #[tokio::test]
    async fn managed_idle_start_stop_is_idempotent_and_terminal() {
        let client = Client::builder(1)
            .receive_updates(false)
            .build()
            .await
            .unwrap();
        assert!(matches!(
            client
                .invoke(&grammers_tl_types::functions::Ping { ping_id: 0 })
                .await,
            Err(InvocationError::Dropped(_))
        ));
        assert!(matches!(
            client.take_updates(),
            Err(LifecycleError::UpdatesDisabled)
        ));
        client.start().await.unwrap();
        client.start().await.unwrap();
        client.stop().await.unwrap();
        client.stop().await.unwrap();
        assert!(client.status().stopped);
        assert!(matches!(
            client.start().await,
            Err(LifecycleError::AlreadyStopped)
        ));
    }

    #[tokio::test]
    async fn stop_before_start_closes_parked_runner() {
        let client = Client::builder(1).build().await.unwrap();
        client.stop().await.unwrap();
        assert!(client.status().stopped);
        assert!(matches!(
            client.start().await,
            Err(LifecycleError::AlreadyStopped)
        ));
    }

    #[tokio::test]
    async fn builder_rejects_invalid_values_without_spawning() {
        assert!(matches!(
            Client::builder(0).build().await,
            Err(ClientBuildError::InvalidApiId)
        ));
        assert!(matches!(
            Client::builder(1).max_pending_requests(0).build().await,
            Err(ClientBuildError::InvalidCapacity)
        ));
        assert!(matches!(
            Client::builder(1)
                .max_pending_requests(usize::MAX)
                .build()
                .await,
            Err(ClientBuildError::InvalidCapacity)
        ));
        let client = Client::builder(1).build().await.unwrap();
        assert!(!client.status().stopped);
        assert!(client.take_updates().is_ok());
        assert!(matches!(
            client.take_updates(),
            Err(LifecycleError::UpdatesTaken)
        ));
    }

    #[tokio::test]
    async fn last_client_drop_terminates_owned_runner() {
        let client = Client::builder(1).build().await.unwrap();
        client.start().await.unwrap();
        let thin = client.0.handle.clone();
        let clone = client.clone();
        drop(client);
        assert!(!thin.status().stopped);
        drop(clone);
        tokio::task::yield_now().await;
        assert!(thin.status().stopped);
    }
}
