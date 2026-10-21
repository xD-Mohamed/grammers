//! Classify invocation failures without retrying ambiguous side effects.
//! TG_ID=... cargo run -p grammers-client --example error_handling
mod support;
use grammers_client::{Client, InvocationErrorCategory};
use grammers_mtsender::NoRetries;
use grammers_tl_types as tl;
use support::Result;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result {
    let client = Client::builder(support::api_id()?)
        .receive_updates(false)
        .retry_policy(NoRetries)
        .build()
        .await?;
    let result = async {
        client.start().await?;
        match client.invoke(&tl::functions::Ping { ping_id: 1 }).await {
            Ok(_) => println!("Ping succeeded (no account authorization implied)"),
            Err(error) => {
                eprintln!("Invocation category: {:?}", error.category());
                if let Some(delay) = error.retry_after() {
                    eprintln!(
                        "Server explicitly requested a wait of {} seconds",
                        delay.as_secs()
                    );
                }
                match error.category() {
                    InvocationErrorCategory::Overloaded => eprintln!("Reduce concurrent requests"),
                    InvocationErrorCategory::Authentication => {
                        eprintln!("Check account authorization")
                    }
                    InvocationErrorCategory::Network => {
                        eprintln!("Reconcile any side effects before considering retry")
                    }
                    _ => (),
                }
                return Err(error.into());
            }
        }
        Ok(())
    }
    .await;
    support::finish(&client, result).await
}
