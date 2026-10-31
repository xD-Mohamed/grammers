// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;
use std::time::Duration;

use grammers_session::storages::ErasedSession;
use grammers_session::types::{PeerId, PeerInfo, UpdateState, UpdatesState};
use grammers_session::updates::{
    MessageBoxes, PrematureEndReason, State, UpdateAndPeers, UpdatesLike,
};
use grammers_tl_types::{self as tl, Deserializable};
use tokio::sync::mpsc;
use tokio::time::timeout_at;
use tracing::{info, trace, warn};

use crate::{InvocationError, SenderPoolHandle, UpdatesConfiguration};

// See https://core.telegram.org/method/updates.getChannelDifference.
const BOT_CHANNEL_DIFF_LIMIT: i32 = 100000;
const USER_CHANNEL_DIFF_LIMIT: i32 = 100;
const RECOVERY_RETRY_DELAY: Duration = Duration::from_secs(1);
// Consecutive unreadable replies before a backlog is abandoned so that its sequence
// is not wedged for good. Nothing else can make such a reply readable.
const MAX_UNREADABLE_DIFFERENCES: u8 = 30;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Authorization {
    Unknown,
    Inferred,
    Confirmed,
    // A rejected inference must not be rearmed by queued socket updates.
    Rejected,
}

impl Authorization {
    fn can_recover(self) -> bool {
        matches!(self, Self::Inferred | Self::Confirmed)
    }
}

fn proves_login(updates: &UpdatesLike) -> bool {
    fn account_update(update: &tl::enums::Update) -> bool {
        // These service pushes can precede login. See core.telegram.org/api/updates
        // and api/qr-login; peer lists alone are not proof either.
        !matches!(
            update,
            tl::enums::Update::LoginToken
                | tl::enums::Update::SentPhoneCode(_)
                | tl::enums::Update::DcOptions(_)
                | tl::enums::Update::Config
                | tl::enums::Update::LangPackTooLong(_)
                | tl::enums::Update::LangPack(_)
                | tl::enums::Update::ServiceNotification(_)
        )
    }
    match updates {
        UpdatesLike::ConnectionClosed | UpdatesLike::MalformedUpdates => false,
        UpdatesLike::Updates(updates) => match updates {
            tl::enums::Updates::UpdateShort(updates) => account_update(&updates.update),
            tl::enums::Updates::Updates(updates) => updates.updates.iter().any(account_update),
            tl::enums::Updates::Combined(updates) => updates.updates.iter().any(account_update),
            // TooLong is an account catch-up hint, including for imported sessions
            // without self metadata. Non-PTS account updates also qualify above.
            _ => true,
        },
        // These are results of account-authorized requests.
        _ => true,
    }
}

pub struct UpdatesReceiver {
    handle: SenderPoolHandle,
    session: Arc<ErasedSession>,
    message_box: MessageBoxes,
    updates: mpsc::Receiver<UpdatesLike>,
    buffer: VecDeque<UpdateAndPeers>,
    should_get_state: bool,
    home_failure_generation: u64,
    prefer_channel: bool,
    prefer_live: bool,
    authorization: Authorization,
    is_bot: Option<bool>,
    next_self_check: tokio::time::Instant,
    recovery_paused_until: tokio::time::Instant,
    // Keyed by channel; `None` is the account sequence.
    unreadable_differences: BTreeMap<Option<i64>, u8>,
}

async fn prepare_channel_difference(
    mut request: tl::functions::updates::GetChannelDifference,
    session: Arc<ErasedSession>,
    message_box: &mut MessageBoxes,
    is_bot: bool,
) -> Result<Option<tl::functions::updates::GetChannelDifference>, InvocationError> {
    let id = match &request.channel {
        tl::enums::InputChannel::Channel(channel) => PeerId::channel(channel.channel_id).ok_or(
            grammers_mtproto::mtp::DeserializeError::InvalidEnvelope {
                reason: "invalid channel update-state identifier",
            },
        )?,
        _ => unreachable!(),
    };

    if let Some(PeerInfo::Channel {
        id,
        auth: Some(auth),
        ..
    }) = session.peer(id).await?
    {
        message_box.channel_difference_accessible();
        request.channel = tl::enums::InputChannel::Channel(tl::types::InputChannel {
            channel_id: id,
            access_hash: auth.hash(),
        });
        request.limit = if is_bot {
            BOT_CHANNEL_DIFF_LIMIT
        } else {
            USER_CHANNEL_DIFF_LIMIT
        };
        trace!(?request, "requesting");
        Ok(Some(request))
    } else if is_bot && message_box.try_exhaust_channel_grace() {
        // The skeleton request carries a zero hash, which bots can use to address
        // channels they are allowed to read (TDLib's get_input_channel does this).
        // Try it before abandoning the backlog. A rejection takes the ordinary
        // channel-error paths and exhaustion persists, so this cannot loop quickly.
        warn!(
            channel_id = ?id,
            "missing channel hash after grace; trying the bot zero-hash form before discarding its backlog",
        );
        request.limit = BOT_CHANNEL_DIFF_LIMIT;
        Ok(Some(request))
    } else {
        warn!(
            channel_id = ?id,
            "cannot getChannelDifference as we're missing its hash",
        );
        message_box.defer_channel_difference(Duration::from_secs(1));
        Ok(None)
    }
}

impl UpdatesReceiver {
    pub async fn create(
        handle: SenderPoolHandle,
        session: Arc<ErasedSession>,
        updates: mpsc::Receiver<UpdatesLike>,
        configuration: UpdatesConfiguration,
    ) -> Result<Self, Box<dyn std::error::Error + Send + Sync>> {
        if !handle.updates_enabled() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "API updates are disabled by ConnectionParams.receive_updates",
            )
            .into());
        }
        let message_box = if configuration.catch_up {
            MessageBoxes::load(session.updates_state().await?)
        } else {
            // If the user doesn't want to bother with catching up on previous update, start with
            // pristine state instead.
            MessageBoxes::new()
        };

        Ok(UpdatesReceiver {
            handle,
            session,
            message_box,
            updates,
            buffer: VecDeque::new(),
            should_get_state: false,
            home_failure_generation: 0,
            prefer_channel: false,
            prefer_live: true,
            authorization: Authorization::Unknown,
            is_bot: None,
            next_self_check: tokio::time::Instant::now(),
            recovery_paused_until: tokio::time::Instant::now(),
            unreadable_differences: BTreeMap::new(),
        })
    }

    /// Pops an updates from the queue, waiting for an updates to arrive first if the queue is empty.
    pub async fn next(
        &mut self,
    ) -> Result<
        (
            Vec<(tl::enums::Update, State)>,
            Vec<tl::enums::User>,
            Vec<tl::enums::Chat>,
        ),
        InvocationError,
    > {
        let session = self.session.clone();

        loop {
            tokio::task::consume_budget().await;
            let generation = self.handle.home_failure_generation();
            if generation != self.home_failure_generation {
                self.process_socket_updates(UpdatesLike::ConnectionClosed)
                    .await;
                self.home_failure_generation = generation;
            }
            if let Some((updates, users, chats)) = self.message_box.take_released_updates() {
                self.extend_update_queue(updates, users, chats);
            }
            if let Some(update) = self.buffer.pop_front() {
                return Ok(update);
            }
            // Before consulting stored login metadata, let one queued socket
            // payload establish authorization. Once proven, recovery runs before
            // draining further packets so a busy pending key cannot starve it.
            if let Some(Ok(update)) =
                (!self.authorization.can_recover() && !self.should_get_state && self.prefer_live)
                    .then(|| self.updates.try_recv())
            {
                self.process_socket_updates(update).await;
                if let Some(update) = self.buffer.pop_front() {
                    return Ok(update);
                }
            }
            self.message_box.check_deadlines();
            let needs_authorization = !self.message_box.has_primary_state()
                || self.message_box.get_difference().is_some()
                || self.message_box.get_channel_difference().is_some();
            if !self.authorization.can_recover()
                && needs_authorization
                && tokio::time::Instant::now() >= self.next_self_check
            {
                self.next_self_check = tokio::time::Instant::now() + Duration::from_secs(1);
                if let Some(PeerInfo::User { bot, .. }) = session.peer(PeerId::self_user()).await? {
                    self.authorization = Authorization::Confirmed;
                    self.is_bot = Some(bot.unwrap_or(false));
                }
            }
            self.should_get_state |=
                self.authorization.can_recover() && !self.message_box.has_primary_state();
            // A failure can repeat until login, access or the schema change, and must
            // not turn a caller's retry loop into an RPC loop. Live updates still flow,
            // including the service pushes a re-login may be waiting for.
            let recovery_paused = tokio::time::Instant::now() < self.recovery_paused_until;
            if self.should_get_state && !recovery_paused {
                match self.invoke(&tl::functions::updates::GetState {}).await {
                    Ok(tl::enums::updates::State::State(state)) => {
                        // Validate before persisting or changing the in-memory
                        // state. A rejected reply must leave recovery armed.
                        if state.pts < 0 || state.qts < 0 || state.date < 0 || state.seq < 0 {
                            let error = grammers_mtproto::mtp::DeserializeError::InvalidEnvelope {
                                reason: "invalid account update state",
                            }
                            .into();
                            self.pause_recovery();
                            return Err(error);
                        }
                        let channels = self.message_box.session_state().channels;
                        if let Err(error) = session
                            .clone()
                            .set_update_state(UpdateState::All(UpdatesState {
                                pts: state.pts,
                                qts: state.qts,
                                date: state.date,
                                seq: state.seq,
                                channels,
                            }))
                            .await
                        {
                            self.pause_recovery();
                            return Err(error.into());
                        }
                        // Live updates may have established a provisional position
                        // while a paced GetState waited. This reply supersedes it.
                        self.message_box.discard_account_state();
                        self.message_box.set_state(state);
                        self.authorization = Authorization::Confirmed;
                        // Cancellation or storage failure must leave recovery armed.
                        self.should_get_state = false;
                    }
                    Err(error)
                        if self.authorization == Authorization::Inferred
                            && matches!(error.root_cause(), InvocationError::Rpc(rpc) if rpc.code == 401) =>
                    {
                        warn!(
                            rpc = "updates.getState",
                            %error,
                            "provisional login rejected; account recovery is paused until confirmed self metadata is cached (complete login or call Client::get_me successfully)"
                        );
                        // An account-shaped push is only provisional proof. During
                        // login/2FA, wait for cached self metadata instead of exposing
                        // a premature authorization error or retrying on every next().
                        self.authorization = Authorization::Rejected;
                        self.should_get_state = false;
                        self.next_self_check = tokio::time::Instant::now() + Duration::from_secs(1);
                        continue;
                    }
                    Err(error) => {
                        // Recovery failures must reach the caller. Otherwise a connection failing during GetState
                        // can advance the home generation and spin through
                        // repeated GetState attempts inside this same next() call.
                        self.should_get_state = true;
                        self.pause_recovery();
                        return Err(error);
                    }
                }
            }

            // Alternate ready live batches with recovery. This preference lives
            // in the receiver so returning a batch or cancelling next() cannot
            // reset it and let a busy, deliverable socket stream starve catch-up.
            let recovery_ready = self.authorization.can_recover()
                && !recovery_paused
                && (self.message_box.get_difference().is_some()
                    || self.message_box.get_channel_difference().is_some());
            if let Some(Ok(update)) =
                (self.prefer_live || !recovery_ready).then(|| self.updates.try_recv())
            {
                self.process_socket_updates(update).await;
            }
            let (deadline, get_diff, get_channel_diff, can_recover) = {
                if let Some(update) = self.buffer.pop_front() {
                    return Ok(update);
                }
                let deadline = self.message_box.check_deadlines();
                // Saved state alone is not evidence of a current login.
                let can_recover = self.authorization.can_recover();
                let may_recover = can_recover && !recovery_paused;
                (
                    deadline,
                    self.message_box.get_difference().filter(|_| may_recover),
                    match self
                        .message_box
                        .get_channel_difference()
                        .filter(|_| may_recover)
                    {
                        Some(gd) => {
                            // Resolve account kind once on recovery, never on ordinary
                            // live delivery. Commit the result after the await so a
                            // cancelled or failed lookup can be retried.
                            let is_bot = match self.is_bot {
                                Some(is_bot) => is_bot,
                                None => {
                                    let is_bot = if let Some(PeerInfo::User { bot, .. }) =
                                        session.peer(PeerId::self_user()).await?
                                    {
                                        self.authorization = Authorization::Confirmed;
                                        bot.unwrap_or(false)
                                    } else {
                                        false
                                    };
                                    self.is_bot = Some(is_bot);
                                    is_bot
                                }
                            };
                            match prepare_channel_difference(
                                gd,
                                session.clone(),
                                &mut self.message_box,
                                is_bot,
                            )
                            .await?
                            {
                                Some(request) => Some(request),
                                None => {
                                    self.prefer_live = true;
                                    continue;
                                }
                            }
                        }
                        None => None,
                    },
                    can_recover,
                )
            };

            if let Some(request) =
                get_diff.filter(|_| get_channel_diff.is_none() || !self.prefer_channel)
            {
                self.prefer_channel = true;
                let response = match self.invoke(&request).await {
                    Ok(response) => response,
                    Err(error) => return Err(self.account_recovery_failed(error)),
                };
                self.prefer_live = true;
                let (updates, users, chats) = match self.message_box.try_apply_difference(response)
                {
                    Ok(applied) => applied,
                    Err(_) => {
                        return Err(self.account_recovery_failed(
                            grammers_mtproto::mtp::DeserializeError::InvalidEnvelope {
                                reason: "invalid account difference or missing pending state",
                            }
                            .into(),
                        ));
                    }
                };
                self.unreadable_differences.remove(&None);
                self.extend_update_queue(updates, users, chats);
                continue;
            }

            if let Some(request) = get_channel_diff {
                self.prefer_channel = false;
                let maybe_response = self.invoke(&request).await;
                self.prefer_live = true;
                // Only consecutive unreadable replies count towards abandoning it.
                if let (Err(error), tl::enums::InputChannel::Channel(channel)) =
                    (&maybe_response, &request.channel)
                    && !matches!(error.root_cause(), InvocationError::Deserialize(_))
                {
                    self.unreadable_differences
                        .remove(&Some(channel.channel_id));
                }

                let response = match maybe_response {
                    Ok(r) => r,
                    Err(e)
                        if e.is("FLOOD_*")
                            || e.is("SLOWMODE_WAIT*")
                            || e.is("AUTH_KEY_*")
                            || e.is("SESSION_*")
                            || e.is("CONNECTION_*")
                            || e.is("API_ID_*")
                            || matches!(e.root_cause(), InvocationError::Rpc(rpc) if matches!(rpc.code, 401 | 420 | 429)) =>
                    {
                        self.pause_recovery();
                        return Err(e);
                    }
                    Err(e) if e.is("PERSISTENT_TIMESTAMP_OUTDATED") => {
                        // According to Telegram's docs:
                        // "Channel internal replication issues, try again later (treat this like an RPC_CALL_FAIL)."
                        // We can treat this as "empty difference" and not update the local pts.
                        // Then this same call will be retried when another gap is detected or timeout expires.
                        //
                        // Another option would be to literally treat this like an RPC_CALL_FAIL and retry after a few
                        // seconds, but if Telegram is having issues it's probably best to wait for it to send another
                        // update (hinting it may be okay now) and retry then.
                        //
                        // This is a bit hacky because MessageBox doesn't really have a way to "not update" the pts.
                        // Instead we manually extract the previously-known pts and use that.
                        warn!(
                            "Getting difference for channel updates caused PersistentTimestampOutdated; ending getting difference prematurely until server issues are resolved"
                        );
                        {
                            self.message_box
                                .end_channel_difference(PrematureEndReason::TemporaryServerIssues);
                        }
                        continue;
                    }
                    Err(e)
                        if [
                            "CHANNEL_INVALID",
                            "CHANNEL_PRIVATE",
                            "CHANNEL_PUBLIC_GROUP_NA",
                            "CHAT_WRITE_FORBIDDEN",
                            "FROZEN_PARTICIPANT_MISSING",
                            "PERSISTENT_TIMESTAMP_EMPTY",
                            "PERSISTENT_TIMESTAMP_INVALID",
                            "USER_BANNED_IN_CHANNEL",
                        ]
                        .iter()
                        .any(|name| e.is(name)) =>
                    {
                        info!(
                            ?request,
                            error = ?e.root_cause(),
                            "channel difference rejected; discarding this channel's saved position",
                        );
                        {
                            self.message_box
                                .end_channel_difference(PrematureEndReason::Banned);
                        }
                        continue;
                    }
                    // One channel's unreadable reply must not block its siblings.
                    Err(e) if matches!(e.root_cause(), InvocationError::Deserialize(_)) => {
                        return Err(self.channel_difference_unreadable(&request, e));
                    }
                    Err(e) if matches!(e.root_cause(), InvocationError::Rpc(rpc_error) if matches!(rpc_error.code, 400 | 403 | 406 | 500)) =>
                    {
                        warn!(error = ?e.root_cause(), "channel recovery deferred with its saved position intact");
                        {
                            self.message_box
                                .end_channel_difference(PrematureEndReason::TemporaryServerIssues);
                        }
                        continue;
                    }
                    Err(e) => {
                        self.pause_recovery();
                        return Err(e);
                    }
                };

                let (updates, users, chats) =
                    match self.message_box.try_apply_channel_difference(response) {
                        Ok(applied) => applied,
                        Err(_) => {
                            return Err(self.channel_difference_unreadable(
                                &request,
                                grammers_mtproto::mtp::DeserializeError::InvalidEnvelope {
                                    reason: "invalid channel difference or missing pending state",
                                }
                                .into(),
                            ));
                        }
                    };
                if let tl::enums::InputChannel::Channel(channel) = &request.channel {
                    self.unreadable_differences
                        .remove(&Some(channel.channel_id));
                }
                self.extend_update_queue(updates, users, chats);
                continue;
            }

            // A stream can be created before login. Session caching does not
            // signal this receiver, so periodically recheck while account state
            // or the cached self user is missing.
            let deadline = tokio::time::Instant::from_std(deadline);
            let deadline = if !can_recover && needs_authorization {
                self.next_self_check
            } else if recovery_paused {
                self.recovery_paused_until
            } else {
                deadline
            };
            let received = tokio::select! {
                result = timeout_at(deadline, self.updates.recv()) => result,
                _ = self.handle.wait_home_failure(self.home_failure_generation) => continue,
            };
            match received {
                Ok(Some(updates)) => self.process_socket_updates(updates).await,
                Ok(None) => break Err(InvocationError::Dropped("updates channel was closed")),
                Err(_) => {}
            }
        }
    }

    /// Synchronize the updates state to the session.
    ///
    /// This is **not** automatically done on drop.
    pub async fn sync_update_state(&self) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if self.message_box.is_empty() {
            return Ok(());
        }
        self.session
            .clone()
            .set_update_state(UpdateState::All(self.message_box.session_state()))
            .await?;
        Ok(())
    }

    fn pause_recovery(&mut self) {
        self.recovery_paused_until = tokio::time::Instant::now() + RECOVERY_RETRY_DELAY;
    }

    fn difference_unreadable(&mut self, sequence: Option<i64>) -> bool {
        let count = self.unreadable_differences.entry(sequence).or_insert(0);
        *count += 1;
        let abandon = *count >= MAX_UNREADABLE_DIFFERENCES;
        if abandon {
            self.unreadable_differences.remove(&sequence);
        }
        abandon
    }

    fn account_recovery_failed(&mut self, error: InvocationError) -> InvocationError {
        self.pause_recovery();
        if !matches!(error.root_cause(), InvocationError::Deserialize(_)) {
            // Only consecutive unreadable replies count towards abandoning it.
            self.unreadable_differences.remove(&None);
        } else if self.difference_unreadable(None) {
            warn!(%error, "abandoning account backlog after repeated unreadable differences; those updates are lost");
            self.message_box.discard_account_state();
        }
        error
    }

    fn channel_difference_unreadable(
        &mut self,
        request: &tl::functions::updates::GetChannelDifference,
        error: InvocationError,
    ) -> InvocationError {
        let channel = match &request.channel {
            tl::enums::InputChannel::Channel(channel) => Some(channel.channel_id),
            _ => None,
        };
        let abandon = self.difference_unreadable(channel);
        if abandon {
            warn!(%error, ?channel, "abandoning channel backlog after repeated unreadable differences; those updates are lost");
        } else {
            warn!(%error, ?channel, "channel recovery deferred with its saved position intact");
        }
        self.message_box.end_channel_difference(if abandon {
            PrematureEndReason::Banned
        } else {
            PrematureEndReason::TemporaryServerIssues
        });
        self.pause_recovery();
        error
    }

    async fn invoke<R: tl::RemoteCall>(&self, request: &R) -> Result<R::Return, InvocationError> {
        let dc_id = self.session.clone().home_dc_id()?;
        self.handle
            .do_invoke_in_dc(dc_id, request.to_bytes())
            .await
            .and_then(|body| R::Return::from_bytes(&body).map_err(|e| e.into()))
    }

    async fn process_socket_updates(&mut self, updates: UpdatesLike) {
        self.prefer_live = false;
        if self.authorization == Authorization::Unknown && proves_login(&updates) {
            self.authorization = Authorization::Inferred;
        }
        if let Ok((updates, users, chats)) = self.message_box.process_updates(updates) {
            self.extend_update_queue(updates, users, chats);
        }
    }

    fn extend_update_queue(
        &mut self,
        updates: Vec<(tl::enums::Update, State)>,
        users: Vec<tl::enums::User>,
        chats: Vec<tl::enums::Chat>,
    ) {
        if !updates.is_empty() || !users.is_empty() || !chats.is_empty() {
            self.buffer.push_back((updates, users, chats));
        }
    }
}
