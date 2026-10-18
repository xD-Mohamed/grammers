// Copyright 2026 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Connect with an existing exported session entirely in memory.
//!
//! Set TELEGRAM_SESSION and TELEGRAM_API_ID in your environment, then run:
//! cargo run -p grammers-client --example import_session --features grammers-session/session-import
//! This example never writes a session file or logs the session string.

use std::{env, error::Error, sync::Arc, time::Duration};

use grammers_client::{Client, client::ClientConfiguration};
use grammers_mtsender::{ConnectionParams, SenderPool};
use grammers_session::import::ImportedSession;

async fn run() -> Result<(), Box<dyn Error>> {
    let exported = env::var("TELEGRAM_SESSION")
        .map_err(|_| "TELEGRAM_SESSION is missing or is not valid Unicode")?;
    let api_id: i32 = env::var("TELEGRAM_API_ID")?.parse()?;
    if api_id <= 0 {
        return Err("TELEGRAM_API_ID must be positive".into());
    }
    let imported = ImportedSession::parse(&exported)?;
    let params = ConnectionParams {
        use_ipv6: imported.prefer_ipv6(),
        connection_timeout: Some(Duration::from_secs(15)),
        ..Default::default()
    };
    let session = Arc::new(imported.into_memory_session());
    let SenderPool { runner, handle, .. } = SenderPool::with_configuration(session, api_id, params);
    let client = Client::with_configuration(
        handle.clone(),
        ClientConfiguration {
            auto_cache_peers: false,
        },
    );
    let pool_task = tokio::spawn(runner.run());
    let result = tokio::time::timeout(Duration::from_secs(30), client.is_authorized()).await;
    handle.quit();
    pool_task.await?;
    println!("Session authorized: {}", result??);
    Ok(())
}

fn main() -> Result<(), Box<dyn Error>> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?
        .block_on(run())
}
