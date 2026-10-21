// Copyright 2026 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

/// Common optional message flags for [`super::InputMessage::with_options`] and
/// [`crate::Client::send_message_with_options`].
///
/// Defaults match [`super::InputMessage::default`]: all booleans are `false`,
/// including `link_preview`, and `reply_to` is `None`. Applying options replaces
/// these six fields even when the value is `false` or `None`; it preserves text,
/// formatting entities, media, reply markup, rich content, and scheduling.
///
/// ```
/// use grammers_client::SendOptions;
/// let options = SendOptions { silent: true, ..Default::default() };
/// assert!(options.silent);
/// assert!(!options.link_preview);
/// ```
#[derive(Clone, Debug, Default)]
pub struct SendOptions {
    /// Send without a notification sound; defaults to `false`.
    pub silent: bool,
    /// Send the message in the background; defaults to `false`.
    pub background: bool,
    /// Clear the peer's draft when sending; defaults to `false`.
    pub clear_draft: bool,
    /// Request a link preview for text messages; defaults to `false`.
    /// This has no effect when sending media.
    pub link_preview: bool,
    /// Invert the media/link-preview placement; defaults to `false`.
    pub invert_media: bool,
    /// Message ID to reply to, or `None` for no reply.
    pub reply_to: Option<i32>,
}
