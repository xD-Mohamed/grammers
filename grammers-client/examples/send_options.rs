//! Send one quiet message to Saved Messages using an existing native session.
//! Set TG_ID; cargo run -p grammers-client --example send_options --features session-string -- NATIVE_STRING_FILE
//! The file contains a secret session string. This example makes a real send when run.
mod support;
use grammers_client::{Client, SendOptions, SessionSource};
use grammers_session::types::PeerId;
use support::Result;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result {
    let mut args = std::env::args().skip(1);
    let path = args
        .next()
        .ok_or("Usage: send_options NATIVE_STRING_FILE")?;
    if args.next().is_some() {
        return Err("Usage: send_options NATIVE_STRING_FILE".into());
    }
    let text = tokio::fs::read_to_string(path).await?;
    let client = Client::builder(support::api_id()?)
        .session(SessionSource::native(text.trim()))
        .receive_updates(false)
        .build()
        .await?;
    let result = async {
        client.start().await?;
        if !client.is_authorized().await? {
            return Err("The supplied session is not authorized; log in separately first".into());
        }
        let message = client
            .send_message_with_options(
                PeerId::self_user().to_ambient_ref(),
                "A quiet message from the SendOptions example",
                SendOptions {
                    silent: true,
                    ..Default::default()
                },
            )
            .await?;
        println!("Sent message {} to Saved Messages", message.id());
        Ok(())
    }
    .await;
    support::finish(&client, result).await
}
