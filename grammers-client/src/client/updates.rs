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
                // Cancellation leaves the input here for the next cache attempt. The
                // receiver read the clock when it applied this batch: that is recent
                // enough to age the table of stored peers by.
                let now = self.updates.last_clock_read();
                self.client.cache_peer_map_at(peers, now).await;
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

    /// Counts storage writes and how many peers they carried, failing on request.
    #[derive(Default)]
    struct CountingWrites {
        memory: MemorySession,
        writes: std::sync::atomic::AtomicUsize,
        fail: AtomicBool,
    }

    impl Session for CountingWrites {
        type Error = std::io::Error;
        fn home_dc_id(&self) -> Result<i32, Self::Error> {
            self.memory.home_dc_id().map_err(std::io::Error::other)
        }
        fn set_home_dc_id(&self, id: i32) -> BoxFuture<'_, Result<(), Self::Error>> {
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
        fn set_dc_option(&self, option: &DcOption) -> BoxFuture<'_, Result<(), Self::Error>> {
            let option = option.clone();
            Box::pin(async move {
                self.memory
                    .set_dc_option(&option)
                    .await
                    .map_err(std::io::Error::other)
            })
        }
        fn peer(&self, peer: PeerId) -> BoxFuture<'_, Result<Option<PeerInfo>, Self::Error>> {
            Box::pin(async move { self.memory.peer(peer).await.map_err(std::io::Error::other) })
        }
        fn cache_peer(&self, peer: PeerInfo) -> BoxFuture<'_, Result<(), Self::Error>> {
            self.cache_peers(vec![peer])
        }
        fn cache_peers(&self, peers: Vec<PeerInfo>) -> BoxFuture<'_, Result<(), Self::Error>> {
            Box::pin(async move {
                self.writes.fetch_add(1, Ordering::Relaxed);
                if self.fail.load(Ordering::Relaxed) {
                    return Err(std::io::Error::other("full disk"));
                }
                self.memory
                    .cache_peers(peers)
                    .await
                    .map_err(std::io::Error::other)
            })
        }
        fn updates_state(&self) -> BoxFuture<'_, Result<UpdatesState, Self::Error>> {
            Box::pin(async {
                self.memory
                    .updates_state()
                    .await
                    .map_err(std::io::Error::other)
            })
        }
        fn set_update_state(&self, update: UpdateState) -> BoxFuture<'_, Result<(), Self::Error>> {
            Box::pin(async {
                self.memory
                    .set_update_state(update)
                    .await
                    .map_err(std::io::Error::other)
            })
        }
    }

    fn user_with_hash(id: i64, access_hash: i64) -> tl::enums::User {
        let mut user = tl::types::User {
            is_self: false,
            contact: false,
            mutual_contact: false,
            deleted: false,
            bot: false,
            bot_chat_history: false,
            bot_nochats: false,
            verified: false,
            restricted: false,
            min: false,
            bot_inline_geo: false,
            support: false,
            scam: false,
            apply_min_photo: false,
            fake: false,
            bot_attach_menu: false,
            premium: false,
            attach_menu_enabled: false,
            bot_can_edit: false,
            close_friend: false,
            stories_hidden: false,
            stories_unavailable: false,
            contact_require_premium: false,
            bot_business: false,
            bot_has_main_app: false,
            bot_forum_view: false,
            bot_forum_can_manage_topics: false,
            bot_can_manage_bots: false,
            bot_guestchat: false,
            bot_guard: false,
            bot_forum_sidebar: false,
            id,
            access_hash: None,
            first_name: None,
            last_name: None,
            username: None,
            phone: None,
            photo: None,
            status: None,
            bot_info_version: None,
            restriction_reason: None,
            bot_inline_placeholder: None,
            lang_code: None,
            emoji_status: None,
            usernames: None,
            stories_max_id: None,
            color: None,
            profile_color: None,
            bot_active_users: None,
            bot_verification_icon: None,
            send_paid_messages_stars: None,
            linked_community_id: None,
        };
        user.access_hash = Some(access_hash);
        user.into()
    }

    #[tokio::test]
    async fn unchanged_peers_are_written_to_storage_only_once() {
        let session = Arc::new(CountingWrites::default());
        let pool = SenderPool::new(Arc::clone(&session), 1);
        let client = Client::new(pool.handle);
        let writes = || session.writes.load(Ordering::Relaxed);

        let peers = client.peer_map_from_raw(vec![user_with_hash(7, 100)], Vec::new());
        client.cache_peer_map(&peers).await;
        assert_eq!(writes(), 1);
        let stored = session.memory.peer(PeerId::user(7).unwrap()).await.unwrap();
        assert!(stored.unwrap().auth().is_some());

        // The same peer in a later update needs nothing.
        client.cache_peer_map(&peers).await;
        let same = client.peer_map_from_raw(vec![user_with_hash(7, 100)], Vec::new());
        client.cache_peer_map(&same).await;
        assert_eq!(writes(), 1);

        // Changed information is written, also when it goes back to an earlier value.
        for (expected, hash) in [(2, 200), (3, 100)] {
            let changed = client.peer_map_from_raw(vec![user_with_hash(7, hash)], Vec::new());
            client.cache_peer_map(&changed).await;
            assert_eq!(writes(), expected);
        }
        let stored = session.memory.peer(PeerId::user(7).unwrap()).await.unwrap();
        assert_eq!(stored.unwrap().auth().unwrap().hash(), 100);

        // A batch with only some unknown peers writes those.
        let mixed = client.peer_map_from_raw(
            vec![user_with_hash(7, 100), user_with_hash(8, 800)],
            Vec::new(),
        );
        client.cache_peer_map(&mixed).await;
        assert_eq!(writes(), 4);
        assert!(
            session
                .memory
                .peer(PeerId::user(8).unwrap())
                .await
                .unwrap()
                .is_some()
        );
    }

    // A session may forget peers, so an active one must be offered to it again.
    #[tokio::test]
    async fn unchanged_peers_are_written_again_after_a_while() {
        let session = Arc::new(CountingWrites::default());
        let pool = SenderPool::new(Arc::clone(&session), 1);
        let client = Client::new(pool.handle);
        let peers = client.peer_map_from_raw(vec![user_with_hash(7, 100)], Vec::new());
        client.cache_peer_map(&peers).await;
        client.cache_peer_map(&peers).await;
        assert_eq!(session.writes.load(Ordering::Relaxed), 1);
        client.0.stored_peers.expire_for_tests();
        client.cache_peer_map(&peers).await;
        assert_eq!(session.writes.load(Ordering::Relaxed), 2);
    }

    // The update stream ages the table with the time its receiver read for the batch,
    // instead of reading the clock for every batch.
    #[tokio::test]
    async fn stored_peers_age_by_the_time_the_caller_passes() {
        let session = Arc::new(CountingWrites::default());
        let pool = SenderPool::new(Arc::clone(&session), 1);
        let client = Client::new(pool.handle);
        let peers = client.peer_map_from_raw(vec![user_with_hash(7, 100)], Vec::new());
        let start = std::time::Instant::now();
        client.cache_peer_map_at(&peers, Some(start)).await;
        assert_eq!(session.writes.load(Ordering::Relaxed), 1);

        // Short of the lifetime nothing is forgotten, by a passed time or by the clock.
        let early = start + Duration::from_secs(599);
        client.cache_peer_map_at(&peers, Some(early)).await;
        client.cache_peer_map(&peers).await;
        assert_eq!(session.writes.load(Ordering::Relaxed), 1);

        // Past it, the peer is written once more. It is then remembered for a lifetime
        // counted from the clock, not from the reading that was passed.
        let late = std::time::Instant::now() + Duration::from_secs(601);
        client.cache_peer_map_at(&peers, Some(late)).await;
        assert_eq!(session.writes.load(Ordering::Relaxed), 2);
        let now = std::time::Instant::now();
        client.cache_peer_map_at(&peers, Some(now)).await;
        client
            .cache_peer_map_at(&peers, Some(now + Duration::from_secs(599)))
            .await;
        assert_eq!(session.writes.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn a_failed_peer_write_is_attempted_again() {
        let session = Arc::new(CountingWrites::default());
        let pool = SenderPool::new(Arc::clone(&session), 1);
        let client = Client::new(pool.handle);
        let peers = client.peer_map_from_raw(vec![user_with_hash(7, 100)], Vec::new());

        session.fail.store(true, Ordering::Relaxed);
        client.cache_peer_map(&peers).await;
        session.fail.store(false, Ordering::Relaxed);
        client.cache_peer_map(&peers).await;
        assert_eq!(session.writes.load(Ordering::Relaxed), 2);
        assert!(
            session
                .memory
                .peer(PeerId::user(7).unwrap())
                .await
                .unwrap()
                .is_some()
        );
    }

    #[tokio::test]
    async fn stored_peers_stay_bounded_and_recognize_recent_peers_after_growing() {
        let session = Arc::new(CountingWrites::default());
        let pool = SenderPool::new(Arc::clone(&session), 1);
        let client = Client::new(pool.handle);
        // Far more peers than the table can ever hold.
        for id in 1..=200_000 {
            let info = PeerInfo::User {
                id,
                auth: None,
                bot: None,
                is_self: None,
            };
            client.0.stored_peers.remember(&info);
        }
        assert!(client.0.stored_peers.capacity_for_tests() <= 65_536);
        let recent = PeerInfo::User {
            id: 200_000,
            auth: None,
            bot: None,
            is_self: None,
        };
        assert!(client.0.stored_peers.contains(&recent));
    }
}
