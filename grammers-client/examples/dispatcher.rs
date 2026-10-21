// Copyright 2026 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Sequential bot commands with an explicit fail-fast handler error policy.
//! Run with `--features dispatcher,session-file --example dispatcher`.
//! Set TG_ID, TG_HASH and TG_BOT_TOKEN at runtime.

use grammers_client::sender::UpdatesConfiguration;
use grammers_client::{
    Client, InvocationError, SessionSource,
    dispatcher::{Filter, Router},
};
use std::{env, error::Error, ops::ControlFlow};

type Result<T = ()> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

async fn receive(client: &Client) -> Result {
    if !client.is_authorized().await? {
        client
            .bot_sign_in(&env::var("TG_BOT_TOKEN")?, &env::var("TG_HASH")?)
            .await?;
    }
    let mut updates = client
        .stream_updates(client.take_updates()?, UpdatesConfiguration::default())
        .await?;
    let mut router = Router::<InvocationError>::new();
    router.on(
        Filter::new_message() & Filter::incoming() & Filter::command("start"),
        |update| async move {
            if let grammers_client::update::Update::NewMessage(message) = update {
                message
                    .respond("Hello! Updates are processed one at a time.")
                    .await?;
            }
            Ok(ControlFlow::Break(()))
        },
    );

    let result: Result = async {
        loop {
            let update = tokio::select! {
                signal = tokio::signal::ctrl_c() => { signal?; break; },
                update = updates.next() => update?,
            };
            // Errors and caught unwinds stop this example. An application can
            // inspect DispatchError and choose its own policy for the next update.
            let _ = router.dispatch(&update).await?;
        }
        Ok(())
    }
    .await;
    let synced = updates.sync_update_state().await;
    result?;
    synced?;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result {
    let client = Client::builder(env::var("TG_ID")?.parse()?)
        .session(SessionSource::file("dispatcher.session"))
        .build()
        .await?;
    client.start().await?;
    let result = receive(&client).await;
    let stopped = client.stop().await;
    result?;
    stopped?;
    Ok(())
}
