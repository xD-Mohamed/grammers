// Copyright 2026 - developers of the `grammers` project.
//
// Licensed under the Apache License, Version 2.0 <LICENSE-APACHE or
// https://www.apache.org/licenses/LICENSE-2.0> or the MIT license
// <LICENSE-MIT or https://opensource.org/licenses/MIT>, at your
// option. This file may not be copied, modified, or distributed
// except according to those terms.

//! Explicit QR-login steps; run with `--features qr-login,session-file --example qr_login`.
//! Set TELEGRAM_API_ID and TELEGRAM_API_HASH at runtime. Render the printed login
//! URL as a QR code using a trusted local UI. The URL is a short-lived credential.
//! This example checks only when Enter is pressed; it does not poll Telegram.

use grammers_client::{Client, QrLogin, SessionSource, SignInError};
use std::{env, error::Error, io, time::SystemTime};

type Result<T = ()> = std::result::Result<T, Box<dyn Error + Send + Sync>>;

async fn wait_for_enter() -> Result {
    // Await the blocking reader; no input task is detached.
    tokio::task::spawn_blocking(|| {
        let mut line = String::new();
        if io::stdin().read_line(&mut line)? == 0 {
            return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "input closed"));
        }
        Ok::<_, io::Error>(())
    })
    .await??;
    Ok(())
}

async fn login(client: &Client) -> Result {
    if client.is_authorized().await? {
        println!("The saved session is already authorized.");
        return Ok(());
    }
    let api_hash = env::var("TELEGRAM_API_HASH")?;
    loop {
        match client.qr_login_step(&api_hash, &[]).await {
            Ok(QrLogin::Display(token)) => {
                println!(
                    "Render this credential as a QR code in your trusted UI: {}",
                    token.url()
                );
                println!(
                    "Expires at {:?}. Scan in Telegram, then press Enter to check or refresh.",
                    token.expires_at()
                );
                wait_for_enter().await?;
                if token.is_expired_at(SystemTime::now()) {
                    println!("The displayed token expired; requesting a fresh step.");
                }
            }
            Ok(QrLogin::Authorized(_)) => {
                println!("Signed in; session stored in qr-login.session.");
                return Ok(());
            }
            Err(SignInError::PasswordRequired(mut password_token)) => loop {
                let password =
                    tokio::task::spawn_blocking(|| rpassword::prompt_password("2FA password: "))
                        .await??;
                match client.check_password(password_token, password).await {
                    Ok(_) => {
                        println!("Signed in; session stored in qr-login.session.");
                        return Ok(());
                    }
                    Err(SignInError::InvalidPassword(token)) => {
                        password_token = token;
                        println!("Invalid password. Try again.");
                    }
                    Err(error) => return Err(error.into()),
                }
            },
            Err(error) => return Err(error.into()),
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result {
    let client = Client::builder(env::var("TELEGRAM_API_ID")?.parse()?)
        .session(SessionSource::file("qr-login.session"))
        .build()
        .await?;
    client.start().await?;
    let result = login(&client).await;
    let stopped = client.stop().await;
    result?;
    stopped?;
    Ok(())
}
