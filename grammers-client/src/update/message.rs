// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::ops::{Deref, DerefMut};
use std::sync::OnceLock;

use grammers_session::updates::State;
use grammers_tl_types as tl;

/// Which of the four updates carrying a message one arrived in.
///
/// With the message and its position this is all it takes to put that update back
/// together, so the update is not kept beside the message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Kind {
    /// `updateNewMessage`.
    New,
    /// `updateNewChannelMessage`.
    NewChannel,
    /// `updateEditMessage`.
    Edit,
    /// `updateEditChannelMessage`.
    EditChannel,
}

impl Kind {
    /// The raw update of this kind carrying `message` at the given position.
    pub(crate) fn update(
        self,
        message: tl::enums::Message,
        pts: i32,
        pts_count: i32,
    ) -> tl::enums::Update {
        match self {
            Self::New => tl::types::UpdateNewMessage {
                message,
                pts,
                pts_count,
            }
            .into(),
            Self::NewChannel => tl::types::UpdateNewChannelMessage {
                message,
                pts,
                pts_count,
            }
            .into(),
            Self::Edit => tl::types::UpdateEditMessage {
                message,
                pts,
                pts_count,
            }
            .into(),
            Self::EditChannel => tl::types::UpdateEditChannelMessage {
                message,
                pts,
                pts_count,
            }
            .into(),
        }
    }
}

/// Update that all receive whenever a message is received or edited.
///
/// For bots to receive this update, they must either be an administrator
/// or have disabled the privacy mode via [@BotFather](https://t.me/BotFather).
///
/// The message is held once and reached through [`Deref`]. The raw update it arrived in
/// is not kept beside it: [`Message::raw`] and [`Message::into_raw`] put it back together.
#[derive(Debug, Clone)]
pub struct Message {
    pub(crate) msg: crate::message::Message,
    kind: Kind,
    pts: i32,
    pts_count: i32,
    raw: OnceLock<Box<tl::enums::Update>>,
    pub state: State,
}

impl Message {
    pub(crate) fn new(
        msg: crate::message::Message,
        kind: Kind,
        pts: i32,
        pts_count: i32,
        state: State,
    ) -> Self {
        Self {
            msg,
            kind,
            pts,
            pts_count,
            raw: OnceLock::new(),
            state,
        }
    }

    /// The update as Telegram sent it.
    ///
    /// It is built on first use, by cloning the message, and kept from then on: callers
    /// that never ask do not pay for it.
    ///
    /// It reflects the message as it was at that first call. Changes made later through
    /// [`DerefMut`] are not copied into a value that was already built, and a clone of
    /// this update carries the value built so far.
    /// [`Message::into_raw`] always takes the message as it is, and does not clone it.
    pub fn raw(&self) -> &tl::enums::Update {
        self.raw.get_or_init(|| {
            let message = self.msg.raw.clone();
            Box::new(self.kind.update(message, self.pts, self.pts_count))
        })
    }

    /// Consume this update and return the raw update, without cloning the message.
    ///
    /// It is always put together from the message as it is now, whether or not
    /// [`Message::raw`] was called before.
    pub fn into_raw(self) -> tl::enums::Update {
        self.kind.update(self.msg.raw, self.pts, self.pts_count)
    }

    pub fn into_inner(self) -> crate::message::Message {
        self.msg
    }

    /// Whether [`Message::raw`] has built its value.
    #[cfg(test)]
    pub(crate) fn raw_is_built(&self) -> bool {
        self.raw.get().is_some()
    }
}

impl Deref for Message {
    type Target = crate::message::Message;

    fn deref(&self) -> &Self::Target {
        &self.msg
    }
}

impl DerefMut for Message {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.msg
    }
}
