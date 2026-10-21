//! Inline Fibonacci pagination with ordered handling and checked callback data.
//! Set TG_ID, TG_HASH, TG_BOT_TOKEN; cargo run -p grammers-client --example inline-pagination --features session-file
mod support;
use grammers_client::{
    Client, SessionSource,
    message::{Button, InputMessage, ReplyMarkup},
    update::Update,
};
use support::Result;

fn fib_markup(mut a: u128, mut b: u128) -> ReplyMarkup {
    let mut rows = Vec::with_capacity(5);
    let mut exhausted = false;
    for _ in 0..4 {
        let text = a.to_string();
        rows.push(vec![Button::data(&text, text.as_bytes())]);
        let Some(next) = a.checked_add(b) else {
            exhausted = true;
            break;
        };
        a = b;
        b = next;
    }
    let next = format!("{a},{b}");
    if exhausted || next.len() > 64 {
        rows.push(vec![Button::data("Done", b"done")]);
    } else {
        rows.push(vec![
            Button::data("Restart", b"0,1"),
            Button::data("More", next.as_bytes()),
        ]);
    }
    ReplyMarkup::from_buttons(&rows)
}

async fn handle(update: Update) -> Result {
    match update {
        Update::NewMessage(message) if message.text() == "/start" => {
            message
                .respond(
                    InputMessage::new()
                        .text("Fibonacci numbers")
                        .reply_markup(fib_markup(0, 1)),
                )
                .await?;
        }
        Update::CallbackQuery(query) => {
            // Callback payloads are untrusted, including data from old buttons.
            let Ok(data) = std::str::from_utf8(query.data()) else {
                query.answer().text("Invalid button data").send().await?;
                return Ok(());
            };
            if data == "done" {
                query.answer().edit("Thanks for trying it!").await?;
            } else if let Some((a, b)) = data.split_once(',') {
                if let (Ok(a), Ok(b)) = (a.parse::<u128>(), b.parse::<u128>()) {
                    query
                        .answer()
                        .edit(
                            InputMessage::new()
                                .text("More Fibonacci numbers")
                                .reply_markup(fib_markup(a, b)),
                        )
                        .await?;
                } else {
                    query.answer().text("Invalid button data").send().await?;
                }
            } else if let Ok(number) = data.parse::<u128>() {
                let text = if number % 2 == 0 {
                    "Even number"
                } else {
                    "Odd number"
                };
                query.answer().text(text).send().await?;
            } else {
                query.answer().text("Invalid button data").send().await?;
            }
        }
        _ => (),
    }
    Ok(())
}

async fn listen(client: &Client) -> Result {
    let mut updates = client
        .stream_updates(client.take_updates()?, Default::default())
        .await?;
    let result: Result = async {
        println!("Send /start to the bot; Ctrl-C stops");
        let shutdown = tokio::signal::ctrl_c();
        tokio::pin!(shutdown);
        loop {
            tokio::select! {
                signal = &mut shutdown => { signal?; break; }
                update = updates.next() => handle(update?).await?,
            }
        }
        Ok(())
    }
    .await;
    let synced = updates.sync_update_state().await;
    result?;
    synced?;
    Ok(())
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result {
    let token = support::env("TG_BOT_TOKEN")?;
    let hash = support::env("TG_HASH")?;
    let client = Client::builder(support::api_id()?)
        .session(SessionSource::file("inline-pagination.session"))
        .build()
        .await?;
    let result = async {
        client.start().await?;
        if !client.is_authorized().await? {
            client.bot_sign_in(&token, &hash).await?;
        }
        listen(&client).await
    }
    .await;
    support::finish(&client, result).await
}
