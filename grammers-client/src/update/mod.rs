// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Update types about events received from Telegram.
//!
//! Properties containing raw types are public and will either be called "raw" or prefixed with "raw_".\
//! Keep in mind that **these fields are not part of the semantic versioning guarantees**.
//!
//! [`Message`] holds its message once, so its raw update is not a field: [`Message::raw`]
//! and [`Message::into_raw`] put it back together on request. Written on a [`Message`],
//! `.raw` is the field of the message it wraps, a raw message and not a raw update.

mod callback_query;
mod guestchat_query;
mod inline_query;
mod inline_send;
mod message;
mod message_deletion;
#[path = "update.rs"]
mod model;
mod raw;

pub use callback_query::CallbackQuery;
pub use guestchat_query::GuestChatQuery;
pub use inline_query::{Article, InlineQuery};
pub use inline_send::InlineSend;
pub use message::Message;
pub use message_deletion::MessageDeletion;
pub use model::Update;
pub use raw::Raw;
