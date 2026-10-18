// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::sync::Arc;

use crate::types::{DcOption, PeerId, PeerInfo, PeerRef, UpdateState, UpdatesState};
use crate::{BoxFuture, Session};

/// Erased session storage alias.
///
/// This is not a storage type by itself, but is what [`erase`]d storages are turned into.
///
/// Client implementations should generally not care about what the specific session storage is,
/// so erasing the session type is a good way to avoid infecting their type with more generics.
pub type ErasedSession = dyn Session<Error = Box<dyn std::error::Error + Send + Sync + 'static>>;

/// Erase a concrete session storage into one without a concrete error type.
pub fn erase<S>(session: Arc<S>) -> Arc<ErasedSession>
where
    S: Session + Sized,
    S::Error: std::error::Error + Send + Sync + 'static,
{
    Arc::new(Eraser(session))
}

struct Eraser<S: Session>(Arc<S>);

impl<S> Session for Eraser<S>
where
    S: Session,
    S::Error: std::error::Error + Send + Sync,
{
    type Error = Box<dyn std::error::Error + Send + Sync>;

    fn home_dc_id(&self) -> Result<i32, Self::Error> {
        self.0.home_dc_id().map_err(|e| e.into())
    }

    fn set_home_dc_id(&self, dc_id: i32) -> BoxFuture<'_, Result<(), Self::Error>> {
        Box::pin(async move { self.0.set_home_dc_id(dc_id).await.map_err(|e| e.into()) })
    }

    fn dc_option(&self, dc_id: i32) -> Result<Option<DcOption>, Self::Error> {
        self.0.dc_option(dc_id).map_err(|e| e.into())
    }

    fn set_dc_option(&self, dc_option: &DcOption) -> BoxFuture<'_, Result<(), Self::Error>> {
        let dc_option = dc_option.clone();
        Box::pin(async move { self.0.set_dc_option(&dc_option).await.map_err(|e| e.into()) })
    }

    fn peer(&self, peer: PeerId) -> BoxFuture<'_, Result<Option<PeerInfo>, Self::Error>> {
        Box::pin(async move { self.0.peer(peer).await.map_err(|e| e.into()) })
    }

    fn peer_ref(&self, peer: PeerId) -> BoxFuture<'_, Result<Option<PeerRef>, Self::Error>> {
        Box::pin(async move { self.0.peer_ref(peer).await.map_err(|e| e.into()) })
    }

    fn cache_peer(&self, peer: PeerInfo) -> BoxFuture<'_, Result<(), Self::Error>> {
        Box::pin(async move { self.0.cache_peer(peer).await.map_err(|e| e.into()) })
    }

    fn cache_peers(&self, peers: Vec<PeerInfo>) -> BoxFuture<'_, Result<(), Self::Error>> {
        Box::pin(async move { self.0.cache_peers(peers).await.map_err(|e| e.into()) })
    }

    fn updates_state(&self) -> BoxFuture<'_, Result<UpdatesState, Self::Error>> {
        Box::pin(async { self.0.updates_state().await.map_err(|e| e.into()) })
    }

    fn set_update_state(&self, update: UpdateState) -> BoxFuture<'_, Result<(), Self::Error>> {
        Box::pin(async { self.0.set_update_state(update).await.map_err(|e| e.into()) })
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::Mutex;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::storages::MemorySession;
    use crate::types::{ChannelKind, PeerAuth};

    #[derive(Default)]
    struct BulkSession {
        single_calls: AtomicUsize,
        bulk_calls: AtomicUsize,
        peers: Mutex<Vec<PeerInfo>>,
        fail: bool,
    }

    impl Session for BulkSession {
        type Error = io::Error;

        fn home_dc_id(&self) -> Result<i32, Self::Error> {
            unreachable!()
        }
        fn set_home_dc_id(&self, _: i32) -> BoxFuture<'_, Result<(), Self::Error>> {
            unreachable!()
        }
        fn dc_option(&self, _: i32) -> Result<Option<DcOption>, Self::Error> {
            unreachable!()
        }
        fn set_dc_option(&self, _: &DcOption) -> BoxFuture<'_, Result<(), Self::Error>> {
            unreachable!()
        }
        fn peer(&self, _: PeerId) -> BoxFuture<'_, Result<Option<PeerInfo>, Self::Error>> {
            unreachable!()
        }
        fn updates_state(&self) -> BoxFuture<'_, Result<UpdatesState, Self::Error>> {
            unreachable!()
        }
        fn set_update_state(&self, _: UpdateState) -> BoxFuture<'_, Result<(), Self::Error>> {
            unreachable!()
        }

        fn cache_peer(&self, peer: PeerInfo) -> BoxFuture<'_, Result<(), Self::Error>> {
            Box::pin(async move {
                self.single_calls.fetch_add(1, Ordering::Relaxed);
                self.peers.lock().unwrap().push(peer);
                Ok(())
            })
        }

        fn cache_peers(&self, peers: Vec<PeerInfo>) -> BoxFuture<'_, Result<(), Self::Error>> {
            Box::pin(async move {
                self.bulk_calls.fetch_add(1, Ordering::Relaxed);
                if self.fail {
                    return Err(io::Error::other("bulk storage failure"));
                }
                *self.peers.lock().unwrap() = peers;
                Ok(())
            })
        }
    }

    fn run(future: impl Future<Output = ()>) {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap()
            .block_on(future);
    }

    #[test]
    fn erased_cache_peers_preserves_bulk_override_and_input_order() {
        run(async {
            let storage = Arc::new(BulkSession::default());
            let erased = erase(Arc::clone(&storage));
            let peers = vec![
                PeerInfo::Chat { id: 2 },
                PeerInfo::Chat { id: 1 },
                PeerInfo::Chat { id: 2 },
            ];
            erased.cache_peers(peers.clone()).await.unwrap();
            assert_eq!(storage.bulk_calls.load(Ordering::Relaxed), 1);
            assert_eq!(storage.single_calls.load(Ordering::Relaxed), 0);
            assert_eq!(*storage.peers.lock().unwrap(), peers);
        });
    }

    #[test]
    fn erased_cache_peers_forwards_empty_input_and_original_error() {
        run(async {
            let storage = Arc::new(BulkSession {
                fail: true,
                ..Default::default()
            });
            let erased = erase(Arc::clone(&storage));
            let error = erased.cache_peers(Vec::new()).await.unwrap_err();
            assert_eq!(
                error.downcast_ref::<io::Error>().unwrap().kind(),
                io::ErrorKind::Other
            );
            assert_eq!(storage.bulk_calls.load(Ordering::Relaxed), 1);
            assert_eq!(storage.single_calls.load(Ordering::Relaxed), 0);
        });
    }

    async fn check_ordered_cache_peers(batch: Arc<ErasedSession>, sequential: Arc<ErasedSession>) {
        let initial = vec![
            PeerInfo::User {
                id: 7,
                auth: Some(PeerAuth::from_hash(10)),
                bot: Some(true),
                is_self: Some(true),
            },
            PeerInfo::Channel {
                id: 7,
                auth: Some(PeerAuth::from_hash(20)),
                kind: Some(ChannelKind::Broadcast),
            },
        ];
        for peer in initial {
            batch.cache_peer(peer.clone()).await.unwrap();
            sequential.cache_peer(peer).await.unwrap();
        }
        let updates = vec![
            PeerInfo::User {
                id: 7,
                auth: Some(PeerAuth::from_hash(11)),
                bot: Some(false),
                is_self: None,
            },
            PeerInfo::Channel {
                id: 7,
                auth: None,
                kind: Some(ChannelKind::Megagroup),
            },
            PeerInfo::User {
                id: 8,
                auth: Some(PeerAuth::from_hash(30)),
                bot: Some(true),
                is_self: Some(false),
            },
            PeerInfo::User {
                id: 7,
                auth: None,
                bot: None,
                is_self: Some(true),
            },
            PeerInfo::Channel {
                id: 7,
                auth: Some(PeerAuth::from_hash(21)),
                kind: None,
            },
            PeerInfo::User {
                id: 8,
                auth: None,
                bot: None,
                is_self: None,
            },
            PeerInfo::Chat { id: 7 },
        ];
        batch.cache_peers(updates.clone()).await.unwrap();
        for peer in updates {
            sequential.cache_peer(peer).await.unwrap();
        }
        let expected = [
            PeerInfo::User {
                id: 7,
                auth: Some(PeerAuth::from_hash(11)),
                bot: Some(false),
                is_self: Some(true),
            },
            PeerInfo::Channel {
                id: 7,
                auth: Some(PeerAuth::from_hash(21)),
                kind: Some(ChannelKind::Megagroup),
            },
            PeerInfo::User {
                id: 8,
                auth: Some(PeerAuth::from_hash(30)),
                bot: Some(true),
                is_self: Some(false),
            },
            PeerInfo::Chat { id: 7 },
        ];
        batch.cache_peers(Vec::new()).await.unwrap();
        for peer in expected {
            assert_eq!(batch.peer(peer.id()).await.unwrap(), Some(peer.clone()));
            assert_eq!(sequential.peer(peer.id()).await.unwrap(), Some(peer));
        }
        assert_eq!(batch.peer(PeerId::user_unchecked(9)).await.unwrap(), None);
    }

    #[test]
    fn memory_cache_peers_matches_ordered_individual_updates() {
        run(check_ordered_cache_peers(
            erase(Arc::new(MemorySession::default())),
            erase(Arc::new(MemorySession::default())),
        ));
    }

    #[cfg(feature = "sqlite-storage")]
    #[test]
    fn sqlite_cache_peers_matches_ordered_individual_updates() {
        run(async {
            // Exercise the concrete bulk implementation even before erasure forwards it.
            let batch = Arc::new(
                crate::storages::SqliteSession::open(":memory:")
                    .await
                    .unwrap(),
            );
            let duplicate = vec![
                PeerInfo::Channel {
                    id: 10,
                    auth: Some(PeerAuth::from_hash(40)),
                    kind: None,
                },
                PeerInfo::Channel {
                    id: 10,
                    auth: None,
                    kind: Some(ChannelKind::Broadcast),
                },
            ];
            batch.cache_peers(duplicate).await.unwrap();
            assert_eq!(
                batch.peer(PeerId::channel_unchecked(10)).await.unwrap(),
                Some(PeerInfo::Channel {
                    id: 10,
                    auth: Some(PeerAuth::from_hash(40)),
                    kind: Some(ChannelKind::Broadcast),
                })
            );
            check_ordered_cache_peers(
                erase(batch),
                erase(Arc::new(
                    crate::storages::SqliteSession::open(":memory:")
                        .await
                        .unwrap(),
                )),
            )
            .await;
        });
    }

    fn legacy_memory_cache_peers(
        session: &MemorySession,
        peers: Vec<PeerInfo>,
    ) -> BoxFuture<'_, Result<(), crate::storages::MemorySessionError>> {
        Box::pin(async move {
            let mut result = Ok(());
            for peer in peers {
                if let Err(error) = session.cache_peer(peer).await {
                    result = Err(error);
                }
            }
            result
        })
    }

    fn legacy_erased_cache_peer(
        session: &Arc<MemorySession>,
        peer: PeerInfo,
    ) -> BoxFuture<'_, Result<(), Box<dyn std::error::Error + Send + Sync>>> {
        let peer = peer.clone();
        Box::pin(async move {
            Arc::clone(session)
                .cache_peer(peer)
                .await
                .map_err(Into::into)
        })
    }

    fn legacy_erased_cache_peers(
        session: &Arc<MemorySession>,
        peers: Vec<PeerInfo>,
    ) -> BoxFuture<'_, Result<(), Box<dyn std::error::Error + Send + Sync>>> {
        Box::pin(async move {
            let mut result = Ok(());
            for peer in peers {
                if let Err(error) = legacy_erased_cache_peer(session, peer).await {
                    result = Err(error);
                }
            }
            result
        })
    }

    #[test]
    #[ignore = "local session-storage microbenchmark; run explicitly in release mode"]
    fn benchmark_memory_and_erased_cache_peers() {
        use std::hint::black_box;
        use std::time::Instant;

        run(async {
            for size in [0, 1, 8, 100, 1000] {
                let peers: Vec<_> = (1..=size)
                    .map(|id| PeerInfo::User {
                        id: id as i64,
                        auth: Some(PeerAuth::from_hash(id as i64)),
                        bot: Some(false),
                        is_self: Some(false),
                    })
                    .collect();
                let iterations = (100_000 / size.max(1)).max(100);
                let mut samples: [Vec<f64>; 4] = std::array::from_fn(|_| Vec::new());
                for sample in 0..7 {
                    for offset in 0..4 {
                        let variant = (sample + offset) % 4;
                        let session = Arc::new(MemorySession::default());
                        let erased = erase(Arc::clone(&session));
                        session.cache_peers(peers.clone()).await.unwrap();
                        // Fixture cloning, runtime, erasure, and cache growth are outside timing.
                        let batches = vec![peers.clone(); iterations];
                        let started = Instant::now();
                        for batch in batches {
                            match variant {
                                0 => legacy_memory_cache_peers(&session, black_box(batch))
                                    .await
                                    .unwrap(),
                                1 => session.cache_peers(black_box(batch)).await.unwrap(),
                                2 => legacy_erased_cache_peers(&session, black_box(batch))
                                    .await
                                    .unwrap(),
                                3 => erased.cache_peers(black_box(batch)).await.unwrap(),
                                _ => unreachable!(),
                            }
                        }
                        samples[variant]
                            .push(started.elapsed().as_secs_f64() * 1e9 / iterations as f64);
                    }
                }
                for sample in &mut samples {
                    sample.sort_by(f64::total_cmp);
                }
                eprintln!(
                    "peers={size} iterations={iterations} ns/batch: memory sequential={:.1} bulk={:.1}; erased legacy-equivalent sequential={:.1} bulk={:.1}",
                    samples[0][3], samples[1][3], samples[2][3], samples[3][3]
                );
            }
        });
    }
}
