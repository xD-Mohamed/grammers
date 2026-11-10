//! Optional lifetime tracking for callers that must not overlap requests.
use std::sync::{
    Arc, OnceLock,
    atomic::{AtomicU8, AtomicU64, Ordering},
};
use std::time::Instant;
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::sync::{OwnedSemaphorePermit, oneshot};

/// Origin for write timestamps, so they fit in an atomic.
static ORIGIN: OnceLock<Instant> = OnceLock::new();

fn origin() -> Instant {
    *ORIGIN.get_or_init(Instant::now)
}

#[derive(Default)]
struct TrackerState {
    stage: AtomicU8,
    /// Nanoseconds after `ORIGIN` plus one when the request was first fully
    /// written; 0 while unsent.
    sent_at: AtomicU64,
}

/// Lifetime of a tracked RPC in the sender, independent of its waiting caller.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InvocationStage {
    /// No SDK request still owns this reservation.
    Idle,
    /// Reserved/queued, or not yet completely written to the socket.
    Pending,
    /// The containing transport packet was fully written at least once.
    Sent,
}

/// Reusable tracker allocated once for an invocation group. Clones share the same admission.
#[derive(Clone, Default)]
pub struct InvocationTracker(Arc<TrackerState>);

impl InvocationTracker {
    /// Observe SDK lifetime; dropping a response future does not imply Idle.
    pub fn stage(&self) -> InvocationStage {
        match self.0.stage.load(Ordering::Acquire) {
            0 => InvocationStage::Idle,
            1 => InvocationStage::Pending,
            _ => InvocationStage::Sent,
        }
    }

    /// Reserve this invocation group until the SDK completes/discards the request.
    pub fn try_acquire(&self) -> Option<InvocationPermit> {
        self.0
            .stage
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Relaxed)
            .ok()
            .map(|_| {
                // Every tracked write follows this shared timestamp origin,
                // even when reports from concurrent writes arrive out of order.
                let _ = origin();
                self.0.sent_at.store(0, Ordering::Relaxed);
                InvocationPermit {
                    tracker: Some(Arc::clone(&self.0)),
                    capacity: None,
                    bytes: None,
                }
            })
    }

    /// When the most recent reservation's request was first fully written to
    /// the socket. Kept after the request completes, until the next reservation.
    pub fn sent_at(&self) -> Option<Instant> {
        match self.0.sent_at.load(Ordering::Acquire) {
            0 => None,
            nanos => Some(origin() + std::time::Duration::from_nanos(nanos - 1)),
        }
    }
}

/// Exclusive reservation moved into the SDK request, not retained by its caller.
/// It is released on completion, an unsent cancellation, or connection teardown.
pub struct InvocationPermit {
    tracker: Option<Arc<TrackerState>>,
    capacity: Option<OwnedSemaphorePermit>,
    bytes: Option<OwnedSemaphorePermit>,
}

impl InvocationPermit {
    pub(crate) fn with_capacity(permit: Option<Self>, capacity: OwnedSemaphorePermit) -> Self {
        let mut permit = permit.unwrap_or(Self {
            tracker: None,
            capacity: None,
            bytes: None,
        });
        permit.capacity = Some(capacity);
        permit
    }

    pub(crate) fn with_bytes(permit: Option<Self>, bytes: OwnedSemaphorePermit) -> Self {
        let mut permit = permit.unwrap_or(Self {
            tracker: None,
            capacity: None,
            bytes: None,
        });
        permit.bytes = Some(bytes);
        permit
    }

    /// Record that the packet carrying this request was written at `at`: the one
    /// clock read of that write, shared by every request the packet carried.
    pub(crate) fn mark_sent(&self, at: Instant) {
        let Some(tracker) = &self.tracker else {
            return;
        };
        // Acquiring the tracked permit initialized the origin before this write.
        let origin = origin();
        let since_origin = at.saturating_duration_since(origin);
        let nanos = u64::try_from(since_origin.as_nanos()).unwrap_or(u64::MAX - 1);
        // The first complete write is the one that reached the network.
        let _ =
            tracker
                .sent_at
                .compare_exchange(0, nanos + 1, Ordering::Release, Ordering::Relaxed);
        tracker.stage.store(2, Ordering::Release);
    }
}

impl Drop for InvocationPermit {
    fn drop(&mut self) {
        if let Some(tracker) = &self.tracker {
            tracker.stage.store(0, Ordering::Release);
        }
    }
}

/// An owned, movable response waiter. Cancelling it does not release a tracked
/// reservation until the SDK discards/completes its request or closes the connection.
pub struct PendingInvocation {
    pub(crate) receiver: oneshot::Receiver<Result<Vec<u8>, crate::InvocationError>>,
}

impl Future for PendingInvocation {
    type Output = Result<Vec<u8>, crate::InvocationError>;
    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        match Pin::new(&mut self.receiver).poll(cx) {
            Poll::Pending => Poll::Pending,
            Poll::Ready(Ok(result)) => Poll::Ready(result),
            Poll::Ready(Err(_)) => Poll::Ready(Err(crate::InvocationError::Dropped(
                "request response channel was closed",
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn out_of_order_write_reports_keep_their_actual_times() {
        const CHILD: &str = "GRAMMERS_TRACKER_ORIGIN_TEST_CHILD";
        if std::env::var_os(CHILD).is_none() {
            let output = std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "invocation_tracker::tests::out_of_order_write_reports_keep_their_actual_times",
                    "--nocapture",
                    "--test-threads=1",
                ])
                .env(CHILD, "1")
                .output()
                .unwrap();
            assert!(
                output.status.success()
                    && String::from_utf8_lossy(&output.stdout)
                        .contains("test result: ok. 1 passed; 0 failed;"),
                "child status: {}\nstdout: {}\nstderr: {}",
                output.status,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            return;
        }

        assert!(ORIGIN.get().is_none());
        let earlier_tracker = InvocationTracker::default();
        let later_tracker = InvocationTracker::default();
        let earlier_permit = earlier_tracker.try_acquire().unwrap();
        let later_permit = later_tracker.try_acquire().unwrap();
        let earlier = Instant::now();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let later = Instant::now();
        assert!(earlier < later);
        later_permit.mark_sent(later);
        earlier_permit.mark_sent(earlier);
        assert_eq!(earlier_tracker.sent_at(), Some(earlier));
        assert_eq!(later_tracker.sent_at(), Some(later));
        earlier_permit.mark_sent(later);
        assert_eq!(earlier_tracker.sent_at(), Some(earlier));
    }
}

#[cfg(test)]
mod benchmarks {
    use super::*;

    #[test]
    #[ignore = "local tracker cost comparison; no routing, sockets or requests"]
    fn benchmark_invocation_tracking_cost() {
        use std::hint::black_box;
        const SAMPLES: usize = 200_000;
        let tracker = InvocationTracker::default();
        let mut samples = [Vec::new(), Vec::new()];
        for round in 0..8 {
            for timestamps in [round % 2 == 0, round % 2 != 0] {
                let began = Instant::now();
                for _ in 0..SAMPLES {
                    let permit = tracker.try_acquire().unwrap();
                    if timestamps {
                        permit.mark_sent(Instant::now());
                    } else {
                        // Comparison only: keep production timestamp semantics.
                        permit
                            .tracker
                            .as_ref()
                            .unwrap()
                            .stage
                            .store(2, Ordering::Release);
                    }
                    black_box(&permit);
                    drop(permit);
                }
                samples[usize::from(timestamps)]
                    .push(began.elapsed().as_nanos() as f64 / SAMPLES as f64);
            }
        }
        for sample in &mut samples {
            sample.sort_by(f64::total_cmp);
        }
        println!(
            "invocation lifetime cycle: state-only {:.1} ns; with socket timestamp {:.1} ns (local only)",
            samples[0][4], samples[1][4]
        );
    }
}
