// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::collections::HashMap;
use std::fmt;
use std::net::{SocketAddrV4, SocketAddrV6};
use std::sync::{Mutex, MutexGuard};

use crate::types::{ChannelState, DcOption, PeerId, PeerInfo, UpdateState, UpdatesState};
use crate::{BoxFuture, Session, SessionData};

/// In-memory session interface.
///
/// Does not actually offer direct ways to persist the state anywhere,
/// so it should only be used in very few select cases.
///
/// Logging in has a very high cost in terms of flood wait errors,
/// so the state really should be persisted by other means.
pub struct MemorySession(Mutex<MemoryData>);

/// [`SessionData`] with compact datacenter options. Usually only the home
/// datacenter has a key, and an inline `Option<[u8; 256]>` costs 257 bytes
/// for every option; applications holding many sessions pay that per option.
struct MemoryData {
    home_dc: i32,
    dc_options: Vec<StoredDcOption>,
    peer_infos: HashMap<PeerId, PeerInfo>,
    updates_state: UpdatesState,
}

/// One datacenter option, under the key it was stored with.
struct StoredDcOption {
    key: i32,
    id: i32,
    ipv4: SocketAddrV4,
    ipv6: SocketAddrV6,
    auth_key: Option<Box<[u8; 256]>>,
}

impl StoredDcOption {
    fn new(key: i32, option: &DcOption) -> Self {
        Self {
            key,
            id: option.id,
            ipv4: option.ipv4,
            ipv6: option.ipv6,
            auth_key: option.auth_key.map(Box::new),
        }
    }

    fn to_option(&self) -> DcOption {
        DcOption {
            id: self.id,
            ipv4: self.ipv4,
            ipv6: self.ipv6,
            auth_key: self.auth_key.as_deref().copied(),
        }
    }
}

impl Default for MemorySession {
    fn default() -> Self {
        Self::from(SessionData::default())
    }
}

impl From<SessionData> for MemorySession {
    /// Constructs a memory session from the entirety of the session data,
    /// unlike the blanket `From` implementation which cannot import all values
    fn from(session_data: SessionData) -> Self {
        let SessionData {
            home_dc,
            dc_options,
            peer_infos,
            updates_state,
        } = session_data;
        Self(Mutex::new(MemoryData {
            home_dc,
            dc_options: dc_options
                .iter()
                .map(|(key, option)| StoredDcOption::new(*key, option))
                .collect(),
            peer_infos,
            updates_state,
        }))
    }
}

#[derive(Debug)]
pub enum MemorySessionError {
    Poisoned,
}

impl std::error::Error for MemorySessionError {}

impl fmt::Display for MemorySessionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MemorySessionError::Poisoned => write!(f, "session lock is poisoned"),
        }
    }
}

impl MemorySession {
    /// Lock the session data, mapping a poisoned mutex into [`MemorySessionError`].
    fn data(&self) -> Result<MutexGuard<'_, MemoryData>, MemorySessionError> {
        self.0.lock().map_err(|_| MemorySessionError::Poisoned)
    }
}

impl Session for MemorySession {
    type Error = MemorySessionError;

    fn home_dc_id(&self) -> Result<i32, MemorySessionError> {
        Ok(self.data()?.home_dc)
    }

    fn set_home_dc_id(&self, dc_id: i32) -> BoxFuture<'_, Result<(), MemorySessionError>> {
        Box::pin(async move {
            self.data()?.home_dc = dc_id;
            Ok(())
        })
    }

    fn dc_option(&self, dc_id: i32) -> Result<Option<DcOption>, MemorySessionError> {
        Ok(self
            .data()?
            .dc_options
            .iter()
            .find(|option| option.key == dc_id)
            .map(StoredDcOption::to_option))
    }

    fn set_dc_option(&self, dc_option: &DcOption) -> BoxFuture<'_, Result<(), MemorySessionError>> {
        let stored = StoredDcOption::new(dc_option.id, dc_option);
        Box::pin(async move {
            let mut data = self.data()?;
            match data
                .dc_options
                .iter_mut()
                .find(|option| option.key == stored.key)
            {
                Some(existing) => *existing = stored,
                None => data.dc_options.push(stored),
            }
            Ok(())
        })
    }

    fn peer(&self, peer: PeerId) -> BoxFuture<'_, Result<Option<PeerInfo>, MemorySessionError>> {
        Box::pin(async move {
            let data = self.data()?;
            Ok(if peer == PeerId::self_user() {
                data.peer_infos.values().find(|info| {
                    matches!(
                        info,
                        PeerInfo::User {
                            is_self: Some(true),
                            ..
                        }
                    )
                })
            } else {
                data.peer_infos.get(&peer)
            }
            .cloned())
        })
    }

    fn cache_peer(&self, peer: PeerInfo) -> BoxFuture<'_, Result<(), MemorySessionError>> {
        Box::pin(async move {
            self.data()?
                .peer_infos
                .entry(peer.id())
                .or_insert_with(|| peer.clone())
                .extend_info(&peer);
            Ok(())
        })
    }

    fn cache_peers(&self, peers: Vec<PeerInfo>) -> BoxFuture<'_, Result<(), MemorySessionError>> {
        Box::pin(async move {
            if peers.is_empty() {
                return Ok(());
            }
            let mut data = self.data()?;
            for peer in peers {
                data.peer_infos
                    .entry(peer.id())
                    .and_modify(|existing| {
                        existing.extend_info(&peer);
                    })
                    .or_insert(peer);
            }
            Ok(())
        })
    }

    fn updates_state(&self) -> BoxFuture<'_, Result<UpdatesState, MemorySessionError>> {
        Box::pin(async move { Ok(self.data()?.updates_state.clone()) })
    }

    fn set_update_state(
        &self,
        update: UpdateState,
    ) -> BoxFuture<'_, Result<(), MemorySessionError>> {
        Box::pin(async move {
            let mut data = self.data()?;

            match update {
                UpdateState::All(updates_state) => {
                    data.updates_state = updates_state;
                }
                UpdateState::Primary { pts, date, seq } => {
                    data.updates_state.pts = pts;
                    data.updates_state.date = date;
                    data.updates_state.seq = seq;
                }
                UpdateState::Secondary { qts } => {
                    data.updates_state.qts = qts;
                }
                UpdateState::Channel { id, pts } => {
                    data.updates_state.channels.retain(|c| c.id != id);
                    data.updates_state.channels.push(ChannelState { id, pts });
                }
            }

            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::PeerAuth;

    #[test]
    fn self_user_lookup_uses_cached_identity_and_tracks_flag_changes() {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async {
                let session = MemorySession::default();
                let identity = PeerInfo::User {
                    id: 7,
                    auth: Some(PeerAuth::from_hash(100)),
                    bot: Some(false),
                    is_self: Some(true),
                };
                session.cache_peer(identity.clone()).await.unwrap();
                assert_eq!(
                    session.peer(PeerId::self_user()).await.unwrap(),
                    Some(identity.clone())
                );
                assert_eq!(
                    session.peer(PeerId::user_unchecked(7)).await.unwrap(),
                    Some(identity)
                );
                session
                    .cache_peer(PeerInfo::User {
                        id: 7,
                        auth: None,
                        bot: None,
                        is_self: Some(false),
                    })
                    .await
                    .unwrap();
                assert_eq!(session.peer(PeerId::self_user()).await.unwrap(), None);
                assert_eq!(
                    session.peer(PeerId::user_unchecked(7)).await.unwrap(),
                    Some(PeerInfo::User {
                        id: 7,
                        auth: Some(PeerAuth::from_hash(100)),
                        bot: Some(false),
                        is_self: Some(false),
                    })
                );
            });
    }

    #[test]
    fn self_user_lookup_recognizes_bulk_cached_identity() {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async {
                let session = MemorySession::default();
                let identity = PeerInfo::User {
                    id: 9,
                    auth: Some(PeerAuth::from_hash(200)),
                    bot: Some(true),
                    is_self: Some(true),
                };
                session
                    .cache_peers(vec![PeerInfo::Chat { id: 9 }, identity.clone()])
                    .await
                    .unwrap();
                assert_eq!(
                    session.peer(PeerId::self_user()).await.unwrap(),
                    Some(identity)
                );
            });
    }

    #[test]
    fn compact_dc_options_round_trip_session_data() {
        let mut data = SessionData::default();
        let mut keyed = data.dc_options[&2].clone();
        keyed.auth_key = Some([3; 256]);
        data.dc_options.insert(2, keyed.clone());
        // An entry stored under a different key keeps that key, as in a map.
        let mut foreign = data.dc_options[&5].clone();
        foreign.id = 9;
        data.dc_options.insert(5, foreign.clone());
        let expected = data.dc_options.clone();
        let session = MemorySession::from(data);
        for (key, option) in &expected {
            assert_eq!(session.dc_option(*key).unwrap().as_ref(), Some(option));
        }
        assert_eq!(session.dc_option(9).unwrap(), None);

        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async {
                let mut replaced = keyed.clone();
                replaced.auth_key = None;
                session.set_dc_option(&replaced).await.unwrap();
                assert_eq!(session.dc_option(2).unwrap(), Some(replaced));
                let mut added = keyed;
                added.id = 203;
                added.auth_key = Some([4; 256]);
                session.set_dc_option(&added).await.unwrap();
                assert_eq!(session.dc_option(203).unwrap(), Some(added));
                assert_eq!(session.dc_option(5).unwrap(), Some(foreign.clone()));
                session.set_dc_option(&foreign).await.unwrap();
                assert_eq!(session.dc_option(9).unwrap(), Some(foreign));
            });
    }

    #[test]
    fn cache_peers_empty_input_succeeds_with_poisoned_storage() {
        let session = MemorySession::default();
        let _ = std::panic::catch_unwind(|| {
            let _data = session.0.lock().unwrap();
            panic!("poison storage for error-path test");
        });
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(async {
                session.cache_peers(Vec::new()).await.unwrap();
                assert!(matches!(
                    session.cache_peers(vec![PeerInfo::Chat { id: 1 }]).await,
                    Err(MemorySessionError::Poisoned)
                ));
            });
    }
}
