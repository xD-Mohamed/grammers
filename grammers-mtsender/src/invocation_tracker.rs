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
                self.0.sent_at.store(0, Ordering::Relaxed);
                InvocationPermit {
                    tracker: Some(Arc::clone(&self.0)),
                    capacity: None,
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
}

impl InvocationPermit {
    pub(crate) fn with_capacity(permit: Option<Self>, capacity: OwnedSemaphorePermit) -> Self {
        let mut permit = permit.unwrap_or(Self {
            tracker: None,
            capacity: None,
        });
        permit.capacity = Some(capacity);
        permit
    }

    pub(crate) fn mark_sent(&self) {
        let Some(tracker) = &self.tracker else {
            return;
        };
        let nanos = u64::try_from(origin().elapsed().as_nanos()).unwrap_or(u64::MAX - 1);
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
                        permit.mark_sent();
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
