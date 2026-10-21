// Copyright 2026 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Sequential, explicitly driven update routing (opt-in `dispatcher` feature).

use crate::update::Update;
use futures_util::{FutureExt, future::BoxFuture};
use std::{
    fmt,
    future::Future,
    ops::{BitAnd, BitOr, ControlFlow, Not},
    panic::AssertUnwindSafe,
    sync::Arc,
};

/// A shared update predicate. `&`, `|` short-circuit; `!` negates.
#[derive(Clone)]
pub struct Filter(Arc<dyn Fn(&Update) -> bool + Send + Sync>);
impl Filter {
    /// Build an application-defined predicate.
    pub fn new(predicate: impl Fn(&Update) -> bool + Send + Sync + 'static) -> Self {
        Self(Arc::new(predicate))
    }
    /// Test a predicate directly. Panics propagate here; routing catches unwinds.
    pub fn matches(&self, update: &Update) -> bool {
        (self.0)(update)
    }
    /// Newly received or sent messages.
    pub fn new_message() -> Self {
        Self::new(|u| matches!(u, Update::NewMessage(_)))
    }
    /// Message edits.
    pub fn edited_message() -> Self {
        Self::new(|u| matches!(u, Update::MessageEdited(_)))
    }
    /// Inline button callbacks.
    pub fn callback_query() -> Self {
        Self::new(|u| matches!(u, Update::CallbackQuery(_)))
    }
    /// Incoming new messages and edits; other update kinds do not match.
    pub fn incoming() -> Self {
        Self::new(|u| match u {
            Update::NewMessage(m) | Update::MessageEdited(m) => !m.outgoing(),
            _ => false,
        })
    }
    /// Match the first whitespace-delimited word `/name` in new messages or edits.
    /// Pass `name@bot` to require a bot suffix. Matching is exact and case-sensitive;
    /// an empty name or a name containing whitespace or `/` matches nothing.
    pub fn command(name: impl Into<String>) -> Self {
        let name = name.into();
        let valid =
            !name.is_empty() && !name.contains('/') && !name.chars().any(char::is_whitespace);
        Self::new(move |u| {
            valid
                && match u {
                    Update::NewMessage(m) | Update::MessageEdited(m) => {
                        m.text()
                            .split_whitespace()
                            .next()
                            .and_then(|word| word.strip_prefix('/'))
                            == Some(name.as_str())
                    }
                    _ => false,
                }
        })
    }
}
impl BitAnd for Filter {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self {
        Self::new(move |u| self.matches(u) && rhs.matches(u))
    }
}
impl BitOr for Filter {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self::new(move |u| self.matches(u) || rhs.matches(u))
    }
}
impl Not for Filter {
    type Output = Self;
    fn not(self) -> Self {
        Self::new(move |u| !self.matches(u))
    }
}

/// Stage at which an unwind was caught. Panic payloads are never retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PanicStage {
    /// Testing the registered predicate.
    Filter,
    /// Constructing the handler future.
    HandlerSetup,
    /// Polling the handler future.
    HandlerFuture,
}

/// Dispatch stops at the first failure. Indices are zero-based registration indices.
#[derive(Debug)]
pub enum DispatchError<E> {
    /// Application handler returned an error; the caller selects its error policy.
    Handler { index: usize, source: E },
    /// An unwind was caught. This cannot protect programs using `panic = "abort"`.
    /// The process's panic hook still runs according to its own logging policy.
    Panicked { index: usize, stage: PanicStage },
}
impl<E> fmt::Display for DispatchError<E> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Handler { index, .. } => write!(f, "update handler {index} failed"),
            Self::Panicked { index, stage } => {
                write!(f, "update handler {index} panicked during {stage:?}")
            }
        }
    }
}
impl<E: std::error::Error + 'static> std::error::Error for DispatchError<E> {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Handler { source, .. } => Some(source),
            Self::Panicked { .. } => None,
        }
    }
}

type Handler<E> = Box<dyn FnMut(Update) -> BoxFuture<'static, Result<ControlFlow<()>, E>> + Send>;

/// Explicitly driven, sequential handlers with no queue or background tasks.
///
/// `dispatch` requires exclusive access and awaits each matching handler. Cancelling
/// it drops the active future and skips later handlers; prior effects remain. Updates
/// are cloned only for matching handlers, so futures can own their input.
pub struct Router<E> {
    handlers: Vec<(Filter, Handler<E>)>,
}
impl<E> Default for Router<E> {
    fn default() -> Self {
        Self::new()
    }
}
impl<E> Router<E> {
    /// Create an empty router.
    pub fn new() -> Self {
        Self {
            handlers: Vec::new(),
        }
    }
    /// Register a handler. `Break(())` stops propagation for this update.
    pub fn on<F, Fut>(&mut self, filter: Filter, mut handler: F) -> &mut Self
    where
        F: FnMut(Update) -> Fut + Send + 'static,
        Fut: Future<Output = Result<ControlFlow<()>, E>> + Send + 'static,
    {
        self.handlers
            .push((filter, Box::new(move |u| Box::pin(handler(u)))));
        self
    }
    /// Dispatch one update, returning the propagation decision or first failure.
    pub async fn dispatch(&mut self, update: &Update) -> Result<ControlFlow<()>, DispatchError<E>> {
        for (index, (filter, handler)) in self.handlers.iter_mut().enumerate() {
            if !std::panic::catch_unwind(AssertUnwindSafe(|| filter.matches(update))).map_err(
                |_| DispatchError::Panicked {
                    index,
                    stage: PanicStage::Filter,
                },
            )? {
                continue;
            }
            let future = std::panic::catch_unwind(AssertUnwindSafe(|| handler(update.clone())))
                .map_err(|_| DispatchError::Panicked {
                    index,
                    stage: PanicStage::HandlerSetup,
                })?;
            let decision = AssertUnwindSafe(future)
                .catch_unwind()
                .await
                .map_err(|_| DispatchError::Panicked {
                    index,
                    stage: PanicStage::HandlerFuture,
                })?
                .map_err(|source| DispatchError::Handler { index, source })?;
            if decision.is_break() {
                return Ok(decision);
            }
        }
        Ok(ControlFlow::Continue(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        tl,
        update::{Raw, Update},
    };
    use std::{
        ops::ControlFlow,
        sync::{Arc, Mutex},
    };

    fn update() -> Update {
        Update::Raw(Raw {
            raw: tl::enums::Update::LoginToken,
            state: grammers_session::updates::State {
                message_box: None,
                seq: 0,
                date: 0,
            },
        })
    }

    async fn message(out: bool, text: &str, edited: bool) -> Update {
        let client = crate::Client::builder(1).build().await.unwrap();
        let raw = tl::types::UpdateShortSentMessage {
            out,
            id: 1,
            pts: 1,
            pts_count: 1,
            date: 1,
            media: None,
            entities: None,
            ttl_period: None,
        };
        let message = crate::message::Message::from_raw_short_updates(
            &client,
            raw,
            text.into(),
            crate::session::types::PeerId::user(42)
                .unwrap()
                .to_ambient_ref(),
        );
        let wrapped = crate::update::Message {
            msg: message,
            raw: tl::enums::Update::LoginToken,
            state: update().state().clone(),
        };
        if edited {
            Update::MessageEdited(wrapped)
        } else {
            Update::NewMessage(wrapped)
        }
    }

    #[tokio::test]
    async fn builtin_filters_distinguish_message_direction_edits_and_exact_commands() {
        let incoming = message(false, "/start arguments", false).await;
        assert!(
            (Filter::new_message() & Filter::incoming() & Filter::command("start"))
                .matches(&incoming)
        );
        assert!(!Filter::edited_message().matches(&incoming));
        assert!(!Filter::command("star").matches(&incoming));
        let outgoing = message(true, "/start", false).await;
        assert!(!Filter::incoming().matches(&outgoing));
        let edited = message(false, "/start@mybot\nmore", true).await;
        assert!(Filter::edited_message().matches(&edited));
        assert!(!Filter::new_message().matches(&edited));
        assert!(Filter::command("start@mybot").matches(&edited));
        assert!(!Filter::command("start").matches(&edited));
        for name in ["", "/start", "start arguments"] {
            assert!(!Filter::command(name).matches(&incoming));
        }
        let plain = message(false, "hello /start", false).await;
        assert!(!Filter::command("start").matches(&plain));
        let client = crate::Client::builder(1).build().await.unwrap();
        let callback = Update::CallbackQuery(crate::update::CallbackQuery {
            raw: tl::types::UpdateBotCallbackQuery {
                query_id: 1,
                user_id: 42,
                peer: tl::types::PeerUser { user_id: 42 }.into(),
                msg_id: 1,
                chat_instance: 1,
                data: None,
                game_short_name: None,
            }
            .into(),
            state: update().state().clone(),
            client: client.clone(),
            peers: client.empty_peer_map(),
        });
        assert!(Filter::callback_query().matches(&callback));
        assert!(!Filter::incoming().matches(&callback));
    }

    #[tokio::test]
    async fn handlers_run_in_registration_order_and_break_stops_propagation() {
        let calls = Arc::new(Mutex::new(Vec::new()));
        let mut router = Router::<std::io::Error>::new();
        for id in 0..3 {
            let calls = calls.clone();
            router.on(Filter::new(|_| true), move |_| {
                let calls = calls.clone();
                async move {
                    calls.lock().unwrap().push(id);
                    Ok(if id == 1 {
                        ControlFlow::Break(())
                    } else {
                        ControlFlow::Continue(())
                    })
                }
            });
        }
        assert_eq!(
            router.dispatch(&update()).await.unwrap(),
            ControlFlow::Break(())
        );
        assert_eq!(*calls.lock().unwrap(), [0, 1]);
    }

    #[test]
    fn composition_short_circuits_and_negation_preserves_predicates() {
        let forbidden = Filter::new(|_| panic!("must short-circuit"));
        let yes = Filter::new(|_| true);
        let no = Filter::new(|_| false);
        assert!(!(no.clone() & forbidden.clone()).matches(&update()));
        assert!((yes.clone() | forbidden).matches(&update()));
        assert!((!no & yes).matches(&update()));
        assert!(!Filter::new_message().matches(&update()));
        assert!(!Filter::incoming().matches(&update()));
        assert!(!Filter::command("start").matches(&update()));
        assert!(!Filter::edited_message().matches(&update()));
        assert!(!Filter::callback_query().matches(&update()));
    }

    #[tokio::test]
    async fn errors_preserve_source_and_skip_later_handlers() {
        let mut router = Router::<std::io::Error>::new();
        router.on(Filter::new(|_| false), |_| async {
            panic!("filtered handler ran")
        });
        router.on(Filter::new(|_| true), |_| async {
            Err(std::io::Error::other("application failure"))
        });
        router.on(Filter::new(|_| true), |_| async {
            panic!("later handler ran")
        });
        let error = router.dispatch(&update()).await.unwrap_err();
        assert!(
            matches!(&error, DispatchError::Handler { index: 1, source } if source.to_string() == "application failure")
        );
        assert_eq!(
            std::error::Error::source(&error).unwrap().to_string(),
            "application failure"
        );
    }

    #[tokio::test]
    async fn panics_at_all_stages_are_typed_and_payload_is_discarded() {
        for stage in [
            PanicStage::Filter,
            PanicStage::HandlerSetup,
            PanicStage::HandlerFuture,
        ] {
            let mut router = Router::<std::io::Error>::new();
            router.on(
                Filter::new(move |_| {
                    assert_ne!(stage, PanicStage::Filter, "private filter fixture");
                    true
                }),
                move |_| {
                    assert_ne!(stage, PanicStage::HandlerSetup, "private setup fixture");
                    async { panic!("private future fixture") }
                },
            );
            router.on(Filter::new(|_| true), |_| async {
                panic!("later handler ran")
            });
            let error = router.dispatch(&update()).await.unwrap_err();
            assert!(
                matches!(error, DispatchError::Panicked { index: 0, stage: actual } if actual == stage)
            );
            assert!(!format!("{error:?}").contains("private"));
        }
    }

    #[tokio::test]
    async fn cancellation_drops_active_handler_and_never_starts_next() {
        use std::{
            sync::atomic::{AtomicBool, Ordering},
            task::Poll,
        };
        struct Guard(Arc<AtomicBool>);
        impl Drop for Guard {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let dropped = Arc::new(AtomicBool::new(false));
        let observed = dropped.clone();
        let mut router = Router::<std::io::Error>::new();
        router.on(Filter::new(|_| true), move |_| {
            let guard = Guard(observed.clone());
            async move {
                let _guard = guard;
                std::future::pending().await
            }
        });
        router.on(Filter::new(|_| true), |_| async {
            panic!("later handler ran")
        });
        let update = update();
        {
            let future = router.dispatch(&update);
            tokio::pin!(future);
            std::future::poll_fn(|cx| {
                assert!(future.as_mut().poll(cx).is_pending());
                Poll::Ready(())
            })
            .await;
        }
        assert!(dropped.load(Ordering::SeqCst));
    }
}
