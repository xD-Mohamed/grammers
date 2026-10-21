//! Interactive user login; blocking terminal reads stay off the async reactor.
use super::support::{self, Result};
use grammers_client::{Client, SignInError};
use std::io::{self, Write};

async fn prompt(message: &'static str, secret: bool) -> Result<String> {
    Ok(tokio::task::spawn_blocking(move || -> io::Result<String> {
        if secret {
            return rpassword::prompt_password(message);
        }
        print!("{message}");
        io::stdout().flush()?;
        let mut line = String::new();
        if io::stdin().read_line(&mut line)? == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "terminal input closed",
            ));
        }
        Ok(line.trim_end_matches(['\r', '\n']).to_owned())
    })
    .await??)
}

pub async fn sign_in(client: &Client) -> Result {
    if client.is_authorized().await? {
        return Ok(());
    }
    let hash = support::env("TG_HASH")?;
    let phone = prompt("Phone (international format): ", false).await?;
    let token = client.request_login_code(phone.trim(), &hash).await?;
    let code = prompt("Login code: ", true).await?;
    match client.sign_in(&token, code.trim()).await {
        Ok(_) => (),
        Err(SignInError::PasswordRequired(token)) => {
            let password = prompt("Two-factor password (hidden): ", true).await?;
            client.check_password(token, &password).await?;
        }
        Err(error) => return Err(error.into()),
    }
    Ok(())
}
