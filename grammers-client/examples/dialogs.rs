//! Log in interactively and list dialog titles and IDs.
//! cargo run -p grammers-client --example dialogs --features session-file
mod support;
#[path = "support/user_login.rs"]
mod user_login;
use grammers_client::{Client, SessionSource};
use support::Result;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result {
    let client = Client::builder(support::api_id()?)
        .session(SessionSource::file("dialogs.session"))
        .build()
        .await?;
    let result = async {
        client.start().await?;
        user_login::sign_in(&client).await?;
        let mut dialogs = client.iter_dialogs();
        println!("Up to {} dialogs:", dialogs.total().await?);
        while let Some(dialog) = dialogs.next().await? {
            println!(
                "{} {}",
                dialog.peer().id(),
                dialog.peer().name().unwrap_or_default()
            );
        }
        Ok(())
    }
    .await;
    support::finish(&client, result).await
}
