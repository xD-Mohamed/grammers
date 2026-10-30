// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Methods to deal with and offer access to updates.

use std::collections::VecDeque;

use grammers_mtsender::{InvocationError, UpdatesConfiguration, UpdatesReceiver};
use grammers_session::updates::{State, UpdatesLike};
use grammers_tl_types as tl;
use tokio::sync::mpsc;

use super::Client;
use crate::peer::PeerMap;
use crate::update::Update;

/// Iterator returned by [`Client::stream_updates`].
pub struct UpdateStream {
    client: Client,
    updates: UpdatesReceiver,
    buffer: VecDeque<(tl::enums::Update, State, PeerMap)>,
    pending_peer_cache: Option<PeerMap>,
}

impl UpdateStream {
    /// Pops an update from the queue, waiting for an update to arrive first if the queue is empty.
    pub async fn next(&mut self) -> Result<Update, InvocationError> {
        let (update, state, peers) = self.next_raw().await?;
        Ok(Update::from_raw(&self.client, update, state, peers))
    }

    pub async fn next_raw(
        &mut self,
    ) -> Result<(tl::enums::Update, State, PeerMap), InvocationError> {
        loop {
            if let Some(peers) = self.pending_peer_cache.as_ref() {
                // Cancellation leaves the input here for the next cache attempt.
                self.client.cache_peer_map(peers).await;
                self.pending_peer_cache = None;
            }
            if let Some(update) = self.buffer.pop_front() {
                return Ok(update);
            }

            let (updates, users, chats) = self.updates.next().await?;
            let peer_map = self.client.peer_map_from_raw(users, chats);
            // The receiver has already advanced its state. Retain the entire batch
            // before peer storage can yield, so cancelling next cannot lose updates.
            self.buffer
                .extend(updates.into_iter().map(|(u, s)| (u, s, peer_map.handle())));
            self.pending_peer_cache = Some(peer_map);
        }
    }

    /// Synchronize the updates state to the session.
    ///
    /// This is **not** automatically done on drop.
    pub async fn sync_update_state(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        self.updates.sync_update_state().await
    }
}

impl Client {
    /// Returns an asynchronous stream of processed updates.
    ///
    /// The updates are guaranteed to be in order, and any gaps will be resolved.\
    /// **Important** to note that for gaps to be resolved, the peers must have been
    /// persisted in the session cache beforehand (i.e. be retrievable with [`grammers_session::Session::peer`]).
    /// A good way to achieve this is to use [`Self::iter_dialogs`] at least once after login.
    ///
    /// The updates are wrapped in [`crate::update::Update`] to make them more convenient to use,
    /// but their raw type is still accessible to bridge any missing functionality.
    pub async fn stream_updates(
        &self,
        updates: mpsc::Receiver<UpdatesLike>,
        configuration: UpdatesConfiguration,
    ) -> Result<UpdateStream, Box<dyn std::error::Error + Send + Sync>> {
        let updates = UpdatesReceiver::create(
            self.0.handle.clone(),
            self.0.session.clone(),
            updates,
            configuration,
        )
        .await?;

        Ok(UpdateStream {
            client: self.clone(),
            updates,
            buffer: VecDeque::new(),
            pending_peer_cache: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use core::future::Future;
    use grammers_mtsender::SenderPool;
    use grammers_session::storages::MemorySession;
    use grammers_session::types::{DcOption, PeerId, PeerInfo, UpdateState, UpdatesState};
    use grammers_session::updates::MessageBox;
    use grammers_session::{BoxFuture, Session};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;
    use tokio::sync::Notify;

    use super::*;

    #[derive(Default)]
    struct PendingPeerCache {
        memory: MemorySession,
        cache_started: Notify,
        writable: AtomicBool,
    }

    impl Session for PendingPeerCache {
        type Error = <MemorySession as Session>::Error;
        fn home_dc_id(&self) -> Result<i32, Self::Error> {
            self.memory.home_dc_id()
        }
        fn set_home_dc_id(&self, dc_id: i32) -> BoxFuture<'_, Result<(), Self::Error>> {
            self.memory.set_home_dc_id(dc_id)
        }
        fn dc_option(&self, dc_id: i32) -> Result<Option<DcOption>, Self::Error> {
            self.memory.dc_option(dc_id)
        }
        fn set_dc_option(&self, dc: &DcOption) -> BoxFuture<'_, Result<(), Self::Error>> {
            self.memory.set_dc_option(dc)
        }
        fn peer(&self, peer: PeerId) -> BoxFuture<'_, Result<Option<PeerInfo>, Self::Error>> {
            self.memory.peer(peer)
        }
        fn cache_peer(&self, peer: PeerInfo) -> BoxFuture<'_, Result<(), Self::Error>> {
            self.memory.cache_peer(peer)
        }
        fn cache_peers(&self, peers: Vec<PeerInfo>) -> BoxFuture<'_, Result<(), Self::Error>> {
            Box::pin(async move {
                assert_eq!(peers.len(), 1);
                self.cache_started.notify_one();
                if !self.writable.load(Ordering::Acquire) {
                    std::future::pending::<()>().await;
                }
                self.memory.cache_peers(peers).await
            })
        }
        fn updates_state(&self) -> BoxFuture<'_, Result<UpdatesState, Self::Error>> {
            self.memory.updates_state()
        }
        fn set_update_state(&self, update: UpdateState) -> BoxFuture<'_, Result<(), Self::Error>> {
            self.memory.set_update_state(update)
        }
    }

    #[tokio::test]
    async fn cancelled_peer_cache_preserves_the_entire_update_batch() {
        let session = Arc::new(PendingPeerCache::default());
        let pool = SenderPool::new(Arc::clone(&session), 1);
        let client = Client::new(pool.handle);
        let (tx, rx) = mpsc::channel(1);
        let mut stream = client
            .stream_updates(rx, UpdatesConfiguration { catch_up: false })
            .await
            .unwrap();
        let updates: Vec<tl::enums::Update> = [101, 102]
            .into_iter()
            .map(|pts| {
                tl::types::UpdateDeleteMessages {
                    messages: vec![pts],
                    pts,
                    pts_count: 1,
                }
                .into()
            })
            .collect();
        tx.send(UpdatesLike::Updates(
            tl::types::Updates {
                updates: updates.clone(),
                users: Vec::new(),
                chats: vec![
                    tl::types::ChannelForbidden {
                        broadcast: true,
                        megagroup: false,
                        monoforum: false,
                        id: 73,
                        access_hash: 123,
                        title: "fixture".into(),
                        until_date: None,
                    }
                    .into(),
                ],
                date: 1,
                seq: 1,
            }
            .into(),
        ))
        .await
        .unwrap();
        // Cancel after state advances but while peer storage is suspended.
        tokio::time::timeout(Duration::from_secs(1), async {
            tokio::select! {
                biased;
                _ = stream.next_raw() => panic!("peer storage must remain pending"),
                _ = session.cache_started.notified() => {}
            }
        })
        .await
        .unwrap();
        stream.sync_update_state().await.unwrap();
        let saved = session.updates_state().await.unwrap();
        assert_eq!((saved.pts, saved.seq, saved.date), (102, 1, 1));
        assert!(
            session
                .peer(PeerId::channel(73).unwrap())
                .await
                .unwrap()
                .is_none()
        );
        session.writable.store(true, Ordering::Release);
        for (index, expected) in updates.into_iter().enumerate() {
            let (update, state, peers) =
                tokio::time::timeout(Duration::from_secs(1), stream.next_raw())
                    .await
                    .unwrap()
                    .unwrap();
            assert_eq!(update, expected);
            assert_eq!(
                state.message_box,
                Some(MessageBox::Common {
                    pts: 101 + index as i32
                })
            );
            assert!(peers.get(PeerId::channel(73).unwrap()).is_some());
        }
        assert!(stream.buffer.is_empty());
        assert!(stream.pending_peer_cache.is_none());
        assert!(matches!(
            session.peer(PeerId::channel(73).unwrap()).await.unwrap(),
            Some(PeerInfo::Channel { auth: Some(auth), .. }) if auth.hash() == 123
        ));
        stream.sync_update_state().await.unwrap();
        assert_eq!(session.updates_state().await.unwrap(), saved);
        tokio::select! {
            biased;
            _ = stream.next_raw() => panic!("the batch must not be delivered twice"),
            _ = std::future::ready(()) => {}
        }
    }

    fn get_update_stream() -> UpdateStream {
        panic!()
    }

    #[test]
    fn ensure_next_update_future_impls_send() {
        if false {
            // We just want it to type-check, not actually run.
            fn typeck(_: impl Future + Send) {}
            typeck(get_update_stream().next());
        }
    }
}
