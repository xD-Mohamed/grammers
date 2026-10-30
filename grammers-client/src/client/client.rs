// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::sync::Arc;

use grammers_mtsender::SenderPoolHandle;
use grammers_session::storages::ErasedSession;

#[cfg(test)]
pub(crate) type TestRpcResponder =
    dyn Fn(i32, &[u8]) -> Result<Vec<u8>, grammers_mtsender::InvocationError> + Send + Sync;

pub(crate) struct ClientInner {
    pub(crate) session: Arc<ErasedSession>,
    pub(crate) api_id: i32,
    pub(crate) handle: SenderPoolHandle,
    pub(crate) configuration: ClientConfiguration,
    pub(crate) auth_copied_to_dcs: tokio::sync::Mutex<Vec<(i32, Box<[u8; 256]>)>>,
    pub(crate) lifecycle: Option<super::runtime::ManagedLifecycle>,
    #[cfg(test)]
    pub(crate) rpc_responder: Option<Arc<TestRpcResponder>>,
}

/// Wrapper around [`SenderPool`] to facilitate interaction with Telegram's API.
///
/// This structure is the "entry point" of the library, from which you can start using the rest.
///
/// [`Client::builder`] creates a managed client with `start`, `stop`, and `run`.
/// Legacy [`Client::new`] instances use an externally owned sender-pool runner.
///
/// Drop does not synchronize asynchronous update state. Explicitly call your
/// update stream's `sync_update_state`, then await [`Client::stop`] for managed
/// clients. Dropping the last managed clone requests quit and aborts its runner.
///
/// [`SenderPool`]: grammers_mtsender::SenderPool
/// [`Session`]: grammers_session::Session
#[derive(Clone)]
pub struct Client(pub(crate) Arc<ClientInner>);

/// Configuration that controls the [`Client`] behaviour when making requests.
pub struct ClientConfiguration {
    /// Whether to call [`grammers_session::Session::cache_peer`] on all peer information that
    /// the high-level methods receive as a response (e.g. [`Client::iter_dialogs`]).
    ///
    /// The cached peers are then usable by other methods such as [`Client::resolve_peer`]
    /// for as long as the same persisted session is used.
    ///
    /// Live updates establish login proof even without cached self metadata.
    /// Explicit login and [`Client::get_me`] still cache that metadata when this
    /// option is disabled.
    ///
    /// Channel catch-up needs a usable cached access hash. Without one, the first
    /// recovery episode waits up to 30 one-second retries; live PTS updates for
    /// that channel are suppressed during this grace and its backlog is discarded
    /// if access stays unavailable. Scheduling can extend the wait. Later episodes
    /// skip this grace until access is learned or a difference succeeds. This
    /// exhaustion memory lasts only for the current update receiver. Disabling
    /// caching therefore cannot guarantee lossless channel updates.
    pub auto_cache_peers: bool,
}

impl Default for ClientConfiguration {
    /// Returns an instance that encountered peers are automatically passed to [`grammers_session::Session::cache_peer`].
    fn default() -> Self {
        Self {
            auto_cache_peers: true,
        }
    }
}
