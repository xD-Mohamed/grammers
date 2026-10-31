// Copyright 2026 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your option.

use std::collections::VecDeque;

use tracing::warn;

const CAPACITY: usize = 1024;
// https://core.telegram.org/mtproto/security_guidelines: msg_id is about unixtime
// * 2^32 and values over 300 seconds in the past are ignored. Comparing server IDs
// with each other depends on neither the local clock nor its correction.
const MAX_AGE: i64 = 300 << 32;
// Retained IDs span at most MAX_AGE (about 2^40), so 48-bit offsets always fit.
const OFFSET_LIMIT: u64 = 1 << 48;
const MIN_SHRINK_CAPACITY: usize = 64;

// Exact, sorted IDs of authenticated server messages, allocated on use. IDs are
// never aliased. Bounded by count and by age relative to the newest retained ID;
// whatever leaves this history is covered by the caller's monotonic floor.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct ReplayIds {
    base: i64,
    offsets: VecDeque<[u8; 6]>,
}

fn pack(offset: u64) -> [u8; 6] {
    let bytes = offset.to_be_bytes();
    [bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7]]
}

fn unpack(bytes: [u8; 6]) -> u64 {
    u64::from_be_bytes([
        0, 0, bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5],
    ])
}

impl ReplayIds {
    pub(super) fn new() -> Self {
        Self {
            base: 0,
            offsets: VecDeque::new(),
        }
    }

    fn id(&self, offset: [u8; 6]) -> i64 {
        self.base.wrapping_add(unpack(offset) as i64)
    }

    fn offset(&self, id: i64) -> Option<[u8; 6]> {
        let offset = id.wrapping_sub(self.base) as u64;
        (offset < OFFSET_LIMIT).then(|| pack(offset))
    }

    pub(super) fn contains(&self, id: &i64) -> bool {
        self.offset(*id).is_some_and(|offset| {
            self.offsets.back().is_some_and(|last| *last >= offset)
                && self.offsets.binary_search(&offset).is_ok()
        })
    }

    /// Records a new ID and returns the highest ID that left the history, which
    /// may be `id` itself. Duplicates are filtered by the caller before recording.
    pub(super) fn remember(&mut self, id: i64) -> Option<i64> {
        let previous_newest = self.offsets.back().map(|last| self.id(*last));
        if previous_newest.is_some_and(|previous| id.saturating_sub(previous) > MAX_AGE) {
            // Everything below this now counts as old, which only a clock step or a
            // misbehaving server would cause. Until reconnecting, order becomes stricter.
            warn!("server message ID jumped more than 300 seconds ahead");
        }
        let newest = previous_newest.map_or(id, |previous| previous.max(id));
        let oldest_allowed = newest.saturating_sub(MAX_AGE);
        if id < oldest_allowed
            || (self.offsets.len() == CAPACITY
                && self
                    .offsets
                    .front()
                    .is_some_and(|first| id < self.id(*first)))
        {
            return Some(id);
        }

        let mut evicted = None;
        while let Some(first) = self.offsets.front().map(|first| self.id(*first))
            && first < oldest_allowed
        {
            self.offsets.pop_front();
            evicted = Some(first);
        }
        if self.offsets.len() == CAPACITY {
            evicted = self.offsets.pop_front().map(|first| self.id(first));
        }

        let offset = match self.offset(id).filter(|_| !self.offsets.is_empty()) {
            Some(offset) => offset,
            None => {
                // Everything retained, and every ID still allowed, is at or above
                // this base. Rewriting is bounded and needed about once in 18 hours.
                let previous = self.base;
                self.base = oldest_allowed;
                for slot in &mut self.offsets {
                    let retained = previous.wrapping_add(unpack(*slot) as i64);
                    *slot = pack(retained.wrapping_sub(oldest_allowed) as u64);
                }
                pack(id.wrapping_sub(oldest_allowed) as u64)
            }
        };
        if self.offsets.back().is_none_or(|last| *last < offset) {
            self.offsets.push_back(offset);
        } else if let Err(index) = self.offsets.binary_search(&offset) {
            self.offsets.insert(index, offset);
        }

        // Release a burst's storage once traffic falls, without reallocating at
        // a steady rate (growing after this shrink needs twice the retained IDs).
        let capacity = self.offsets.capacity();
        if capacity >= MIN_SHRINK_CAPACITY && self.offsets.len() * 4 <= capacity {
            self.offsets.shrink_to(self.offsets.len() * 2);
        }
        evicted
    }

    #[cfg(test)]
    pub(super) fn len(&self) -> usize {
        self.offsets.len()
    }

    #[cfg(test)]
    pub(super) fn capacity(&self) -> usize {
        self.offsets.capacity()
    }

    #[cfg(test)]
    pub(super) fn is_empty(&self) -> bool {
        self.offsets.is_empty()
    }

    #[cfg(test)]
    pub(super) fn values(&self) -> Vec<i64> {
        self.offsets.iter().map(|offset| self.id(*offset)).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    const SECOND: i64 = 1 << 32;
    const REAL_TIME: i64 = 0x6500_0000 * SECOND;

    #[derive(Default)]
    struct Model {
        ids: BTreeSet<i64>,
        floor: i64,
    }

    impl Model {
        fn remember(&mut self, id: i64) {
            let newest = self.ids.last().map_or(id, |last| id.max(*last));
            let oldest_allowed = newest.saturating_sub(MAX_AGE);
            if id < oldest_allowed
                || (self.ids.len() == CAPACITY && self.ids.first().is_some_and(|f| id < *f))
            {
                self.floor = self.floor.max(id);
                return;
            }
            while self
                .ids
                .first()
                .is_some_and(|first| *first < oldest_allowed)
            {
                self.floor = self.floor.max(self.ids.pop_first().unwrap());
            }
            if self.ids.len() == CAPACITY {
                self.floor = self.floor.max(self.ids.pop_first().unwrap());
            }
            self.ids.insert(id);
        }
    }

    #[test]
    fn history_matches_exact_model_across_ordering_age_and_base_changes() {
        let mut actual = ReplayIds::new();
        let mut model = Model::default();
        let mut actual_floor = 0;
        let mut seen = Vec::new();
        let mut inputs: Vec<i64> = (0..2200).map(|i| REAL_TIME + i * 4 + 3).collect();
        let mut seed = 0x7412_8891_u64;
        for _ in 0..4096 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            inputs.push(REAL_TIME + ((seed % 3300) as i64) * 4 + 1);
        }
        // Out-of-window, base-crossing and multi-window jumps, then random spread.
        inputs.extend([
            REAL_TIME - 1,
            REAL_TIME + (1 << 47) + 1,
            REAL_TIME + (1 << 47) - MAX_AGE + 1,
            REAL_TIME + (1 << 48) + 1,
            REAL_TIME + (1 << 48) - 1,
            REAL_TIME + 7,
            REAL_TIME + (5 << 48) + 1,
        ]);
        for _ in 0..4096 {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            inputs.push(REAL_TIME + (5 << 48) + ((seed % (400 * 1024)) as i64) * (SECOND >> 10));
        }
        inputs.extend([i64::MAX, i64::MAX - 2, 1]);
        for id in inputs {
            assert_eq!(actual.contains(&id), model.ids.contains(&id), "{id}");
            if model.ids.contains(&id) {
                continue;
            }
            if let Some(evicted) = actual.remember(id) {
                actual_floor = actual_floor.max(evicted);
            }
            model.remember(id);
            assert_eq!(actual_floor, model.floor);
            // The safety property itself: a seen ID is stored or covered by the floor.
            seen.push(id);
            assert!(
                seen.iter()
                    .all(|id| *id <= actual_floor || actual.contains(id)),
                "a seen ID could be accepted as new"
            );
            assert_eq!(
                actual.values(),
                model.ids.iter().copied().collect::<Vec<_>>()
            );
            assert!(actual.capacity() <= CAPACITY);
        }
    }

    #[test]
    fn burst_uses_six_bytes_per_id_and_evicts_only_the_lowest() {
        let mut history = ReplayIds::new();
        assert_eq!(history.capacity(), 0);
        for index in 0..CAPACITY as i64 {
            assert_eq!(history.remember(REAL_TIME + index * 4 + 3), None);
        }
        assert_eq!(history.capacity() * size_of::<[u8; 6]>(), 6144);
        assert_eq!(
            history.remember(REAL_TIME + CAPACITY as i64 * 4 + 3),
            Some(REAL_TIME + 3)
        );
        // A delayed ID below a full window is itself the eviction.
        assert_eq!(history.remember(REAL_TIME + 1), Some(REAL_TIME + 1));
        assert_eq!(history.len(), CAPACITY);
        assert_eq!(history.capacity(), CAPACITY);
    }

    #[test]
    fn age_window_bounds_quiet_sessions_and_releases_burst_capacity() {
        let mut history = ReplayIds::new();
        for index in 0..CAPACITY as i64 {
            history.remember(REAL_TIME + index * 4 + 3);
        }
        assert_eq!(history.capacity(), CAPACITY);
        // One message per server minute (a keepalive reply, say) drains the burst.
        let mut id = REAL_TIME + 3;
        let mut floor = 0;
        for _ in 0..16 {
            id += 60 * SECOND;
            if let Some(evicted) = history.remember(id) {
                floor = floor.max(evicted);
            }
            assert!(history.values().iter().all(|kept| *kept >= id - MAX_AGE));
        }
        // The highest removed ID, not the age limit, defines the floor.
        let kept = history.values();
        assert_eq!(kept.len(), 6);
        assert_eq!(floor, kept[0] - 60 * SECOND);
        assert!(!history.contains(&floor));
        assert!(
            history.capacity() < MIN_SHRINK_CAPACITY,
            "{}",
            history.capacity()
        );
        // A steady message per second retains only its age window.
        for _ in 0..2000 {
            id += SECOND;
            history.remember(id);
        }
        assert_eq!(history.len(), 301);
        assert!(history.capacity() < 1024, "{}", history.capacity());
    }

    #[test]
    fn far_future_id_raises_floor_only_to_real_ids() {
        let mut history = ReplayIds::new();
        let real: Vec<i64> = (0..8).map(|i| REAL_TIME + i * 4 + 3).collect();
        for id in &real {
            assert_eq!(history.remember(*id), None);
        }
        let future = REAL_TIME + 3600 * SECOND + 1;
        assert_eq!(history.remember(future), Some(*real.last().unwrap()));
        assert_eq!(history.values(), [future]);
        // Later genuine IDs remain above the floor. Below the future ID's window,
        // they fall back to a strict monotonic filter instead of being retained.
        let later = REAL_TIME + 100;
        assert!(later > *real.last().unwrap());
        assert_eq!(history.remember(later), Some(later));
        assert_eq!(history.values(), [future]);
    }

    #[test]
    fn rebasing_retained_ids_preserves_every_decision() {
        let mut replay = ReplayIds::new();
        let start = REAL_TIME;
        // Near the end of the offset range with IDs retained, which ordinary traffic
        // reaches after about 18 hours. Everything recorded here stays within MAX_AGE.
        replay.base = start - (OFFSET_LIMIT as i64 - MAX_AGE / 2);
        let retained: Vec<i64> = (0..4).map(|i| start + i * SECOND + 1).collect();
        for id in &retained {
            let offset = replay.offset(*id).unwrap();
            replay.offsets.push_back(offset);
        }
        let before = replay.base;
        // This one no longer fits the offsets, and nothing retained has aged out.
        let beyond = start + MAX_AGE - SECOND + 1;
        assert_eq!(replay.offset(beyond), None);
        assert_eq!(replay.remember(beyond), None);
        assert_ne!(replay.base, before);
        for id in retained.iter().chain([&beyond]) {
            assert!(replay.contains(id), "{id} must still be known");
        }
        assert!(!replay.contains(&(start + 5)));
        assert_eq!(replay.len(), retained.len() + 1);
        // Later IDs and the eviction order carry on as if nothing happened.
        assert_eq!(replay.remember(beyond + 2 * SECOND), Some(retained[0]));
    }
}
