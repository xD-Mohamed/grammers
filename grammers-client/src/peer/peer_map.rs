// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

use grammers_session::storages::ErasedSession;
use grammers_session::types::{PeerId, PeerRef};

use crate::peer::{Peer, User};

/// Up to this many peers are kept in the order they came and found by looking through
/// them. A response to a bot carries two or three: for those a hash of the identifier
/// and a table sized for it cost more than they save.
const FEW_PEERS: usize = 16;

/// The peers of one response, by identifier.
pub(crate) enum Peers {
    Few(Vec<(PeerId, Peer)>),
    Many(HashMap<PeerId, Peer>),
}

impl Peers {
    /// One shared empty set: most messages a bot sends come back with no peers at all.
    pub(crate) fn empty() -> Arc<Self> {
        static EMPTY: OnceLock<Arc<Peers>> = OnceLock::new();
        Arc::clone(EMPTY.get_or_init(|| Arc::new(Peers::Few(Vec::new()))))
    }

    /// Collect peers by identifier. Of two with the same identifier the later one is
    /// kept, as when collecting into a map.
    pub(crate) fn collect(peers: impl ExactSizeIterator<Item = Peer>) -> Self {
        if peers.len() > FEW_PEERS {
            return Self::Many(peers.map(|peer| (peer.id(), peer)).collect());
        }
        let mut few: Vec<(PeerId, Peer)> = Vec::with_capacity(peers.len());
        for peer in peers {
            let id = peer.id();
            match few.iter_mut().find(|(known, _)| *known == id) {
                Some((_, known)) => *known = peer,
                None => few.push((id, peer)),
            }
        }
        Self::Few(few)
    }

    pub(crate) fn get(&self, id: PeerId) -> Option<&Peer> {
        match self {
            Self::Few(few) => few
                .iter()
                .find(|(known, _)| *known == id)
                .map(|(_, peer)| peer),
            Self::Many(many) => many.get(&id),
        }
    }

    fn remove(&mut self, id: PeerId) -> Option<Peer> {
        match self {
            Self::Few(few) => few
                .iter()
                .position(|(known, _)| *known == id)
                .map(|index| few.swap_remove(index).1),
            Self::Many(many) => many.remove(&id),
        }
    }

    pub(crate) fn iter(&self) -> impl Iterator<Item = (PeerId, &Peer)> {
        let (few, many) = match self {
            Self::Few(few) => (Some(few.iter()), None),
            Self::Many(many) => (None, Some(many.iter())),
        };
        few.into_iter()
            .flatten()
            .map(|(id, peer)| (*id, peer))
            .chain(many.into_iter().flatten().map(|(id, peer)| (*id, peer)))
    }

    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        match self {
            Self::Few(few) => few.len(),
            Self::Many(many) => many.len(),
        }
    }
}

/// Helper structure to efficiently retrieve peers via their peer.
///
/// A lot of responses include the peers related to them in the form of a list of users
/// and peers, making it annoying to extract a specific peer. This structure lets you
/// save those separate vectors in a single place and query them by using a `Peer`.
///
/// While this type derives `Clone` for convenience, it is recommended to use
/// [`PeerMap::handle`] instead to signal that it is a cheap clone.
#[derive(Clone)]
pub struct PeerMap {
    pub(crate) map: Arc<Peers>,
    pub(crate) session: Arc<ErasedSession>,
}

impl PeerMap {
    /// Retrieve the full `Peer` object given its `PeerId`.
    pub fn get(&self, peer: PeerId) -> Option<&Peer> {
        self.map.get(peer)
    }

    /// Retrieve a non-min `PeerRef` from either the in-memory cache or the session.
    pub async fn get_ref(
        &self,
        peer: PeerId,
    ) -> Result<Option<PeerRef>, Box<dyn std::error::Error + Send + Sync>> {
        match self.map.get(peer) {
            Some(peer) => peer.to_ref().await,
            None => self.session.peer_ref(peer).await,
        }
    }

    /// Take the full `Peer` object given its `PeerId`.
    ///
    /// The peer will be removed from the map if there are no other strong references to it.
    pub fn take(&mut self, peer: PeerId) -> Option<Peer> {
        match Arc::get_mut(&mut self.map) {
            Some(map) => map.remove(peer),
            None => self.get(peer).cloned(),
        }
    }

    pub(crate) fn take_user(&mut self, user_id: i64) -> Option<User> {
        self.take(PeerId::user_unchecked(user_id))
            .map(|peer| match peer {
                Peer::User(user) => user,
                _ => unreachable!(),
            })
    }

    /// Iterate over the peers and peers in the map.
    pub fn iter(&self) -> impl Iterator<Item = (PeerId, &Peer)> {
        self.map.iter()
    }

    /// Iterate over the peers in the map.
    pub fn iter_peers(&self) -> impl Iterator<Item = &Peer> {
        self.map.iter().map(|(_, peer)| peer)
    }

    /// Return a new strong reference to the map and session contained within.
    pub fn handle(&self) -> Self {
        Self {
            map: Arc::clone(&self.map),
            session: Arc::clone(&self.session),
        }
    }
}
