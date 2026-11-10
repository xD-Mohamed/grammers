// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! A reading of the clock that may be one kernel tick old, for callers that only
//! compare it with deadlines far coarser than that.
//!
//! On Linux the kernel keeps the time of its last tick (1 to 10 ms apart, depending on
//! how it was built) where every process can read it without entering the kernel. The
//! precise clock is read the same way on most machines, in a few nanoseconds more. On
//! a virtual machine whose clock source the kernel cannot serve that way, each precise
//! reading is a system call of a few hundred nanoseconds, and a connection that takes
//! one for every packet and every batch of updates spends a tenth of its time on them.
//!
//! Both readings here are never later than the precise clock and at most one tick
//! earlier. Other systems, and a kernel that refuses, get the precise clock, and so
//! does a process in which the two are ever found further apart than a tick explains.

use std::time::{Duration, Instant};

/// The monotonic clock as of the kernel's last tick: never later than
/// [`Instant::now`], and at most one tick earlier.
///
/// Do not wait for a deadline with this alone. A timer that fires at a deadline can
/// find this reading still a tick short of it: read [`Instant::now`] after a timer.
/// For the same reason a reading can be earlier than an [`Instant::now`] taken before
/// it. Readings of this function alone do not go backwards.
///
/// The tick's time is converted to an [`Instant`] through one pairing of the two
/// clocks, made on first use, which sets every reading back by the fraction of a
/// microsecond that pairing took.
pub fn instant() -> Instant {
    imp::instant()
}

/// The time since the Unix epoch as of the kernel's last tick: never later than
/// [`std::time::SystemTime::now`], and at most one tick earlier.
///
/// # Panics
///
/// If the system clock is set before the Unix epoch.
pub fn unix_time() -> Duration {
    imp::unix_time()
}

#[cfg(not(any(target_os = "linux", target_os = "android")))]
mod imp {
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    pub(super) fn instant() -> Instant {
        Instant::now()
    }

    pub(super) fn unix_time() -> Duration {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time is before epoch")
    }
}

#[cfg(any(target_os = "linux", target_os = "android"))]
mod imp {
    use std::sync::OnceLock;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    /// How far behind [`Instant::now`] a reading may be found before this stops
    /// trusting the kernel's coarse clock. Ticks are 10 ms at the longest.
    pub(super) const TOLERANCE: Duration = Duration::from_millis(50);
    /// How often a reading is compared with [`Instant::now`], in nanoseconds of the
    /// coarse clock itself. One precise reading a second is not measurable.
    const VALIDATE_EVERY: u64 = 1_000_000_000;
    /// How many times the two clocks are paired to find the tightest pair.
    const ANCHOR_ATTEMPTS: usize = 8;

    /// `clock` since its epoch, or `None` if the kernel does not have that clock.
    #[allow(unsafe_code)]
    fn read(clock: libc::clockid_t) -> Option<Duration> {
        let mut time = std::mem::MaybeUninit::<libc::timespec>::zeroed();
        // SAFETY: `time` is a valid, writable `timespec` that outlives the call, which
        // writes nothing else. It was zeroed, and every bit pattern of its integer
        // fields is a value, so it is initialized whatever the call wrote.
        let (status, time) = unsafe {
            let status = libc::clock_gettime(clock, time.as_mut_ptr());
            (status, time.assume_init())
        };
        let seconds = u64::try_from(time.tv_sec).ok()?;
        let nanoseconds = u32::try_from(time.tv_nsec).ok()?;
        (status == 0 && nanoseconds < 1_000_000_000).then(|| Duration::new(seconds, nanoseconds))
    }

    /// One moment as both an [`Instant`] and a reading of `CLOCK_MONOTONIC`, which is the
    /// clock [`Instant`] reads here but offers no way to convert from.
    pub(super) struct Anchor {
        pub(super) instant: Instant,
        pub(super) monotonic: Duration,
    }

    impl Anchor {
        /// The `Instant` of a reading of the monotonic clock, coarse or not.
        pub(super) fn instant_of(&self, monotonic: Duration) -> Option<Instant> {
            match monotonic.checked_sub(self.monotonic) {
                Some(after) => self.instant.checked_add(after),
                // The tick before the anchor was taken.
                None => self.instant.checked_sub(self.monotonic - monotonic),
            }
        }
    }

    /// Whether a coarse reading can be what it claims beside a precise one taken just
    /// after it: not later, and not further behind than a tick and a wait explain.
    pub(super) fn plausible(reading: Instant, precise: Instant) -> bool {
        reading <= precise && precise.duration_since(reading) <= TOLERANCE
    }

    static ANCHOR: OnceLock<Option<Anchor>> = OnceLock::new();
    static DISTRUSTED: AtomicBool = AtomicBool::new(false);
    static VALIDATED_AT: AtomicU64 = AtomicU64::new(0);
    static REALTIME_REFUSED: AtomicBool = AtomicBool::new(false);

    fn anchor() -> Option<&'static Anchor> {
        ANCHOR
            .get_or_init(|| {
                // The `Instant` first: it is then no later than the raw reading, and
                // every time derived from the pair is no later than the true one. It is
                // earlier by what passed between the two, so keep the closest of a few
                // pairs: a thread made to wait between them would set every later
                // reading back by that wait.
                let mut best: Option<(Duration, Anchor)> = None;
                for _ in 0..ANCHOR_ATTEMPTS {
                    let instant = Instant::now();
                    let monotonic = read(libc::CLOCK_MONOTONIC)?;
                    let apart = instant.elapsed();
                    if best.as_ref().is_none_or(|(least, _)| apart < *least) {
                        best = Some((apart, Anchor { instant, monotonic }));
                    }
                }
                best.map(|(_, anchor)| anchor)
            })
            .as_ref()
    }

    fn coarse(anchor: &Anchor) -> Option<Instant> {
        anchor.instant_of(read(libc::CLOCK_MONOTONIC_COARSE)?)
    }

    pub(super) fn instant() -> Instant {
        if DISTRUSTED.load(Ordering::Relaxed) {
            return Instant::now();
        }
        let Some(anchor) = anchor() else {
            DISTRUSTED.store(true, Ordering::Relaxed);
            return Instant::now();
        };
        let Some(reading) = coarse(anchor) else {
            DISTRUSTED.store(true, Ordering::Relaxed);
            return Instant::now();
        };

        // The standard library does not promise which clock an `Instant` is. Should it
        // ever be one that runs while the machine sleeps, the pair above would drift
        // apart: compare with it once a second, and give the coarse clock up for good
        // if the two disagree by more than a tick can explain.
        let since_anchor = reading.saturating_duration_since(anchor.instant);
        let nanos = since_anchor.as_nanos().min(u64::MAX as u128) as u64;
        if nanos.wrapping_sub(VALIDATED_AT.load(Ordering::Relaxed)) >= VALIDATE_EVERY {
            VALIDATED_AT.store(nanos, Ordering::Relaxed);
            let precise = Instant::now();
            if !plausible(reading, precise) {
                // A thread kept waiting between the two readings looks the same. Look
                // once more before giving up something that is never given back.
                let again = coarse(anchor);
                let precise = Instant::now();
                return match again {
                    Some(reading) if plausible(reading, precise) => reading,
                    _ => {
                        DISTRUSTED.store(true, Ordering::Relaxed);
                        precise
                    }
                };
            }
        }
        reading
    }

    pub(super) fn unix_time() -> Duration {
        // The same clock as `SystemTime`, by definition: there is nothing to convert.
        if !REALTIME_REFUSED.load(Ordering::Relaxed) {
            if let Some(time) = read(libc::CLOCK_REALTIME_COARSE) {
                return time;
            }
            REALTIME_REFUSED.store(true, Ordering::Relaxed);
        }
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time is before epoch")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Longer than any tick, and than a test thread is likely to be kept waiting.
    const SLACK: Duration = Duration::from_millis(500);

    #[test]
    fn a_reading_is_never_later_than_the_precise_clock_and_not_far_behind() {
        for _ in 0..20_000 {
            let before = Instant::now();
            let coarse = instant();
            let after = Instant::now();
            assert!(coarse <= after, "{coarse:?} is later than {after:?}");
            assert!(before.saturating_duration_since(coarse) < SLACK);
        }
    }

    #[test]
    fn readings_do_not_go_backwards_and_advance() {
        let first = instant();
        let mut last = first;
        let started = Instant::now();
        while started.elapsed() < Duration::from_millis(60) {
            let now = instant();
            assert!(now >= last);
            last = now;
        }
        // Ticks are 10 ms at the longest: several have passed.
        assert!(last > first);
    }

    #[test]
    fn unix_time_is_never_later_than_the_system_clock_and_not_far_behind() {
        let system = || SystemTime::now().duration_since(UNIX_EPOCH).unwrap();
        for _ in 0..20_000 {
            let before = system();
            let coarse = unix_time();
            let after = system();
            // The system clock can be stepped, but not within these few nanoseconds.
            assert!(coarse <= after, "{coarse:?} is later than {after:?}");
            assert!(before.saturating_sub(coarse) < SLACK);
        }
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn a_tick_is_placed_on_either_side_of_the_anchor() {
        let anchor = imp::Anchor {
            instant: Instant::now(),
            monotonic: Duration::new(1000, 500),
        };
        let at = |seconds, nanos| anchor.instant_of(Duration::new(seconds, nanos)).unwrap();
        assert_eq!(at(1000, 500), anchor.instant);
        assert_eq!(at(1003, 500), anchor.instant + Duration::from_secs(3));
        // The tick the anchor was taken in began before it.
        assert_eq!(at(1000, 100), anchor.instant - Duration::from_nanos(400));
        assert_eq!(at(999, 500), anchor.instant - Duration::from_secs(1));
    }

    #[cfg(any(target_os = "linux", target_os = "android"))]
    #[test]
    fn a_reading_is_distrusted_when_later_or_too_far_behind() {
        let precise = Instant::now();
        assert!(imp::plausible(precise, precise));
        assert!(imp::plausible(precise - Duration::from_millis(10), precise));
        assert!(imp::plausible(precise - imp::TOLERANCE, precise));
        // An `Instant` that ran on while the machine slept would leave readings behind.
        assert!(!imp::plausible(
            precise - imp::TOLERANCE - Duration::from_nanos(1),
            precise
        ));
        // And nothing that is derived from an earlier moment can be later.
        assert!(!imp::plausible(precise + Duration::from_nanos(1), precise));
    }

    /// What a reading costs here, beside the precise clock.
    #[test]
    #[ignore = "local clock cost comparison"]
    fn benchmark_coarse_clock_cost() {
        const READS: u32 = 2_000_000;
        let time = |read: &dyn Fn() -> Duration| {
            let started = Instant::now();
            let mut sum = Duration::ZERO;
            for _ in 0..READS {
                sum += std::hint::black_box(read());
            }
            std::hint::black_box(sum);
            started.elapsed().as_secs_f64() * 1e9 / READS as f64
        };
        let origin = Instant::now();
        println!(
            "ns per reading: Instant::now {:.1}, coarse instant {:.1}, SystemTime::now {:.1}, coarse unix time {:.1}",
            time(&|| Instant::now().duration_since(origin)),
            time(&|| instant().saturating_duration_since(origin)),
            time(&|| SystemTime::now().duration_since(UNIX_EPOCH).unwrap()),
            time(&unix_time),
        );
    }
}
