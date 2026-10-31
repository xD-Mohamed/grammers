//! Optional, read-only check of this library against Telegram itself, with a bot.
//!
//! **It is ignored by default and needs your own credentials.** Nothing else in the test
//! suite contacts Telegram. Use a bot created only for testing, and your own `api_id`
//! and `api_hash` from <https://my.telegram.org>. Credentials are read from the
//! environment and neither stored nor printed; the session lives in memory.
//!
//! ```sh
//! TG_ID=12345 TG_HASH=0123abcd TG_TEST_BOT_TOKEN=123:abc \
//!     cargo test --locked -p grammers-client --test live_telegram_bot -- --ignored --nocapture
//! ```
//!
//! Run it **once**, not in a loop: each run imports the bot's authorization again, which
//! Telegram rate limits. It logs in, then makes one read-only call of each kind below and
//! listens for updates for a minute at most. It sends no messages and changes nothing
//! except creating that authorization on Telegram's side. Should Telegram answer with a
//! flood wait, the test reports it and stops rather than waiting or retrying.
//!
//! What it shows, which the local mock cannot: that Telegram accepts this layer and
//! client identification, a real login through a real key exchange, salts and time
//! correction, a real `updates.getState`, and receiving a real pushed update if someone
//! writes to the bot meanwhile. It does not exercise overload, gap recovery, or the
//! zero-hash channel form, which would need a channel the bot has no hash for.

use std::future::Future;
use std::time::Duration;

use grammers_client::Client;
use grammers_mtsender::{InvocationError, NoRetries, SenderPool, UpdatesConfiguration};
use grammers_session::storages::MemorySession;
use grammers_tl_types as tl;
use std::sync::Arc;

fn required(name: &str) -> String {
    std::env::var(name).unwrap_or_else(|_| panic!("set {name}; see this file's header"))
}

/// Stops on any flood wait, as this check must not add pressure in its own retries.
fn checked<T>(step: &str, result: Result<T, InvocationError>) -> T {
    match result {
        Ok(value) => {
            println!("ok: {step}");
            value
        }
        Err(error) => panic!(
            "{step} failed (flood wait: {:?}): {error}",
            error.retry_after()
        ),
    }
}

/// No step of this check is expected to take long, whatever the network does.
async fn within<T>(step: &str, future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(60), future)
        .await
        .unwrap_or_else(|_| panic!("{step} did not finish within a minute"))
}

#[tokio::test(flavor = "current_thread")]
#[ignore = "contacts Telegram with your own test bot; see the header of this file"]
async fn read_only_bot_session_against_telegram() {
    let api_id: i32 = required("TG_ID").parse().expect("TG_ID is a number");
    let api_hash = required("TG_HASH");
    let token = required("TG_TEST_BOT_TOKEN");

    let pool = SenderPool::with_configuration(
        Arc::new(MemorySession::default()),
        api_id,
        grammers_mtsender::ConnectionParams {
            // No automatic retry at all, which keeps this check to the requests below.
            retry_policy: Box::new(NoRetries),
            connection_timeout: Some(Duration::from_secs(30)),
            ..Default::default()
        },
    );
    let client = Client::new(pool.handle.clone());
    let SenderPool {
        runner,
        handle,
        updates,
    } = pool;
    let runner = tokio::spawn(runner.run());

    let bot = checked(
        "bot login",
        within("bot login", client.bot_sign_in(&token, &api_hash)).await,
    );
    assert!(bot.is_bot(), "the token must belong to a bot");
    println!("logged in as a bot with id {:?}", bot.id());

    let me = checked("get_me", within("get_me", client.get_me()).await);
    assert_eq!(me.id(), bot.id());

    let config = checked(
        "help.getConfig",
        within(
            "help.getConfig",
            client.invoke(&tl::functions::help::GetConfig {}),
        )
        .await,
    );
    let tl::enums::Config::Config(config) = config;
    println!("this_dc={} layer={}", config.this_dc, tl::LAYER);

    let state = checked(
        "updates.getState",
        within(
            "updates.getState",
            client.invoke(&tl::functions::updates::GetState {}),
        )
        .await,
    );
    let tl::enums::updates::State::State(state) = state;
    assert!(state.pts >= 0 && state.seq >= 0, "valid update state");

    // Telegram's own acknowledgement of a keepalive, and the update stream staying open.
    let mut stream = client
        .stream_updates(updates, UpdatesConfiguration { catch_up: false })
        .await
        .expect("create update stream");
    println!("listening for up to 60 s; write to the bot to see an update arrive");
    match tokio::time::timeout(Duration::from_secs(60), stream.next_raw()).await {
        Err(_) => println!("no update within the minute, which is not a failure"),
        Ok(result) => {
            let (update, _, _) = checked("update stream", result);
            // Only its kind: an update can hold other people's messages.
            let kind = format!("{update:?}");
            let kind = kind.split('(').next().unwrap_or_default();
            println!("received an update of kind {kind}");
        }
    }

    handle.thin.quit();
    let _ = tokio::time::timeout(Duration::from_secs(10), runner).await;
}
