// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Unbounded single-consumer queue whose storage grows only when used.
//!
//! Tokio's unbounded channel preallocates a 32-slot block and a cache-padded
//! header: about 2.8 KiB for a 72-byte RPC. A pool keeps two such queues per
//! session and they are almost always empty, so applications holding thousands
//! of sessions paid for storage they never used. This queue starts empty and
//! keeps only the capacity it has needed. It mirrors the subset of Tokio's
//! `UnboundedSender`/`UnboundedReceiver` API used by the pool: receiving is
//! cancel-safe, closing still lets queued items be received, and dropping the
//! receiver drops queued items.

use std::collections::VecDeque;
use std::fmt;
use std::future::poll_fn;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll, Waker};

struct State<T> {
    items: VecDeque<T>,
    waker: Option<Waker>,
    senders: usize,
    closed: bool,
}

struct Shared<T>(Mutex<State<T>>);

impl<T> Shared<T> {
    // No user code runs while the lock is held, so a poisoned lock still
    // guards consistent state.
    fn lock(&self) -> MutexGuard<'_, State<T>> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

pub(crate) struct UnboundedSender<T>(Arc<Shared<T>>);

pub(crate) struct UnboundedReceiver<T>(Arc<Shared<T>>);

/// The receiver is closed or gone; the item was not queued.
pub(crate) struct SendError<T>(pub T);

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum TryRecvError {
    Empty,
    Disconnected,
}

pub(crate) fn unbounded_channel<T>() -> (UnboundedSender<T>, UnboundedReceiver<T>) {
    let shared = Arc::new(Shared(Mutex::new(State {
        items: VecDeque::new(),
        waker: None,
        senders: 1,
        closed: false,
    })));
    (
        UnboundedSender(Arc::clone(&shared)),
        UnboundedReceiver(shared),
    )
}

impl<T> UnboundedSender<T> {
    pub(crate) fn send(&self, item: T) -> Result<(), SendError<T>> {
        let waker = {
            let mut state = self.0.lock();
            if state.closed {
                return Err(SendError(item));
            }
            state.items.push_back(item);
            state.waker.take()
        };
        if let Some(waker) = waker {
            waker.wake();
        }
        Ok(())
    }

    pub(crate) fn is_closed(&self) -> bool {
        self.0.lock().closed
    }
}

impl<T> Clone for UnboundedSender<T> {
    fn clone(&self) -> Self {
        self.0.lock().senders += 1;
        Self(Arc::clone(&self.0))
    }
}

impl<T> Drop for UnboundedSender<T> {
    fn drop(&mut self) {
        let waker = {
            let mut state = self.0.lock();
            state.senders -= 1;
            if state.senders == 0 {
                state.waker.take()
            } else {
                None
            }
        };
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

impl<T> UnboundedReceiver<T> {
    fn poll_recv(&mut self, cx: &mut Context<'_>) -> Poll<Option<T>> {
        let mut state = self.0.lock();
        if let Some(item) = state.items.pop_front() {
            return Poll::Ready(Some(item));
        }
        if state.closed || state.senders == 0 {
            return Poll::Ready(None);
        }
        if !state
            .waker
            .as_ref()
            .is_some_and(|waker| waker.will_wake(cx.waker()))
        {
            state.waker = Some(cx.waker().clone());
        }
        Poll::Pending
    }

    /// Next item, or `None` once closed or without senders and empty.
    /// Cancel-safe: a dropped call has not removed anything.
    pub(crate) async fn recv(&mut self) -> Option<T> {
        poll_fn(|cx| self.poll_recv(cx)).await
    }

    pub(crate) fn try_recv(&mut self) -> Result<T, TryRecvError> {
        let mut state = self.0.lock();
        match state.items.pop_front() {
            Some(item) => Ok(item),
            None if state.closed || state.senders == 0 => Err(TryRecvError::Disconnected),
            None => Err(TryRecvError::Empty),
        }
    }

    /// Reject new items; queued items can still be received.
    pub(crate) fn close(&mut self) {
        self.0.lock().closed = true;
    }
}

impl<T> Drop for UnboundedReceiver<T> {
    fn drop(&mut self) {
        let items = {
            let mut state = self.0.lock();
            state.closed = true;
            state.waker = None;
            std::mem::take(&mut state.items)
        };
        // Outside the lock: dropping an item may wake or run other code.
        drop(items);
    }
}

impl<T> fmt::Debug for SendError<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SendError").finish_non_exhaustive()
    }
}

impl<T> fmt::Debug for UnboundedSender<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnboundedSender").finish_non_exhaustive()
    }
}

impl<T> fmt::Debug for UnboundedReceiver<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("UnboundedReceiver").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    #[tokio::test]
    async fn items_arrive_in_order_and_storage_starts_empty() {
        let (tx, mut rx) = unbounded_channel();
        assert_eq!(tx.0.lock().items.capacity(), 0);
        for i in 0..100 {
            tx.send(i).unwrap();
        }
        for i in 0..100 {
            assert_eq!(rx.recv().await, Some(i));
        }
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    }

    #[tokio::test]
    async fn receiver_wakes_for_items_and_for_the_last_sender() {
        let (tx, mut rx) = unbounded_channel();
        let extra = tx.clone();
        let waiting = tokio::spawn(async move {
            let first = rx.recv().await;
            let end = rx.recv().await;
            (first, end, rx)
        });
        tokio::task::yield_now().await;
        tx.send(7).unwrap();
        drop(tx);
        tokio::task::yield_now().await;
        assert!(!waiting.is_finished(), "one sender remains");
        drop(extra);
        let (first, end, mut rx) = tokio::time::timeout(Duration::from_secs(1), waiting)
            .await
            .unwrap()
            .unwrap();
        assert_eq!((first, end), (Some(7), None));
        assert_eq!(rx.try_recv(), Err(TryRecvError::Disconnected));
    }

    #[tokio::test]
    async fn close_rejects_sends_but_drains_queued_items() {
        let (tx, mut rx) = unbounded_channel();
        tx.send(1).unwrap();
        assert!(!tx.is_closed());
        rx.close();
        assert!(tx.is_closed());
        assert_eq!(tx.send(2).err().map(|error| error.0), Some(2));
        assert_eq!(rx.recv().await, Some(1));
        assert_eq!(rx.recv().await, None);
        assert_eq!(rx.try_recv(), Err(TryRecvError::Disconnected));
    }

    #[test]
    fn dropping_the_receiver_drops_queued_items_and_rejects_sends() {
        struct Counted(Arc<AtomicUsize>);
        impl Drop for Counted {
            fn drop(&mut self) {
                self.0.fetch_add(1, Ordering::Relaxed);
            }
        }
        let dropped = Arc::new(AtomicUsize::new(0));
        let (tx, rx) = unbounded_channel();
        tx.send(Counted(Arc::clone(&dropped))).ok().unwrap();
        tx.send(Counted(Arc::clone(&dropped))).ok().unwrap();
        drop(rx);
        assert_eq!(dropped.load(Ordering::Relaxed), 2);
        assert!(tx.is_closed());
        let rejected = tx.send(Counted(Arc::clone(&dropped))).err().unwrap();
        assert_eq!(dropped.load(Ordering::Relaxed), 2, "returned, not dropped");
        drop(rejected);
        assert_eq!(dropped.load(Ordering::Relaxed), 3);
    }

    #[tokio::test]
    async fn cancelled_receive_loses_nothing() {
        let (tx, mut rx) = unbounded_channel();
        assert!(
            tokio::time::timeout(Duration::from_millis(1), rx.recv())
                .await
                .is_err()
        );
        tx.send(5).unwrap();
        assert_eq!(rx.recv().await, Some(5));
    }

    #[test]
    #[ignore = "local queue cost comparison; run release with nocapture"]
    fn benchmark_queue_handoff() {
        // A sender task hands one 72-byte item at a time to a receiving task
        // and waits for it to be taken, as a caller hands an RPC to its
        // connection. Alternating order, median of eight rounds.
        const ITEMS: usize = 200_000;
        type Item = [u64; 9];
        async fn ours() -> f64 {
            let (tx, mut rx) = unbounded_channel::<Item>();
            let (ack_tx, mut ack_rx) = unbounded_channel::<()>();
            let receiver = tokio::spawn(async move {
                while let Some(item) = rx.recv().await {
                    std::hint::black_box(item);
                    ack_tx.send(()).unwrap();
                }
            });
            let began = std::time::Instant::now();
            for i in 0..ITEMS {
                tx.send([i as u64; 9]).unwrap();
                ack_rx.recv().await.unwrap();
            }
            let spent = began.elapsed();
            drop(tx);
            receiver.await.unwrap();
            spent.as_nanos() as f64 / ITEMS as f64
        }
        async fn tokio_channel() -> f64 {
            let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<Item>();
            let (ack_tx, mut ack_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
            let receiver = tokio::spawn(async move {
                while let Some(item) = rx.recv().await {
                    std::hint::black_box(item);
                    ack_tx.send(()).unwrap();
                }
            });
            let began = std::time::Instant::now();
            for i in 0..ITEMS {
                tx.send([i as u64; 9]).unwrap();
                ack_rx.recv().await.unwrap();
            }
            let spent = began.elapsed();
            drop(tx);
            receiver.await.unwrap();
            spent.as_nanos() as f64 / ITEMS as f64
        }
        for (name, builder) in [
            (
                "current-thread",
                tokio::runtime::Builder::new_current_thread(),
            ),
            ("two workers", {
                let mut builder = tokio::runtime::Builder::new_multi_thread();
                builder.worker_threads(2);
                builder
            }),
        ] {
            let mut builder = builder;
            let runtime = builder.enable_all().build().unwrap();
            let mut samples = [Vec::new(), Vec::new()];
            for round in 0..8 {
                for ours_first in [round % 2 == 0, round % 2 != 0] {
                    let ns = if ours_first {
                        runtime.block_on(ours())
                    } else {
                        runtime.block_on(tokio_channel())
                    };
                    samples[usize::from(ours_first)].push(ns);
                }
            }
            for sample in &mut samples {
                sample.sort_by(f64::total_cmp);
            }
            println!(
                "{name} handoff round trip: tokio unbounded {:.1} ns, lazy queue {:.1} ns (median)",
                samples[0][4], samples[1][4]
            );
        }
    }

    #[test]
    fn concurrent_senders_deliver_everything() {
        let (tx, mut rx) = unbounded_channel();
        std::thread::scope(|scope| {
            for thread in 0..4 {
                let tx = tx.clone();
                scope.spawn(move || {
                    for i in 0..1_000 {
                        tx.send(thread * 1_000 + i).unwrap();
                    }
                });
            }
        });
        drop(tx);
        let mut seen = Vec::new();
        while let Ok(item) = rx.try_recv() {
            seen.push(item);
        }
        seen.sort_unstable();
        assert_eq!(seen, (0..4_000).collect::<Vec<_>>());
        assert_eq!(rx.try_recv(), Err(TryRecvError::Disconnected));
    }
}
