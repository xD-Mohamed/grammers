// Copyright 2026 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Read an explicitly selected session format into memory and check authorization.
//! cargo run -p grammers-client --example import_session --features session-string,session-import-files -- FORMAT INPUT_FILE [--ipv6]
//! FORMAT: native, pyrogram-string, telethon-string, gotd-file, pyrogram-file, telethon-file.
//! Source files are not modified; no login is attempted and no session is saved.
mod support;
use grammers_client::{Client, SessionSource, client::ClientConfiguration};
use std::{path::PathBuf, time::Duration};
use support::Result;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result {
    let mut args = std::env::args().skip(1);
    let format = args
        .next()
        .ok_or("Usage: import_session FORMAT INPUT_FILE [--ipv6]")?;
    let path = PathBuf::from(
        args.next()
            .ok_or("Input file is required; do not pass session text in argv")?,
    );
    let ipv6 = match args.next().as_deref() {
        None => false,
        Some("--ipv6") => true,
        Some(_) => return Err("Only --ipv6 is accepted after INPUT_FILE".into()),
    };
    if args.next().is_some() {
        return Err("Too many arguments".into());
    }
    let source = match format.as_str() {
        "native" | "pyrogram-string" | "telethon-string" => {
            // Read secret text from a file, never command-line arguments or logs.
            let text = tokio::fs::read_to_string(&path).await?;
            match format.as_str() {
                "native" => SessionSource::native(text.trim()),
                "pyrogram-string" => SessionSource::pyrogram(text.trim()),
                _ => SessionSource::telethon(text.trim()),
            }
        }
        "gotd-file" => SessionSource::gotd_file(path),
        "pyrogram-file" => SessionSource::pyrogram_file(path),
        "telethon-file" => SessionSource::telethon_file(path),
        _ => return Err("Unknown format; choose native, pyrogram-string, telethon-string, gotd-file, pyrogram-file or telethon-file".into()),
    };
    let mut builder = Client::builder(support::api_id()?)
        .session(source)
        .receive_updates(false)
        .connection_timeout(Duration::from_secs(15))
        .client_configuration(ClientConfiguration {
            auto_cache_peers: false,
        });
    // Omit the override for native strings so their exact saved preference survives.
    if ipv6 {
        builder = builder.ipv6(true);
    }
    let client = builder.build().await?;
    let result = async {
        client.start().await?;
        let authorized =
            tokio::time::timeout(Duration::from_secs(30), client.is_authorized()).await??;
        println!("Session authorized: {authorized}");
        Ok(())
    }
    .await;
    support::finish(&client, result).await
}
