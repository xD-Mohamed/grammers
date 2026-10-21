//! Send a raw MTProto Ping without account authorization or session files.
//! Run with TG_ID set: cargo run -p grammers-client --example ping
mod support;
use grammers_client::Client;
use grammers_tl_types as tl;
use support::Result;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result {
    let client = Client::builder(support::api_id()?)
        .receive_updates(false)
        .build()
        .await?;
    let result = async {
        client.start().await?;
        let pong = client.invoke(&tl::functions::Ping { ping_id: 0 }).await?;
        println!("{pong:?}");
        // A successful Ping establishes connectivity, not account authorization.
        Ok(())
    }
    .await;
    support::finish(&client, result).await
}
