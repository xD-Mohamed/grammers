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
use tokio::sync::oneshot;

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
                InvocationPermit(Arc::clone(&self.0))
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
pub struct InvocationPermit(Arc<TrackerState>);

impl InvocationPermit {
    pub(crate) fn mark_sent(&self) {
        let nanos = u64::try_from(origin().elapsed().as_nanos()).unwrap_or(u64::MAX - 1);
        // The first complete write is the one that reached the network.
        let _ = self
            .0
            .sent_at
            .compare_exchange(0, nanos + 1, Ordering::Release, Ordering::Relaxed);
        self.0.stage.store(2, Ordering::Release);
    }
}

impl Drop for InvocationPermit {
    fn drop(&mut self) {
        self.0.stage.store(0, Ordering::Release);
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
            Poll::Ready(Err(_)) => Poll::Ready(Err(crate::InvocationError::Dropped)),
        }
    }
}
