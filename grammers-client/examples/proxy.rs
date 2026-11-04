//! Connectivity through a proxy, without account login or session files.
//! Set TG_ID and TG_PROXY in the runtime environment, then deliberately run:
//! cargo run -p grammers-client --example proxy --features proxy
//! TG_PROXY examples: http://127.0.0.1:8080 or http://user:pass@127.0.0.1:8080.
//! HTTPS, SOCKS4/5 and MTProxy URLs use the same setting; see docs/proxies.md.
mod support;
use grammers_client::Client;
use support::Result;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result {
    let client = Client::builder(support::api_id()?)
        .proxy(support::env("TG_PROXY")?)
        .receive_updates(false)
        .build()
        .await?;
    let result = async {
        client.connect().await?;
        println!("Connected through the configured proxy (connectivity only).");
        Ok(())
    }
    .await;
    support::finish(&client, result).await
}
