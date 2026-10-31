// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Session type definitions.

use std::fmt;
use std::net::{SocketAddrV4, SocketAddrV6};

pub use crate::peer::{ChannelKind, PeerAuth, PeerId, PeerInfo, PeerKind, PeerRef};

/// A datacenter option.
///
/// This is very similar to Telegram's own `dcOption` type, except it also
/// contains the permanent authentication key and serves as a stable interface.
#[derive(Clone, PartialEq, Eq)]
#[cfg_attr(feature = "serde", serde_with::serde_as)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct DcOption {
    /// Datacenter identifier.
    ///
    /// The primary datacenters have IDs from 1 to 5 inclusive, and are known statically by the session.
    /// ```
    /// let data = grammers_session::SessionData::default();
    /// assert_eq!(data.dc_options.len(), 5);
    /// (1..=5).for_each(|dc_id| assert!(data.dc_options.contains_key(&dc_id)));
    /// ```
    pub id: i32,
    /// IPv4 address corresponding to this datacenter.
    pub ipv4: SocketAddrV4,
    /// IPv6 address corresponding to this datacenter. May actually be embedding the [`Self::ipv4`] address.
    pub ipv6: SocketAddrV6,
    /// Permanent authentication key generated for encrypted communication with this datacenter.
    ///
    /// A logged-in user may or not be bound to this authentication key.
    #[cfg(not(feature = "serde"))]
    pub auth_key: Option<[u8; 256]>,

    #[cfg(feature = "serde")]
    #[serde_as(as = "Option<serde_with::hex::Hex>")]
    pub auth_key: Option<[u8; 256]>,
}

impl fmt::Debug for DcOption {
    /// The permanent authentication key is left out: it grants full account access,
    /// and debug output tends to end up in logs.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DcOption")
            .field("id", &self.id)
            .field("ipv4", &self.ipv4)
            .field("ipv6", &self.ipv6)
            .field(
                "auth_key",
                &self.auth_key.as_ref().map(|_| format_args!("<redacted>")),
            )
            .finish()
    }
}

/// Full update state needed to process updates in order without gaps.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct UpdatesState {
    /// Primary persistent timestamp value.
    pub pts: i32,
    /// Secondary persistent timestamp value.
    pub qts: i32,
    /// Auxiliary date value.
    pub date: i32,
    /// Auxiliary sequence value.
    pub seq: i32,
    /// Persistent timestamp of each known channel.
    pub channels: Vec<ChannelState>,
}

/// Update state for a single channel.
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub struct ChannelState {
    /// The [`PeerId::bare_id`] of the channel.
    pub id: i64,
    /// Persistent timestamp value.
    pub pts: i32,
}

/// Used in [`crate::Session::set_update_state`] to update parts of the overall [`UpdatesState`].
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum UpdateState {
    /// Updates the entirety of the state.
    All(UpdatesState),
    /// Updates only what's known as the "primary" state of the account.
    Primary {
        /// New [`UpdatesState::pts`] value.
        pts: i32,
        /// New [`UpdatesState::date`] value.
        date: i32,
        /// New [`UpdatesState::seq`] value.
        seq: i32,
    },
    /// Updates only what's known as the "secondary" state of the account.
    Secondary {
        /// New [`UpdatesState::qts`] value.
        qts: i32,
    },
    /// Updates the state of a single channel.
    Channel {
        /// The [`PeerId::bare_id`] of the channel.
        id: i64,
        /// New [`ChannelState::pts`] value.
        pts: i32,
    },
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn debug_output_never_contains_the_auth_key() {
        let option = DcOption {
            id: 2,
            ipv4: SocketAddrV4::new(Ipv4Addr::LOCALHOST, 443),
            ipv6: SocketAddrV6::new(Ipv4Addr::LOCALHOST.to_ipv6_mapped(), 443, 0, 0),
            auth_key: Some([0xab; 256]),
        };
        for text in [format!("{option:?}"), format!("{option:#?}")] {
            assert!(!text.contains("171"), "{text}"); // 0xab in decimal
            assert!(text.contains("<redacted>"));
            assert!(text.contains("127.0.0.1:443"));
        }
        let keyless = DcOption {
            auth_key: None,
            ..option
        };
        assert!(format!("{keyless:?}").contains("auth_key: None"));
    }
}
