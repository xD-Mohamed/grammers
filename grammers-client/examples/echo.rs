//! Echo incoming text using at most 16 concurrent handlers.
//! Set TG_ID, TG_HASH and TG_BOT_TOKEN; cargo run -p grammers-client --example echo --features session-file
mod support;
use grammers_client::{Client, SessionSource, update::Update};
use grammers_mtsender::UpdatesConfiguration;
use support::Result;
use tokio::task::JoinSet;

async fn handle(client: Client, update: Update) -> Result {
    match update {
        Update::NewMessage(message) if !message.outgoing() && !message.text().is_empty() => {
            let peer = message
                .peer()
                .ok_or("Message peer unavailable")?
                .to_ref()
                .await?
                .ok_or("Peer access hash unavailable")?;
            client.send_message(peer, message.text()).await?;
        }
        _ => (),
    }
    Ok(())
}

async fn listen(client: &Client) -> Result {
    let mut updates = client
        .stream_updates(
            client.take_updates()?,
            UpdatesConfiguration { catch_up: true },
        )
        .await?;
    let mut handlers = JoinSet::new();
    let result: Result = async {
        println!("Waiting for text; Ctrl-C stops and drains handlers");
        // Register once; do not recreate the shutdown future on every update.
        let shutdown = tokio::signal::ctrl_c();
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                signal = &mut shutdown => { signal?; break; }
                update = updates.next(), if handlers.len() < 16 => {
                    handlers.spawn(handle(client.clone(), update?));
                }
                Some(completed) = handlers.join_next(), if !handlers.is_empty() => {
                    completed??;
                }
            }
        }
        Ok(())
    }
    .await;
    // Bound graceful draining so an ordinary RPC cannot stall shutdown forever.
    let mut drained: Result = Ok(());
    let drain = async {
        while let Some(completed) = handlers.join_next().await {
            let completed = completed
                .map_err(|error| error.into())
                .and_then(|result| result);
            if drained.is_ok() {
                drained = completed;
            }
        }
    };
    if tokio::time::timeout(std::time::Duration::from_secs(10), drain)
        .await
        .is_err()
    {
        handlers.abort_all();
        while handlers.join_next().await.is_some() {}
        // Caller cancellation does not release sent SDK reservations; stop below
        // owns connection teardown and retirement of pending requests.
        drained = Err("Handler drain exceeded 10 seconds".into());
    }
    let synced = updates.sync_update_state().await;
    result?;
    drained?;
    synced?;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result {
    let token = support::env("TG_BOT_TOKEN")?;
    let hash = support::env("TG_HASH")?;
    let client = Client::builder(support::api_id()?)
        .session(SessionSource::file("echo.session"))
        .build()
        .await?;
    let result = async {
        client.start().await?;
        if !client.is_authorized().await? {
            client.bot_sign_in(&token, &hash).await?;
        }
        listen(&client).await
    }
    .await;
    support::finish(&client, result).await
}
