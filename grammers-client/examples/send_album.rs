//! Reuse two existing photo/document messages as an album in Saved Messages.
//! cargo run -p grammers-client --example send_album --features session-file -- PEER ID ID
mod support;
#[path = "support/user_login.rs"]
mod user_login;
use grammers_client::{Client, SessionSource, media::InputMedia};
use grammers_session::types::PeerId;
use grammers_tl_types as tl;
use support::Result;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result {
    let args: Vec<_> = std::env::args().skip(1).collect();
    if args.len() != 3 {
        return Err("Usage: send_album PEER MESSAGE_ID MESSAGE_ID".into());
    }
    let ids = [args[1].parse::<i32>()?, args[2].parse::<i32>()?];
    if ids.iter().any(|id| *id <= 0) {
        return Err("Message IDs must be positive".into());
    }
    let client = Client::builder(support::api_id()?)
        .session(SessionSource::file("send_album.session"))
        .build()
        .await?;
    let result = async {
        client.start().await?;
        user_login::sign_in(&client).await?;
        let peer = client
            .resolve_username(&args[0])
            .await?
            .ok_or("No peer with this username")?
            .to_ref()
            .await?
            .ok_or("Peer access hash is unavailable")?;
        let mut album = Vec::with_capacity(ids.len());
        for message in client.get_messages_by_id(peer, &ids).await? {
            let mut media = message
                .ok_or("Source message is unavailable")?
                .media()
                .ok_or("Source message has no media")?
                .to_raw_input_media()
                .ok_or("Source media cannot be reused")?;
            match &mut media {
                tl::enums::InputMedia::Document(document) => document.spoiler = true,
                tl::enums::InputMedia::Photo(photo) => photo.spoiler = true,
                _ => return Err("Albums need photo or document media".into()),
            }
            album.push(InputMedia::new().media(media));
        }
        let sent = client
            .send_album(PeerId::self_user().to_ambient_ref(), album)
            .await?;
        println!("Sent {} album messages to Saved Messages", sent.len());
        Ok(())
    }
    .await;
    support::finish(&client, result).await
}
