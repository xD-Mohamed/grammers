// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Methods related to sending messages.

use std::collections::HashMap;
use std::future::Future;

use grammers_mtsender::InvocationError;
use grammers_session::types::{PeerAuth, PeerId, PeerInfo, PeerKind, PeerRef};
use grammers_tl_types::{self as tl, enums::InputPeer};
use jiff::Timestamp;
use tracing::warn;

use super::{Client, IterBuffer};
use crate::media::{InputMedia, Media};
use crate::message::{InputMessage, InputReactions, Message, SendOptions};
use crate::utils::{generate_random_id, generate_random_ids};

/// The contents of `tl::enums::Updates` which can map to sent messages.
///
/// Taken outside any future: that enum is several kilobytes, which every enclosing
/// future would otherwise carry for its whole life.
type SentUpdateParts = Option<(
    Vec<tl::enums::Update>,
    Vec<tl::enums::User>,
    Vec<tl::enums::Chat>,
)>;

fn sent_update_parts(updates: tl::enums::Updates) -> SentUpdateParts {
    match updates {
        tl::enums::Updates::Updates(tl::types::Updates {
            updates,
            users,
            chats,
            date: _,
            seq: _,
        }) => Some((updates, users, chats)),
        _ => None,
    }
}

async fn map_random_ids_to_messages(
    client: &Client,
    fetched_in: PeerRef,
    random_ids: &[i64],
    parts: SentUpdateParts,
) -> Result<Vec<Option<Message>>, Box<dyn std::error::Error + Send + Sync>> {
    let Some((updates, users, chats)) = parts else {
        // The send may have succeeded without a usable message mapping. Preserve
        // one result per input for the existing fallback, without resending.
        return Ok(random_ids.iter().map(|_| None).collect());
    };
    let peers = client.build_peer_map(users, chats).await;

    let rnd_to_id = updates
        .iter()
        .filter_map(|update| match update {
            tl::enums::Update::MessageId(u) => Some((u.random_id, u.id)),
            _ => None,
        })
        .collect::<HashMap<_, _>>();

    // TODO ideally this would use the same UpdateIter mechanism to make sure we don't
    //      accidentally miss variants
    let mut id_to_msg = updates
        .into_iter()
        .filter_map(|update| match update {
            tl::enums::Update::NewMessage(tl::types::UpdateNewMessage { message, .. }) => {
                Some(message)
            }
            tl::enums::Update::NewChannelMessage(tl::types::UpdateNewChannelMessage {
                message,
                ..
            }) => Some(message),
            tl::enums::Update::NewScheduledMessage(tl::types::UpdateNewScheduledMessage {
                message,
                ..
            }) => Some(message),
            _ => None,
        })
        .map(|message| Message::from_raw(client, message, Some(fetched_in), peers.handle()))
        .map(|message| (message.id(), message))
        .collect::<HashMap<_, _>>();

    Ok(random_ids
        .iter()
        .map(|rnd| {
            rnd_to_id
                .get(rnd)
                .and_then(|id| id_to_msg.remove(id))
                .or_else(|| {
                    if id_to_msg.len() == 1 {
                        // If there's no random_id to map from, in the common case a single message
                        // should've been produced regardless, so try to recover by returning that.
                        id_to_msg.drain().next().map(|(_, m)| m)
                    } else {
                        None
                    }
                })
        })
        .collect())
}

async fn send_message_peer(client: &Client, peer: PeerRef) -> Result<PeerRef, InvocationError> {
    if peer.id.bare_id().is_some() {
        return Ok(peer);
    }
    let unavailable = || {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "cached self-user information has an invalid identity",
        )
    };
    let (id, auth) = match client.0.session.peer(peer.id).await? {
        Some(PeerInfo::User { id, auth, .. }) => (PeerId::user(id).ok_or_else(unavailable)?, auth),
        _ => {
            let user = client.get_me().await?;
            (user.id(), user.auth())
        }
    };
    Ok(PeerRef {
        id,
        auth: auth.unwrap_or(peer.auth),
    })
}

/// The reply to a send which is not short. A plain function, so that its future does
/// not store the several kilobytes of `updates` as an argument.
fn message_from_full_updates(
    client: &Client,
    peer: PeerRef,
    random_id: i64,
    updates: tl::enums::Updates,
) -> impl Future<Output = Result<Message, InvocationError>> {
    let constructor = updates_constructor(&updates);
    let parts = sent_update_parts(updates);
    async move {
        let mapped = map_random_ids_to_messages(client, peer, &[random_id], parts)
            .await?
            .pop()
            .flatten();
        Ok(mapped.unwrap_or_else(|| {
            warn!(
                constructor,
                "failed to find just-sent message in response updates; please report this"
            );
            Message::from_raw(
                client,
                tl::enums::Message::Empty(tl::types::MessageEmpty {
                    id: 0,
                    peer_id: Some(peer.id.into()),
                }),
                Some(peer),
                client.empty_peer_map(),
            )
        }))
    }
}

// Logged instead of the updates themselves, which hold other people's messages.
fn updates_constructor(updates: &tl::enums::Updates) -> &'static str {
    match updates {
        tl::enums::Updates::TooLong => "updatesTooLong",
        tl::enums::Updates::UpdateShortMessage(_) => "updateShortMessage",
        tl::enums::Updates::UpdateShortChatMessage(_) => "updateShortChatMessage",
        tl::enums::Updates::UpdateShort(_) => "updateShort",
        tl::enums::Updates::Combined(_) => "updatesCombined",
        tl::enums::Updates::Updates(_) => "updates",
        tl::enums::Updates::UpdateShortSentMessage(_) => "updateShortSentMessage",
    }
}

pub(crate) async fn parse_mention_entities(
    client: &Client,
    mut entities: Vec<tl::enums::MessageEntity>,
) -> Option<Vec<tl::enums::MessageEntity>> {
    if entities.is_empty() {
        return None;
    }

    if entities
        .iter()
        .any(|e| matches!(e, tl::enums::MessageEntity::MentionName(_)))
    {
        for entity in entities.iter_mut() {
            if let tl::enums::MessageEntity::MentionName(mention_name) = entity {
                *entity = tl::types::InputMessageEntityMentionName {
                    offset: mention_name.offset,
                    length: mention_name.length,
                    user_id: tl::enums::InputUser::User(tl::types::InputUser {
                        user_id: mention_name.user_id,
                        access_hash: client
                            .0
                            .session
                            .peer(PeerId::user_unchecked(mention_name.user_id))
                            .await
                            .unwrap_or_default()
                            .and_then(|peer| peer.auth())
                            .unwrap_or_default()
                            .hash(),
                    }),
                }
                .into()
            }
        }
    }

    Some(entities)
}

const MAX_LIMIT: usize = 100;

/// Build a reply-to a plain message by its ID, leaving all other fields unset.
fn input_reply_to(reply_to_msg_id: i32) -> tl::enums::InputReplyTo {
    tl::types::InputReplyToMessage {
        reply_to_msg_id,
        top_msg_id: None,
        reply_to_peer_id: None,
        quote_text: None,
        quote_entities: None,
        quote_offset: None,
        monoforum_peer_id: None,
        todo_item_id: None,
        poll_option: None,
    }
    .into()
}

/// Flatten a `messages.Messages` response into its messages, users and chats.
///
/// Panics on `NotModified`, which is only returned when a non-zero `hash` was sent.
fn unpack_messages(
    res: tl::enums::messages::Messages,
) -> (
    Vec<tl::enums::Message>,
    Vec<tl::enums::User>,
    Vec<tl::enums::Chat>,
) {
    use tl::enums::messages::Messages;

    match res {
        Messages::Messages(m) => (m.messages, m.users, m.chats),
        Messages::Slice(m) => (m.messages, m.users, m.chats),
        Messages::ChannelMessages(m) => (m.messages, m.users, m.chats),
        Messages::NotModified(_) => {
            panic!("API returned Messages::NotModified even though GetMessages was used")
        }
    }
}

impl<R: tl::RemoteCall<Return = tl::enums::messages::Messages>> IterBuffer<R, Message> {
    /// Fetches the total unless cached.
    ///
    /// The `request.limit` should be set to the right value before calling this method.
    async fn get_total(&mut self) -> Result<usize, InvocationError> {
        if let Some(total) = self.total {
            return Ok(total);
        }

        use tl::enums::messages::Messages;

        let total = match self.client.invoke(&self.request).await? {
            Messages::Messages(messages) => messages.messages.len(),
            Messages::Slice(messages) => messages.count as usize,
            Messages::ChannelMessages(messages) => messages.count as usize,
            Messages::NotModified(messages) => messages.count as usize,
        };
        self.total = Some(total);
        Ok(total)
    }

    /// Performs the network call, fills the buffer, and returns the `offset_rate` if any.
    ///
    /// The `request.limit` should be set to the right value before calling this method.
    async fn fill_buffer(
        &mut self,
        limit: i32,
        reverse: bool,
        peer: Option<PeerRef>,
    ) -> Result<Option<i32>, InvocationError> {
        use tl::enums::messages::Messages;

        let (mut messages, users, chats, rate) = match self.client.invoke(&self.request).await? {
            Messages::Messages(m) => {
                self.last_chunk = true;
                self.total = Some(m.messages.len());
                (m.messages, m.users, m.chats, None)
            }
            Messages::Slice(m) => {
                // Can't rely on `count(messages) < limit` as the stop condition.
                // See https://t.me/tdlibchat/57257 for discussion. As an example:
                // offset_id 132002, limit 4 => we get msg 131999 & 131998
                // offset_id 132002, limit 3 => we get msg 131999
                // offset_id 132002, limit 2 => we get msg 132000
                // offset_id 132002, limit 1 => we get msg 132001
                //
                // When iterating newest-to-oldest, the highest fetched message ID is
                // lower than or equal to the limit, there can't be more messages after
                // (highest ID - limit), because the absolute lowest message ID is 1.
                self.last_chunk =
                    m.messages.is_empty() || (!reverse && m.messages[0].id() <= limit);

                self.total = Some(m.count as usize);

                (m.messages, m.users, m.chats, m.next_rate)
            }
            Messages::ChannelMessages(m) => {
                self.last_chunk =
                    m.messages.is_empty() || (!reverse && m.messages[0].id() <= limit);
                self.total = Some(m.count as usize);
                (m.messages, m.users, m.chats, None)
            }
            Messages::NotModified(_) => {
                panic!("API returned Messages::NotModified even though hash = 0")
            }
        };

        if reverse {
            messages.reverse();
        }

        let peers = self.client.build_peer_map(users, chats).await;

        let client = self.client.clone();
        self.buffer.extend(
            messages
                .into_iter()
                .map(|message| Message::from_raw(&client, message, peer, peers.handle())),
        );

        Ok(rate)
    }
}

/// Iterator returned by [`Client::iter_messages`].
pub struct MessageIter {
    inner: IterBuffer<tl::functions::messages::GetHistory, Message>,
    reverse: bool,
}

impl MessageIter {
    fn new(client: &Client, peer: PeerRef) -> Self {
        let inner = IterBuffer::from_request(
            client,
            MAX_LIMIT,
            tl::functions::messages::GetHistory {
                peer: peer.into(),
                offset_id: 0,
                offset_date: 0,
                add_offset: 0,
                limit: 0,
                max_id: 0,
                min_id: 0,
                hash: 0,
            },
        );

        Self {
            inner,
            reverse: false,
        }
    }

    /// Changes the starting message ID (exclusive).
    pub fn offset_id(mut self, offset: i32) -> Self {
        self.inner.request.offset_id = offset;
        self
    }

    /// Changes the starting message date.
    pub fn offset_date(mut self, offset: i32) -> Self {
        self.inner.request.offset_date = offset;
        self
    }

    /// Changes the order to oldest-to-newest. (default is newest-to-oldest)
    pub fn reverse(mut self, reverse: bool) -> Self {
        self.reverse = reverse;
        self
    }

    pub fn limit(mut self, limit: usize) -> Self {
        self.inner = self.inner.limit(limit);
        self
    }

    /// Determines the total number of messages in the chat.
    ///
    /// This only performs a network call if `next` has not been called before.
    pub async fn total(&mut self) -> Result<usize, InvocationError> {
        self.inner.request.limit = 1;
        self.inner.get_total().await
    }

    /// Returns the next `Message` from the internal buffer, filling the buffer previously if it's
    /// empty.
    ///
    /// Returns `None` if the `limit` is reached or there are no messages left.
    pub async fn next(&mut self) -> Result<Option<Message>, InvocationError> {
        if let Some(result) = self.inner.next_raw() {
            return result;
        }

        self.inner.request.limit = self.inner.determine_limit(MAX_LIMIT);
        let request = &mut self.inner.request;
        if self.reverse {
            request.add_offset = -request.limit;
            request.min_id = request.offset_id;
            if request.offset_id == 0 && request.offset_date == 0 {
                // With no explicit offset, start from the oldest available page.
                request.offset_id = 1;
                request.add_offset = -request.limit + 1; // +1 to include the first message (ID=1)
            }
        }
        self.inner
            .fill_buffer(
                self.inner.request.limit,
                self.reverse,
                Some(self.inner.request.peer.clone().into()),
            )
            .await?;

        // Don't bother updating offsets if this is the last time stuff has to be fetched.
        if !self.inner.last_chunk && !self.inner.buffer.is_empty() {
            let last = &self.inner.buffer[self.inner.buffer.len() - 1];
            self.inner.request.offset_id = last.id();
            self.inner.request.offset_date = last.date_timestamp();
        }

        Ok(self.inner.pop_item())
    }
}

/// Iterator returned by [`Client::search_messages`].
pub type SearchIter = IterBuffer<tl::functions::messages::Search, Message>;

impl SearchIter {
    fn new(client: &Client, peer: PeerRef) -> Self {
        // TODO let users tweak all the options from the request
        Self::from_request(
            client,
            MAX_LIMIT,
            tl::functions::messages::Search {
                peer: peer.into(),
                q: String::new(),
                from_id: None,
                saved_peer_id: None,
                saved_reaction: None,
                top_msg_id: None,
                filter: tl::enums::MessagesFilter::InputMessagesFilterEmpty,
                min_date: 0,
                max_date: 0,
                offset_id: 0,
                add_offset: 0,
                limit: 0,
                max_id: 0,
                min_id: 0,
                hash: 0,
            },
        )
    }

    /// Changes the message identifier upper bound.
    pub fn offset_id(mut self, offset: i32) -> Self {
        self.request.offset_id = offset;
        self
    }

    /// Changes the query of the search. Telegram servers perform a somewhat fuzzy search over
    /// this query (so a word in singular may also return messages with the word in plural, for
    /// example).
    pub fn query(mut self, query: &str) -> Self {
        self.request.q = query.to_string();
        self
    }

    /// Restricts results to messages sent by the logged-in user
    pub fn sent_by_self(mut self) -> Self {
        self.request.from_id = Some(InputPeer::PeerSelf);
        self
    }

    /// Returns only messages with date bigger than date_time.
    ///
    /// ```
    /// use jiff::Timestamp;
    ///
    /// # async fn f(peer: grammers_session::types::PeerRef, client: grammers_client::Client) -> Result<(), Box<dyn std::error::Error>> {
    /// // Search messages sent after Jan 1st, 2021
    /// let min_date: Timestamp = "2022-12-25T00:00:00-00:00".parse().unwrap();
    ///
    /// let mut messages = client.search_messages(peer).min_date(min_date);
    ///
    /// # Ok(())
    /// # }
    /// ```
    pub fn min_date(mut self, date_time: Timestamp) -> Self {
        self.request.min_date = date_time.as_second() as i32;
        self
    }

    /// Returns only messages with date smaller than date_time
    ///
    /// ```
    /// use jiff::Timestamp;
    ///
    /// # async fn f(peer: grammers_session::types::PeerRef, client: grammers_client::Client) -> Result<(), Box<dyn std::error::Error>> {
    /// // Search messages sent before Dec, 25th 2022
    /// let max_date: Timestamp = "2022-12-25T00:00:00-00:00".parse().unwrap();
    ///
    /// let mut messages = client.search_messages(peer).max_date(max_date);
    ///
    /// # Ok(())
    /// # }
    /// ```
    pub fn max_date(mut self, date_time: Timestamp) -> Self {
        self.request.max_date = date_time.as_second() as i32;
        self
    }

    /// Changes the media filter. Only messages with this type of media will be fetched.
    pub fn filter(mut self, filter: tl::enums::MessagesFilter) -> Self {
        self.request.filter = filter;
        self
    }

    /// Determines how many messages there are in total.
    ///
    /// This only performs a network call if `next` has not been called before.
    pub async fn total(&mut self) -> Result<usize, InvocationError> {
        // Unlike most requests, a limit of 0 actually returns 0 and not a default amount
        // (as of layer 120).
        self.request.limit = 0;
        self.get_total().await
    }

    /// Return the next `Message` from the internal buffer, filling the buffer previously if it's
    /// empty.
    ///
    /// Returns `None` if the `limit` is reached or there are no messages left.
    pub async fn next(&mut self) -> Result<Option<Message>, InvocationError> {
        if let Some(result) = self.next_raw() {
            return result;
        }

        self.request.limit = self.determine_limit(MAX_LIMIT);
        self.fill_buffer(
            self.request.limit,
            false,
            Some(self.request.peer.clone().into()),
        )
        .await?;

        // Don't bother updating offsets if this is the last time stuff has to be fetched.
        if !self.last_chunk && !self.buffer.is_empty() {
            let last = &self.buffer[self.buffer.len() - 1];
            self.request.offset_id = last.id();
            self.request.max_date = last.date_timestamp();
        }

        Ok(self.pop_item())
    }
}

/// Iterator returned by [`Client::search_all_messages`].
pub type GlobalSearchIter = IterBuffer<tl::functions::messages::SearchGlobal, Message>;

impl GlobalSearchIter {
    fn new(client: &Client) -> Self {
        // TODO let users tweak all the options from the request
        Self::from_request(
            client,
            MAX_LIMIT,
            tl::functions::messages::SearchGlobal {
                folder_id: None,
                q: String::new(),
                filter: tl::enums::MessagesFilter::InputMessagesFilterEmpty,
                min_date: 0,
                max_date: 0,
                offset_rate: 0,
                offset_peer: tl::enums::InputPeer::Empty,
                offset_id: 0,
                limit: 0,
                broadcasts_only: false,
                groups_only: false,
                users_only: false,
                community: None,
            },
        )
    }

    /// Changes the message identifier upper bound.
    pub fn offset_id(mut self, offset: i32) -> Self {
        self.request.offset_id = offset;
        self
    }

    /// Changes the query of the search. Telegram servers perform a somewhat fuzzy search over
    /// this query (so a word in singular may also return messages with the word in plural, for
    /// example).
    pub fn query(mut self, query: &str) -> Self {
        self.request.q = query.to_string();
        self
    }

    /// Changes the media filter. Only messages with this type of media will be fetched.
    pub fn filter(mut self, filter: tl::enums::MessagesFilter) -> Self {
        self.request.filter = filter;
        self
    }

    /// Determines how many messages there are in total.
    ///
    /// This only performs a network call if `next` has not been called before.
    pub async fn total(&mut self) -> Result<usize, InvocationError> {
        self.request.limit = 1;
        self.get_total().await
    }

    /// Return the next `Message` from the internal buffer, filling the buffer previously if it's
    /// empty.
    ///
    /// Returns `None` if the `limit` is reached or there are no messages left.
    pub async fn next(&mut self) -> Result<Option<Message>, InvocationError> {
        if let Some(result) = self.next_raw() {
            return result;
        }

        self.request.limit = self.determine_limit(MAX_LIMIT);
        let offset_rate = self.fill_buffer(self.request.limit, false, None).await?;

        // Don't bother updating offsets if this is the last time stuff has to be fetched.
        if !self.last_chunk && !self.buffer.is_empty() {
            let last = &self.buffer[self.buffer.len() - 1];
            self.request.offset_rate = offset_rate.unwrap_or(0);
            self.request.offset_peer = last
                .peer_ref()
                .await?
                .map(|peer| peer.into())
                .unwrap_or(tl::enums::InputPeer::Empty);
            self.request.offset_id = last.id();
        }

        Ok(self.pop_item())
    }
}

/// Method implementations related to sending, modifying or getting messages.
impl Client {
    /// Sends a message to the desired peer.
    ///
    /// This method can also be used to send media such as photos, videos, documents, polls, etc.
    ///
    /// If you want to send a local file as media, you will need to use
    /// [`Client::upload_file`] first.
    ///
    /// Refer to [`InputMessage`] to learn more formatting options, such as using markdown or
    /// adding buttons under your message (if you're logged in as a bot).
    ///
    /// See also: [`Message::respond`], [`Message::reply`].
    ///
    /// # Examples
    ///
    /// ```
    /// # async fn f(peer: grammers_session::types::PeerRef, client: grammers_client::Client) -> Result<(), Box<dyn std::error::Error>> {
    /// client.send_message(peer, "Boring text message :-(").await?;
    ///
    /// use grammers_client::message::InputMessage;
    ///
    /// client.send_message(peer, InputMessage::new().text("Sneaky message").silent(true)).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send_message<C: Into<PeerRef>, M: Into<InputMessage>>(
        &self,
        peer: C,
        message: M,
    ) -> Result<Message, InvocationError> {
        let peer = peer.into();
        // Resolve before any mutating RPC; retain InputPeerSelf on the wire.
        // Reply reconstruction must not fail on a post-delivery self lookup.
        let fetched_in = if peer.id.bare_id().is_some() {
            peer
        } else {
            Box::pin(send_message_peer(self, peer)).await?
        };
        let mut message: InputMessage = message.into();
        let random_id = generate_random_id();
        let entities = parse_mention_entities(self, message.entities.clone()).await;
        let updates = if let Some(media) = message.media.clone() {
            self.invoke(&tl::functions::messages::SendMedia {
                silent: message.silent,
                background: message.background,
                clear_draft: message.clear_draft,
                peer: peer.into(),
                reply_to: message.reply_to.map(input_reply_to),
                media,
                message: message.text.clone(),
                random_id,
                reply_markup: message.reply_markup.clone(),
                entities,
                schedule_date: message.schedule_date,
                schedule_repeat_period: None,
                send_as: None,
                noforwards: false,
                update_stickersets_order: false,
                invert_media: message.invert_media,
                quick_reply_shortcut: None,
                effect: None,
                allow_paid_floodskip: false,
                allow_paid_stars: None,
                suggested_post: None,
            })
            .await
        } else {
            // The text only comes back for a short reply, so lend it to the request.
            let request = tl::functions::messages::SendMessage {
                no_webpage: !message.link_preview,
                silent: message.silent,
                background: message.background,
                clear_draft: message.clear_draft,
                peer: peer.into(),
                reply_to: message.reply_to.map(input_reply_to),
                message: std::mem::take(&mut message.text),
                random_id,
                reply_markup: message.reply_markup.clone(),
                entities,
                schedule_date: message.schedule_date,
                schedule_repeat_period: None,
                send_as: None,
                noforwards: false,
                update_stickersets_order: false,
                invert_media: message.invert_media,
                quick_reply_shortcut: None,
                effect: None,
                allow_paid_floodskip: false,
                allow_paid_stars: None,
                suggested_post: None,
                rich_message: message.rich_message.clone(),
            };
            let result = self.invoke(&request).await;
            message.text = request.message;
            result
        }?;

        // A short reply needs nothing awaited; keep it free of the other path's state.
        match updates {
            tl::enums::Updates::UpdateShortSentMessage(updates) => Ok(
                Message::from_raw_short_updates(self, updates, message, fetched_in),
            ),
            updates => message_from_full_updates(self, fetched_in, random_id, updates).await,
        }
    }

    /// Send text or media using a struct of common optional flags.
    ///
    /// This delegates to [`Self::send_message`] after applying
    /// [`InputMessage::with_options`]. All six options replace any earlier values
    /// on the message, including `false` and `None`. Other message fields remain
    /// intact. Defaults retain existing behavior, including disabled link previews.
    /// There is no additional RPC or retry policy.
    ///
    /// ```
    /// # async fn example(client: &grammers_client::Client, peer: grammers_client::session::types::PeerRef) -> grammers_client::Result<()> {
    /// use grammers_client::SendOptions;
    /// client.send_message(peer, "Hello").await?;
    /// client.send_message_with_options(peer, "A quiet message", SendOptions {
    ///     silent: true,
    ///     ..Default::default()
    /// }).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send_message_with_options<C: Into<PeerRef>, M: Into<InputMessage>>(
        &self,
        peer: C,
        message: M,
        options: SendOptions,
    ) -> Result<Message, InvocationError> {
        self.send_message(peer, message.into().with_options(options))
            .await
    }

    /// Sends a album to the desired peer.
    ///
    /// This method can also be used to send a bunch of media such as photos, videos, documents, polls, etc.
    ///
    /// If you want to send a local file as media, you will need to use
    /// [`Client::upload_file`] first.
    ///
    /// Refer to [`InputMedia`] to learn more formatting options, such as using markdown.
    ///
    /// See also: [`Message::respond_album`], [`Message::reply_album`].
    ///
    /// # Examples
    ///
    /// ```
    /// # async fn f(peer: grammers_session::types::PeerRef, client: grammers_client::Client) -> Result<(), Box<dyn std::error::Error>> {
    /// use grammers_client::media::InputMedia;
    ///
    /// client.send_album(peer, vec![InputMedia::new().caption("A album").photo_url("https://example.com/cat.jpg")]).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send_album<C: Into<PeerRef>>(
        &self,
        peer: C,
        mut medias: Vec<InputMedia>,
    ) -> Result<Vec<Option<Message>>, InvocationError> {
        if medias.is_empty() || medias.iter().any(|media| media.media.is_none()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "album must contain media in every item",
            )
            .into());
        }
        let peer = peer.into();
        let random_ids = generate_random_ids(medias.len());

        // Upload external files
        for media in medias.iter_mut() {
            let raw_media = media.media.clone().unwrap();

            if matches!(
                raw_media,
                tl::enums::InputMedia::UploadedPhoto(_)
                    | tl::enums::InputMedia::PhotoExternal(_)
                    | tl::enums::InputMedia::UploadedDocument(_)
                    | tl::enums::InputMedia::DocumentExternal(_)
            ) {
                let uploaded = self.upload_media(peer, raw_media).await?;
                media.media = Some(
                    uploaded
                        .and_then(|media| media.to_raw_input_media())
                        .ok_or_else(|| {
                            std::io::Error::new(
                                std::io::ErrorKind::InvalidData,
                                "uploadMedia returned unsupported album media",
                            )
                        })?,
                );
            }
        }

        let first_media_reply = medias.first().unwrap().reply_to;

        let mut multi_media = Vec::with_capacity(medias.len());
        for (input_media, random_id) in medias.into_iter().zip(random_ids.iter()) {
            let entities = parse_mention_entities(self, input_media.entities).await;
            let raw_media = input_media.media.unwrap();
            multi_media.push(tl::enums::InputSingleMedia::Media(
                tl::types::InputSingleMedia {
                    media: raw_media,
                    random_id: *random_id,
                    message: input_media.caption,
                    entities,
                },
            ))
        }

        let updates = self
            .invoke(&tl::functions::messages::SendMultiMedia {
                silent: false,
                background: false,
                clear_draft: false,
                peer: peer.into(),
                reply_to: first_media_reply.map(input_reply_to),
                schedule_date: None,
                multi_media,
                send_as: None,
                noforwards: false,
                update_stickersets_order: false,
                invert_media: false,
                quick_reply_shortcut: None,
                effect: None,
                allow_paid_floodskip: false,
                allow_paid_stars: None,
            })
            .await?;

        Ok(map_random_ids_to_messages(self, peer, &random_ids, sent_update_parts(updates)).await?)
    }

    /// Uploads or resolves input media into reusable Telegram media without sending a message.
    ///
    /// This invokes `messages.uploadMedia`; local file bytes must first be uploaded with
    /// [`Client::upload_file`] or [`Client::upload_stream`]. Returns `None` for empty or
    /// unsupported media responses. RPC and deserialization errors are propagated.
    ///
    /// See also [`Client::send_album`], which rejects unsupported album media.
    pub async fn upload_media<P: Into<PeerRef>, M: Into<tl::enums::InputMedia>>(
        &self,
        peer: P,
        input_media: M,
    ) -> Result<Option<Media>, InvocationError> {
        let media = self
            .invoke(&tl::functions::messages::UploadMedia {
                business_connection_id: None,
                peer: peer.into().into(),
                media: input_media.into(),
            })
            .await?;
        Ok(Media::from_raw(media))
    }

    /// Edits an existing message.
    ///
    /// Similar to [`Client::send_message`], advanced formatting can be achieved with the
    /// options offered by [`InputMessage`].
    ///
    /// See also: [`Message::edit`].
    ///
    /// # Note
    ///
    /// An empty message text ("") is filtered out (treated as `None`), to avoid the `RpcError 400: MESSAGE_EMPTY`
    /// error. And makes it possible to edit only the reply markup (buttons) without changing the text.
    ///
    /// # Examples
    ///
    /// ```
    /// # async fn f(
    /// #     peer: grammers_session::types::PeerRef,
    /// #     client: grammers_client::Client,
    /// # ) -> Result<(), Box<dyn std::error::Error>> {
    /// let old_message_id = 123;
    /// client.edit_message(peer, old_message_id, "New text message").await?;
    ///
    /// use grammers_client::message::{Button, InputMessage, ReplyMarkup};
    /// // Add a button to the message without editing it's text.
    /// client
    ///     .edit_message(
    ///         peer,
    ///         old_message_id,
    ///         InputMessage::new().reply_markup(ReplyMarkup::from_buttons_row(&[
    ///             Button::url("url", "https://example.com")
    ///         ]))
    ///     )
    ///     .await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn edit_message<C: Into<PeerRef>, M: Into<InputMessage>>(
        &self,
        peer: C,
        message_id: i32,
        new_message: M,
    ) -> Result<(), InvocationError> {
        let new_message = new_message.into();
        let entities = parse_mention_entities(self, new_message.entities).await;
        self.invoke(&tl::functions::messages::EditMessage {
            no_webpage: !new_message.link_preview,
            invert_media: new_message.invert_media,
            peer: peer.into().into(),
            id: message_id,
            message: Some(new_message.text).filter(|text| !text.is_empty()),
            media: new_message.media,
            reply_markup: new_message.reply_markup,
            entities,
            schedule_date: new_message.schedule_date,
            schedule_repeat_period: None,
            quick_reply_shortcut_id: None,
            rich_message: None,
        })
        .await?;

        Ok(())
    }

    /// Deletes up to 100 messages in a peer.
    ///
    /// <div class="warning">
    ///
    /// When deleting messages from small group peers or private conversations, this
    /// method cannot validate that the provided message IDs actually belong to the input peer due
    /// to the way Telegram's API works. Make sure to pass correct [`Message::id`]'s.
    ///
    /// </div>
    ///
    /// The messages are deleted for both ends.
    ///
    /// The amount of deleted messages is returned (it might be less than the amount of input
    /// message IDs if some of them were already missing). It is not possible to find out which
    /// messages were actually deleted, but if the request succeeds, none of the specified message
    /// IDs will appear in the message history from that point on.
    ///
    /// See also: [`Message::delete`].
    ///
    /// # Examples
    ///
    /// ```
    /// # async fn f(peer: grammers_session::types::PeerRef, client: grammers_client::Client) -> Result<(), Box<dyn std::error::Error>> {
    /// let message_ids = [123, 456, 789];
    ///
    /// // Careful, these messages will be gone after the method succeeds!
    /// client.delete_messages(peer, &message_ids).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn delete_messages<C: Into<PeerRef>>(
        &self,
        peer: C,
        message_ids: &[i32],
    ) -> Result<usize, InvocationError> {
        let peer = peer.into();
        let tl::enums::messages::AffectedMessages::Messages(affected) =
            if peer.id.kind() == PeerKind::Channel {
                self.invoke(&tl::functions::channels::DeleteMessages {
                    channel: peer.into(),
                    id: message_ids.to_vec(),
                })
                .await
            } else {
                self.invoke(&tl::functions::messages::DeleteMessages {
                    revoke: true,
                    id: message_ids.to_vec(),
                })
                .await
            }?;

        Ok(affected.pts_count as usize)
    }

    /// Forwards up to 100 messages from `source` into `destination`.
    ///
    /// For consistency with other methods, the peer upon which this request acts comes first
    /// (destination), and then the source peer.
    ///
    /// Returns the new forwarded messages in a list. Those messages that could not be forwarded
    /// will be `None`. The length of the resulting list is the same as the length of the input
    /// message IDs, and the indices from the list of IDs map to the indices in the result so
    /// you can find which messages were forwarded and which message they became.
    ///
    /// See also: [`Message::forward_to`].
    ///
    /// # Examples
    ///
    /// ```
    /// # async fn f(destination: grammers_session::types::PeerRef, source: grammers_session::types::PeerRef, client: grammers_client::Client) -> Result<(), Box<dyn std::error::Error>> {
    /// let message_ids = [123, 456, 789];
    ///
    /// let messages = client.forward_messages(destination, &message_ids, source).await?;
    /// let fwd_count = messages.into_iter().filter(Option::is_some).count();
    /// println!("Forwarded {} out of {} messages!", fwd_count, message_ids.len());
    /// # Ok(())
    /// # }
    /// ```
    pub async fn forward_messages<C: Into<PeerRef>, S: Into<PeerRef>>(
        &self,
        destination: C,
        message_ids: &[i32],
        source: S,
    ) -> Result<Vec<Option<Message>>, InvocationError> {
        // TODO let user customize more options
        let peer = destination.into();
        let request = tl::functions::messages::ForwardMessages {
            silent: false,
            background: false,
            with_my_score: false,
            drop_author: false,
            drop_media_captions: false,
            from_peer: source.into().into(),
            id: message_ids.to_vec(),
            random_id: generate_random_ids(message_ids.len()),
            to_peer: peer.into(),
            top_msg_id: None,
            reply_to: None,
            schedule_date: None,
            schedule_repeat_period: None,
            send_as: None,
            noforwards: false,
            quick_reply_shortcut: None,
            allow_paid_floodskip: false,
            effect: None,
            video_timestamp: None,
            allow_paid_stars: None,
            suggested_post: None,
            from_ephemeral: false,
        };
        let result = self.invoke(&request).await?;
        Ok(
            map_random_ids_to_messages(self, peer, &request.random_id, sent_update_parts(result))
                .await?,
        )
    }

    /// Gets the [`Message`] to which the input message is replying to.
    ///
    /// See also: [`Message::get_reply`].
    ///
    /// # Examples
    ///
    /// ```
    /// # async fn f(message: grammers_client::message::Message, client: grammers_client::Client) -> Result<(), Box<dyn std::error::Error>> {
    /// if let Some(reply) = client.get_reply_to_message(&message).await? {
    ///     println!("The reply said: {}", reply.text());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn get_reply_to_message(
        &self,
        message: &Message,
    ) -> Result<Option<Message>, InvocationError> {
        /// Helper method to fetch a single message by its input message.
        async fn get_message(
            client: &Client,
            peer: PeerRef,
            id: tl::enums::InputMessage,
        ) -> Result<(tl::enums::messages::Messages, bool), InvocationError> {
            if peer.id.kind() == PeerKind::Channel {
                client
                    .invoke(&tl::functions::channels::GetMessages {
                        id: vec![id],
                        channel: peer.into(),
                    })
                    .await
                    .map(|res| (res, false))
            } else {
                client
                    .invoke(&tl::functions::messages::GetMessages { id: vec![id] })
                    .await
                    .map(|res| (res, true))
            }
        }

        // TODO shouldn't this method take in a message id anyway?
        let peer_id = message.peer_id();
        let peer = match peer_id.kind() {
            PeerKind::User | PeerKind::Chat => PeerRef {
                id: peer_id,
                auth: PeerAuth::default(), // unused, so no need to bother fetching it
            },
            PeerKind::Channel => message.peer_ref().await?.ok_or(InvocationError::Dropped(
                "could not obtain channel peer ref from message",
            ))?,
        };
        let reply_to_message_id = match message.reply_to_message_id() {
            Some(id) => id,
            None => return Ok(None),
        };

        let input_id =
            tl::enums::InputMessage::ReplyTo(tl::types::InputMessageReplyTo { id: message.id() });

        let (res, filter_req) = match get_message(self, peer, input_id).await {
            Ok(tup) => tup,
            Err(_) => {
                let input_id = tl::enums::InputMessage::Id(tl::types::InputMessageId {
                    id: reply_to_message_id,
                });
                get_message(self, peer, input_id).await?
            }
        };

        let (messages, users, chats) = unpack_messages(res);

        let peers = self.build_peer_map(users, chats).await;
        Ok(messages
            .into_iter()
            .map(|m| Message::from_raw(self, m, Some(peer), peers.handle()))
            .next()
            .filter(|m| !filter_req || m.peer_id() == message.peer_id()))
    }

    /// Iterate over the message history of a peer, from most recent to oldest.
    ///
    /// # Examples
    ///
    /// ```
    /// # async fn f(peer: grammers_session::types::PeerRef, client: grammers_client::Client) -> Result<(), Box<dyn std::error::Error>> {
    /// // Note we're setting a reasonable limit, or we'd print out ALL the messages in peer!
    /// let mut messages = client.iter_messages(peer).limit(100);
    ///
    /// while let Some(message) = messages.next().await? {
    ///     println!("{}", message.text());
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn iter_messages<C: Into<PeerRef>>(&self, peer: C) -> MessageIter {
        MessageIter::new(self, peer.into())
    }

    /// Iterate over the messages that match certain search criteria.
    ///
    /// This allows you to search by text within a peer or filter by media among other things.
    ///
    /// # Examples
    ///
    /// ```
    /// # async fn f(peer: grammers_session::types::PeerRef, client: grammers_client::Client) -> Result<(), Box<dyn std::error::Error>> {
    /// // Let's print all the people who think grammers is cool.
    /// let mut messages = client.search_messages(peer).query("grammers is cool");
    ///
    /// while let Some(message) = messages.next().await? {
    ///     let sender = message.sender().unwrap();
    ///     println!("{}", sender.name().unwrap_or(&sender.id().to_string()));
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn search_messages<C: Into<PeerRef>>(&self, peer: C) -> SearchIter {
        SearchIter::new(self, peer.into())
    }

    /// Iterate over the messages that match certain search criteria, without being restricted to
    /// searching in a specific peer. The downside is that this global search supports less filters.
    ///
    /// This allows you to search by text within a peer or filter by media among other things.
    ///
    /// # Examples
    ///
    /// ```
    /// # async fn f(client: grammers_client::Client) -> Result<(), Box<dyn std::error::Error>> {
    /// // Let's print all the peers were people think grammers is cool.
    /// let mut messages = client.search_all_messages().query("grammers is cool");
    ///
    /// while let Some(message) = messages.next().await? {
    ///     println!("{}", message.peer().unwrap().name().unwrap_or(&message.peer().unwrap().id().to_string()));
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn search_all_messages(&self) -> GlobalSearchIter {
        GlobalSearchIter::new(self)
    }

    /// Get up to 100 messages using their ID.
    ///
    /// Returns the new retrieved messages in a list. Those messages that could not be retrieved
    /// or do not belong to the input peer will be `None`. The length of the resulting list is the
    /// same as the length of the input message IDs, and the indices from the list of IDs map to
    /// the indices in the result so you can map them into the new list.
    ///
    /// # Examples
    ///
    /// ```
    /// # async fn f(peer: grammers_session::types::PeerRef, client: grammers_client::Client) -> Result<(), Box<dyn std::error::Error>> {
    /// let message_ids = [123, 456, 789];
    ///
    /// let messages = client.get_messages_by_id(peer, &message_ids).await?;
    /// let count = messages.into_iter().filter(Option::is_some).count();
    /// println!("{} out of {} messages were deleted!", message_ids.len() - count, message_ids.len());
    /// # Ok(())
    /// # }
    /// ```
    pub async fn get_messages_by_id<C: Into<PeerRef>>(
        &self,
        peer: C,
        message_ids: &[i32],
    ) -> Result<Vec<Option<Message>>, InvocationError> {
        let peer = peer.into();
        let id = message_ids
            .iter()
            .map(|&id| tl::enums::InputMessage::Id(tl::types::InputMessageId { id }))
            .collect();

        let result = if peer.id.kind() == PeerKind::Channel {
            self.invoke(&tl::functions::channels::GetMessages {
                channel: peer.into(),
                id,
            })
            .await
        } else {
            self.invoke(&tl::functions::messages::GetMessages { id })
                .await
        }?;

        let (messages, users, chats) = unpack_messages(result);

        let peers = self.build_peer_map(users, chats).await;
        let mut map = messages
            .into_iter()
            .map(|m| Message::from_raw(self, m, Some(peer), peers.handle()))
            .filter(|m| m.peer_id() == peer.id)
            .map(|m| (m.id(), m))
            .collect::<HashMap<_, _>>();

        Ok(message_ids.iter().map(|id| map.remove(id)).collect())
    }

    /// Get the latest pin from a peer.
    ///
    /// # Examples
    ///
    /// ```
    /// # async fn f(peer: grammers_session::types::PeerRef, client: grammers_client::Client) -> Result<(), Box<dyn std::error::Error>> {
    /// if let Some(message) = client.get_pinned_message(peer).await? {
    ///     println!("There is a message pinned: {}", message.text());
    /// } else {
    ///     println!("There are no messages pinned");
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn get_pinned_message<C: Into<PeerRef>>(
        &self,
        peer: C,
    ) -> Result<Option<Message>, InvocationError> {
        let peer = peer.into();
        // TODO return types::Message and print its text in the example
        let id = vec![tl::enums::InputMessage::Pinned];

        let result = if peer.id.kind() == PeerKind::Channel {
            self.invoke(&tl::functions::channels::GetMessages {
                channel: peer.into(),
                id,
            })
            .await
        } else {
            self.invoke(&tl::functions::messages::GetMessages { id })
                .await
        }?;

        let (messages, users, chats) = unpack_messages(result);

        let peers = self.build_peer_map(users, chats).await;
        Ok(messages
            .into_iter()
            .map(|m| Message::from_raw(self, m, Some(peer), peers.handle()))
            .find(|m| m.peer_id() == peer.id))
    }

    /// Pin a message in the peer. This will not notify any users.
    ///
    /// # Examples
    ///
    /// ```
    /// # async fn f(peer: grammers_session::types::PeerRef, client: grammers_client::Client) -> Result<(), Box<dyn std::error::Error>> {
    /// let message_id = 123;
    /// client.pin_message(peer, message_id).await?;
    /// # Ok(())
    /// # }
    /// ```
    // TODO return produced Option<service message>
    pub async fn pin_message<C: Into<PeerRef>>(
        &self,
        peer: C,
        message_id: i32,
    ) -> Result<(), InvocationError> {
        self.update_pinned(peer.into(), message_id, true).await
    }

    /// Unpin a message from the peer.
    ///
    /// # Examples
    ///
    /// ```
    /// # async fn f(peer: grammers_session::types::PeerRef, client: grammers_client::Client) -> Result<(), Box<dyn std::error::Error>> {
    /// let message_id = 123;
    /// client.unpin_message(peer, message_id).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn unpin_message<C: Into<PeerRef>>(
        &self,
        peer: C,
        message_id: i32,
    ) -> Result<(), InvocationError> {
        self.update_pinned(peer.into(), message_id, false).await
    }

    async fn update_pinned(
        &self,
        peer: PeerRef,
        id: i32,
        pin: bool,
    ) -> Result<(), InvocationError> {
        self.invoke(&tl::functions::messages::UpdatePinnedMessage {
            silent: true,
            unpin: !pin,
            pm_oneside: false,
            peer: peer.into(),
            id,
        })
        .await
        .map(drop)
    }

    /// Unpin all currently-pinned messages from the peer.
    ///
    /// # Examples
    ///
    /// ```
    /// # async fn f(peer: grammers_session::types::PeerRef, client: grammers_client::Client) -> Result<(), Box<dyn std::error::Error>> {
    /// client.unpin_all_messages(peer).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn unpin_all_messages<C: Into<PeerRef>>(
        &self,
        peer: C,
    ) -> Result<(), InvocationError> {
        self.invoke(&tl::functions::messages::UnpinAllMessages {
            peer: peer.into().into(),
            top_msg_id: None,
            saved_peer_id: None,
        })
        .await?;
        Ok(())
    }

    /// Send reaction.
    ///
    /// # Examples
    ///
    /// Via emoticon
    ///
    /// ```
    /// # async fn f(peer: grammers_session::types::PeerRef, client: grammers_client::Client) -> Result<(), Box<dyn std::error::Error>> {
    /// let message_id = 123;
    ///
    /// client.send_reactions(peer, message_id, "👍").await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Make animation big & Add to recent
    ///
    /// ```
    /// # async fn f(peer: grammers_session::types::PeerRef, client: grammers_client::Client) -> Result<(), Box<dyn std::error::Error>> {
    /// use grammers_client::message::InputReactions;
    ///
    /// let message_id = 123;
    /// let reactions = InputReactions::emoticon("🤯").big().add_to_recent();
    ///
    /// client.send_reactions(peer, message_id, reactions).await?;
    /// # Ok(())
    /// # }
    /// ```
    ///
    /// Remove reactions
    ///
    /// ```
    /// # async fn f(peer: grammers_session::types::PeerRef, client: grammers_client::Client) -> Result<(), Box<dyn std::error::Error>> {
    /// use grammers_client::message::InputReactions;
    ///
    /// let message_id = 123;
    ///
    /// client.send_reactions(peer, message_id, InputReactions::remove()).await?;
    /// # Ok(())
    /// # }
    /// ```
    pub async fn send_reactions<C: Into<PeerRef>, R: Into<InputReactions>>(
        &self,
        peer: C,
        message_id: i32,
        reactions: R,
    ) -> Result<(), InvocationError> {
        let reactions = reactions.into();

        self.invoke(&tl::functions::messages::SendReaction {
            big: reactions.big,
            add_to_recent: reactions.add_to_recent,
            peer: peer.into().into(),
            msg_id: message_id,
            reaction: Some(reactions.reactions),
        })
        .await?;

        Ok(())
    }
}

#[cfg(test)]
mod response_mapping_tests {
    use super::*;
    use grammers_mtsender::SenderPool;
    use grammers_session::Session;
    use grammers_session::storages::MemorySession;
    use std::sync::Arc;

    fn media_client(mode: u8, calls: Arc<std::sync::Mutex<Vec<u32>>>) -> Client {
        use tl::{Deserializable, Identifiable, Serializable};

        let pool = SenderPool::new(Arc::new(MemorySession::default()), 1);
        let mut client = Client::new(pool.handle);
        drop(pool.runner);
        Arc::get_mut(&mut client.0).unwrap().rpc_responder = Some(Arc::new(move |_, body| {
            let constructor = u32::from_le_bytes(body[..4].try_into().unwrap());
            calls.lock().unwrap().push(constructor);
            if constructor == tl::functions::messages::SendMultiMedia::CONSTRUCTOR_ID {
                assert_eq!(mode, 0, "invalid uploads must prevent album delivery");
                let request =
                    tl::functions::messages::SendMultiMedia::from_bytes(&body[4..]).unwrap();
                assert_eq!(request.multi_media.len(), 2);
                for item in request.multi_media {
                    let tl::enums::InputSingleMedia::Media(item) = item;
                    assert_eq!(item.message, "caption");
                    assert!(matches!(item.media, tl::enums::InputMedia::Photo(_)));
                }
                return Ok(tl::enums::Updates::TooLong.to_bytes());
            }
            assert_eq!(
                constructor,
                tl::functions::messages::UploadMedia::CONSTRUCTOR_ID
            );
            let request = tl::functions::messages::UploadMedia::from_bytes(&body[4..]).unwrap();
            assert_eq!(
                request.peer,
                PeerId::user(7).unwrap().to_ambient_ref().into()
            );
            assert!(request.business_connection_id.is_none());
            let tl::enums::InputMedia::PhotoExternal(input) = request.media else {
                panic!("external photo was not forwarded intact");
            };
            assert_eq!(input.url, "https://example.invalid/photo");
            let response: tl::enums::MessageMedia = match mode {
                0 => tl::types::MessageMediaPhoto {
                    spoiler: false,
                    live_photo: false,
                    photo: Some(
                        tl::types::Photo {
                            has_stickers: false,
                            id: 9,
                            access_hash: 77,
                            file_reference: vec![1, 2, 3],
                            date: 1,
                            sizes: Vec::new(),
                            video_sizes: None,
                            dc_id: 2,
                        }
                        .into(),
                    ),
                    ttl_seconds: Some(5),
                    video: None,
                }
                .into(),
                1 => tl::enums::MessageMedia::Empty,
                2 => tl::enums::MessageMedia::Unsupported,
                3 => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::PermissionDenied,
                        "upload rejected",
                    )
                    .into());
                }
                4 => return Ok(vec![0; 4]),
                _ => unreachable!(),
            };
            Ok(response.to_bytes())
        }));
        client
    }

    #[tokio::test]
    async fn upload_media_preserves_requests_responses_and_errors() {
        use tl::Identifiable;
        let peer = PeerId::user(7).unwrap().to_ambient_ref();
        for mode in 0..5 {
            let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
            let client = media_client(mode, Arc::clone(&calls));
            let result = client
                .upload_media(
                    peer,
                    tl::types::InputMediaPhotoExternal {
                        spoiler: false,
                        url: "https://example.invalid/photo".into(),
                        ttl_seconds: None,
                    },
                )
                .await;
            match mode {
                0 => {
                    let input = result.unwrap().unwrap().to_raw_input_media().unwrap();
                    let tl::enums::InputMedia::Photo(input) = input else {
                        panic!("missing photo")
                    };
                    let tl::enums::InputPhoto::Photo(photo) = input.id else {
                        panic!("missing photo identity")
                    };
                    assert_eq!((photo.id, photo.access_hash), (9, 77));
                    assert_eq!(photo.file_reference, [1, 2, 3]);
                    assert_eq!(input.ttl_seconds, Some(5));
                }
                1 | 2 => assert!(result.unwrap().is_none()),
                3 => assert!(matches!(result, Err(InvocationError::Io(e))
                    if e.kind() == std::io::ErrorKind::PermissionDenied)),
                4 => assert!(result.is_err()),
                _ => unreachable!(),
            }
            assert_eq!(
                *calls.lock().unwrap(),
                [tl::functions::messages::UploadMedia::CONSTRUCTOR_ID]
            );
        }
    }

    #[tokio::test]
    async fn albums_keep_cardinality_and_reject_failed_or_missing_media() {
        use tl::Identifiable;
        let peer = PeerId::user(7).unwrap().to_ambient_ref();
        for mode in 0..5 {
            let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
            let client = media_client(mode, Arc::clone(&calls));
            for invalid in [vec![], vec![InputMedia::new()]] {
                assert!(matches!(client.send_album(peer, invalid).await,
                    Err(InvocationError::Io(e)) if e.kind() == std::io::ErrorKind::InvalidInput));
            }
            assert!(calls.lock().unwrap().is_empty());
            let result = client
                .send_album(
                    peer,
                    (0..2)
                        .map(|_| {
                            InputMedia::new()
                                .caption("caption")
                                .photo_url("https://example.invalid/photo")
                        })
                        .collect(),
                )
                .await;
            if mode == 0 {
                let messages = result.unwrap();
                assert_eq!(messages.len(), 2);
                assert!(messages.iter().all(Option::is_none));
                assert_eq!(
                    *calls.lock().unwrap(),
                    [
                        tl::functions::messages::UploadMedia::CONSTRUCTOR_ID,
                        tl::functions::messages::UploadMedia::CONSTRUCTOR_ID,
                        tl::functions::messages::SendMultiMedia::CONSTRUCTOR_ID,
                    ]
                );
            } else {
                match mode {
                    1 | 2 => assert!(matches!(result, Err(InvocationError::Io(e))
                        if e.kind() == std::io::ErrorKind::InvalidData)),
                    3 => assert!(matches!(result, Err(InvocationError::Io(e))
                        if e.kind() == std::io::ErrorKind::PermissionDenied)),
                    4 => assert!(result.is_err()),
                    _ => unreachable!(),
                }
                assert_eq!(
                    *calls.lock().unwrap(),
                    [tl::functions::messages::UploadMedia::CONSTRUCTOR_ID]
                );
            }
        }
    }

    #[tokio::test]
    async fn unmappable_send_responses_preserve_input_cardinality() {
        let pool = SenderPool::new(Arc::new(MemorySession::default()), 1);
        let client = Client::new(pool.handle);
        let peer = PeerId::user(7).unwrap().to_ambient_ref();
        let replies = [
            tl::enums::Updates::TooLong,
            tl::types::UpdateShort {
                update: tl::enums::Update::Config,
                date: 1,
            }
            .into(),
            tl::types::UpdatesCombined {
                updates: Vec::new(),
                users: Vec::new(),
                chats: Vec::new(),
                date: 1,
                seq_start: 1,
                seq: 1,
            }
            .into(),
        ];
        for reply in replies {
            for ids in [&[][..], &[42][..], &[42, 43, 44][..]] {
                let mapped = map_random_ids_to_messages(
                    &client,
                    peer,
                    ids,
                    sent_update_parts(reply.clone()),
                )
                .await
                .unwrap();
                assert_eq!(mapped.len(), ids.len());
                assert!(mapped.iter().all(Option::is_none));
            }
        }
    }

    #[tokio::test]
    async fn self_send_resolves_identity_before_text_or_media_delivery() {
        use crate::client::ClientConfiguration;
        use tl::{Deserializable, Identifiable, Serializable};
        for cached in [false, true] {
            for hash in [None, Some(123)] {
                for media in [false, true] {
                    for short in [false, true] {
                        let session = Arc::new(MemorySession::default());
                        if cached {
                            session
                                .cache_peer(PeerInfo::User {
                                    id: 7,
                                    auth: hash.map(PeerAuth::from_hash),
                                    bot: Some(false),
                                    is_self: Some(true),
                                })
                                .await
                                .unwrap();
                        }
                        let pool = SenderPool::new(Arc::clone(&session), 1);
                        let mut client = Client::with_configuration(
                            pool.handle,
                            ClientConfiguration {
                                auto_cache_peers: false,
                            },
                        );
                        drop(pool.runner);
                        let calls = Arc::new(std::sync::Mutex::new(Vec::new()));
                        let recorded = Arc::clone(&calls);
                        let response: tl::enums::Updates = if short {
                            tl::types::UpdateShortSentMessage {
                                out: true,
                                id: 42,
                                pts: 1,
                                pts_count: 1,
                                date: 1,
                                media: None,
                                entities: None,
                                ttl_period: None,
                            }
                            .into()
                        } else {
                            tl::enums::Updates::TooLong
                        };
                        Arc::get_mut(&mut client.0).unwrap().rpc_responder =
                            Some(Arc::new(move |_, body| {
                                let constructor = u32::from_le_bytes(body[..4].try_into().unwrap());
                                recorded.lock().unwrap().push(constructor);
                                if constructor == tl::functions::users::GetUsers::CONSTRUCTOR_ID {
                                    assert!(!cached, "cached identity must avoid another fetch");
                                    let request =
                                        tl::functions::users::GetUsers::from_bytes(&body[4..])
                                            .unwrap();
                                    assert_eq!(request.id, vec![tl::enums::InputUser::UserSelf]);
                                    return Ok(vec![super::super::net::test_self_user(
                                        7, hash, false,
                                    )]
                                    .to_bytes());
                                }
                                if media {
                                    assert_eq!(
                                        constructor,
                                        tl::functions::messages::SendMedia::CONSTRUCTOR_ID
                                    );
                                    let request =
                                        tl::functions::messages::SendMedia::from_bytes(&body[4..])
                                            .unwrap();
                                    assert!(matches!(request.peer, tl::enums::InputPeer::PeerSelf));
                                    assert_eq!(request.message, "fixture");
                                } else {
                                    assert_eq!(
                                        constructor,
                                        tl::functions::messages::SendMessage::CONSTRUCTOR_ID
                                    );
                                    let request = tl::functions::messages::SendMessage::from_bytes(
                                        &body[4..],
                                    )
                                    .unwrap();
                                    assert!(matches!(request.peer, tl::enums::InputPeer::PeerSelf));
                                    assert_eq!(request.message, "fixture");
                                }
                                Ok(response.to_bytes())
                            }));
                        let mut input = InputMessage::new().text("fixture");
                        if media {
                            input = input.media(tl::enums::InputMedia::Empty);
                        }
                        let message = client
                            .send_message(PeerId::self_user().to_ambient_ref(), input)
                            .await
                            .unwrap();
                        assert_eq!(message.id(), if short { 42 } else { 0 });
                        assert_eq!(message.peer_id(), PeerId::user(7).unwrap());
                        let peer = message.peer_ref().await.unwrap().unwrap();
                        assert_eq!(peer.id, PeerId::user(7).unwrap());
                        assert_eq!(peer.auth.hash(), hash.unwrap_or(0));
                        let mut expected = Vec::new();
                        if !cached {
                            expected.push(tl::functions::users::GetUsers::CONSTRUCTOR_ID);
                        }
                        expected.push(if media {
                            tl::functions::messages::SendMedia::CONSTRUCTOR_ID
                        } else {
                            tl::functions::messages::SendMessage::CONSTRUCTOR_ID
                        });
                        assert_eq!(*calls.lock().unwrap(), expected);
                        assert!(session.peer(PeerId::self_user()).await.unwrap().is_some());
                    }
                }
            }
        }
    }

    #[tokio::test]
    async fn self_lookup_failure_prevents_text_and_media_send_requests() {
        use crate::client::ClientConfiguration;
        use tl::{Identifiable, Serializable};
        for media in [false, true] {
            for missing_response in [false, true] {
                let session = Arc::new(MemorySession::default());
                let pool = SenderPool::new(Arc::clone(&session), 1);
                let mut client = Client::with_configuration(
                    pool.handle,
                    ClientConfiguration {
                        auto_cache_peers: false,
                    },
                );
                drop(pool.runner);
                let calls = Arc::new(std::sync::atomic::AtomicUsize::new(0));
                let recorded = Arc::clone(&calls);
                let original = Arc::new(InvocationError::Io(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "self lookup failed before sending",
                )));
                let returned = Arc::clone(&original);
                Arc::get_mut(&mut client.0).unwrap().rpc_responder =
                    Some(Arc::new(move |_, body| {
                        recorded.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        assert_eq!(
                            u32::from_le_bytes(body[..4].try_into().unwrap()),
                            tl::functions::users::GetUsers::CONSTRUCTOR_ID,
                            "no mutating request after failed self lookup"
                        );
                        if missing_response {
                            Ok(Vec::<tl::enums::User>::new().to_bytes())
                        } else {
                            Err(InvocationError::Shared(Arc::clone(&returned)))
                        }
                    }));
                let mut input = InputMessage::new().text("fixture");
                if media {
                    input = input.media(tl::enums::InputMedia::Empty);
                }
                let error = client
                    .send_message(PeerId::self_user().to_ambient_ref(), input)
                    .await
                    .unwrap_err();
                if missing_response {
                    assert!(
                        matches!(error, InvocationError::Io(error) if error.kind() == std::io::ErrorKind::InvalidData)
                    );
                } else {
                    let InvocationError::Shared(actual) = error else {
                        panic!("original lookup failure must survive");
                    };
                    assert!(Arc::ptr_eq(&actual, &original));
                }
                assert_eq!(calls.load(std::sync::atomic::Ordering::Relaxed), 1);
                assert!(session.peer(PeerId::self_user()).await.unwrap().is_none());
            }
        }
    }
}
