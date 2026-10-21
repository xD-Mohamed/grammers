//! Download a peer's messages/media to an explicitly selected output directory.
//! cargo run -p grammers-client --example downloader --features session-file -- PEER OUTPUT_DIR
mod support;
#[path = "support/user_login.rs"]
mod user_login;
use grammers_client::{Client, SessionSource, media::Media};
use std::path::PathBuf;
use support::Result;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result {
    let mut args = std::env::args().skip(1);
    let peer_name = args.next().ok_or("Usage: downloader PEER OUTPUT_DIR")?;
    let destination = PathBuf::from(args.next().ok_or("Usage: downloader PEER OUTPUT_DIR")?);
    if args.next().is_some() {
        return Err("Usage: downloader PEER OUTPUT_DIR".into());
    }
    let client = Client::builder(support::api_id()?)
        .session(SessionSource::file("downloader.session"))
        .build()
        .await?;
    let result = async {
        client.start().await?;
        user_login::sign_in(&client).await?;
        let peer = client
            .resolve_username(&peer_name)
            .await?
            .ok_or("No peer with this username")?
            .to_ref()
            .await?
            .ok_or("Peer access hash is unavailable")?;
        tokio::fs::create_dir_all(&destination).await?;
        let mut messages = client.iter_messages(peer);
        println!("Up to {} messages", messages.total().await?);
        while let Some(message) = messages.next().await? {
            println!("{}: {}", message.id(), message.text());
            if let Some(media) = message.media() {
                let path =
                    destination.join(format!("message-{}{}", message.id(), extension(&media)));
                client.download_media(&media, &path).await?;
            }
        }
        Ok(())
    }
    .await;
    support::finish(&client, result).await
}

fn extension(media: &Media) -> String {
    let mime = match media {
        Media::Photo(_) => return ".jpg".into(),
        Media::Sticker(sticker) => sticker.document.mime_type(),
        Media::Document(document) => document.mime_type(),
        Media::Contact(_) => return ".vcf".into(),
        _ => None,
    };
    // Use the MIME database's known extensions, never server-provided path text.
    mime.and_then(mime_guess::get_mime_extensions_str)
        .and_then(|extensions| extensions.first())
        .map(|extension| format!(".{extension}"))
        .unwrap_or_default()
}
