// Copyright 2020 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

use std::num::{NonZeroU32, NonZeroUsize};
use std::ops::ControlFlow;
use std::time::Duration;

use crate::errors::{InvocationError, RpcError};

const DEFAULT_LOCALE: &str = "en";

/// Connection parameters used whenever a new connection is initialized.
///
/// After creating a [`crate::SenderPool::with_configuration`], the connection of
/// any of the [`crate::Sender`]s that it uses internally will be initialized with
/// an instance of [`grammers_tl_types::functions::InitConnection`].
///
/// Some fields are hidden to encourage using the Struct Update Syntax with a default.
pub struct ConnectionParams {
    /// "Device model" according to [`initConnection`](https://core.telegram.org/method/initConnection).
    pub device_model: String,

    /// "Operation system version" according to [`initConnection`](https://core.telegram.org/method/initConnection).
    pub system_version: String,

    /// "Application version" according to [`initConnection`](https://core.telegram.org/method/initConnection).
    pub app_version: String,

    /// Code for the language used on the device's OS, formatted using the ISO 639-1 standard.
    pub system_lang_code: String,

    /// Either an ISO 639-1 language code or a language pack name obtained from
    /// a [language pack link](https://core.telegram.org/api/links#language-pack-links).
    pub lang_code: String,

    /// URL of the proxy to use. Requires the `proxy` feature to be enabled.
    ///
    /// HTTP/HTTPS CONNECT and SOCKS4/5 are supported, e.g.:
    /// - `http://127.0.0.1:8080`
    /// - `http://username:password@example.com:8080` (Basic authentication)
    /// - `https://username:password@example.com:8443` (certificate-verified TLS to the proxy)
    /// - `socks5://username:password@example.com:1080`
    /// - `socks4://userid@example.com:1080` (IPv4 targets, no password)
    ///
    /// URL credentials are percent-decoded as UTF-8. HTTP defaults to port 80,
    /// HTTPS to 443; SOCKS requires an explicit port. `socks4a`/`socks5h` are
    /// accepted aliases; DC targets are already numeric, so no remote DNS is needed.
    /// MTProxy accepts `mtproxy://host:port?secret=HEX_OR_BASE64`, `tg://proxy?...`
    /// and `https://t.me/proxy?...` links, including classic, dd and ee secrets.
    /// The pool selects its framing/DC tag automatically. Lower-level senders
    /// must use [`crate::ProxyTransport::for_proxy`] for MTProxy.
    ///
    /// If a domain is used for the proxy host, its address will be looked up,
    /// and the first IP address found will be used. If a different IP address should be used, consider resolving
    /// the host manually and selecting an IP address of your choice.
    /// The configured proxy is used for every DC/reconnect, with no direct fallback.
    /// HTTPS uses bundled Mozilla roots; ee is authenticated fake TLS, not HTTPS.
    /// Experimental Telegram WEB relays are not supported.
    #[cfg(feature = "proxy")]
    pub proxy_url: Option<String>,

    /// Whether to connect via IPv6 instead of defaulting to IPv4.
    pub use_ipv6: bool,

    /// The retry policy to use when encountering errors after invoking a request.
    pub retry_policy: Box<dyn super::RetryPolicy>,

    /// Maximum number of updates that can be buffered in the internal channel
    /// between the network senders and the updates receiver.
    ///
    /// When the channel is full, newly received updates are **dropped**.
    pub updates_channel_capacity: NonZeroUsize,

    /// Receive unsolicited API updates and copies of updates produced by RPCs.
    /// Defaults to true. When false, API calls use invokeWithoutUpdates and
    /// incoming API updates are acknowledged but not decoded/forwarded.
    /// RPC return values and required MTProto service messages still work.
    /// The updates channel is closed and creating an update stream is an error.
    pub receive_updates: bool,

    /// How many acknowledgements to collect before attaching them to a request.
    /// Defaults to 1, which attaches them to every request inside a container.
    /// Larger values send most requests without that container; an
    /// acknowledgement still waits at most ten seconds once another request goes out.
    pub ack_batch: usize,

    /// Optional deadline for DNS, socket connection, proxy/TLS negotiation,
    /// key exchange and initialization.
    /// None preserves unlimited establishment; this is not an ordinary RPC timeout.
    pub connection_timeout: Option<Duration>,

    /// Optional maximum time without a complete valid inbound packet. This
    /// detects silent established connections; it is not a per-RPC deadline.
    /// Partial socket reads do not reset it, so a slow or large response may
    /// expire despite ongoing transport progress. Allow enough time to receive
    /// a complete packet, or leave this disabled (`None`).
    pub liveness_timeout: Option<Duration>,

    /// Independent inbound decompression/work limits, applied before initialization.
    pub decode_limits: grammers_mtproto::mtp::DecodeLimits,

    /// Maximum queued plus in-flight requests. At capacity admission fails with
    /// [`std::io::ErrorKind::WouldBlock`]. Caller cancellation does not release
    /// capacity held by an already serialized request. `None` is unlimited.
    pub max_pending_requests: Option<NonZeroUsize>,

    /// Maximum sum of admitted request-body bytes, queued or in flight. Shared
    /// bodies count once per invocation. `None` preserves the unlimited default.
    /// Cancellation of a sent call does not release its byte reservation.
    pub max_pending_bytes: Option<NonZeroUsize>,

    #[doc(hidden)]
    pub __non_exhaustive: (),
}

/// Configuration that controls [`crate::UpdatesReceiver`].
pub struct UpdatesConfiguration {
    /// Should the [`crate::UpdatesReceiver`] catch-up on updates sent to it while it was offline?
    ///
    /// By default, updates sent while the [`crate::UpdatesReceiver`] was offline are ignored.
    pub catch_up: bool,
}

/// This trait controls how the [`SenderPoolRunner`] should behave when
/// an invoked request fails with an [`InvocationError`].
///
/// [`SenderPoolRunner`]: crate::SenderPoolRunner
pub trait RetryPolicy: Send + Sync {
    /// Determines whether the failing request should retry.
    ///
    /// If it should Continue, a sleep duration before retrying is included.\
    /// If it should Break, the context error will be propagated to the caller.
    fn should_retry(&self, ctx: &RetryContext) -> ControlFlow<(), Duration>;
}

/// Context passed to [`RetryPolicy::should_retry`].
pub struct RetryContext {
    /// Amount of times the instance of this request has failed.
    pub fail_count: NonZeroU32,
    /// Sum of the durations for all previous continuations (not total time elapsed since first failure).
    pub slept_so_far: Duration,
    /// The most recent error caused by the instance of the request.
    pub error: InvocationError,
}

/// Retry policy that will never retry.
pub struct NoRetries;

/// Retry policy that will retry up to `tries` times on flood-wait and slow mode wait errors.
///
/// The library will sleep only if the duration to sleep for is below or equal to the threshold.
pub struct AutoSleep {
    /// The (inclusive) number of tries below which the request should be retried.
    pub tries: NonZeroU32,

    /// The (inclusive) threshold below which the library should automatically sleep.
    pub threshold: Duration,

    /// Delay for retryable connection I/O errors, independent of `threshold`
    /// and still bounded by `tries`. Retries `ConnectionRefused`, `ConnectionReset`,
    /// `ConnectionAborted`, `NotConnected`, `BrokenPipe`, `TimedOut`,
    /// `UnexpectedEof`, `NetworkDown`, `NetworkUnreachable`, and `HostUnreachable`.
    /// For [`InvocationError::Read`], the request must be definitely unsent.
    /// Legacy [`InvocationError::Io`] has no delivery marker and retries these
    /// same kinds; producers must use `Read` for failures after possible delivery.
    /// Other error kinds, including overload and invalid data, are not retried.
    pub io_errors_as_flood_of: Option<Duration>,
}

impl Default for ConnectionParams {
    /// Returns an instance with an [`AutoSleep::default`] retry policy,
    /// with a capacity of 100 updates.
    ///
    /// [`AutoSleep::default`]: super::AutoSleep::default
    fn default() -> Self {
        let info = os_info::get();

        let mut system_lang_code = String::new();
        let mut lang_code = String::new();

        #[cfg(not(target_os = "android"))]
        {
            system_lang_code.push_str(&locate_locale::system());
            lang_code.push_str(&locate_locale::user());
        }
        if system_lang_code.is_empty() {
            system_lang_code.push_str(DEFAULT_LOCALE);
        }
        if lang_code.is_empty() {
            lang_code.push_str(DEFAULT_LOCALE);
        }

        Self {
            device_model: format!("{} {}", info.os_type(), info.bitness()),
            system_version: info.version().to_string(),
            app_version: env!("CARGO_PKG_VERSION").to_string(),
            system_lang_code,
            lang_code,
            use_ipv6: false,
            #[cfg(feature = "proxy")]
            proxy_url: None,
            retry_policy: Box::new(super::AutoSleep::default()),
            updates_channel_capacity: NonZeroUsize::new(100).unwrap(),
            receive_updates: true,
            ack_batch: 1,
            connection_timeout: None,
            liveness_timeout: None,
            decode_limits: Default::default(),
            max_pending_requests: None,
            max_pending_bytes: None,
            __non_exhaustive: (),
        }
    }
}

impl Default for UpdatesConfiguration {
    /// Returns an instance that will not catch up.
    fn default() -> Self {
        Self { catch_up: false }
    }
}

impl RetryPolicy for NoRetries {
    fn should_retry(&self, _: &RetryContext) -> ControlFlow<(), Duration> {
        ControlFlow::Break(())
    }
}

fn retryable_io(error: &std::io::Error) -> bool {
    use std::io::ErrorKind::*;
    matches!(
        error.kind(),
        ConnectionRefused
            | ConnectionReset
            | ConnectionAborted
            | NotConnected
            | BrokenPipe
            | TimedOut
            | UnexpectedEof
            | NetworkDown
            | NetworkUnreachable
            | HostUnreachable
    )
}

impl RetryPolicy for AutoSleep {
    fn should_retry(&self, ctx: &RetryContext) -> ControlFlow<(), Duration> {
        match ctx.error.root_cause() {
            InvocationError::Read {
                error,
                may_have_been_sent: false,
            } if ctx.fail_count <= self.tries && error.io_error().is_some_and(retryable_io) => self
                .io_errors_as_flood_of
                .map_or(ControlFlow::Break(()), ControlFlow::Continue),
            InvocationError::Rpc(RpcError {
                code: 420,
                value: Some(seconds),
                ..
            }) if ctx.fail_count <= self.tries && *seconds as u64 <= self.threshold.as_secs() => {
                ControlFlow::Continue(Duration::from_secs(*seconds as _))
            }
            InvocationError::Io(error) if ctx.fail_count <= self.tries && retryable_io(error) => {
                if let Some(duration) = self.io_errors_as_flood_of {
                    ControlFlow::Continue(duration)
                } else {
                    ControlFlow::Break(())
                }
            }
            _ => ControlFlow::Break(()),
        }
    }
}

impl Default for AutoSleep {
    /// Returns an instance with a threshold of 60 seconds.
    ///
    /// I/O errors will be treated as if they were a 1-second flood.
    fn default() -> Self {
        Self {
            tries: NonZeroU32::MIN,
            threshold: Duration::from_secs(60),
            io_errors_as_flood_of: Some(Duration::from_secs(1)),
        }
    }
}

/// Bound cumulative sleep approved by another retry policy. The wrapper either
/// preserves the complete delay or stops; it never shortens a server flood wait.
/// This is a sleep budget, not an elapsed request deadline or replay guarantee.
///
/// ```
/// use std::time::Duration;
/// use grammers_mtsender::{AutoSleep, RetryBudget};
/// let policy = RetryBudget::new(AutoSleep::default(), Duration::from_secs(30));
/// ```
pub struct RetryBudget<P> {
    policy: P,
    max_sleep: Duration,
}

impl<P: RetryPolicy> RetryBudget<P> {
    /// Wrap `policy` with a maximum cumulative retry sleep.
    pub fn new(policy: P, max_sleep: Duration) -> Self {
        Self { policy, max_sleep }
    }
}

impl<P: RetryPolicy> RetryPolicy for RetryBudget<P> {
    fn should_retry(&self, ctx: &RetryContext) -> ControlFlow<(), Duration> {
        match self.policy.should_retry(ctx) {
            ControlFlow::Continue(delay)
                if ctx
                    .slept_so_far
                    .checked_add(delay)
                    .is_some_and(|total| total <= self.max_sleep) =>
            {
                ControlFlow::Continue(delay)
            }
            _ => ControlFlow::Break(()),
        }
    }
}

#[cfg(test)]
mod retry_budget_tests {
    use super::*;

    #[test]
    fn auto_sleep_retries_only_definite_connection_errors() {
        use std::io::{Error, ErrorKind};
        let policy = AutoSleep::default();
        for kind in [
            ErrorKind::InvalidInput,
            ErrorKind::InvalidData,
            ErrorKind::PermissionDenied,
            ErrorKind::WouldBlock,
            ErrorKind::Unsupported,
            ErrorKind::ConnectionRefused,
        ] {
            let expected = if kind == ErrorKind::ConnectionRefused {
                ControlFlow::Continue(Duration::from_secs(1))
            } else {
                ControlFlow::Break(())
            };
            let mut ctx = context(Duration::ZERO);
            ctx.error = InvocationError::Io(Error::from(kind));
            assert_eq!(policy.should_retry(&ctx), expected);
            ctx.error = InvocationError::Read {
                error: std::sync::Arc::new(crate::ReadError::Io(Error::from(kind))),
                may_have_been_sent: false,
            };
            assert_eq!(policy.should_retry(&ctx), expected);
        }
    }

    struct Fixed(Option<Duration>);
    impl RetryPolicy for Fixed {
        fn should_retry(&self, _: &RetryContext) -> ControlFlow<(), Duration> {
            self.0.map_or(ControlFlow::Break(()), ControlFlow::Continue)
        }
    }
    fn context(slept_so_far: Duration) -> RetryContext {
        RetryContext {
            fail_count: NonZeroU32::MIN,
            slept_so_far,
            error: InvocationError::Dropped("test"),
        }
    }
    #[test]
    fn retry_budget_preserves_delays_and_stops_at_total_limit() {
        let delay = Duration::from_secs(2);
        let policy = RetryBudget::new(Fixed(Some(delay)), Duration::from_secs(5));
        assert_eq!(
            policy.should_retry(&context(Duration::from_secs(3))),
            ControlFlow::Continue(delay)
        );
        assert_eq!(
            policy.should_retry(&context(Duration::from_secs(4))),
            ControlFlow::Break(())
        );
        assert_eq!(
            RetryBudget::new(Fixed(Some(delay)), Duration::ZERO)
                .should_retry(&context(Duration::ZERO)),
            ControlFlow::Break(())
        );
        assert_eq!(
            RetryBudget::new(Fixed(Some(Duration::ZERO)), Duration::ZERO)
                .should_retry(&context(Duration::ZERO)),
            ControlFlow::Continue(Duration::ZERO)
        );
        assert_eq!(
            RetryBudget::new(Fixed(None), Duration::MAX).should_retry(&context(Duration::ZERO)),
            ControlFlow::Break(())
        );
        assert_eq!(
            RetryBudget::new(Fixed(Some(delay)), Duration::MAX)
                .should_retry(&context(Duration::MAX)),
            ControlFlow::Break(())
        );
    }
}
