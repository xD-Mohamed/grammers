//! Export a native SQLite session to a secret native string without networking.
//! cargo run -p grammers-client --example export_session --features session-file,session-string -- INPUT_SESSION (--output NEW_FILE | --stdout)
//! The exported string grants account access. Store it privately; do not share or commit it.
mod support;
use grammers_client::{Client, SessionSource};
use std::io::Write;
use support::Result;

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result {
    let mut args = std::env::args().skip(1);
    let input = args
        .next()
        .ok_or("Usage: export_session INPUT_SESSION (--output NEW_FILE | --stdout)")?;
    let option = args
        .next()
        .ok_or("Choose --output NEW_FILE or explicitly choose --stdout")?;
    let output = match option.as_str() {
        "--output" => Some(args.next().ok_or("--output requires a new file path")?),
        "--stdout" => None,
        _ => return Err("Choose --output NEW_FILE or --stdout".into()),
    };
    if args.next().is_some() {
        return Err("Too many arguments".into());
    }
    // SessionSource::file normally creates missing databases; refuse that here.
    if !std::path::Path::new(&input).is_file() {
        return Err("Input session file does not exist".into());
    }
    eprintln!("SECRET: exported session strings grant account access. Protect the output.");
    let client = Client::builder(support::api_id()?)
        .session(SessionSource::file(input))
        .build()
        .await?;
    let result = async {
        // Deliberately do not start/connect: export is local and does not authorize.
        let exported = client.export_session_string()?;
        tokio::task::spawn_blocking(move || -> Result {
            if let Some(path) = output {
                let mut options = std::fs::OpenOptions::new();
                options.write(true).create_new(true);
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;
                    options.mode(0o600);
                }
                let mut file = options.open(path)?;
                file.write_all(exported.as_bytes())?;
                file.sync_all()?;
            } else {
                let mut stdout = std::io::stdout().lock();
                stdout.write_all(exported.as_bytes())?;
                stdout.write_all(b"\n")?;
                stdout.flush()?;
            }
            Ok(())
        })
        .await??;
        Ok(())
    }
    .await;
    support::finish(&client, result).await
}
