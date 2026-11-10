// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use grammers_session::updates::State;
use grammers_tl_types as tl;

use super::message::Kind;
use super::{
    CallbackQuery, GuestChatQuery, InlineQuery, InlineSend, Message, MessageDeletion, Raw,
};
use crate::{Client, peer::PeerMap, utils};

/// An update that indicates some event, which may be of interest to the logged-in account, has occured.
///
/// Only updates pertaining to messages are guaranteed to be delivered, and can be fetched on-demand if
/// they occured while the client was offline by enabling [`catch_up`](grammers_mtsender::UpdatesConfiguration::catch_up).
#[non_exhaustive]
#[derive(Debug, Clone)]
pub enum Update {
    /// Occurs whenever a new text message or a message with media is produced.
    NewMessage(Message),
    /// Occurs when a message is updated.
    MessageEdited(Message),
    /// Occurs when a message is deleted.
    MessageDeleted(MessageDeletion),
    /// Occurs when Telegram calls back into your bot because an inline callback
    /// button was pressed.
    CallbackQuery(CallbackQuery),
    /// Occurs whenever you sign in as a bot and a user sends an inline query
    /// such as `@bot query`.
    InlineQuery(InlineQuery),
    /// Represents an update of user choosing the result of inline query and sending it to their peer partner.
    InlineSend(InlineSend),
    /// Occurs whenever you sign in as a bot and a user sends a message mention you in a public group
    /// such as `hi @bot`.
    GuestChatQuery(GuestChatQuery),
    /// Raw events are not actual events.
    /// Instead, they are the raw Update object that Telegram sends. You
    /// normally shouldn’t need these.
    ///
    /// <div class="warning">The library can "move" raw update types into
    /// a separate update wrapper variant during minor version bumps, so use
    /// this only as a workaround when such variant is not available yet.</div>
    Raw(Raw),
}

impl Update {
    /// Create new friendly to use `Update`` from its raw version and peer map.
    ///
    /// The message of a new or edited message is moved out of its update, not cloned.
    pub fn from_raw(
        client: &Client,
        update: tl::enums::Update,
        state: State,
        peers: PeerMap,
    ) -> Self {
        match update {
            // NewMessage
            tl::enums::Update::NewMessage(raw) => Self::from_message(
                client,
                Kind::New,
                raw.message,
                raw.pts,
                raw.pts_count,
                state,
                peers,
            ),
            tl::enums::Update::NewChannelMessage(raw) => Self::from_message(
                client,
                Kind::NewChannel,
                raw.message,
                raw.pts,
                raw.pts_count,
                state,
                peers,
            ),

            // MessageEdited
            tl::enums::Update::EditMessage(raw) => Self::from_message(
                client,
                Kind::Edit,
                raw.message,
                raw.pts,
                raw.pts_count,
                state,
                peers,
            ),
            tl::enums::Update::EditChannelMessage(raw) => Self::from_message(
                client,
                Kind::EditChannel,
                raw.message,
                raw.pts,
                raw.pts_count,
                state,
                peers,
            ),

            // MessageDeleted
            tl::enums::Update::DeleteMessages(_) => {
                Self::MessageDeleted(MessageDeletion { raw: update, state })
            }
            tl::enums::Update::DeleteChannelMessages(_) => {
                Self::MessageDeleted(MessageDeletion { raw: update, state })
            }

            // CallbackQuery
            tl::enums::Update::BotCallbackQuery(_) => Self::CallbackQuery(CallbackQuery {
                raw: update,
                state,
                client: client.clone(),
                peers,
            }),

            // InlineCallbackQuery
            tl::enums::Update::InlineBotCallbackQuery(_) => Self::CallbackQuery(CallbackQuery {
                raw: update,
                state,
                client: client.clone(),
                peers,
            }),

            // InlineQuery
            tl::enums::Update::BotInlineQuery(_) => Self::InlineQuery(InlineQuery {
                raw: update,
                state,
                client: client.clone(),
                peers,
            }),

            // InlineSend
            tl::enums::Update::BotInlineSend(_) => Self::InlineSend(InlineSend {
                raw: update,
                state,
                client: client.clone(),
                peers,
            }),

            // GuestChatQuery
            tl::enums::Update::BotGuestChatQuery(ref raw) => {
                if utils::peer_from_message(&raw.message).is_none()
                    || raw.reference_messages.as_ref().is_some_and(|messages| {
                        messages
                            .iter()
                            .any(|message| utils::peer_from_message(message).is_none())
                    })
                {
                    return Self::Raw(Raw { raw: update, state });
                }
                Self::GuestChatQuery(GuestChatQuery {
                    message: crate::message::Message::from_raw(
                        client,
                        raw.message.clone(),
                        None,
                        peers.clone(),
                    ),
                    reference_messages: raw
                        .reference_messages
                        .clone()
                        .unwrap_or_default()
                        .into_iter()
                        .map(|msg| {
                            crate::message::Message::from_raw(client, msg, None, peers.clone())
                        })
                        .collect(),
                    raw: update,
                    state,
                })
            }

            // Raw
            _ => Self::Raw(Raw { raw: update, state }),
        }
    }

    /// Wrap the message one of the four message updates carried, without cloning it.
    ///
    /// A message that names no peer cannot be wrapped: its update is put back together
    /// from the same parts and kept whole as [`Update::Raw`].
    fn from_message(
        client: &Client,
        kind: Kind,
        message: tl::enums::Message,
        pts: i32,
        pts_count: i32,
        state: State,
        peers: PeerMap,
    ) -> Self {
        if utils::peer_from_message(&message).is_none() {
            let raw = kind.update(message, pts, pts_count);
            return Self::Raw(Raw { raw, state });
        }

        let msg = crate::message::Message::from_raw(client, message, None, peers);
        let message = Message::new(msg, kind, pts, pts_count, state);
        match kind {
            Kind::New | Kind::NewChannel => Self::NewMessage(message),
            Kind::Edit | Kind::EditChannel => Self::MessageEdited(message),
        }
    }

    /// Update state.
    pub fn state(&self) -> &State {
        match self {
            Update::NewMessage(update) => &update.state,
            Update::MessageEdited(update) => &update.state,
            Update::MessageDeleted(update) => &update.state,
            Update::CallbackQuery(update) => &update.state,
            Update::InlineQuery(update) => &update.state,
            Update::InlineSend(update) => &update.state,
            Update::GuestChatQuery(update) => &update.state,
            Update::Raw(update) => &update.state,
        }
    }

    /// Raw update, as sent by Telegram.
    ///
    /// Only contains the individual [`Update`](tl::enums::Update),
    /// not the [`Updates`](tl::enums::Updates) container from which it may have come from.
    ///
    /// A new or edited message does not keep its raw update beside the message: the first
    /// call builds it by cloning the message, as [`Message::raw`] describes.
    pub fn raw(&self) -> &tl::enums::Update {
        match self {
            Update::NewMessage(update) => update.raw(),
            Update::MessageEdited(update) => update.raw(),
            Update::MessageDeleted(update) => &update.raw,
            Update::CallbackQuery(update) => &update.raw,
            Update::InlineQuery(update) => &update.raw,
            Update::InlineSend(update) => &update.raw,
            Update::GuestChatQuery(update) => &update.raw,
            Update::Raw(update) => &update.raw,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use grammers_mtsender::SenderPool;
    use grammers_session::storages::MemorySession;
    use grammers_session::updates::MessageBox;
    use std::sync::Arc;

    /// A message as complete as this layer makes one, in the chat `peer`.
    fn message(id: i32, peer: tl::enums::Peer, text: &str) -> tl::enums::Message {
        tl::types::Message {
            out: false,
            mentioned: false,
            media_unread: false,
            silent: false,
            post: false,
            from_scheduled: false,
            legacy: false,
            edit_hide: false,
            pinned: false,
            noforwards: false,
            invert_media: false,
            offline: false,
            video_processing_pending: false,
            paid_suggested_post_stars: false,
            paid_suggested_post_ton: false,
            id,
            from_id: Some(tl::types::PeerUser { user_id: 7 }.into()),
            from_boosts_applied: None,
            from_rank: None,
            peer_id: peer,
            saved_peer_id: None,
            fwd_from: None,
            via_bot_id: None,
            via_business_bot_id: None,
            guestchat_via_from: None,
            reply_to: None,
            date: 1,
            message: text.into(),
            media: None,
            reply_markup: None,
            entities: None,
            views: None,
            forwards: None,
            replies: None,
            edit_date: None,
            post_author: None,
            grouped_id: None,
            reactions: None,
            restriction_reason: None,
            ttl_period: None,
            quick_reply_shortcut_id: None,
            effect: None,
            factcheck: None,
            report_delivery_until_date: None,
            paid_message_stars: None,
            suggested_post: None,
            schedule_repeat_period: None,
            summary_from_language: None,
            rich_message: None,
        }
        .into()
    }

    /// The updates that carry `message`: its arrival, or an edit of it when `edited`, each
    /// on the common sequence and on a channel's. No two share a position, so that one
    /// put back together as another, or at another's position, cannot compare equal.
    fn message_updates(message: &tl::enums::Message, edited: bool) -> [tl::enums::Update; 2] {
        if edited {
            [
                tl::enums::Update::EditMessage(tl::types::UpdateEditMessage {
                    message: message.clone(),
                    pts: 13,
                    pts_count: 3,
                }),
                tl::enums::Update::EditChannelMessage(tl::types::UpdateEditChannelMessage {
                    message: message.clone(),
                    pts: 14,
                    pts_count: 4,
                }),
            ]
        } else {
            [
                tl::enums::Update::NewMessage(tl::types::UpdateNewMessage {
                    message: message.clone(),
                    pts: 11,
                    pts_count: 1,
                }),
                tl::enums::Update::NewChannelMessage(tl::types::UpdateNewChannelMessage {
                    message: message.clone(),
                    pts: 12,
                    pts_count: 2,
                }),
            ]
        }
    }

    /// `updateNewMessage` for a message that says `text`.
    fn new_message(text: &str) -> tl::enums::Update {
        tl::enums::Update::NewMessage(tl::types::UpdateNewMessage {
            message: message(5, tl::types::PeerUser { user_id: 7 }.into(), text),
            pts: 11,
            pts_count: 1,
        })
    }

    /// Change the text of a wrapped message as a caller can: through its public field.
    fn set_text(message: &mut crate::message::Message, text: &str) {
        match &mut message.raw {
            tl::enums::Message::Message(raw) => raw.message = text.into(),
            other => panic!("{other:?} has no text"),
        }
    }

    fn common_state() -> State {
        State {
            message_box: Some(MessageBox::Common { pts: 11 }),
            seq: 1,
            date: 1,
        }
    }

    /// What `from_raw` must make of `raw`, an update carrying `original`, which has a peer.
    fn assert_wraps_message(
        client: &Client,
        raw: tl::enums::Update,
        original: &tl::enums::Message,
        edited: bool,
    ) {
        let state = common_state();
        let peers = client.empty_peer_map();
        let update = Update::from_raw(client, raw.clone(), state.clone(), peers);
        let message = match &update {
            Update::NewMessage(message) if !edited => message,
            Update::MessageEdited(message) if edited => message,
            other => panic!("{raw:?} became {other:?}"),
        };
        assert_eq!(update.state(), &state);
        assert_eq!(message.state, state);

        // The message was moved into the wrapper whole.
        let wrapped: &crate::message::Message = message;
        assert_eq!(&wrapped.raw, original);

        // Nothing has asked for the raw update yet, and giving it away builds none.
        assert!(!message.raw_is_built());
        let unbuilt = message.clone();
        assert_eq!(message.clone().into_raw(), raw);
        assert!(!message.raw_is_built());

        // The first call builds it, and every later one returns that same value.
        assert_eq!(update.raw(), &raw);
        assert!(message.raw_is_built());
        assert!(std::ptr::eq(message.raw(), message.raw()));
        assert!(std::ptr::eq(update.raw(), message.raw()));
        assert_eq!(message.clone().into_raw(), raw);

        // A clone taken before that builds its own. One taken after carries a copy.
        assert!(!unbuilt.raw_is_built());
        assert_eq!(unbuilt.raw(), &raw);
        let built = message.clone();
        assert!(built.raw_is_built());
        assert_eq!(built.raw(), &raw);
        assert!(!std::ptr::eq(built.raw(), message.raw()));
    }

    #[tokio::test]
    async fn message_updates_hold_one_message_and_rebuild_their_raw_update_on_request() {
        let pool = SenderPool::new(Arc::new(MemorySession::default()), 1);
        let client = Client::new(pool.handle);
        let user: tl::enums::Peer = tl::types::PeerUser { user_id: 7 }.into();
        let channel: tl::enums::Peer = tl::types::PeerChannel { channel_id: 73 }.into();
        for peer in [user, channel] {
            let original = message(5, peer, "hello");
            for edited in [false, true] {
                for raw in message_updates(&original, edited) {
                    assert_wraps_message(&client, raw, &original, edited);
                }
            }
        }
    }

    #[tokio::test]
    async fn raw_keeps_its_first_reading_and_into_raw_takes_the_message_as_it_is() {
        let pool = SenderPool::new(Arc::new(MemorySession::default()), 1);
        let client = Client::new(pool.handle);
        let peers = client.empty_peer_map();
        let update = Update::from_raw(&client, new_message("received"), common_state(), peers);
        let Update::NewMessage(mut message) = update else {
            panic!("a message with a peer is wrapped");
        };

        // Not built yet: the first call reads the message as it is by then.
        set_text(&mut message, "changed once");
        assert_eq!(message.raw(), &new_message("changed once"));

        // Built: a later change stays out of it, and is not lost to `into_raw`.
        set_text(&mut message, "changed twice");
        assert_eq!(message.raw(), &new_message("changed once"));
        assert_eq!(message.text(), "changed twice");

        // A clone carries the value already built, whatever is then done to its message.
        let mut clone = message.clone();
        set_text(&mut clone, "changed in the clone");
        assert_eq!(clone.raw(), &new_message("changed once"));
        assert_eq!(clone.into_raw(), new_message("changed in the clone"));
        assert_eq!(message.into_raw(), new_message("changed twice"));
    }

    #[tokio::test]
    async fn peerless_messages_of_every_kind_are_retained_as_their_own_raw_update() {
        let pool = SenderPool::new(Arc::new(MemorySession::default()), 1);
        let client = Client::new(pool.handle);
        let peerless = tl::enums::Message::Empty(tl::types::MessageEmpty {
            id: 7,
            peer_id: None,
        });
        for edited in [false, true] {
            for raw in message_updates(&peerless, edited) {
                let state = common_state();
                let peers = client.empty_peer_map();
                let update = Update::from_raw(&client, raw.clone(), state.clone(), peers);
                assert!(matches!(&update, Update::Raw(_)));
                assert_eq!(update.raw(), &raw);
                assert_eq!(update.state(), &state);
            }
        }
    }

    #[tokio::test]
    async fn peerless_channel_edit_is_retained_as_raw() {
        let pool = SenderPool::new(Arc::new(MemorySession::default()), 1);
        let client = Client::new(pool.handle);
        let raw = tl::enums::Update::EditChannelMessage(tl::types::UpdateEditChannelMessage {
            message: tl::types::MessageEmpty {
                id: 7,
                peer_id: None,
            }
            .into(),
            pts: 11,
            pts_count: 1,
        });
        let state = State {
            message_box: Some(MessageBox::Channel {
                channel_id: 73,
                pts: 11,
            }),
            seq: 1,
            date: 1,
        };
        let update = Update::from_raw(&client, raw.clone(), state.clone(), client.empty_peer_map());
        assert!(matches!(&update, Update::Raw(_)));
        assert_eq!(update.raw(), &raw);
        assert_eq!(update.state(), &state);
    }

    #[tokio::test]
    async fn peerless_guest_chat_messages_preserve_the_entire_raw_update() {
        let pool = SenderPool::new(Arc::new(MemorySession::default()), 1);
        let client = Client::new(pool.handle);
        for main_has_peer in [false, true] {
            for reference_has_peer in [false, true] {
                let message = |id, has_peer: bool| {
                    tl::enums::Message::Empty(tl::types::MessageEmpty {
                        id,
                        peer_id: has_peer.then(|| tl::types::PeerUser { user_id: 7 }.into()),
                    })
                };
                let raw =
                    tl::enums::Update::BotGuestChatQuery(tl::types::UpdateBotGuestChatQuery {
                        query_id: 42,
                        message: message(7, main_has_peer),
                        reference_messages: Some(vec![message(8, reference_has_peer)]),
                        qts: 11,
                    });
                let state = State {
                    message_box: Some(MessageBox::Secondary { qts: 11 }),
                    seq: 1,
                    date: 1,
                };
                let update =
                    Update::from_raw(&client, raw.clone(), state.clone(), client.empty_peer_map());
                if main_has_peer && reference_has_peer {
                    assert!(matches!(&update, Update::GuestChatQuery(_)));
                } else {
                    assert!(matches!(&update, Update::Raw(_)));
                }
                assert_eq!(update.raw(), &raw);
                assert_eq!(update.state(), &state);
            }
        }
    }
}
