// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::cell::RefCell;
use std::ops::Add;
use std::time::Duration;

use grammers_tl_types as tl;

use super::defs::{
    Gap, Key, MessageBox, MessageBoxes, NO_DATE, NO_PTS, NO_SEQ, NO_UPDATES_TIMEOUT, State,
    UpdateAndPeers,
};
use super::{MAX_HELD_UPDATES, PrematureEndReason, next_updates_deadline};
use crate::message_box::POSSIBLE_GAP_TIMEOUT;
use crate::message_box::defs::UpdatesLike;
use crate::types::{ChannelState, UpdatesState};

thread_local! {
    static NOW: RefCell<Instant> = const { RefCell::new(Instant(Duration::ZERO)) };
}

#[test]
fn exhausted_missing_hash_grace_survives_a_second_episode() {
    reset_time();
    let mut boxes = MessageBoxes::load(UpdatesState {
        channels: vec![ChannelState { id: 7, pts: 10 }],
        ..Default::default()
    });
    for _ in 0..30 {
        defer_one_second(&mut boxes);
        assert_eq!(
            boxes.session_state().channels,
            vec![ChannelState { id: 7, pts: 10 }]
        );
    }
    boxes.defer_channel_difference(Duration::from_secs(1));
    assert!(boxes.session_state().channels.is_empty());
    assert_eq!(
        boxes
            .process_updates(channel_updates(7, 0, 0, 50))
            .unwrap()
            .0
            .len(),
        1
    );
    boxes
        .process_updates(channel_too_long(7, Some(100)))
        .unwrap();
    assert_eq!(
        boxes.get_channel_difference(),
        Some(get_channel_difference(7, 50))
    );
    let before = Instant::now();
    boxes.defer_channel_difference(Duration::from_secs(1));
    assert!(boxes.get_channel_difference().is_none());
    assert!(boxes.session_state().channels.is_empty());
    assert_eq!(Instant::now(), before);
    assert_eq!(
        boxes
            .process_updates(channel_updates(7, 0, 0, 51))
            .unwrap()
            .0
            .len(),
        1
    );
}

fn channel_too_long(channel_id: i64, pts: Option<i32>) -> UpdatesLike {
    UpdatesLike::Updates(
        tl::types::UpdateShort {
            date: 0,
            update: tl::types::UpdateChannelTooLong { channel_id, pts }.into(),
        }
        .into(),
    )
}

fn defer_one_second(boxes: &mut MessageBoxes) {
    boxes.defer_channel_difference(Duration::from_secs(1));
    assert!(boxes.get_channel_difference().is_none());
    advance_time_by(Duration::from_millis(999));
    boxes.check_deadlines();
    assert!(boxes.get_channel_difference().is_none());
    advance_time_by(Duration::from_millis(1));
    boxes.check_deadlines();
    assert!(boxes.get_channel_difference().is_some());
}

#[test]
fn usable_access_and_success_each_reset_missing_hash_allowance() {
    for successful_difference in [false, true] {
        reset_time();
        let mut boxes = MessageBoxes::load(UpdatesState {
            channels: vec![ChannelState { id: 7, pts: 10 }],
            ..Default::default()
        });
        for _ in 0..30 {
            defer_one_second(&mut boxes);
        }
        boxes.defer_channel_difference(Duration::from_secs(1));
        boxes.process_updates(channel_updates(7, 0, 0, 50)).unwrap();
        boxes.process_updates(channel_too_long(7, None)).unwrap();
        if successful_difference {
            boxes
                .try_apply_channel_difference(
                    tl::types::updates::ChannelDifferenceEmpty {
                        r#final: true,
                        pts: 51,
                        timeout: None,
                    }
                    .into(),
                )
                .unwrap();
            boxes.process_updates(channel_too_long(7, None)).unwrap();
        } else {
            boxes.channel_difference_accessible();
        }
        for _ in 0..30 {
            defer_one_second(&mut boxes);
            assert_eq!(boxes.session_state().channels.len(), 1);
        }
        boxes.defer_channel_difference(Duration::from_secs(1));
        assert!(boxes.session_state().channels.is_empty());
    }
}

#[test]
fn usable_access_mid_grace_resets_the_current_counter() {
    reset_time();
    let mut boxes = MessageBoxes::load(UpdatesState {
        channels: vec![ChannelState { id: 7, pts: 10 }],
        ..Default::default()
    });
    for _ in 0..29 {
        defer_one_second(&mut boxes);
    }
    boxes.channel_difference_accessible();
    for _ in 0..30 {
        defer_one_second(&mut boxes);
    }
    assert_eq!(
        boxes.session_state().channels,
        vec![ChannelState { id: 7, pts: 10 }]
    );
    boxes.defer_channel_difference(Duration::from_secs(1));
    assert!(boxes.session_state().channels.is_empty());
}

#[test]
fn channel_too_long_preserves_local_pts_and_requires_a_positive_initial_hint() {
    for hint in [None, Some(0), Some(5), Some(10), Some(100)] {
        reset_time();
        let mut boxes = MessageBoxes::load(UpdatesState {
            channels: vec![ChannelState { id: 7, pts: 10 }],
            ..Default::default()
        });
        boxes.end_channel_difference(PrematureEndReason::TemporaryServerIssues);
        boxes.process_updates(channel_too_long(7, hint)).unwrap();
        // Requested once that rejection's delay is over.
        assert_eq!(boxes.get_channel_difference(), None);
        advance_time_by(Duration::from_secs(1));
        boxes.check_deadlines();
        assert_eq!(
            boxes.get_channel_difference(),
            Some(get_channel_difference(7, 10))
        );
        assert_eq!(
            boxes.session_state().channels,
            vec![ChannelState { id: 7, pts: 10 }]
        );
        let mut fresh = MessageBoxes::new();
        fresh.process_updates(channel_too_long(7, hint)).unwrap();
        assert_eq!(
            fresh.get_channel_difference(),
            hint.filter(|pts| *pts > 0)
                .map(|pts| get_channel_difference(7, pts))
        );
        assert_eq!(
            fresh.session_state().channels.len(),
            usize::from(hint.is_some_and(|pts| pts > 0))
        );
    }
}

#[test]
fn deferred_channels_rejoin_ready_queue_once_and_keep_fair_order() {
    reset_time();
    let mut boxes = MessageBoxes::load(UpdatesState {
        channels: (1..=3).map(|id| ChannelState { id, pts: 10 }).collect(),
        ..Default::default()
    });
    for id in 1..=3 {
        assert_eq!(
            boxes.get_channel_difference(),
            Some(get_channel_difference(id, 10))
        );
        boxes.defer_channel_difference(Duration::from_secs(1));
    }
    assert!(boxes.getting_diff_for.is_empty());
    assert_eq!(boxes.ready_diff_count, 0);
    advance_time_by(Duration::from_secs(1));
    for _ in 0..3 {
        boxes.check_deadlines();
    }
    assert_eq!(boxes.ready_diff_count, 3);
    for id in 1..=3 {
        assert_eq!(
            boxes.get_channel_difference(),
            Some(get_channel_difference(id, 10))
        );
        boxes
            .try_apply_channel_difference(
                tl::types::updates::ChannelDifferenceEmpty {
                    r#final: false,
                    pts: 11,
                    timeout: None,
                }
                .into(),
            )
            .unwrap();
    }
    for id in 1..=3 {
        assert_eq!(
            boxes.get_channel_difference(),
            Some(get_channel_difference(id, 11))
        );
        boxes.end_channel_difference(PrematureEndReason::Banned);
    }
    assert!(boxes.getting_diff_for.is_empty());
    assert_eq!(boxes.ready_diff_count, 0);
    assert!(boxes.session_state().channels.is_empty());
}

#[test]
#[ignore = "local release component benchmark; run explicitly with --ignored --nocapture"]
fn benchmark_missing_hash_retry_pass() {
    use std::hint::black_box;
    for count in [1_000, 10_000] {
        reset_time();
        let mut boxes = MessageBoxes::load(UpdatesState {
            channels: (1..=count).map(|id| ChannelState { id, pts: 1 }).collect(),
            ..Default::default()
        });
        let mut elapsed = [Duration::ZERO; 2];
        for duration in &mut elapsed {
            boxes.check_deadlines();
            let started = std::time::Instant::now();
            for _ in 0..count {
                assert!(black_box(&boxes).get_channel_difference().is_some());
                black_box(&mut boxes).defer_channel_difference(Duration::from_secs(1));
                black_box(black_box(&mut boxes).check_deadlines());
            }
            *duration = started.elapsed();
            assert!(boxes.get_channel_difference().is_none());
            advance_time_by(Duration::from_secs(1));
        }
        eprintln!(
            "missing_hash_retry channels={count} initial_pass_ns={} retry_pass_ns={}",
            elapsed[0].as_nanos(),
            elapsed[1].as_nanos()
        );
    }
}

#[test]
fn final_channel_timeout_replaces_deadline_computed_during_recovery() {
    reset_time();
    let mut boxes = MessageBoxes::load(UpdatesState {
        channels: vec![ChannelState { id: 7, pts: 10 }],
        ..Default::default()
    });
    advance_time_by(Duration::from_secs(1));
    assert_eq!(boxes.check_deadlines(), Instant::now());
    advance_time_by(Duration::from_secs(1));
    boxes
        .try_apply_channel_difference(
            tl::types::updates::ChannelDifferenceEmpty {
                r#final: true,
                pts: 11,
                timeout: Some(5),
            }
            .into(),
        )
        .unwrap();
    assert_eq!(
        boxes.check_deadlines(),
        Instant::now() + Duration::from_secs(5)
    );
    advance_time_by(Duration::from_secs(5));
    assert_eq!(boxes.check_deadlines(), Instant::now());
    assert_eq!(
        boxes.get_channel_difference(),
        Some(get_channel_difference(7, 11))
    );
}

#[test]
fn unavailable_channel_grace_is_bounded_and_other_channels_keep_delivering() {
    reset_time();
    let mut boxes = MessageBoxes::load(UpdatesState {
        channels: vec![ChannelState { id: 7, pts: 10 }],
        ..Default::default()
    });
    for attempt in 0..30 {
        assert_eq!(
            boxes.get_channel_difference(),
            Some(get_channel_difference(7, 10))
        );
        boxes.defer_channel_difference(Duration::from_secs(1));
        assert!(boxes.get_channel_difference().is_none());
        assert!(
            boxes
                .process_updates(channel_updates(7, 0, 0, 11))
                .unwrap()
                .0
                .is_empty()
        );
        let independent = boxes
            .process_updates(channel_updates(8, 0, 0, attempt + 1))
            .unwrap()
            .0;
        assert_eq!(independent.len(), 1);
        assert_eq!(independent[0].0, channel_update(8, attempt + 1));
        assert_eq!(
            boxes.session_state().channels[0],
            ChannelState { id: 7, pts: 10 }
        );
        assert_eq!(
            boxes.check_deadlines(),
            Instant::now() + Duration::from_secs(1)
        );
        advance_time_by(Duration::from_secs(1));
        boxes.check_deadlines();
    }
    boxes.defer_channel_difference(Duration::from_secs(1));
    assert!(boxes.get_channel_difference().is_none());
    assert_eq!(
        boxes.session_state().channels,
        vec![ChannelState { id: 8, pts: 30 }]
    );
    let resumed = boxes
        .process_updates(channel_updates(7, 0, 0, 50))
        .unwrap()
        .0;
    assert_eq!(resumed.len(), 1);
    assert_eq!(resumed[0].0, channel_update(7, 50));
    assert_eq!(
        boxes.session_state().channels[0],
        ChannelState { id: 7, pts: 50 }
    );
}

#[test]
fn ready_recovery_accounting_survives_mixed_completion_and_defer() {
    reset_time();
    let mut boxes = MessageBoxes::load(UpdatesState {
        pts: 100,
        qts: 78,
        date: 12,
        seq: 34,
        channels: vec![
            ChannelState { id: 7, pts: 10 },
            ChannelState { id: 8, pts: 20 },
        ],
    });
    boxes.defer_channel_difference(Duration::from_secs(5));
    assert_eq!(
        boxes.get_channel_difference(),
        Some(get_channel_difference(8, 20))
    );
    boxes.end_channel_difference(PrematureEndReason::TemporaryServerIssues);
    assert!(boxes.get_channel_difference().is_none());
    assert!(boxes.get_difference().is_some());
    assert_eq!(boxes.check_deadlines(), Instant::now());
    boxes
        .try_apply_difference(tl::types::updates::DifferenceEmpty { date: 13, seq: 35 }.into())
        .unwrap();
    assert!(boxes.get_difference().is_none());
    assert_eq!(
        boxes.check_deadlines(),
        Instant::now() + Duration::from_secs(5)
    );
    advance_time_by(Duration::from_secs(5));
    assert_eq!(boxes.check_deadlines(), Instant::now());
    boxes
        .try_apply_channel_difference(
            tl::types::updates::ChannelDifferenceEmpty {
                r#final: true,
                pts: 11,
                timeout: None,
            }
            .into(),
        )
        .unwrap();
    assert!(boxes.get_channel_difference().is_none());
    assert!(boxes.check_deadlines() > Instant::now());
}

#[test]
#[ignore = "local release component benchmark; run explicitly with --ignored --nocapture"]
fn benchmark_pending_channel_deadlines() {
    use std::hint::black_box;
    const ITERATIONS: u32 = 32;
    for count in [1_000, 10_000] {
        reset_time();
        let mut boxes = MessageBoxes::load(UpdatesState {
            channels: (1..=count).map(|id| ChannelState { id, pts: 1 }).collect(),
            ..Default::default()
        });
        black_box(boxes.check_deadlines());
        let started = std::time::Instant::now();
        for _ in 0..ITERATIONS {
            black_box(black_box(&mut boxes).check_deadlines());
        }
        let hot = started.elapsed();
        assert!(boxes.get_channel_difference().is_some());
        advance_time_by(NO_UPDATES_TIMEOUT);
        let started = std::time::Instant::now();
        black_box(black_box(&mut boxes).check_deadlines());
        let expired = started.elapsed();
        eprintln!(
            "pending_deadlines channels={count} iterations={ITERATIONS} hot_total_ns={} hot_ns_per_call={} expired_sweep_ns={}",
            hot.as_nanos(),
            hot.as_nanos() / u128::from(ITERATIONS),
            expired.as_nanos()
        );
    }
}

#[test]
fn malformed_socket_metadata_cannot_poison_recovery_state() {
    let saved = UpdatesState {
        pts: 100,
        qts: 0,
        date: 1,
        seq: 1,
        channels: vec![ChannelState { id: 7, pts: 10 }],
    };
    for (date, seq_start, seq) in [(-1, 0, 0), (1, -1, 0), (1, 0, -1)] {
        let mut boxes = MessageBoxes::try_load(saved.clone()).unwrap();
        assert!(
            boxes
                .process_updates(UpdatesLike::Updates(
                    tl::types::UpdatesCombined {
                        date,
                        seq_start,
                        seq,
                        updates: vec![tl::enums::Update::Config],
                        users: vec![],
                        chats: vec![],
                    }
                    .into()
                ))
                .is_err()
        );
        assert_eq!(boxes.session_state(), saved);
    }
    for id in [i64::MIN, -1, 0, 997852516353, 1002147483648, i64::MAX] {
        let mut invalid = saved.clone();
        invalid.channels.push(ChannelState { id, pts: 10 });
        assert!(MessageBoxes::try_load(invalid).is_err());
        for pts in [0, 10] {
            let mut boxes = MessageBoxes::try_load(saved.clone()).unwrap();
            assert!(
                boxes
                    .process_updates(UpdatesLike::Updates(
                        tl::types::UpdateShort {
                            date: 1,
                            update: tl::types::UpdateChannelTooLong {
                                channel_id: id,
                                pts: Some(pts)
                            }
                            .into(),
                        }
                        .into()
                    ))
                    .is_err()
            );
            assert_eq!(boxes.session_state(), saved);
        }
    }
}

#[test]
fn relearning_primary_state_preserves_saved_channel_recovery() {
    let channels = vec![ChannelState { id: 7, pts: 10 }];
    let mut boxes = MessageBoxes::try_load(UpdatesState {
        pts: 0,
        qts: 5,
        channels: channels.clone(),
        ..Default::default()
    })
    .unwrap();
    assert!(!boxes.is_empty());
    assert!(!boxes.has_primary_state());
    let before = boxes.get_channel_difference().unwrap();
    boxes.set_state(tl::types::updates::State {
        pts: 100,
        qts: 0,
        date: 1,
        seq: 1,
        unread_count: 0,
    });
    assert!(boxes.has_primary_state());
    assert_eq!(boxes.session_state().channels, channels);
    assert_eq!(boxes.get_channel_difference().unwrap(), before);
}

#[test]
fn checked_saved_state_and_difference_reject_invalid_counters_without_mutation() {
    let state = UpdatesState {
        pts: 10,
        qts: 7,
        date: 1,
        seq: 2,
        channels: vec![],
    };
    let mut invalid = state.clone();
    invalid.pts = 0;
    assert!(MessageBoxes::try_load(invalid).unwrap().is_empty());
    let mut invalid = state.clone();
    invalid.channels.push(ChannelState { id: 10, pts: -1 });
    assert!(MessageBoxes::try_load(invalid).is_err());
    let mut boxes = MessageBoxes::try_load(state).unwrap();
    let before = boxes.get_difference().unwrap();
    assert!(
        boxes
            .try_apply_difference(tl::types::updates::DifferenceEmpty { date: -1, seq: 2 }.into())
            .is_err()
    );
    let after = boxes.get_difference().unwrap();
    assert_eq!(
        (before.pts, before.date, before.qts),
        (after.pts, after.date, after.qts)
    );
    assert!(
        MessageBoxes::new()
            .try_apply_difference(tl::types::updates::DifferenceEmpty { date: 1, seq: 2 }.into())
            .is_err()
    );
}

#[test]
fn saved_state_deduplicates_channels_and_recovers_missing_common_pts() {
    let boxes = MessageBoxes::load(UpdatesState {
        pts: 0,
        qts: 7,
        date: 1,
        seq: i32::MAX,
        channels: vec![
            ChannelState { id: 10, pts: 5 },
            ChannelState { id: 10, pts: 3 },
            ChannelState { id: -1, pts: 5 },
            ChannelState { id: 12, pts: -1 },
        ],
    });
    assert!(boxes.get_difference().is_none());
    assert_eq!(boxes.session_state().qts, 0);
    let saved = boxes.session_state();
    assert_eq!(saved.channels.len(), 1);
    assert_eq!(saved.channels[0].pts, 3);
}

#[test]
fn channel_catch_up_rotates_after_nonfinal_pages() {
    let mut boxes = MessageBoxes::load(UpdatesState {
        pts: 0,
        qts: 0,
        date: 1,
        seq: 0,
        channels: vec![
            ChannelState { id: 10, pts: 5 },
            ChannelState { id: 20, pts: 7 },
        ],
    });
    let channel = |boxes: &MessageBoxes| match boxes.get_channel_difference().unwrap().channel {
        tl::enums::InputChannel::Channel(c) => c.channel_id,
        _ => unreachable!(),
    };
    assert_eq!(channel(&boxes), 10);
    boxes
        .try_apply_channel_difference(
            tl::types::updates::ChannelDifferenceEmpty {
                r#final: false,
                pts: 6,
                timeout: Some(-1),
            }
            .into(),
        )
        .unwrap();
    assert_eq!(channel(&boxes), 20);
    boxes
        .try_apply_channel_difference(
            tl::types::updates::ChannelDifferenceEmpty {
                r#final: false,
                pts: 8,
                timeout: None,
            }
            .into(),
        )
        .unwrap();
    assert_eq!(channel(&boxes), 10);
    assert!(
        MessageBoxes::new()
            .try_apply_channel_difference(
                tl::types::updates::ChannelDifferenceEmpty {
                    r#final: true,
                    pts: 1,
                    timeout: None,
                }
                .into()
            )
            .is_err()
    );
}

#[test]
fn extreme_update_counters_do_not_overflow() {
    let mut boxes = MessageBoxes::load(UpdatesState {
        pts: i32::MAX,
        qts: 0,
        date: 1,
        seq: i32::MAX,
        channels: vec![],
    });
    assert!(
        boxes
            .process_updates(updates(1, i32::MAX, i32::MAX))
            .is_ok()
    );
    let mut boxes = MessageBoxes::new();
    let malformed = UpdatesLike::Updates(
        tl::types::Updates {
            updates: vec![
                tl::types::UpdateDeleteMessages {
                    messages: vec![],
                    pts: i32::MIN,
                    pts_count: 1,
                }
                .into(),
            ],
            users: vec![],
            chats: vec![],
            date: 1,
            seq: 0,
        }
        .into(),
    );
    assert!(boxes.process_updates(malformed).is_err());
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Instant(Duration);

impl Instant {
    pub fn now() -> Self {
        NOW.with_borrow(|now| *now)
    }
}

impl Add<Duration> for Instant {
    type Output = Self;

    fn add(self, rhs: Duration) -> Self::Output {
        Self(self.0 + rhs)
    }
}

fn reset_time() {
    NOW.with_borrow_mut(|now| now.0 = Duration::ZERO);
}

fn advance_time_by(duration: Duration) {
    NOW.with_borrow_mut(|now| now.0 += duration);
}

fn state(date: i32, seq: i32, pts: i32, qts: i32) -> tl::types::updates::State {
    tl::types::updates::State {
        pts,
        qts,
        date,
        seq,
        unread_count: 0,
    }
}

fn update(pts: i32) -> tl::enums::Update {
    tl::enums::Update::DeleteMessages(tl::types::UpdateDeleteMessages {
        messages: Vec::new(),
        pts,
        pts_count: 1,
    })
}

fn updates(date: i32, seq: i32, pts: i32) -> UpdatesLike {
    UpdatesLike::Updates(tl::enums::Updates::Updates(tl::types::Updates {
        updates: vec![update(pts)],
        users: Vec::new(),
        chats: Vec::new(),
        date,
        seq,
    }))
}

fn channel_update(channel_id: i64, pts: i32) -> tl::enums::Update {
    tl::enums::Update::DeleteChannelMessages(tl::types::UpdateDeleteChannelMessages {
        channel_id,
        messages: Vec::new(),
        pts,
        pts_count: 1,
    })
}

fn channel_updates(channel_id: i64, date: i32, seq: i32, pts: i32) -> UpdatesLike {
    UpdatesLike::Updates(tl::enums::Updates::Updates(tl::types::Updates {
        updates: vec![channel_update(channel_id, pts)],
        users: Vec::new(),
        chats: Vec::new(),
        date,
        seq,
    }))
}

fn updates_ok(date: i32, seq: i32, pts: i32) -> Result<UpdateAndPeers, Gap> {
    Ok((
        vec![(
            update(pts),
            State {
                date,
                seq,
                message_box: Some(MessageBox::Common { pts }),
            },
        )],
        Vec::new(),
        Vec::new(),
    ))
}

fn merge_updates(updates: Vec<Result<UpdateAndPeers, Gap>>) -> Result<UpdateAndPeers, Gap> {
    updates
        .into_iter()
        .collect::<Result<Vec<_>, _>>()
        .map(|updates| {
            updates
                .into_iter()
                .fold((Vec::new(), Vec::new(), Vec::new()), |mut result, tuple| {
                    result.0.extend(tuple.0);
                    result.1.extend(tuple.1);
                    result.2.extend(tuple.2);
                    result
                })
        })
}

fn get_difference(date: i32, pts: i32, qts: i32) -> tl::functions::updates::GetDifference {
    tl::functions::updates::GetDifference {
        pts,
        pts_limit: None,
        pts_total_limit: None,
        date,
        qts,
        qts_limit: None,
    }
}

fn get_channel_difference(id: i64, pts: i32) -> tl::functions::updates::GetChannelDifference {
    tl::functions::updates::GetChannelDifference {
        force: false,
        channel: tl::enums::InputChannel::Channel(tl::types::InputChannel {
            channel_id: id,
            access_hash: 0,
        }),
        filter: tl::enums::ChannelMessagesFilter::Empty,
        pts,
        limit: 0,
    }
}

#[test]
fn test_connect_flow_empty() {
    reset_time();
    let state = UpdatesState {
        pts: NO_PTS,
        qts: NO_PTS,
        date: NO_DATE,
        seq: NO_SEQ,
        channels: Vec::new(),
    };
    let message_boxes = MessageBoxes::load(state.clone());

    assert!(message_boxes.is_empty());
    assert_eq!(message_boxes.get_difference(), None);
    assert_eq!(message_boxes.get_channel_difference(), None);
    assert_eq!(message_boxes.session_state(), state)
}

#[test]
fn test_connect_flow_with_data() {
    reset_time();
    let state = UpdatesState {
        pts: 12,
        qts: 34,
        date: 56,
        seq: 78,
        channels: vec![ChannelState { id: 43, pts: 21 }],
    };
    let message_boxes = MessageBoxes::load(state.clone());

    assert!(!message_boxes.is_empty());
    assert_eq!(
        message_boxes.get_difference(),
        Some(get_difference(56, 12, 34))
    );
    assert_eq!(
        message_boxes.get_channel_difference(),
        Some(get_channel_difference(43, 21))
    );
    assert_eq!(message_boxes.session_state(), state)
}

#[test]
fn test_complete_login_flow() {
    reset_time();
    let mut message_boxes = MessageBoxes::new();
    assert!(message_boxes.is_empty());

    message_boxes.set_state(state(56, 78, 12, 34));
    assert!(!message_boxes.is_empty());
    assert_eq!(message_boxes.get_difference(), None);
    assert_eq!(message_boxes.get_channel_difference(), None);
    assert_eq!(
        message_boxes.session_state(),
        UpdatesState {
            pts: 12,
            qts: 34,
            date: 56,
            seq: 78,
            channels: Vec::new()
        }
    )
}

#[test]
fn test_iter_dialogs_flow() {
    reset_time();
    let mut message_boxes = MessageBoxes::new();

    message_boxes.try_set_channel_state(98, 76);
    message_boxes.try_set_channel_state(54, 32);
    message_boxes.try_set_channel_state(98, 10);

    assert_eq!(
        message_boxes.session_state(),
        UpdatesState {
            channels: vec![
                // Notably: sorted, and only the first set is kept.
                ChannelState { id: 54, pts: 32 },
                ChannelState { id: 98, pts: 76 },
            ],
            ..message_boxes.session_state()
        }
    )
}

#[test]
fn test_next_raw_update_flow_empty() {
    reset_time();
    let mut message_boxes = MessageBoxes::new();

    let deadline = next_updates_deadline();
    assert_eq!(message_boxes.check_deadlines(), deadline);
    assert_eq!(message_boxes.get_difference(), None);
    assert_eq!(message_boxes.get_channel_difference(), None);

    advance_time_by(NO_UPDATES_TIMEOUT / 2); // assert unchanged since previous deadline was not met yet
    assert_eq!(message_boxes.check_deadlines(), deadline);
    assert_eq!(message_boxes.get_difference(), None);
    assert_eq!(message_boxes.get_channel_difference(), None);

    advance_time_by(NO_UPDATES_TIMEOUT); // assert changed to avoid sleeping with no delay
    assert_eq!(message_boxes.check_deadlines(), next_updates_deadline());
    assert_eq!(message_boxes.get_difference(), None);
    assert_eq!(message_boxes.get_channel_difference(), None);

    advance_time_by(NO_UPDATES_TIMEOUT + Duration::from_secs(1)); // assert change based on now, not last deadline
    assert_eq!(message_boxes.check_deadlines(), next_updates_deadline());
    assert_eq!(message_boxes.get_difference(), None);
    assert_eq!(message_boxes.get_channel_difference(), None);
}

#[test]
fn test_next_raw_update_flow_common_timeout() {
    reset_time();
    let mut message_boxes = MessageBoxes::new();
    message_boxes.set_state(state(56, 78, 12, 34));

    advance_time_by(NO_UPDATES_TIMEOUT);

    assert_eq!(message_boxes.get_difference(), None);
    assert_eq!(message_boxes.check_deadlines(), Instant::now());
    assert_eq!(
        message_boxes.get_difference(),
        Some(get_difference(56, 12, 34))
    );

    message_boxes
        .apply_difference(tl::types::updates::DifferenceEmpty { date: 90, seq: 91 }.into());

    assert_eq!(message_boxes.get_difference(), None);
    assert_eq!(message_boxes.check_deadlines(), next_updates_deadline());
    assert_eq!(
        message_boxes.session_state(),
        UpdatesState {
            pts: 12,
            qts: 34,
            date: 90,
            seq: 91,
            channels: Vec::new()
        }
    );
}

#[test]
fn test_next_raw_update_flow_channel_timeout() {
    reset_time();
    let mut message_boxes = MessageBoxes::new();
    message_boxes.try_set_channel_state(12, 34);

    advance_time_by(NO_UPDATES_TIMEOUT);

    assert_eq!(message_boxes.get_channel_difference(), None);
    assert_eq!(message_boxes.check_deadlines(), Instant::now());
    assert_eq!(
        message_boxes.get_channel_difference(),
        Some(get_channel_difference(12, 34))
    );

    message_boxes.apply_channel_difference(
        tl::types::updates::ChannelDifferenceEmpty {
            r#final: true,
            pts: 56,
            timeout: None,
        }
        .into(),
    );

    assert_eq!(message_boxes.get_difference(), None);
    assert_eq!(message_boxes.check_deadlines(), next_updates_deadline());
    assert_eq!(
        message_boxes.session_state(),
        UpdatesState {
            channels: vec![ChannelState { id: 12, pts: 56 }],
            ..message_boxes.session_state()
        }
    );
}

#[test]
fn failed_channel_recovery_allows_contiguous_live_updates_without_skipping_gaps() {
    reset_time();
    let mut boxes = MessageBoxes::load(UpdatesState {
        channels: vec![
            ChannelState { id: 7, pts: 10 },
            ChannelState { id: 8, pts: 20 },
        ],
        ..Default::default()
    });
    boxes.end_channel_difference(PrematureEndReason::TemporaryServerIssues);
    assert_eq!(
        boxes.get_channel_difference(),
        Some(get_channel_difference(8, 20))
    );
    boxes.end_channel_difference(PrematureEndReason::TemporaryServerIssues);
    // A rejected attempt retains PTS. Contiguous delivery can resume without
    // skipping backlog that still requires an unavailable channel difference.
    let contiguous = boxes
        .process_updates(channel_updates(7, 0, 0, 11))
        .unwrap()
        .0;
    assert_eq!(contiguous.len(), 1);
    assert_eq!(contiguous[0].0, channel_update(7, 11));
    assert!(
        boxes
            .process_updates(channel_updates(7, 0, 0, 13))
            .unwrap()
            .0
            .is_empty()
    );
    // The channel's rejection delays the next attempt beyond the gap's own timeout.
    advance_time_by(Duration::from_secs(1));
    boxes.check_deadlines();
    assert_eq!(
        boxes.get_channel_difference(),
        Some(get_channel_difference(7, 11))
    );
    boxes.end_channel_difference(PrematureEndReason::TemporaryServerIssues);
    let mut channels = boxes.session_state().channels;
    channels.sort_by_key(|channel| channel.id);
    assert_eq!(
        channels,
        vec![
            ChannelState { id: 7, pts: 11 },
            ChannelState { id: 8, pts: 20 }
        ]
    );
    let sibling = boxes
        .process_updates(channel_updates(8, 0, 0, 21))
        .unwrap()
        .0;
    assert_eq!(sibling.len(), 1);
    assert_eq!(sibling[0].0, channel_update(8, 21));
    // Closing the gap delivers what had waited beyond it.
    let contiguous = boxes
        .process_updates(channel_updates(7, 0, 0, 12))
        .unwrap()
        .0;
    assert_eq!(contiguous.len(), 2);
    assert_eq!(contiguous[0].0, channel_update(7, 12));
    assert_eq!(contiguous[1].0, channel_update(7, 13));
    let mut channels = boxes.session_state().channels;
    channels.sort_by_key(|channel| channel.id);
    assert_eq!(
        channels,
        vec![
            ChannelState { id: 7, pts: 13 },
            ChannelState { id: 8, pts: 21 }
        ]
    );
}

fn delivered_channel_pts(batch: Option<UpdateAndPeers>) -> Vec<i32> {
    batch
        .map(|(updates, _, _)| updates)
        .unwrap_or_default()
        .into_iter()
        .map(|(update, state)| match state.message_box {
            Some(MessageBox::Channel { pts, .. }) => {
                assert_eq!(update, channel_update(7, pts));
                pts
            }
            other => panic!("expected channel state: {other:?}"),
        })
        .collect()
}

#[test]
fn failed_recovery_releases_postponed_channel_updates_without_skipping_gaps() {
    reset_time();
    let mut boxes = MessageBoxes::load(UpdatesState {
        channels: vec![ChannelState { id: 7, pts: 10 }],
        ..Default::default()
    });
    // 13 opens a gap and starts recovery; 11 arrives while recovery is pending.
    assert!(
        boxes
            .process_updates(channel_updates(7, 0, 0, 13))
            .unwrap()
            .0
            .is_empty()
    );
    advance_time_by(POSSIBLE_GAP_TIMEOUT);
    boxes.check_deadlines();
    assert_eq!(
        boxes.get_channel_difference(),
        Some(get_channel_difference(7, 10))
    );
    assert!(
        boxes
            .process_updates(channel_updates(7, 0, 0, 11))
            .unwrap()
            .0
            .is_empty()
    );
    assert_eq!(
        delivered_channel_pts(boxes.take_released_updates()),
        Vec::<i32>::new()
    );
    boxes.end_channel_difference(PrematureEndReason::TemporaryServerIssues);
    // 11 is contiguous. 13 is beyond the gap at 12, which it must not skip. It waits for
    // that gap to close or for the next attempt, which a growing delay keeps from looping.
    assert_eq!(delivered_channel_pts(boxes.take_released_updates()), [11]);
    assert_eq!(boxes.session_state().channels[0].pts, 11);
    advance_time_by(POSSIBLE_GAP_TIMEOUT);
    boxes.check_deadlines();
    assert_eq!(boxes.get_channel_difference(), None);
    // An update closing the gap delivers both.
    let contiguous = boxes.process_updates(channel_updates(7, 0, 0, 12)).unwrap();
    assert_eq!(delivered_channel_pts(Some(contiguous)), [12, 13]);
    assert!(
        boxes
            .process_updates(channel_updates(7, 0, 0, 15))
            .unwrap()
            .0
            .is_empty()
    );
    advance_time_by(Duration::from_secs(1));
    boxes.check_deadlines();
    assert_eq!(
        boxes.get_channel_difference(),
        Some(get_channel_difference(7, 13))
    );
}

#[test]
fn releasing_one_channel_keeps_updates_held_for_another_pending_channel() {
    reset_time();
    let mut boxes = MessageBoxes::load(UpdatesState {
        channels: vec![
            ChannelState { id: 7, pts: 10 },
            ChannelState { id: 8, pts: 20 },
        ],
        ..Default::default()
    });
    for (channel, pts) in [(7, 12), (8, 22)] {
        assert!(
            boxes
                .process_updates(channel_updates(channel, 0, 0, pts))
                .unwrap()
                .0
                .is_empty()
        );
    }
    advance_time_by(POSSIBLE_GAP_TIMEOUT);
    boxes.check_deadlines();
    for (channel, pts) in [(7, 11), (8, 21)] {
        assert!(
            boxes
                .process_updates(channel_updates(channel, 0, 0, pts))
                .unwrap()
                .0
                .is_empty()
        );
    }
    assert_eq!(boxes.held_updates.len(), 4);
    boxes.end_channel_difference(PrematureEndReason::TemporaryServerIssues);
    assert_eq!(
        delivered_channel_pts(boxes.take_released_updates()),
        [11, 12]
    );
    assert_eq!(boxes.held_updates.len(), 2);
    assert_eq!(
        boxes.get_channel_difference(),
        Some(get_channel_difference(8, 20))
    );
    boxes.end_channel_difference(PrematureEndReason::TemporaryServerIssues);
    let released = boxes.take_released_updates().unwrap().0;
    assert_eq!(released.len(), 2);
    assert_eq!(released[0].0, channel_update(8, 21));
    assert_eq!(released[1].0, channel_update(8, 22));
    assert!(boxes.held_updates.is_empty());
}

#[test]
fn abandoned_account_difference_keeps_channel_positions() {
    reset_time();
    let mut boxes = MessageBoxes::load(UpdatesState {
        pts: 100,
        qts: 5,
        date: 1,
        seq: 0,
        channels: vec![ChannelState { id: 7, pts: 10 }],
    });
    assert!(boxes.get_difference().is_some());
    boxes.discard_account_state();
    assert!(!boxes.has_primary_state());
    assert_eq!(boxes.get_difference(), None);
    let state = boxes.session_state();
    assert_eq!((state.pts, state.qts), (NO_PTS, NO_PTS));
    assert_eq!(state.channels, vec![ChannelState { id: 7, pts: 10 }]);
    // No queue accounting is left behind, and fresh account state works normally.
    assert_eq!(boxes.ready_diff_count, boxes.getting_diff_for.len());
    assert!(!boxes.getting_diff_for.contains(&Key::Common));
    assert!(!boxes.getting_diff_for.contains(&Key::Secondary));
    boxes.set_state(tl::types::updates::State {
        pts: 200,
        qts: 6,
        date: 2,
        seq: 3,
        unread_count: 0,
    });
    assert!(boxes.has_primary_state());
    assert_eq!(boxes.get_difference(), None);
}

#[test]
fn exhausted_grace_releases_held_updates_and_starts_a_fresh_position() {
    reset_time();
    let mut boxes = MessageBoxes::load(UpdatesState {
        channels: vec![ChannelState { id: 7, pts: 10 }],
        ..Default::default()
    });
    // The update beyond the gap itself is retained, not only later arrivals.
    assert!(
        boxes
            .process_updates(channel_updates(7, 0, 0, 13))
            .unwrap()
            .0
            .is_empty()
    );
    advance_time_by(POSSIBLE_GAP_TIMEOUT);
    boxes.check_deadlines();
    for deferral in 0..30 {
        boxes.defer_channel_difference(Duration::from_secs(1));
        assert!(boxes.take_released_updates().is_none());
        if deferral == 3 {
            for pts in [14, 15] {
                assert!(
                    boxes
                        .process_updates(channel_updates(7, 0, 0, pts))
                        .unwrap()
                        .0
                        .is_empty()
                );
            }
        }
        advance_time_by(Duration::from_secs(1));
        boxes.check_deadlines();
    }
    boxes.defer_channel_difference(Duration::from_secs(1));
    assert_eq!(boxes.session_state().channels, Vec::new());
    assert_eq!(
        delivered_channel_pts(boxes.take_released_updates()),
        [13, 14, 15]
    );
    assert_eq!(boxes.session_state().channels[0].pts, 15);
    assert!(boxes.held_updates.is_empty());
    assert_eq!(boxes.held_updates.capacity(), 0);
}

#[test]
fn successful_difference_supersedes_held_updates_without_delivering_them_twice() {
    reset_time();
    let mut boxes = MessageBoxes::load(UpdatesState {
        channels: vec![ChannelState { id: 7, pts: 10 }],
        ..Default::default()
    });
    assert!(
        boxes
            .process_updates(channel_updates(7, 0, 0, 13))
            .unwrap()
            .0
            .is_empty()
    );
    advance_time_by(POSSIBLE_GAP_TIMEOUT);
    boxes.check_deadlines();
    for pts in [11, 15, 16] {
        assert!(
            boxes
                .process_updates(channel_updates(7, 0, 0, pts))
                .unwrap()
                .0
                .is_empty()
        );
    }
    // A partial page keeps recovery pending and everything held.
    boxes.apply_channel_difference(tl::enums::updates::ChannelDifference::Difference(
        tl::types::updates::ChannelDifference {
            r#final: false,
            pts: 12,
            timeout: None,
            new_messages: vec![],
            other_updates: vec![],
            chats: vec![],
            users: vec![],
        },
    ));
    assert!(boxes.take_released_updates().is_none());
    assert_eq!(boxes.held_updates.len(), 4);
    boxes.apply_channel_difference(tl::enums::updates::ChannelDifference::Difference(
        tl::types::updates::ChannelDifference {
            r#final: true,
            pts: 15,
            timeout: None,
            new_messages: vec![],
            other_updates: vec![],
            chats: vec![],
            users: vec![],
        },
    ));
    // 11, 13 and 15 are covered by the difference. Only 16 is new.
    assert_eq!(delivered_channel_pts(boxes.take_released_updates()), [16]);
    assert_eq!(boxes.session_state().channels[0].pts, 16);
}

#[test]
fn held_count_zero_update_at_the_position_is_released_as_a_socket_update_would_apply() {
    reset_time();
    let mut boxes = MessageBoxes::load(UpdatesState {
        channels: vec![ChannelState { id: 7, pts: 10 }],
        ..Default::default()
    });
    assert!(
        boxes
            .process_updates(channel_updates(7, 0, 0, 12))
            .unwrap()
            .0
            .is_empty()
    );
    advance_time_by(POSSIBLE_GAP_TIMEOUT);
    boxes.check_deadlines();
    let read_inbox: tl::enums::Update = tl::types::UpdateReadChannelInbox {
        folder_id: None,
        channel_id: 7,
        max_id: 1,
        still_unread_count: 0,
        pts: 10,
    }
    .into();
    assert!(
        boxes
            .process_updates(UpdatesLike::Updates(
                tl::types::UpdateShort {
                    update: read_inbox.clone(),
                    date: 1,
                }
                .into()
            ))
            .unwrap()
            .0
            .is_empty()
    );
    boxes.end_channel_difference(PrematureEndReason::TemporaryServerIssues);
    let released = boxes.take_released_updates().unwrap().0;
    assert_eq!(released.len(), 1);
    assert_eq!(released[0].0, read_inbox);
}

#[test]
fn held_updates_are_bounded_across_channels_and_exclude_account_state() {
    reset_time();
    let mut boxes = MessageBoxes::load(UpdatesState {
        pts: 100,
        qts: 0,
        date: 1,
        seq: 0,
        channels: vec![ChannelState { id: 7, pts: 10 }],
    });
    boxes
        .process_updates(UpdatesLike::ConnectionClosed)
        .unwrap_err();
    assert!(
        boxes
            .process_updates(updates(0, 0, 105))
            .unwrap()
            .0
            .is_empty()
    );
    assert!(boxes.held_updates.is_empty());
    boxes
        .process_updates(UpdatesLike::Updates(
            tl::types::UpdateShort {
                update: tl::types::UpdateChannelTooLong {
                    channel_id: 7,
                    pts: None,
                }
                .into(),
                date: 1,
            }
            .into(),
        ))
        .unwrap();
    for pts in 11..11 + 600 {
        assert!(
            boxes
                .process_updates(channel_updates(7, 0, 0, pts))
                .unwrap()
                .0
                .is_empty()
        );
    }
    assert_eq!(boxes.held_updates.len(), MAX_HELD_UPDATES);
    assert!(boxes.held_overflowed);
    boxes.end_channel_difference(PrematureEndReason::Banned);
    let pts = delivered_channel_pts(boxes.take_released_updates());
    assert_eq!(pts.len(), MAX_HELD_UPDATES);
    assert_eq!(pts.first(), Some(&(11 + 600 - MAX_HELD_UPDATES as i32)));
    assert_eq!(pts.last(), Some(&610));
    assert!(!boxes.held_overflowed);
}

#[test]
fn test_next_raw_update_flow_channel_issues() {
    reset_time();
    let mut message_boxes = MessageBoxes::new();
    message_boxes.try_set_channel_state(12, 34);

    advance_time_by(NO_UPDATES_TIMEOUT);
    assert_eq!(message_boxes.check_deadlines(), Instant::now());
    assert!(message_boxes.get_channel_difference().is_some());

    message_boxes.end_channel_difference(PrematureEndReason::TemporaryServerIssues);
    assert!(message_boxes.get_channel_difference().is_none());
    assert_eq!(
        message_boxes.session_state(),
        UpdatesState {
            channels: vec![ChannelState { id: 12, pts: 34 }],
            ..message_boxes.session_state()
        }
    );
}

#[test]
fn test_next_raw_update_flow_channel_ban() {
    reset_time();
    let mut message_boxes = MessageBoxes::new();
    message_boxes.try_set_channel_state(12, 34);

    advance_time_by(NO_UPDATES_TIMEOUT);
    assert_eq!(message_boxes.check_deadlines(), Instant::now());
    assert!(message_boxes.get_channel_difference().is_some());

    message_boxes.end_channel_difference(PrematureEndReason::Banned);
    assert!(message_boxes.get_channel_difference().is_none());
    assert_eq!(
        message_boxes.session_state(),
        UpdatesState {
            channels: vec![],
            ..message_boxes.session_state()
        }
    );
}

#[test]
fn test_next_raw_update_flow_no_new_get_diff_if_already_fetching() {
    reset_time();
    let mut message_boxes = MessageBoxes::new();

    // t(0) = first channel registers timeout at t(3)
    message_boxes.try_set_channel_state(11, 12);
    let expected_first_deadline = next_updates_deadline();

    // t(2) = second and third channel register timeout at t(5)
    advance_time_by(2 * (NO_UPDATES_TIMEOUT / 3));
    message_boxes.try_set_channel_state(21, 22);
    message_boxes.try_set_channel_state(31, 32);
    let expected_second_deadline = next_updates_deadline();

    // t(4) = first channel times out
    advance_time_by(2 * (NO_UPDATES_TIMEOUT / 3));
    assert_eq!(message_boxes.check_deadlines(), expected_first_deadline);
    assert_eq!(
        message_boxes.get_channel_difference(),
        Some(get_channel_difference(11, 12))
    );

    // t(6) = second and third channel should have timed out by now, but didn't yet
    advance_time_by(2 * (NO_UPDATES_TIMEOUT / 3));
    message_boxes.end_channel_difference(PrematureEndReason::TemporaryServerIssues);
    assert_eq!(message_boxes.get_channel_difference(), None);

    // t(6) = checking deadlines now triggers difference for second and third
    assert_eq!(message_boxes.check_deadlines(), expected_second_deadline);
    assert_eq!(
        message_boxes.get_channel_difference(),
        Some(get_channel_difference(21, 22))
    );
    message_boxes.end_channel_difference(PrematureEndReason::TemporaryServerIssues);
    assert_eq!(
        message_boxes.get_channel_difference(),
        Some(get_channel_difference(31, 32))
    );
    message_boxes.end_channel_difference(PrematureEndReason::TemporaryServerIssues);
    assert_eq!(message_boxes.get_channel_difference(), None);
}

#[test]
fn test_process_socket_updates_flow_already_processed() {
    reset_time();
    let mut message_boxes = MessageBoxes::new();

    message_boxes.set_state(state(12, 34, 56, 78));

    for (seq, pts) in [
        (33, 57),     // seq already applied (<)
        (34, 57),     // seq already applied (=)
        (35, 55),     // seq ok (>), pts already applied (<)
        (35, 56),     // seq ok (>), pts already applied (=)
        (NO_PTS, 55), // seq ok (0), pts already applied (<)
        (NO_PTS, 56), // seq ok (0), pts already applied (=)
    ] {
        assert_eq!(
            message_boxes.process_updates(updates(13, seq, pts)), // date doesn't matter
            Ok((Vec::new(), Vec::new(), Vec::new()))
        );
    }
}

#[test]
fn test_process_socket_updates_flow_common_already_applied() {
    reset_time();
    let mut message_boxes = MessageBoxes::new();

    message_boxes.set_state(state(12, 34, 56, 78));

    for (seq, pts) in [
        (33, 57),     // seq already applied (<)
        (34, 57),     // seq already applied (=)
        (35, 55),     // seq ok (>), pts already applied (<)
        (35, 56),     // seq ok (>), pts already applied (=)
        (NO_PTS, 55), // seq ok (0), pts already applied (<)
        (NO_PTS, 56), // seq ok (0), pts already applied (=)
    ] {
        assert_eq!(
            message_boxes.process_updates(updates(13, seq, pts)), // date doesn't matter
            Ok((Vec::new(), Vec::new(), Vec::new()))
        );
    }
}

#[test]
fn test_process_socket_updates_flow_common_difference_ok() {
    reset_time();
    let mut boxes = MessageBoxes::new();
    boxes.set_state(state(12, 34, 100, 78));
    boxes.try_begin_get_diff(super::Key::Common);
    let live = tl::enums::Update::Config;
    let socket = UpdatesLike::Updates(
        tl::types::Updates {
            updates: vec![update(101), live.clone()],
            users: vec![],
            chats: vec![],
            date: 99,
            seq: 99,
        }
        .into(),
    );
    let delivered = boxes.process_updates(socket).unwrap().0;
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].0, live);
    assert_eq!((delivered[0].1.date, delivered[0].1.seq), (12, 34));
    assert_eq!(boxes.get_difference(), Some(get_difference(12, 100, 78)));
    let (delivered, _, _) = boxes
        .try_apply_difference(
            tl::types::updates::Difference {
                new_messages: vec![],
                new_encrypted_messages: vec![],
                other_updates: vec![update(101)],
                chats: vec![],
                users: vec![],
                state: state(13, 35, 101, 78).into(),
            }
            .into(),
        )
        .unwrap();
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].0, update(101));
    assert!(
        boxes
            .process_updates(updates(NO_DATE, NO_SEQ, 101))
            .unwrap()
            .0
            .is_empty()
    );
}

#[test]
fn live_qts_without_primary_state_does_not_create_orphan_secondary() {
    let mut boxes = MessageBoxes::new();
    for qts in [1, 3] {
        let update = tl::enums::Update::BotStopped(tl::types::UpdateBotStopped {
            stopped: true,
            user_id: 1,
            date: 1,
            qts,
        });
        let delivered = boxes
            .process_updates(UpdatesLike::Updates(
                tl::types::UpdateShort {
                    update: update.clone(),
                    date: 1,
                }
                .into(),
            ))
            .unwrap()
            .0;
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].0, update);
        assert!(delivered[0].1.message_box.is_none());
        assert!(boxes.is_empty());
        assert!(boxes.get_difference().is_none());
    }
}

#[test]
fn peerless_channel_message_preserves_non_pts_siblings_and_difference_progress() {
    let peerless = tl::enums::Update::NewChannelMessage(tl::types::UpdateNewChannelMessage {
        message: tl::types::MessageEmpty {
            id: 1,
            peer_id: None,
        }
        .into(),
        pts: 101,
        pts_count: 1,
    });
    let sibling = tl::enums::Update::Config;
    let mut boxes = MessageBoxes::new();
    boxes.set_state(state(12, 34, 100, 0));
    let delivered = boxes
        .process_updates(UpdatesLike::Updates(
            tl::types::Updates {
                updates: vec![peerless.clone(), sibling.clone()],
                users: vec![],
                chats: vec![],
                date: 13,
                seq: 35,
            }
            .into(),
        ))
        .unwrap()
        .0;
    assert!(delivered.iter().any(|(u, _)| *u == sibling));
    boxes.try_begin_get_diff(super::Key::Common);
    boxes
        .try_apply_difference(
            tl::types::updates::Difference {
                new_messages: vec![],
                new_encrypted_messages: vec![],
                other_updates: vec![peerless],
                chats: vec![],
                users: vec![],
                state: state(14, 36, 102, 0).into(),
            }
            .into(),
        )
        .unwrap();
    assert!(boxes.get_difference().is_none());
}

#[test]
fn test_process_socket_updates_flow_common_ok() {
    reset_time();
    let mut message_boxes = MessageBoxes::new();

    message_boxes.set_state(state(12, 34, 56, 78));

    assert_eq!(
        message_boxes.process_updates(updates(NO_DATE, NO_SEQ, 57)),
        updates_ok(12, 34, 57)
    );
    assert_eq!(
        message_boxes.process_updates(updates(NO_DATE, 35, 58)),
        updates_ok(12, 35, 58)
    );
    assert_eq!(
        message_boxes.process_updates(updates(13, 36, 59)),
        updates_ok(13, 36, 59)
    );
    assert_eq!(
        message_boxes.process_updates(updates(14, NO_SEQ, 60)),
        updates_ok(14, 36, 60)
    );
}

fn widget_data(peer: tl::enums::Peer, pts: i32) -> tl::enums::Update {
    tl::enums::Update::MessageWidgetData(tl::types::UpdateMessageWidgetData {
        peer,
        msg_id: 1,
        public_data: None,
        public_data_hash: None,
        chat_data: Some("state".into()),
        pts,
        pts_count: 1,
    })
}

fn one_update(update: tl::enums::Update) -> UpdatesLike {
    UpdatesLike::Updates(tl::enums::Updates::Updates(tl::types::Updates {
        updates: vec![update],
        users: Vec::new(),
        chats: Vec::new(),
        date: NO_DATE,
        seq: NO_SEQ,
    }))
}

// Layer 233. TDLib, Telegram Desktop and Telegram for Android apply this update to the
// common sequence; treated as an update without one, each would be followed by a gap.
#[test]
fn widget_data_advances_the_common_sequence() {
    reset_time();
    let mut boxes = MessageBoxes::new();
    boxes.set_state(state(12, 34, 56, 78));
    let user = tl::enums::Peer::User(tl::types::PeerUser { user_id: 1 });
    let chat = tl::enums::Peer::Chat(tl::types::PeerChat { chat_id: 2 });

    for (peer, pts) in [(user.clone(), 57), (chat, 58)] {
        let (delivered, _, _) = boxes
            .process_updates(one_update(widget_data(peer, pts)))
            .unwrap();
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].1.message_box, Some(MessageBox::Common { pts }));
        assert_eq!(boxes.session_state().pts, pts);
    }
    // The update after it continues the same sequence: no gap and no difference request.
    assert_eq!(
        boxes.process_updates(updates(NO_DATE, NO_SEQ, 59)),
        updates_ok(12, 34, 59)
    );
    assert_eq!(boxes.get_difference(), None);

    // One already applied is not delivered again.
    let (delivered, _, _) = boxes
        .process_updates(one_update(widget_data(user.clone(), 58)))
        .unwrap();
    assert!(delivered.is_empty());
    assert_eq!(boxes.session_state().pts, 59);

    // One ahead of the sequence waits as a possible gap and is recovered by a difference.
    let (delivered, _, _) = boxes
        .process_updates(one_update(widget_data(user, 61)))
        .unwrap();
    assert!(delivered.is_empty());
    assert_eq!(boxes.session_state().pts, 59);
    advance_time_by(POSSIBLE_GAP_TIMEOUT);
    boxes.check_deadlines();
    assert_eq!(boxes.get_difference(), Some(get_difference(12, 59, 78)));
}

// The schema has no channel form of the update and no client applies it to a channel's
// sequence. TDLib rejects one naming a channel; here that asks for the account difference
// without moving any position, as for `updatePinnedMessages`.
#[test]
fn widget_data_for_a_channel_is_rejected_without_advancing() {
    reset_time();
    let mut boxes = MessageBoxes::new();
    boxes.set_state(state(12, 34, 56, 78));
    let channel = tl::enums::Peer::Channel(tl::types::PeerChannel { channel_id: 3 });

    assert_eq!(
        boxes.process_updates(one_update(widget_data(channel, 57))),
        Err(Gap)
    );
    assert_eq!(boxes.session_state().pts, 56);
    assert!(boxes.session_state().channels.is_empty());
    assert_eq!(boxes.get_difference(), Some(get_difference(12, 56, 78)));
}

// One of each bot update that carries `qts` and used to be delivered without advancing
// it. Telegram documents "New qts value" on each, and TDLib applies each in sequence.
fn qts_updates(first_qts: i32) -> Vec<tl::enums::Update> {
    let peer = tl::enums::Peer::User(tl::types::PeerUser { user_id: 1 });
    vec![
        tl::types::UpdateMessagePollVote {
            poll_id: 1,
            peer: peer.clone(),
            options: Vec::new(),
            positions: Vec::new(),
            qts: first_qts,
        }
        .into(),
        tl::types::UpdateBotBusinessConnect {
            connection: tl::types::BotBusinessConnection {
                disabled: false,
                connection_id: "connection".into(),
                user_id: 1,
                dc_id: 2,
                date: 3,
                rights: None,
            }
            .into(),
            qts: first_qts + 1,
        }
        .into(),
        tl::types::UpdateManagedBot {
            user_id: 1,
            bot_id: 2,
            qts: first_qts + 2,
        }
        .into(),
        tl::types::UpdateBotGuestChatQuery {
            query_id: 1,
            message: tl::types::MessageEmpty {
                id: 1,
                peer_id: None,
            }
            .into(),
            reference_messages: None,
            qts: first_qts + 3,
        }
        .into(),
        tl::types::UpdateBotStarsSubscription {
            canceled: false,
            payment_failed: false,
            restored: false,
            user_id: 1,
            payload: Vec::new(),
            qts: first_qts + 4,
        }
        .into(),
        // Three messages deleted at once are one event.
        tl::types::UpdateBotDeleteBusinessMessage {
            connection_id: "connection".into(),
            peer,
            messages: vec![1, 2, 3],
            qts: first_qts + 5,
        }
        .into(),
    ]
}

#[test]
fn bot_updates_with_qts_advance_the_secondary_sequence() {
    reset_time();
    let mut boxes = MessageBoxes::new();
    boxes.set_state(state(12, 34, 56, 78));

    for (index, update) in qts_updates(79).into_iter().enumerate() {
        let qts = 79 + index as i32;
        let (delivered, _, _) = boxes.process_updates(one_update(update)).unwrap();
        assert_eq!(delivered.len(), 1, "update {index}");
        assert_eq!(
            delivered[0].1.message_box,
            Some(MessageBox::Secondary { qts }),
            "update {index}"
        );
        assert_eq!(boxes.session_state().qts, qts);
    }

    // None of them left a gap behind: the next event applies and nothing is requested.
    let stopped = tl::types::UpdateBotStopped {
        user_id: 1,
        date: 2,
        stopped: true,
        qts: 85,
    };
    let (delivered, _, _) = boxes.process_updates(one_update(stopped.into())).unwrap();
    assert_eq!(delivered.len(), 1);
    assert_eq!(boxes.session_state().qts, 85);
    advance_time_by(POSSIBLE_GAP_TIMEOUT);
    boxes.check_deadlines();
    assert_eq!(boxes.get_difference(), None);

    // Delivered once: the same events arriving again are recognised as applied.
    for update in qts_updates(79) {
        let (delivered, _, _) = boxes.process_updates(one_update(update)).unwrap();
        assert!(delivered.is_empty());
    }
    assert_eq!(boxes.session_state().qts, 85);

    // Two in one batch, the second a deletion naming no message: one step each.
    let peer = tl::enums::Peer::User(tl::types::PeerUser { user_id: 1 });
    let batch = UpdatesLike::Updates(tl::enums::Updates::Updates(tl::types::Updates {
        updates: vec![
            tl::types::UpdateManagedBot {
                user_id: 1,
                bot_id: 2,
                qts: 86,
            }
            .into(),
            tl::types::UpdateBotDeleteBusinessMessage {
                connection_id: "connection".into(),
                peer,
                messages: Vec::new(),
                qts: 87,
            }
            .into(),
        ],
        users: Vec::new(),
        chats: Vec::new(),
        date: NO_DATE,
        seq: NO_SEQ,
    }));
    let (delivered, _, _) = boxes.process_updates(batch).unwrap();
    assert_eq!(delivered.len(), 2);
    assert_eq!(boxes.session_state().qts, 87);
    assert_eq!(boxes.get_difference(), None);
}

// Before login there is no account position to hold a `qts`: these are delivered as they
// come, as the other bot updates are.
#[test]
fn bot_updates_with_qts_are_delivered_before_login() {
    reset_time();
    let mut boxes = MessageBoxes::new();
    for update in qts_updates(79) {
        let (delivered, _, _) = boxes.process_updates(one_update(update)).unwrap();
        assert_eq!(delivered.len(), 1);
        assert_eq!(delivered[0].1.message_box, None);
    }
    assert_eq!(boxes.session_state().qts, NO_PTS);
}

#[test]
fn test_process_socket_updates_flow_common_seq_gap() {
    reset_time();
    let mut message_boxes = MessageBoxes::new();

    message_boxes.set_state(state(12, 34, 56, 78));

    assert_eq!(message_boxes.process_updates(updates(13, 36, 57)), Err(Gap));
    assert_eq!(
        message_boxes.get_difference(),
        Some(get_difference(12, 56, 78))
    );
}

#[test]
fn test_process_socket_updates_flow_common_pts_possible_gap_resolves() {
    reset_time();
    let mut message_boxes = MessageBoxes::new();

    message_boxes.set_state(state(12, 34, 56, 78));

    // No additional updates before gap resolves.
    assert_eq!(
        message_boxes.process_updates(updates(NO_DATE, NO_SEQ, 58)),
        Ok((Vec::new(), Vec::new(), Vec::new()))
    );
    advance_time_by(POSSIBLE_GAP_TIMEOUT / 2);
    message_boxes.check_deadlines();
    assert_eq!(
        message_boxes.process_updates(updates(NO_DATE, NO_SEQ, 57)),
        merge_updates(vec![updates_ok(12, 34, 57), updates_ok(12, 34, 58)])
    );

    // One additional update before gap resolves.
    assert_eq!(
        message_boxes.process_updates(updates(NO_DATE, NO_SEQ, 61)),
        Ok((Vec::new(), Vec::new(), Vec::new()))
    );
    advance_time_by(POSSIBLE_GAP_TIMEOUT / 4);
    message_boxes.check_deadlines();
    assert_eq!(
        message_boxes.process_updates(updates(NO_DATE, NO_SEQ, 60)),
        Ok((Vec::new(), Vec::new(), Vec::new()))
    );
    advance_time_by(POSSIBLE_GAP_TIMEOUT / 4);
    message_boxes.check_deadlines();
    assert_eq!(
        message_boxes.process_updates(updates(NO_DATE, NO_SEQ, 59)),
        merge_updates(vec![
            updates_ok(12, 34, 59),
            updates_ok(12, 34, 60),
            updates_ok(12, 34, 61),
        ])
    );
}

#[test]
fn test_process_socket_updates_flow_common_pts_gap() {
    reset_time();
    let mut message_boxes = MessageBoxes::new();

    message_boxes.set_state(state(12, 34, 56, 78));

    assert_eq!(
        message_boxes.process_updates(updates(NO_DATE, NO_SEQ, 58)),
        Ok((Vec::new(), Vec::new(), Vec::new()))
    );
    advance_time_by(3 * (POSSIBLE_GAP_TIMEOUT / 2));
    assert_eq!(message_boxes.get_difference(), None);
    message_boxes.check_deadlines();
    assert_eq!(
        message_boxes.get_difference(),
        Some(get_difference(12, 56, 78))
    );
}

#[test]
fn test_process_socket_updates_trickle_causes_gap() {
    reset_time();
    let gap_deadline: Instant = Instant::now() + POSSIBLE_GAP_TIMEOUT;
    let mut message_boxes = MessageBoxes::new();
    message_boxes.set_state(state(12, 34, 56, 78));

    assert_eq!(
        message_boxes.process_updates(updates(NO_DATE, NO_SEQ, 58)),
        Ok((Vec::new(), Vec::new(), Vec::new()))
    );
    advance_time_by(2 * (POSSIBLE_GAP_TIMEOUT / 5));
    assert_eq!(message_boxes.check_deadlines(), gap_deadline);
    assert_eq!(
        message_boxes.process_updates(updates(NO_DATE, NO_SEQ, 59)),
        Ok((Vec::new(), Vec::new(), Vec::new()))
    );
    advance_time_by(2 * (POSSIBLE_GAP_TIMEOUT / 5));
    assert_eq!(message_boxes.check_deadlines(), gap_deadline);
    assert_eq!(
        message_boxes.process_updates(updates(NO_DATE, NO_SEQ, 60)),
        Ok((Vec::new(), Vec::new(), Vec::new()))
    );
    advance_time_by(2 * (POSSIBLE_GAP_TIMEOUT / 5));
    assert_eq!(message_boxes.check_deadlines(), gap_deadline);
    assert!(message_boxes.get_difference().is_some());
}

#[test]
fn test_process_socket_update_for_new_channel_dispatches_first_update() {
    reset_time();
    let mut message_boxes = MessageBoxes::new();
    let (channel_id, date, seq, pts) = (12, 0, 0, 78);
    let ret = message_boxes.process_updates(channel_updates(channel_id, date, seq, pts));
    assert_eq!(
        ret,
        Ok((
            vec![(
                channel_update(channel_id, pts),
                State {
                    date,
                    seq,
                    message_box: Some(MessageBox::Channel { channel_id, pts })
                }
            )],
            vec![],
            vec![]
        ))
    );
}

#[test]
fn test_common_and_channel_difference_do_not_duplicate_message() {
    reset_time();
    let state = UpdatesState {
        pts: 12,
        qts: 34,
        date: 56,
        seq: 78,
        channels: vec![ChannelState { id: 43, pts: 21 }],
    };
    let mut message_boxes = MessageBoxes::load(state.clone());
    let message = tl::enums::Message::Empty(tl::types::MessageEmpty {
        id: 42,
        peer_id: Some(tl::enums::Peer::Channel(tl::types::PeerChannel {
            channel_id: 43,
        })),
    });
    assert!(
        message_boxes
            .getting_diff_for
            .contains(&super::Key::Channel(43))
    );
    let (updates, _, _) = message_boxes.apply_difference(
        tl::enums::updates::Difference::Difference(tl::types::updates::Difference {
            new_messages: vec![],
            new_encrypted_messages: vec![],
            other_updates: vec![tl::enums::Update::NewChannelMessage(
                tl::types::UpdateNewChannelMessage {
                    message: message.clone(),
                    pts: 23,
                    pts_count: 1,
                },
            )],
            chats: vec![],
            users: vec![],
            state: tl::enums::updates::State::State(tl::types::updates::State {
                pts: 14,
                qts: 0,
                date: NO_DATE,
                seq: NO_SEQ,
                unread_count: 0,
            }),
        }),
    );
    assert_eq!(updates, vec![]);

    let (updates, _, _) = message_boxes.apply_channel_difference(
        tl::enums::updates::ChannelDifference::Difference(tl::types::updates::ChannelDifference {
            r#final: true,
            pts: 23,
            timeout: None,
            new_messages: vec![message.clone()],
            other_updates: vec![],
            chats: vec![],
            users: vec![],
        }),
    );

    assert_eq!(updates.len(), 1);
}

#[test]
fn zero_counter_updates_preserve_siblings_on_socket_and_difference_paths() {
    for path in 0..3 {
        let zero_updates = vec![
            tl::types::UpdateChannelParticipant {
                via_chatlist: false,
                channel_id: 7,
                date: 13,
                actor_id: 2,
                user_id: 3,
                prev_participant: None,
                new_participant: Some(
                    tl::types::ChannelParticipant {
                        user_id: 3,
                        date: 13,
                        subscription_until_date: None,
                        rank: None,
                    }
                    .into(),
                ),
                invite: None,
                qts: 0,
            }
            .into(),
            tl::types::UpdateBotStopped {
                stopped: true,
                user_id: 1,
                date: 1,
                qts: 0,
            }
            .into(),
            update(0),
            channel_update(7, 0),
            tl::enums::Update::Config,
        ];
        let sibling = if path == 2 {
            channel_update(7, 11)
        } else {
            update(101)
        };
        let mut other_updates = zero_updates.clone();
        other_updates.push(sibling.clone());
        let mut boxes = MessageBoxes::new();
        boxes.set_state(state(12, 34, 100, 78));
        boxes.try_set_channel_state(7, 10);
        let delivered = match path {
            0 => boxes.process_updates(UpdatesLike::Updates(
                tl::types::Updates {
                    updates: other_updates,
                    users: vec![],
                    chats: vec![],
                    date: 13,
                    seq: 35,
                }
                .into(),
            )),
            1 => {
                boxes.try_begin_get_diff(super::Key::Common);
                boxes.try_apply_difference(
                    tl::types::updates::Difference {
                        new_messages: vec![],
                        new_encrypted_messages: vec![],
                        other_updates,
                        chats: vec![],
                        users: vec![],
                        state: state(13, 35, 101, 79).into(),
                    }
                    .into(),
                )
            }
            _ => {
                boxes.try_begin_get_diff(super::Key::Channel(7));
                boxes.try_apply_channel_difference(
                    tl::types::updates::ChannelDifference {
                        r#final: true,
                        pts: 11,
                        timeout: None,
                        new_messages: vec![],
                        other_updates,
                        chats: vec![],
                        users: vec![],
                    }
                    .into(),
                )
            }
        }
        .unwrap()
        .0;
        assert_eq!(delivered.len(), zero_updates.len() + 1);
        for expected in zero_updates {
            assert!(
                delivered
                    .iter()
                    .any(|(u, s)| *u == expected && s.message_box.is_none())
            );
        }
        assert!(
            delivered
                .iter()
                .any(|(u, s)| *u == sibling && s.message_box.is_some())
        );
        assert_eq!(boxes.session_state().qts, if path == 1 { 79 } else { 78 });
        assert_eq!(boxes.session_state().pts, if path == 2 { 100 } else { 101 });
        assert_eq!(
            boxes.session_state().channels[0].pts,
            if path == 2 { 11 } else { 10 }
        );
        assert!(boxes.get_difference().is_none());
        assert!(boxes.get_channel_difference().is_none());
    }
    for invalid in [
        tl::types::UpdateDeleteMessages {
            messages: vec![],
            pts: 0,
            pts_count: -1,
        }
        .into(),
        channel_update(-1, 0),
    ] {
        let mut boxes = MessageBoxes::new();
        boxes.set_state(state(12, 34, 100, 78));
        let before = boxes.session_state();
        assert!(
            boxes
                .process_updates(UpdatesLike::Updates(
                    tl::types::UpdateShort {
                        update: invalid,
                        date: 13
                    }
                    .into()
                ))
                .is_err()
        );
        assert_eq!(boxes.session_state(), before);
    }
}

#[test]
fn malformed_difference_elements_do_not_block_account_or_channel_progress() {
    for path in 0..3 {
        let mut boxes = MessageBoxes::new();
        boxes.set_state(state(12, 34, 100, 78));
        boxes.try_set_channel_state(7, 10);
        let sibling = if path == 2 {
            channel_update(7, 11)
        } else {
            update(101)
        };
        let other_updates = vec![
            update(-1),
            channel_update(-1, 11),
            tl::enums::Update::Config,
            sibling.clone(),
        ];
        let new_messages = vec![
            tl::types::MessageEmpty {
                id: 42,
                peer_id: None,
            }
            .into(),
        ];
        let delivered = if path == 2 {
            boxes.try_begin_get_diff(super::Key::Channel(7));
            boxes.try_apply_channel_difference(
                tl::types::updates::ChannelDifference {
                    r#final: false,
                    pts: 11,
                    timeout: None,
                    new_messages,
                    other_updates,
                    chats: vec![],
                    users: vec![],
                }
                .into(),
            )
        } else {
            boxes.try_begin_get_diff(super::Key::Common);
            let difference = if path == 0 {
                tl::types::updates::Difference {
                    new_messages,
                    new_encrypted_messages: vec![],
                    other_updates,
                    chats: vec![],
                    users: vec![],
                    state: state(13, 35, 101, 78).into(),
                }
                .into()
            } else {
                tl::types::updates::DifferenceSlice {
                    new_messages,
                    new_encrypted_messages: vec![],
                    other_updates,
                    chats: vec![],
                    users: vec![],
                    intermediate_state: state(13, 35, 101, 78).into(),
                }
                .into()
            };
            boxes.try_apply_difference(difference)
        }
        .unwrap()
        .0;
        assert_eq!(delivered.len(), 3);
        assert!(delivered.iter().any(|(u, _)| *u == sibling));
        assert!(
            delivered
                .iter()
                .any(|(u, _)| *u == tl::enums::Update::Config)
        );
        if path == 2 {
            assert_eq!(boxes.get_channel_difference().unwrap().pts, 11);
            boxes
                .try_apply_channel_difference(
                    tl::types::updates::ChannelDifferenceEmpty {
                        r#final: true,
                        pts: 12,
                        timeout: None,
                    }
                    .into(),
                )
                .unwrap();
            assert!(boxes.get_channel_difference().is_none());
        } else {
            assert_eq!(boxes.session_state().pts, 101);
            if path == 1 {
                assert_eq!(boxes.get_difference().unwrap().pts, 101);
                boxes
                    .try_apply_difference(
                        tl::types::updates::DifferenceEmpty { date: 14, seq: 36 }.into(),
                    )
                    .unwrap();
            }
            assert!(boxes.get_difference().is_none());
        }
    }
}

#[test]
fn tolerant_saved_state_repair_preserves_valid_channels() {
    let valid_channels = vec![
        ChannelState { id: 7, pts: 10 },
        ChannelState { id: 20, pts: 30 },
    ];
    for invalid_counter in 0..5 {
        let mut saved = UpdatesState {
            pts: 100,
            qts: 78,
            date: 12,
            seq: 34,
            channels: valid_channels.clone(),
        };
        saved.channels.extend([
            ChannelState { id: -1, pts: 5 },
            ChannelState {
                id: i64::MAX,
                pts: 5,
            },
            ChannelState { id: 8, pts: -1 },
        ]);
        match invalid_counter {
            0 => saved.pts = -1,
            1 => saved.qts = -1,
            2 => saved.date = -1,
            3 => saved.seq = -1,
            _ => {}
        }
        assert!(MessageBoxes::try_load(saved.clone()).is_err());
        let boxes = MessageBoxes::load(saved);
        assert_eq!(boxes.session_state().channels, valid_channels);
        assert_eq!(boxes.has_primary_state(), invalid_counter != 0);
        let repaired = boxes.session_state();
        let expected = match invalid_counter {
            0 => (0, 0, 0, 0),
            1 => (100, 0, 12, 34),
            2 => (100, 78, 0, 34),
            3 => (100, 78, 12, 0),
            _ => (100, 78, 12, 34),
        };
        assert_eq!(
            (repaired.pts, repaired.qts, repaired.date, repaired.seq),
            expected
        );
        if invalid_counter != 0 {
            assert_eq!(boxes.get_difference().unwrap().pts, 100);
        }
    }
}

#[test]
fn deferred_channel_keeps_backlog_and_does_not_steal_another_channels_response() {
    reset_time();
    let mut boxes = MessageBoxes::load(UpdatesState {
        channels: vec![
            ChannelState { id: 7, pts: 10 },
            ChannelState { id: 8, pts: 20 },
        ],
        ..Default::default()
    });
    assert!(
        boxes
            .process_updates(channel_updates(7, 0, 0, 11))
            .unwrap()
            .0
            .is_empty()
    );
    boxes.defer_channel_difference(Duration::from_secs(5));
    assert_eq!(
        boxes.get_channel_difference(),
        Some(get_channel_difference(8, 20))
    );
    // Readiness must not change the target while channel 8's RPC is in flight.
    advance_time_by(Duration::from_secs(5));
    boxes
        .try_apply_channel_difference(
            tl::types::updates::ChannelDifferenceEmpty {
                r#final: true,
                pts: 21,
                timeout: None,
            }
            .into(),
        )
        .unwrap();
    assert_eq!(
        boxes.session_state().channels,
        vec![
            ChannelState { id: 7, pts: 10 },
            ChannelState { id: 8, pts: 21 }
        ]
    );
    assert!(boxes.get_channel_difference().is_none());
    assert!(
        boxes
            .process_updates(channel_updates(7, 0, 0, 12))
            .unwrap()
            .0
            .is_empty()
    );
    assert_eq!(boxes.check_deadlines(), Instant::now());
    assert_eq!(
        boxes.get_channel_difference(),
        Some(get_channel_difference(7, 10))
    );
    let backlog = vec![channel_update(7, 11), channel_update(7, 12)];
    let delivered = boxes
        .try_apply_channel_difference(
            tl::types::updates::ChannelDifference {
                r#final: true,
                pts: 12,
                timeout: None,
                new_messages: vec![],
                other_updates: backlog.clone(),
                chats: vec![],
                users: vec![],
            }
            .into(),
        )
        .unwrap()
        .0;
    assert_eq!(
        delivered.into_iter().map(|(u, _)| u).collect::<Vec<_>>(),
        backlog
    );
    assert_eq!(
        boxes
            .process_updates(channel_updates(7, 0, 0, 13))
            .unwrap()
            .0
            .len(),
        1
    );
}

#[test]
fn all_deferred_channels_wait_until_the_earliest_retry() {
    reset_time();
    let mut boxes = MessageBoxes::load(UpdatesState {
        channels: vec![
            ChannelState { id: 7, pts: 10 },
            ChannelState { id: 8, pts: 20 },
        ],
        ..Default::default()
    });
    boxes.defer_channel_difference(Duration::from_secs(5));
    boxes.defer_channel_difference(Duration::from_secs(10));
    assert!(boxes.get_channel_difference().is_none());
    let retry = Instant::now() + Duration::from_secs(5);
    assert_eq!(boxes.check_deadlines(), retry);
    assert_eq!(boxes.check_deadlines(), retry);
    advance_time_by(Duration::from_secs(5));
    assert_eq!(boxes.check_deadlines(), Instant::now());
    assert_eq!(
        boxes.get_channel_difference(),
        Some(get_channel_difference(7, 10))
    );
}

// A live channel, not one still catching up from saved state, so that recovery really
// starts from a gap: the conversions under test only apply to that path.
fn live_channel(id: i64, pts: i32) -> MessageBoxes {
    let mut boxes = MessageBoxes::new();
    boxes.try_set_channel_state(id, pts);
    boxes
}

fn empty_channel_difference(pts: i32) -> tl::enums::updates::ChannelDifference {
    tl::types::updates::ChannelDifferenceEmpty {
        r#final: true,
        pts,
        timeout: None,
    }
    .into()
}

#[test]
fn final_difference_short_of_a_held_update_does_not_restart_recovery() {
    reset_time();
    let mut boxes = live_channel(7, 10);
    // 13 opens a real gap, whose timer moves it to the held updates.
    assert!(
        boxes
            .process_updates(channel_updates(7, 0, 0, 13))
            .unwrap()
            .0
            .is_empty()
    );
    assert_eq!(boxes.get_channel_difference(), None);
    advance_time_by(POSSIBLE_GAP_TIMEOUT);
    boxes.check_deadlines();
    assert_eq!(
        boxes.get_channel_difference(),
        Some(get_channel_difference(7, 10))
    );
    assert_eq!(boxes.held_updates.len(), 1);

    // Replication lag: Telegram calls 11 final, although 13 was already delivered.
    boxes
        .try_apply_channel_difference(empty_channel_difference(11))
        .unwrap();
    assert!(boxes.take_released_updates().is_none());
    assert!(boxes.held_updates.is_empty());
    // Not at once, which would repeat for as long as the lag lasts.
    advance_time_by(Duration::from_secs(1) - Duration::from_millis(1));
    boxes.check_deadlines();
    assert_eq!(boxes.get_channel_difference(), None);
    // But soon, with no other traffic to report the gap: 13 reached this client.
    advance_time_by(Duration::from_millis(1));
    boxes.check_deadlines();
    assert_eq!(
        boxes.get_channel_difference(),
        Some(get_channel_difference(7, 11))
    );
    // A difference with nothing left to reach ends it, at the usual timeout.
    boxes
        .try_apply_channel_difference(empty_channel_difference(13))
        .unwrap();
    assert_eq!(boxes.entry(Key::Channel(7)).unwrap().recovery_failures, 0);
    advance_time_by(Duration::from_secs(120));
    boxes.check_deadlines();
    assert_eq!(boxes.get_channel_difference(), None);
}

#[test]
fn an_unreadable_channel_reply_can_retry_without_the_rejection_delay() {
    reset_time();
    let mut boxes = live_channel(7, 10);
    boxes.process_updates(channel_updates(7, 0, 0, 13)).unwrap();
    advance_time_by(POSSIBLE_GAP_TIMEOUT);
    boxes.check_deadlines();
    assert!(boxes.get_channel_difference().is_some());
    boxes.end_channel_difference(PrematureEndReason::TemporaryServerIssues);
    boxes.check_deadlines();
    assert_eq!(boxes.get_channel_difference(), None);
    // No new traffic is needed, and the reply does not count as a rejection.
    boxes.retry_channel_without_delay(7);
    boxes.check_deadlines();
    assert_eq!(
        boxes.get_channel_difference(),
        Some(get_channel_difference(7, 10))
    );
    assert_eq!(boxes.entry(Key::Channel(7)).unwrap().recovery_failures, 0);
}

#[test]
fn an_update_closing_the_gap_delivers_what_a_short_difference_left_held() {
    reset_time();
    let mut boxes = live_channel(7, 10);
    boxes.process_updates(channel_updates(7, 0, 0, 13)).unwrap();
    advance_time_by(POSSIBLE_GAP_TIMEOUT);
    boxes.check_deadlines();
    boxes
        .try_apply_channel_difference(empty_channel_difference(11))
        .unwrap();
    assert!(boxes.take_released_updates().is_none());
    // Arriving inside the retry window: its normal deadline must not lose the other.
    let (applied, _, _) = boxes.process_updates(channel_updates(7, 0, 0, 12)).unwrap();
    assert_eq!(
        applied
            .into_iter()
            .map(|(update, _)| update)
            .collect::<Vec<_>>(),
        vec![channel_update(7, 12), channel_update(7, 13)]
    );
    assert_eq!(boxes.session_state().channels[0].pts, 13);
    advance_time_by(Duration::from_secs(2));
    boxes.check_deadlines();
    assert_eq!(boxes.get_channel_difference(), None);
}

#[test]
fn a_rejected_channel_also_waits_when_telegram_says_it_is_too_long() {
    reset_time();
    let mut boxes = live_channel(7, 10);
    boxes.process_updates(channel_updates(7, 0, 0, 13)).unwrap();
    advance_time_by(POSSIBLE_GAP_TIMEOUT);
    boxes.check_deadlines();
    boxes.end_channel_difference(PrematureEndReason::TemporaryServerIssues);
    let too_long = UpdatesLike::Updates(tl::enums::Updates::UpdateShort(tl::types::UpdateShort {
        update: tl::types::UpdateChannelTooLong {
            channel_id: 7,
            pts: Some(20),
        }
        .into(),
        date: 0,
    }));
    boxes.process_updates(too_long).unwrap();
    assert_eq!(boxes.get_channel_difference(), None);
    assert_eq!(boxes.ready_diff_count, boxes.getting_diff_for.len());
    advance_time_by(Duration::from_secs(1));
    boxes.check_deadlines();
    assert_eq!(
        boxes.get_channel_difference(),
        Some(get_channel_difference(7, 10))
    );
}

#[test]
fn final_difference_releases_held_updates_directly_after_its_position() {
    reset_time();
    let mut boxes = live_channel(7, 10);
    for pts in [13, 14] {
        boxes
            .process_updates(channel_updates(7, 0, 0, pts))
            .unwrap();
    }
    advance_time_by(POSSIBLE_GAP_TIMEOUT);
    boxes.check_deadlines();
    assert_eq!(boxes.held_updates.len(), 2);
    boxes
        .try_apply_channel_difference(empty_channel_difference(12))
        .unwrap();
    let (released, _, _) = boxes.take_released_updates().unwrap();
    assert_eq!(
        released
            .iter()
            .map(|(update, _)| update.clone())
            .collect::<Vec<_>>(),
        vec![channel_update(7, 13), channel_update(7, 14)]
    );
    assert_eq!(boxes.session_state().channels[0].pts, 14);
}

#[test]
fn updates_of_a_real_gap_survive_until_its_channel_is_abandoned() {
    reset_time();
    let mut boxes = live_channel(7, 10);
    boxes.process_updates(channel_updates(7, 0, 0, 13)).unwrap();
    advance_time_by(POSSIBLE_GAP_TIMEOUT);
    boxes.check_deadlines();
    boxes.end_channel_difference(PrematureEndReason::Banned);
    assert_eq!(boxes.ready_diff_count, boxes.getting_diff_for.len());
    // Its position is gone, so what was received can be delivered and restart it.
    let (released, _, _) = boxes.take_released_updates().unwrap();
    assert_eq!(released[0].0, channel_update(7, 13));
    assert_eq!(boxes.session_state().channels[0].pts, 13);
}

#[test]
fn temporary_rejections_back_off_until_a_difference_succeeds() {
    reset_time();
    let mut boxes = live_channel(7, 10);
    let gap_then_reject = |boxes: &mut MessageBoxes, wait: Duration, pts: i32| {
        // Traffic beyond the gap must not bring the next attempt forward.
        boxes
            .process_updates(channel_updates(7, 0, 0, pts))
            .unwrap();
        let position = boxes.session_state().channels[0].pts;
        advance_time_by(wait - Duration::from_millis(1));
        boxes.check_deadlines();
        assert_eq!(boxes.get_channel_difference(), None, "before {wait:?}");
        advance_time_by(Duration::from_millis(1));
        boxes.check_deadlines();
        assert_eq!(
            boxes.get_channel_difference(),
            Some(get_channel_difference(7, position)),
            "after {wait:?}"
        );
        boxes.end_channel_difference(PrematureEndReason::TemporaryServerIssues);
        assert_eq!(boxes.get_channel_difference(), None);
    };
    gap_then_reject(&mut boxes, POSSIBLE_GAP_TIMEOUT, 13);
    gap_then_reject(&mut boxes, Duration::from_secs(1), 14);
    gap_then_reject(&mut boxes, Duration::from_secs(2), 15);
    gap_then_reject(&mut boxes, Duration::from_secs(4), 16);
    gap_then_reject(&mut boxes, Duration::from_secs(8), 17);
    gap_then_reject(&mut boxes, Duration::from_secs(16), 18);
    gap_then_reject(&mut boxes, Duration::from_secs(32), 19);
    gap_then_reject(&mut boxes, Duration::from_secs(60), 20);
    gap_then_reject(&mut boxes, Duration::from_secs(60), 21);

    // Success forgets all of it: the next gap takes only its usual short time.
    boxes.process_updates(channel_updates(7, 0, 0, 22)).unwrap();
    advance_time_by(Duration::from_secs(60));
    boxes.check_deadlines();
    boxes
        .try_apply_channel_difference(empty_channel_difference(22))
        .unwrap();
    gap_then_reject(&mut boxes, POSSIBLE_GAP_TIMEOUT, 24);
}

#[test]
fn a_gap_kept_open_by_backoff_retains_a_bounded_number_of_updates() {
    reset_time();
    let mut boxes = live_channel(7, 10);
    boxes.process_updates(channel_updates(7, 0, 0, 13)).unwrap();
    advance_time_by(POSSIBLE_GAP_TIMEOUT);
    boxes.check_deadlines();
    boxes.end_channel_difference(PrematureEndReason::TemporaryServerIssues);
    for pts in 0..MAX_HELD_UPDATES as i32 * 2 {
        boxes
            .process_updates(channel_updates(7, 0, 0, 100 + pts))
            .unwrap();
    }
    let entry = boxes.entry(Key::Channel(7)).unwrap();
    assert_eq!(
        entry.possible_gap.as_ref().unwrap().updates.len(),
        MAX_HELD_UPDATES
    );
    assert_eq!(entry.pts, 10);
}

#[test]
fn sorting_updates_is_stable_and_matches_the_standard_sort() {
    let key = |update: &tl::enums::Update| super::update_sort_key(update);
    let update = |pts: i32, count: i32, id: i32| -> tl::enums::Update {
        tl::types::UpdateDeleteMessages {
            messages: vec![id],
            pts,
            pts_count: count,
        }
        .into()
    };
    // Equal keys, different updates: their order must survive.
    // Sizes around the point where it hands over to the standard sort, many equal keys.
    let mut cases: Vec<Vec<tl::enums::Update>> = vec![
        vec![
            update(5, 1, 0),
            update(3, 1, 1),
            update(4, 0, 2),
            update(4, 1, 3),
        ],
        (0..8).map(|i| update(i + 1, 1, i)).collect(),
    ];
    for len in [2, 16, 31, 32, 33, 40] {
        cases.push((0..len).rev().map(|i| update(i + 1, 1, i)).collect());
        cases.push((0..len).map(|i| update(i % 3 + 1, 1, i)).collect());
    }
    for case in cases {
        let mut ours = case.clone();
        super::sort_updates_by_key(&mut ours, key);
        let mut expected = case.clone();
        expected.sort_by_key(key);
        assert_eq!(ours, expected);

        // As the gap path uses it: last to apply first, equal keys still stable.
        let reverse = |update: &tl::enums::Update| std::cmp::Reverse(key(update));
        let mut ours = case.clone();
        super::sort_updates_by_key(&mut ours, reverse);
        let mut expected = case;
        expected.sort_by_key(reverse);
        assert_eq!(ours, expected);
    }
}

#[test]
fn updates_out_of_order_in_one_batch_are_applied_in_order() {
    reset_time();
    let mut boxes = live_channel(7, 10);
    let (applied, _, _) = boxes
        .process_updates(UpdatesLike::Updates(tl::enums::Updates::Updates(
            tl::types::Updates {
                updates: vec![channel_update(7, 12), channel_update(7, 11)],
                users: Vec::new(),
                chats: Vec::new(),
                date: 0,
                seq: 0,
            },
        )))
        .unwrap();
    assert_eq!(
        applied
            .into_iter()
            .map(|(update, _)| update)
            .collect::<Vec<_>>(),
        vec![channel_update(7, 11), channel_update(7, 12)]
    );
    assert_eq!(boxes.session_state().channels[0].pts, 12);
}
