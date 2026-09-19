//! Optional lifetime tracking for callers that must not overlap requests.
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};
use std::{
    future::Future,
    pin::Pin,
    task::{Context, Poll},
};
use tokio::sync::oneshot;

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
pub struct InvocationTracker(Arc<AtomicU8>);

impl InvocationTracker {
    /// Observe SDK lifetime; dropping a response future does not imply Idle.
    pub fn stage(&self) -> InvocationStage {
        match self.0.load(Ordering::Acquire) {
            0 => InvocationStage::Idle,
            1 => InvocationStage::Pending,
            _ => InvocationStage::Sent,
        }
    }

    /// Reserve this invocation group until the SDK completes/discards the request.
    pub fn try_acquire(&self) -> Option<InvocationPermit> {
        self.0
            .compare_exchange(0, 1, Ordering::AcqRel, Ordering::Relaxed)
            .ok()
            .map(|_| InvocationPermit(Arc::clone(&self.0)))
    }
}

/// Exclusive reservation moved into the SDK request, not retained by its caller.
/// It is released on completion, an unsent cancellation, or connection teardown.
pub struct InvocationPermit(Arc<AtomicU8>);

impl InvocationPermit {
    pub(crate) fn mark_sent(&self) {
        self.0.store(2, Ordering::Release);
    }
}

impl Drop for InvocationPermit {
    fn drop(&mut self) {
        self.0.store(0, Ordering::Release);
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
