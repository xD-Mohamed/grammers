// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! This module deals with correct handling of updates,
//! including gaps,
//! and knowing when the code should "get difference"
//! (the set of updates that the client should know by now
//! minus the set of updates that it actually knows).
//!
//! Each chat has its own [`LiveEntry`] in the [`MessageBoxes`]
//! (this `struct` is the "entry point").
//! At any given time,
//! the message box may be either getting difference for them
//! (entry has `recovery_pending` set, even while a retry is deferred)
//! or not.
//! If not getting difference,
//! a possible gap may be found for the updates
//! (entry is in [`MessageBoxes::possible_gaps`]).
//! Otherwise, the entry is on its happy path.
//!
//! Gaps are cleared when they are either
//! resolved on their own
//! (by waiting for a short time)
//! or because we got the difference for the corresponding entry.
//!
//! While there are entries whose difference is ready to be fetched,
//! [`MessageBoxes::check_deadlines`] will always return [`Instant::now`],
//! since "now" is the time to get the difference.

mod adaptor;
mod defs;
#[cfg(test)]
mod tests;

use std::cmp::Ordering;
use std::collections::{BTreeSet, VecDeque};
use std::time::Duration;
#[cfg(not(test))]
use std::time::Instant;

use defs::Key;
pub use defs::{Gap, MessageBox, MessageBoxes, State, UpdateAndPeers, UpdatesLike};
use defs::{LiveEntry, NO_DATE, NO_PTS, NO_SEQ, POSSIBLE_GAP_TIMEOUT, PossibleGap, PtsInfo};
use grammers_tl_types as tl;
#[cfg(test)]
use tests::Instant;
use tracing::{debug, info, trace, warn};

use crate::types::{ChannelState, PeerId, UpdatesState};

const MAX_CHANNEL_DEFERRALS: u8 = 30;

fn next_updates_deadline() -> Instant {
    Instant::now() + defs::NO_UPDATES_TIMEOUT
}

fn update_sort_key(update: &tl::enums::Update) -> i32 {
    match PtsInfo::from_update(update) {
        Some(info) => info.pts.saturating_sub(info.count),
        None => NO_PTS,
    }
}

fn retain_valid_updates(updates: &mut Vec<tl::enums::Update>) {
    updates.retain(|update| {
        let valid = PtsInfo::valid_update(update);
        if !valid {
            warn!("ignoring malformed update within difference result");
        }
        valid
    });
}

impl MessageBox {
    pub fn pts(&self) -> i32 {
        match self {
            MessageBox::Common { pts } => *pts,
            MessageBox::Secondary { qts } => *qts,
            MessageBox::Channel { pts, .. } => *pts,
        }
    }
}

impl From<PtsInfo> for MessageBox {
    fn from(value: PtsInfo) -> Self {
        match value.key {
            Key::Common => Self::Common { pts: value.pts },
            Key::Secondary => Self::Secondary { qts: value.pts },
            Key::Channel(channel_id) => Self::Channel {
                channel_id,
                pts: value.pts,
            },
        }
    }
}

impl LiveEntry {
    fn effective_deadline(&self) -> Instant {
        match &self.possible_gap {
            Some(gap) => gap.deadline.min(self.deadline),
            None => self.deadline,
        }
    }
}

#[allow(clippy::new_without_default)]
/// Creation, querying, and setting base state.
impl MessageBoxes {
    /// Create a new, empty [`MessageBoxes`].
    pub fn new() -> Self {
        trace!("created new message box with no previous state");
        Self {
            entries: Vec::new(),
            date: NO_DATE,
            seq: NO_SEQ,
            getting_diff_for: VecDeque::new(),
            ready_diff_count: 0,
            exhausted_channels: BTreeSet::new(),
            next_deadline: next_updates_deadline(),
        }
    }

    /// Restore saved update state, repairing unusable counters with warnings.
    ///
    /// Negative counters become zero, invalid channel rows are discarded, and
    /// duplicate channels retain their oldest position. A QTS without primary
    /// PTS is discarded together with account date/sequence state so the caller
    /// can obtain fresh account state. Valid channels and counters are retained.
    /// All retained positions start with recovery pending.
    pub fn load(mut state: UpdatesState) -> Self {
        if state.pts < NO_PTS || state.qts < NO_PTS || state.date < NO_DATE || state.seq < NO_SEQ {
            warn!("repairing invalid saved account update counters");
            state.pts = state.pts.max(NO_PTS);
            state.qts = state.qts.max(NO_PTS);
            state.date = state.date.max(NO_DATE);
            state.seq = state.seq.max(NO_SEQ);
        }
        state.channels.retain(|channel| {
            let valid = PeerId::channel(channel.id).is_some() && channel.pts >= NO_PTS;
            if !valid {
                warn!("discarding invalid saved channel update state");
            }
            valid
        });
        if state.pts == NO_PTS && state.qts > NO_PTS {
            warn!("discarding saved QTS without primary state");
            // A QTS alone cannot form a valid getDifference request. Relearn
            // account state instead of creating a fictitious common PTS of 0.
            state.qts = NO_PTS;
            state.seq = NO_SEQ;
            state.date = NO_DATE;
        }
        trace!(?state, "created new message box with state");
        let mut entries = Vec::with_capacity(2 + state.channels.len());
        let mut getting_diff_for = VecDeque::with_capacity(2 + state.channels.len());
        let deadline = next_updates_deadline();

        if state.pts > NO_PTS {
            entries.push(LiveEntry {
                key: Key::Common,
                pts: state.pts.max(NO_PTS),
                deadline,
                recovery_deferred: false,
                recovery_pending: true,
                recovery_deferrals: 0,
                possible_gap: None,
            });
        }
        if state.qts > NO_PTS {
            entries.push(LiveEntry {
                key: Key::Secondary,
                pts: state.qts,
                deadline,
                recovery_deferred: false,
                recovery_pending: true,
                recovery_deferrals: 0,
                possible_gap: None,
            });
        }
        entries.extend(
            state
                .channels
                .iter()
                .filter(|c| c.id > 0 && c.pts > NO_PTS)
                .map(|c| LiveEntry {
                    key: Key::Channel(c.id),
                    pts: c.pts,
                    deadline,
                    recovery_deferred: false,
                    recovery_pending: true,
                    recovery_deferrals: 0,
                    possible_gap: None,
                }),
        );
        entries.sort_by_key(|entry| entry.key);
        // Repeated saved channel IDs violate binary-search and catch-up
        // invariants. Keep the older position so recovery cannot skip updates.
        entries.dedup_by(|later, earlier| {
            if later.key == earlier.key {
                warn!("merging duplicate saved channel state at the older position");
                earlier.pts = earlier.pts.min(later.pts);
                true
            } else {
                false
            }
        });

        getting_diff_for.extend(entries.iter().map(|entry| entry.key));

        Self {
            ready_diff_count: getting_diff_for.len(),
            entries,
            date: state.date,
            seq: state.seq,
            getting_diff_for,
            exhausted_channels: BTreeSet::new(),
            next_deadline: deadline,
        }
    }

    /// Reject negative counters or invalid channel identifiers before loading.
    ///
    /// Valid input still receives the normalization documented by [`Self::load`]:
    /// duplicate channels keep their oldest position, and QTS without primary
    /// PTS is cleared. Use [`Self::load`] to repair rather than reject invalid rows.
    pub fn try_load(state: UpdatesState) -> Result<Self, Gap> {
        if state.pts < 0
            || state.qts < 0
            || state.seq < 0
            || state.date < 0
            || state
                .channels
                .iter()
                .any(|channel| PeerId::channel(channel.id).is_none() || channel.pts < 0)
        {
            return Err(Gap);
        }
        Ok(Self::load(state))
    }

    fn entry(&self, key: Key) -> Option<&LiveEntry> {
        self.entries
            .binary_search_by_key(&key, |entry| entry.key)
            .map(|i| &self.entries[i])
            .ok()
    }

    fn is_getting_diff(&self, key: Key) -> bool {
        self.entry(key).is_some_and(|entry| entry.recovery_pending)
    }

    fn update_entry(&mut self, key: Key, updater: impl FnOnce(&mut LiveEntry)) -> bool {
        match self.entries.binary_search_by_key(&key, |entry| entry.key) {
            Ok(i) => {
                updater(&mut self.entries[i]);
                true
            }
            Err(_) => false,
        }
    }

    fn force_update_entry(&mut self, mut entry: LiveEntry, updater: impl FnOnce(&mut LiveEntry)) {
        match self
            .entries
            .binary_search_by_key(&entry.key, |entry| entry.key)
        {
            Ok(i) => updater(&mut self.entries[i]),
            Err(i) => {
                updater(&mut entry);
                self.entries.insert(i, entry);
            }
        }
    }

    fn set_entry(&mut self, entry: LiveEntry) {
        match self
            .entries
            .binary_search_by_key(&entry.key, |entry| entry.key)
        {
            Ok(i) => self.entries[i] = entry,
            Err(i) => self.entries.insert(i, entry),
        }
    }

    fn set_pts(&mut self, key: Key, pts: i32) {
        if !self.update_entry(key, |entry| entry.pts = pts) {
            self.set_entry(LiveEntry {
                key,
                pts,
                deadline: next_updates_deadline(),
                recovery_deferred: false,
                recovery_pending: false,
                recovery_deferrals: 0,
                possible_gap: None,
            });
        }
    }

    fn pop_entry(&mut self, key: Key) -> Option<LiveEntry> {
        match self.entries.binary_search_by_key(&key, |entry| entry.key) {
            Ok(i) => Some(self.entries.remove(i)),
            Err(_) => None,
        }
    }

    /// Return the current state in a format that sessions understand.
    ///
    /// This should be used for persisting the state.
    pub fn session_state(&self) -> UpdatesState {
        UpdatesState {
            pts: self.entry(Key::Common).map(|s| s.pts).unwrap_or(NO_PTS),
            qts: self.entry(Key::Secondary).map(|s| s.pts).unwrap_or(NO_PTS),
            date: self.date,
            seq: self.seq,
            channels: self
                .entries
                .iter()
                .filter_map(|entry| match entry.key {
                    Key::Channel(id) => Some(ChannelState { id, pts: entry.pts }),
                    _ => None,
                })
                .collect(),
        }
    }

    /// Return true if the message box is empty and has no state yet.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether account state is known, independently of saved channel state.
    pub fn has_primary_state(&self) -> bool {
        self.entry(Key::Common).is_some()
    }

    /// Return the next deadline when receiving updates should timeout.
    ///
    /// If a deadline expired, the corresponding entries will be marked as needing to get its difference.
    /// Ready differences return an expired deadline. Deferred recovery retains
    /// its pending state but wakes at its retry deadline.
    pub fn check_deadlines(&mut self) -> Instant {
        let now = Instant::now();
        let ready_deadline = self.next_deadline.min(now);

        if now < self.next_deadline {
            return if self.ready_diff_count != 0 {
                ready_deadline
            } else {
                self.next_deadline
            };
        }

        if now >= self.next_deadline {
            for entry in &mut self.entries {
                if entry.recovery_deferred && now >= entry.deadline {
                    entry.recovery_deferred = false;
                    self.ready_diff_count += 1;
                    self.getting_diff_for.push_back(entry.key);
                }
                if now >= entry.effective_deadline() && !entry.recovery_pending {
                    debug!(?entry, "deadline for forcibly fetching updates met");
                    entry.possible_gap = None;
                    entry.recovery_deferred = false;
                    entry.recovery_pending = true;
                    entry.recovery_deferrals = 0;
                    self.ready_diff_count += 1;
                    self.getting_diff_for.push_back(entry.key);
                }
            }
        }

        self.next_deadline = self
            .entries
            .iter()
            .filter(|entry| !entry.recovery_pending || entry.recovery_deferred)
            .map(LiveEntry::effective_deadline)
            .min()
            .unwrap_or_else(next_updates_deadline);

        if self.ready_diff_count != 0 {
            return ready_deadline;
        }

        self.next_deadline
    }

    /// Reset the deadline of an existing entry.
    /// Does nothing if the entry doesn't exist.
    ///
    /// If the updated deadline was the currently-next-deadline,
    /// the new closest deadline is calculated and stored as the next.
    fn reset_deadline(&mut self, key: Key, deadline: Instant) {
        let mut old_deadline = self.next_deadline;
        self.update_entry(key, |entry| {
            old_deadline = entry.deadline;
            entry.deadline = deadline;
        });
        if self.next_deadline == old_deadline {
            self.next_deadline = self
                .entries
                .iter()
                .fold(deadline, |d, entry| d.min(entry.effective_deadline()));
        } else {
            // Pending entries may be excluded from the cached deadline. A
            // shorter timeout must still wake the receiver after recovery ends.
            self.next_deadline = self.next_deadline.min(deadline);
        }
    }

    fn reset_timeout(&mut self, key: Key, timeout: Option<i32>) {
        self.reset_deadline(
            key,
            timeout
                .filter(|t| *t >= 0)
                .map(|t| Instant::now() + Duration::from_secs(t as _))
                .unwrap_or_else(next_updates_deadline),
        );
    }

    /// Sets the update state.
    ///
    /// Should be called right after login if [`MessageBoxes::new`] was used, otherwise undesirable
    /// updates will be fetched.
    ///
    /// Should only be called while account state is missing. Saved channel state is preserved.
    pub fn set_state(&mut self, state: tl::types::updates::State) {
        trace!(?state, "setting state");
        debug_assert!(!self.has_primary_state());
        let deadline = next_updates_deadline();
        self.set_entry(LiveEntry {
            key: Key::Common,
            pts: state.pts,
            deadline,
            recovery_deferred: false,
            recovery_pending: false,
            recovery_deferrals: 0,
            possible_gap: None,
        });
        self.set_entry(LiveEntry {
            key: Key::Secondary,
            pts: state.qts,
            deadline,
            recovery_deferred: false,
            recovery_pending: false,
            recovery_deferrals: 0,
            possible_gap: None,
        });
        self.date = state.date;
        self.seq = state.seq;
        self.next_deadline = self.next_deadline.min(deadline);
    }

    /// Like [`MessageBoxes::set_state`], but for channels. Useful when getting dialogs.
    ///
    /// The update state will only be updated if no entry was known previously.
    pub fn try_set_channel_state(&mut self, id: i64, pts: i32) {
        trace!(channel_id = id, pts, "trying to set channel state");
        if self.entry(Key::Channel(id)).is_none() {
            self.set_entry(LiveEntry {
                key: Key::Channel(id),
                pts,
                deadline: next_updates_deadline(),
                recovery_deferred: false,
                recovery_pending: false,
                recovery_deferrals: 0,
                possible_gap: None,
            });
        }
    }

    /// Try to begin getting difference for the given entry.
    /// Fails if the entry does not have a previously-known state that can be used to get its difference.
    ///
    /// Clears any previous gaps.
    fn try_begin_get_diff(&mut self, key: Key) {
        if !self.is_getting_diff(key) {
            if self.update_entry(key, |entry| {
                entry.possible_gap = None;
                entry.recovery_pending = true;
                entry.recovery_deferred = false;
                entry.recovery_deferrals = 0;
            }) {
                self.getting_diff_for.push_back(key);
                self.ready_diff_count += 1;
            } else {
                info!(
                    ?key,
                    "tried to begin getting difference but do not have entry for the key"
                );
            }
        }
    }

    /// Finish getting difference for the given entry, and resets its deadline.
    /// Does nothing if no difference was being fetched for the given key.
    fn try_end_get_diff(&mut self, key: Key) {
        let Some(entry) = self.entry(key).filter(|entry| entry.recovery_pending) else {
            return;
        };
        if !entry.recovery_deferred {
            let index = self
                .getting_diff_for
                .iter()
                .position(|&k| k == key)
                .expect("ready recovery must be queued");
            self.getting_diff_for.remove(index);
            self.ready_diff_count -= 1;
        }
        self.update_entry(key, |entry| {
            entry.recovery_pending = false;
            entry.recovery_deferred = false;
            entry.recovery_deferrals = 0;
        });
        self.reset_deadline(key, next_updates_deadline());

        debug_assert!(
            self.entry(key)
                .is_none_or(|entry| entry.possible_gap.is_none()),
            "gaps shouldn't be created while getting difference"
        );
    }
}

// "Normal" updates flow (processing and detection of gaps).
impl MessageBoxes {
    /// Process an update and return what should be done with it.
    ///
    /// Updates corresponding to entries for which their difference is currently being fetched
    /// will be ignored. While according to the [updates' documentation]:
    ///
    /// > Implementations \[have\] to postpone updates received via the socket while
    /// > filling gaps in the event and `Update` sequences, as well as avoid filling
    /// > gaps in the same sequence.
    ///
    /// In practice, these updates should have also been retrieved through getting difference.
    ///
    /// [updates' documentation]: https://core.telegram.org/api/updates
    pub fn process_updates(&mut self, updates: UpdatesLike) -> Result<UpdateAndPeers, Gap> {
        self.process_updates_inner(updates, &[])
    }

    fn process_updates_inner(
        &mut self,
        updates: UpdatesLike,
        force: &[Key],
    ) -> Result<UpdateAndPeers, Gap> {
        trace!(?updates, "processing updates");

        let deadline = next_updates_deadline();

        // Top level, when handling received `updates` and `updatesCombined`.
        // `updatesCombined` groups all the fields we care about, which is why we use it.
        //
        // This assumes all access hashes are already known to the client (socket updates are
        // expected to use `ensure_known_peer_hashes`, and the result from getting difference
        // has to deal with the peers in a different way).
        let tl::types::UpdatesCombined {
            date,
            seq_start,
            seq,
            mut updates,
            users,
            chats,
        } = match adaptor::adapt(updates) {
            Ok(combined) => combined,
            Err(Gap) => {
                self.try_begin_get_diff(Key::Common);
                return Err(Gap);
            }
        };

        if date < 0 || seq < 0 || seq_start < 0 {
            self.try_begin_get_diff(Key::Common);
            return Err(Gap);
        }

        let account_pending = force.is_empty()
            && (self.is_getting_diff(Key::Common) || self.is_getting_diff(Key::Secondary));
        let new_date = if date == NO_DATE || account_pending {
            self.date
        } else {
            date
        };
        let new_seq = if seq == NO_SEQ || account_pending {
            self.seq
        } else {
            seq
        };
        let mk_state = |message_box| State {
            date: new_date,
            seq: new_seq,
            message_box,
        };

        // > For all the other [not `updates` or `updatesCombined`] `Updates` type constructors
        // > there is no need to check `seq` or change a local state.
        if seq_start != NO_SEQ && !account_pending {
            match (i64::from(self.seq) + 1).cmp(&i64::from(seq_start)) {
                // Apply
                Ordering::Equal => {}
                // Ignore
                Ordering::Greater => {
                    debug!(
                        "skipping updates that were already handled at seq = {}",
                        self.seq
                    );
                    return Ok((Vec::new(), users, chats));
                }
                Ordering::Less => {
                    debug!(
                        "gap detected (local seq {}, remote seq {})",
                        self.seq, seq_start
                    );
                    self.try_begin_get_diff(Key::Common);
                    return Err(Gap);
                }
            }
        }

        if updates.iter().any(|update| !PtsInfo::valid_update(update)) {
            self.try_begin_get_diff(Key::Common);
            return Err(Gap);
        }

        // Telegram can send updates out of order (e.g. `ReadChannelInbox` first
        // and then `NewChannelMessage`, both with the same `pts`, but the `count`
        // is `0` and `1` respectively), so we sort them first.
        updates.sort_by_key(update_sort_key);

        let mut result = Vec::with_capacity(updates.len());
        let mut have_unresolved_gaps = false;

        // This loop does a lot at once to reduce the amount of times we need to iterate over
        // the updates as an optimization.
        //
        // It mutates the local pts state, remembers possible gaps, builds a set of entries for
        // which the deadlines should be reset, and determines whether any local pts was changed
        // so that the seq can be updated too (which could otherwise have been done earlier).
        for update in updates {
            if let tl::enums::Update::ChannelTooLong(u) = update {
                // Can skip this one early since no processing can be done for the entry.
                let key = Key::Channel(u.channel_id);
                if self.is_getting_diff(key) {
                    continue;
                }
                if let Some(pts) = u.pts.filter(|pts| *pts > NO_PTS)
                    && self.entry(key).is_none()
                {
                    self.set_entry(LiveEntry {
                        key,
                        pts,
                        deadline,
                        recovery_deferred: false,
                        recovery_pending: false,
                        recovery_deferrals: 0,
                        possible_gap: None,
                    });
                }
                self.try_begin_get_diff(key);
                continue;
            }

            let info = match PtsInfo::from_update(&update) {
                Some(info) => info,
                None => {
                    // No pts means that the update can be applied in any order.
                    result.push((update, mk_state(None)));
                    continue;
                }
            };
            if info.count < 0 || info.pts < info.count {
                self.try_begin_get_diff(info.key);
                return Err(Gap);
            }

            if force.contains(&info.key) {
                debug!(
                    key = ?info.key,
                    count = ?info.count,
                    remote = ?info.pts,
                    "forcing update to apply (getting difference)",
                );
                // Order does not need to be checked. These always must be applied.
                self.reset_deadline(info.key, next_updates_deadline());
                result.push((update, mk_state(Some(info.into()))));
                continue;
            }
            if self.is_getting_diff(info.key) {
                // Socket updates cannot advance or deliver a sequence whose
                // backlog is being fetched. Difference will deliver them once.
                have_unresolved_gaps = true;
                continue;
            }
            if info.key == Key::Secondary && self.entry(Key::Common).is_none() {
                // Deliver pre-login bot/secret updates without constructing
                // QTS-only state that cannot form a getDifference request.
                result.push((update, mk_state(None)));
                continue;
            }

            let mut gap_deadline = None;
            self.force_update_entry(
                LiveEntry {
                    key: info.key,
                    pts: info.pts - info.count,
                    deadline,
                    recovery_deferred: false,
                    recovery_pending: false,
                    recovery_deferrals: 0,
                    possible_gap: None,
                },
                |entry| {
                    match (i64::from(entry.pts) + i64::from(info.count)).cmp(&i64::from(info.pts)) {
                        // Apply
                        Ordering::Equal => {
                            entry.pts = info.pts;
                            entry.deadline = deadline;
                            result.push((update, mk_state(Some(info.into()))));
                        }
                        // Ignore
                        Ordering::Greater => {
                            debug!(
                                key = ?info.key,
                                local = ?entry.pts,
                                count = ?info.count,
                                remote = ?info.pts,
                                "skipping update",
                            );
                        }
                        Ordering::Less => {
                            // TODO store chats too?
                            info!(
                                key = ?info.key,
                                local = ?entry.pts,
                                count = ?info.count,
                                remote = ?info.pts,
                                "gap on update",
                            );

                            entry
                                .possible_gap
                                .get_or_insert_with(|| PossibleGap {
                                    deadline: Instant::now() + POSSIBLE_GAP_TIMEOUT,
                                    updates: Vec::new(),
                                })
                                .updates
                                .push(update.clone());
                        }
                    };

                    if let Some(mut gap) = entry.possible_gap.take() {
                        gap.updates
                            .sort_by_key(|u| std::cmp::Reverse(update_sort_key(u)));
                        while let Some(gap_update) = gap.updates.pop() {
                            let gap_info = PtsInfo::from_update(&gap_update)
                                .expect("only updates with pts info may be stored as gaps");

                            match (i64::from(entry.pts) + i64::from(gap_info.count))
                                .cmp(&i64::from(gap_info.pts))
                            {
                                Ordering::Equal => {
                                    entry.pts = gap_info.pts;
                                    // possible gap can only be resolve if new update closed
                                    // the gap - this has already updated its deadline if so.
                                    result.push((gap_update, mk_state(Some(gap_info.into()))));
                                }
                                Ordering::Greater => {}
                                Ordering::Less => {
                                    gap.updates.push(gap_update);
                                    break;
                                }
                            };
                        }
                        if !gap.updates.is_empty() {
                            gap_deadline = Some(gap.deadline);
                            entry.possible_gap = Some(gap);
                            have_unresolved_gaps = true;
                        }
                    }
                },
            );

            self.next_deadline = self.next_deadline.min(gap_deadline.unwrap_or(deadline));
        }

        if !result.is_empty() && !have_unresolved_gaps {
            // > If the updates were applied, local *Updates* state must be updated
            // > with `seq` (unless it's 0) and `date` from the constructor.
            self.date = new_date;
            self.seq = new_seq;
        }

        Ok((result, users, chats))
    }
}

/// Getting and applying account difference.
impl MessageBoxes {
    /// Return the request that needs to be made to get the difference, if any.
    pub fn get_difference(&self) -> Option<tl::functions::updates::GetDifference> {
        for entry in [Key::Common, Key::Secondary] {
            if self.is_getting_diff(entry) {
                let pts = self.entry(Key::Common).map(|entry| entry.pts)?;

                let gd = tl::functions::updates::GetDifference {
                    pts,
                    pts_limit: None,
                    pts_total_limit: None,
                    date: self.date.max(1), // non-zero or the request will fail
                    qts: self
                        .entry(Key::Secondary)
                        .map(|entry| entry.pts)
                        .unwrap_or(NO_PTS),
                    qts_limit: None,
                };
                trace!(?gd, "requesting");
                return Some(gd);
            }
        }
        None
    }

    /// Similar to [`MessageBoxes::process_updates`], but using the result from getting difference.
    pub fn apply_difference(
        &mut self,
        difference: tl::enums::updates::Difference,
    ) -> UpdateAndPeers {
        trace!(?difference, "applying account difference");
        debug_assert!(self.is_getting_diff(Key::Common) || self.is_getting_diff(Key::Secondary));
        let finish: bool;
        let result = match difference {
            tl::enums::updates::Difference::Empty(diff) => {
                debug!(
                    "handling empty difference (date = {}, seq = {}); no longer getting diff",
                    diff.date, diff.seq
                );
                finish = true;
                self.date = diff.date;
                self.seq = diff.seq;
                (Vec::new(), Vec::new(), Vec::new())
            }
            tl::enums::updates::Difference::Difference(diff) => {
                debug!(
                    state = ?diff.state,
                    "handling full difference; no longer getting diff",
                );
                finish = true;
                self.apply_difference_type(diff)
            }
            tl::enums::updates::Difference::Slice(tl::types::updates::DifferenceSlice {
                new_messages,
                new_encrypted_messages,
                other_updates,
                chats,
                users,
                intermediate_state: state,
            }) => {
                debug!(?state, "handling partial difference");
                finish = false;
                self.apply_difference_type(tl::types::updates::Difference {
                    new_messages,
                    new_encrypted_messages,
                    other_updates,
                    chats,
                    users,
                    state,
                })
            }
            tl::enums::updates::Difference::TooLong(diff) => {
                debug!(
                    "handling too-long difference (pts = {}); no longer getting diff",
                    diff.pts
                );
                finish = true;
                // the deadline will be reset once the diff ends
                self.set_pts(Key::Common, diff.pts);
                (Vec::new(), Vec::new(), Vec::new())
            }
        };

        if finish {
            self.try_end_get_diff(Key::Common);
            self.try_end_get_diff(Key::Secondary);
        }

        result
    }

    /// Checked account-difference path for untrusted server responses. An
    /// invalid state leaves state unchanged and reports `Gap`. Malformed
    /// individual updates are skipped without discarding valid siblings.
    pub fn try_apply_difference(
        &mut self,
        difference: tl::enums::updates::Difference,
    ) -> Result<UpdateAndPeers, Gap> {
        if !self.is_getting_diff(Key::Common) && !self.is_getting_diff(Key::Secondary) {
            return Err(Gap);
        }
        let valid_state = |state: &tl::enums::updates::State| {
            let tl::enums::updates::State::State(state) = state;
            state.pts >= 0 && state.qts >= 0 && state.date >= 0 && state.seq >= 0
        };
        let valid = match &difference {
            tl::enums::updates::Difference::Empty(d) => d.date >= 0 && d.seq >= 0,
            tl::enums::updates::Difference::TooLong(d) => d.pts >= 0,
            tl::enums::updates::Difference::Difference(d) => valid_state(&d.state),
            tl::enums::updates::Difference::Slice(d) => valid_state(&d.intermediate_state),
        };
        if !valid {
            return Err(Gap);
        }
        Ok(self.apply_difference(difference))
    }

    fn apply_difference_type(
        &mut self,
        tl::types::updates::Difference {
            new_messages,
            new_encrypted_messages,
            other_updates: mut updates,
            chats,
            users,
            state: tl::enums::updates::State::State(state),
        }: tl::types::updates::Difference,
    ) -> UpdateAndPeers {
        self.date = state.date;
        self.seq = state.seq;
        self.set_pts(Key::Common, state.pts);
        self.set_pts(Key::Secondary, state.qts);
        let mk_state = |message_box| State {
            date: state.date,
            seq: state.seq,
            message_box: Some(message_box),
        };

        // other_updates can contain things like UpdateChannelTooLong and UpdateNewChannelMessage.
        retain_valid_updates(&mut updates);
        // We need to process those as if they were socket updates to discard any we have already handled.
        // If the channel's difference is planned to be handled, skip its updates
        updates.retain(|update| {
            let Some(info) = PtsInfo::from_update(update) else {
                return true;
            };
            if let Key::Channel(_) = info.key {
                !self.is_getting_diff(info.key)
            } else {
                true
            }
        });
        let us = UpdatesLike::Updates(tl::enums::Updates::Updates(tl::types::Updates {
            updates,
            users,
            chats,
            date: NO_DATE,
            seq: NO_SEQ,
        }));

        // It is possible that the result from `GetDifference` includes users with `min = true`.
        // TODO in that case, we will have to resort to getUsers.
        let (mut result_updates, users, chats) = self
            .process_updates_inner(us, &[Key::Common, Key::Secondary])
            .expect("gap is detected while applying difference");

        result_updates.extend(
            new_messages
                .into_iter()
                .map(|message| {
                    (
                        tl::types::UpdateNewMessage {
                            message,
                            pts: NO_PTS,
                            pts_count: 0,
                        }
                        .into(),
                        mk_state(MessageBox::Common { pts: state.pts }),
                    )
                })
                .chain(new_encrypted_messages.into_iter().map(|message| {
                    (
                        tl::types::UpdateNewEncryptedMessage {
                            message,
                            qts: NO_PTS,
                        }
                        .into(),
                        mk_state(MessageBox::Secondary { qts: state.qts }),
                    )
                })),
        );

        (result_updates, users, chats)
    }
}

/// Getting and applying channel difference.
impl MessageBoxes {
    /// The first channel currently getting its difference, as `(key, channel_id)`, if any.
    fn channel_in_diff(&self) -> Option<(Key, i64)> {
        self.getting_diff_for.iter().find_map(|&key| match key {
            Key::Channel(id) => Some((key, id)),
            _ => None,
        })
    }

    /// Return the request that needs to be made to get a channel's difference, if any.
    ///
    /// Note that this only returns the skeleton of a request.
    /// Both the channel hash and limit will be their default value.
    pub fn get_channel_difference(&self) -> Option<tl::functions::updates::GetChannelDifference> {
        let (key, channel_id) = self.channel_in_diff()?;

        if let Some(pts) = self.entry(key).map(|entry| entry.pts) {
            let gd = tl::functions::updates::GetChannelDifference {
                force: false,
                channel: tl::types::InputChannel {
                    channel_id,
                    access_hash: 0,
                }
                .into(),
                filter: tl::enums::ChannelMessagesFilter::Empty,
                pts,
                limit: 0,
            };
            trace!(?gd, "requesting");
            Some(gd)
        } else {
            panic!("Should not try to get difference for an entry {key:?} without known state");
        }
    }

    /// Similar to [`MessageBoxes::process_updates`], but using the result from getting difference.
    pub fn apply_channel_difference(
        &mut self,
        difference: tl::enums::updates::ChannelDifference,
    ) -> UpdateAndPeers {
        let (key, channel_id) = self
            .channel_in_diff()
            .expect("applying channel difference to have a channel in getting_diff_for");

        trace!(channel_id, ?difference, "applying channel difference");
        self.exhausted_channels.remove(&channel_id);

        self.update_entry(key, |entry| {
            entry.possible_gap = None;
            entry.recovery_deferred = false;
            entry.recovery_deferrals = 0;
        });

        let tl::types::updates::ChannelDifference {
            r#final,
            pts,
            timeout,
            new_messages,
            other_updates: mut updates,
            chats,
            users,
        } = adaptor::adapt_channel_difference(difference);

        retain_valid_updates(&mut updates);

        if r#final {
            debug!(
                "handling channel {} difference; no longer getting diff",
                channel_id
            );
            self.try_end_get_diff(key);
        } else {
            debug!("handling channel {} difference", channel_id);
        }

        self.set_pts(key, pts);
        let us = UpdatesLike::Updates(tl::enums::Updates::Updates(tl::types::Updates {
            updates,
            users,
            chats,
            date: NO_DATE,
            seq: NO_SEQ,
        }));
        let (mut result_updates, users, chats) = self
            .process_updates_inner(us, &[key])
            .expect("gap is detected while applying channel difference");

        result_updates.extend(new_messages.into_iter().map(|message| {
            (
                tl::types::UpdateNewChannelMessage {
                    message,
                    pts: NO_PTS,
                    pts_count: 0,
                }
                .into(),
                State {
                    date: self.date,
                    seq: self.seq,
                    message_box: Some(MessageBox::Channel { channel_id, pts }),
                },
            )
        }));

        self.reset_timeout(key, timeout);

        // A channel with a long backlog yields to every other pending channel
        // after each page. Keep its entry active so gap tracking is unchanged.
        if !r#final && let Some(index) = self.getting_diff_for.iter().position(|k| *k == key) {
            self.getting_diff_for.remove(index);
            self.getting_diff_for.push_back(key);
        }

        (result_updates, users, chats)
    }

    /// Validate externally supplied channel differences before mutating state.
    /// `Err(Gap)` leaves the state unchanged so the caller can report the
    /// protocol failure. Invalid individual updates are skipped while valid
    /// siblings and the channel state advance. Legacy
    /// [`Self::apply_channel_difference`] is retained.
    pub fn try_apply_channel_difference(
        &mut self,
        difference: tl::enums::updates::ChannelDifference,
    ) -> Result<UpdateAndPeers, Gap> {
        let (_, channel_id) = self.channel_in_diff().ok_or(Gap)?;
        let valid = match &difference {
            tl::enums::updates::ChannelDifference::Empty(d) => d.pts >= NO_PTS,
            tl::enums::updates::ChannelDifference::Difference(d) => d.pts >= NO_PTS,
            tl::enums::updates::ChannelDifference::TooLong(d) => match &d.dialog {
                tl::enums::Dialog::Dialog(dialog) => {
                    dialog.pts.is_some_and(|pts| pts >= NO_PTS)
                        && matches!(&dialog.peer, tl::enums::Peer::Channel(peer) if peer.channel_id == channel_id)
                }
                _ => false,
            },
        };
        if !valid {
            return Err(Gap);
        }
        Ok(self.apply_channel_difference(difference))
    }

    /// Postpone the first ready channel difference for a bounded retry grace.
    ///
    /// Socket PTS updates remain suppressed during at most 30 deferrals while
    /// other channels may recover. A further deferral warns and discards the
    /// unavailable channel's saved position, allowing later socket updates to
    /// start fresh; its unrecovered backlog is lost. Successful recovery resets
    /// the allowance, as does learning usable access through
    /// [`Self::channel_difference_accessible`]. Exhaustion survives entry removal:
    /// later gaps with no usable access discard their backlog on the first miss.
    /// The receiver retries missing hashes every second, giving
    /// about 30 seconds of grace. Each supplied delay is capped at the normal
    /// no-updates timeout.
    ///
    /// # Panics
    ///
    /// Panics if no channel difference is ready to be fetched.
    pub fn defer_channel_difference(&mut self, retry_after: Duration) {
        let (key, channel_id) = self
            .channel_in_diff()
            .expect("deferring channel difference requires a ready channel");
        if self.exhausted_channels.contains(&channel_id)
            || self
                .entry(key)
                .is_some_and(|entry| entry.recovery_deferrals >= MAX_CHANNEL_DEFERRALS)
        {
            warn!(
                channel_id,
                "channel recovery grace exhausted; discarding backlog position to resume live updates"
            );
            self.exhausted_channels.insert(channel_id);
            self.try_end_get_diff(key);
            self.pop_entry(key);
            return;
        }
        self.update_entry(key, |entry| {
            entry.recovery_deferred = true;
            entry.recovery_deferrals += 1;
        });
        self.ready_diff_count -= 1;
        let index = self
            .getting_diff_for
            .iter()
            .position(|&queued| queued == key)
            .expect("ready channel must be queued");
        self.getting_diff_for.remove(index);
        let deadline = Instant::now() + retry_after.min(defs::NO_UPDATES_TIMEOUT);
        self.update_entry(key, |entry| entry.deadline = deadline);
        // Keep a conservative earlier wake; recompute on expiry, not per channel.
        self.next_deadline = self.next_deadline.min(deadline);
    }

    /// Reset missing-access grace for the first ready channel after usable access
    /// is found. Successful channel differences reset the same state automatically.
    ///
    /// # Panics
    ///
    /// Panics if no channel difference is ready to be fetched.
    pub fn channel_difference_accessible(&mut self) {
        let (key, channel_id) = self
            .channel_in_diff()
            .expect("accessible channel difference requires a ready channel");
        self.exhausted_channels.remove(&channel_id);
        self.update_entry(key, |entry| entry.recovery_deferrals = 0);
    }

    pub fn end_channel_difference(&mut self, reason: PrematureEndReason) {
        let (key, channel_id) = self
            .channel_in_diff()
            .expect("ending channel difference to have a channel in getting_diff_for");

        trace!(channel_id, ?reason, "ending channel difference");
        match reason {
            PrematureEndReason::TemporaryServerIssues => {
                self.update_entry(key, |entry| entry.possible_gap = None);
                self.try_end_get_diff(key);
            }
            PrematureEndReason::Banned => {
                self.update_entry(key, |entry| entry.possible_gap = None);
                self.try_end_get_diff(key);
                self.pop_entry(key);
            }
        }
    }
}

/// Reason for calling [`MessageBoxes::end_channel_difference`].
#[derive(Debug)]
pub enum PrematureEndReason {
    /// The channel difference failed to be fetched for a temporary reasons.
    ///
    /// The channel state must be kept,
    /// and getting its difference retried in the future.
    TemporaryServerIssues,
    /// The channel cannot be recovered with the current access or request.
    /// Includes documented channel-specific RPC rejections.
    ///
    /// The channel state will be removed,
    /// so this failed recovery is not retried. Later live updates may establish
    /// a fresh position and a subsequent recovery attempt.
    Banned,
}
