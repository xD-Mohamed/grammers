//! Run an existing native session until Ctrl-C, without a manual SenderPool.
//! Set TG_ID and TG_SESSION (a secret native session string) at runtime.
//! Enable `session-string`; this example is not run by the test suite.
use grammers_client::{Client, DeviceConfig, SessionSource};

#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let api_id = std::env::var("TG_ID")?.parse()?;
    let session = std::env::var("TG_SESSION")?;
    let client = Client::builder(api_id)
        .session(SessionSource::native(session))
        .device(DeviceConfig {
            device_model: "Managed client example".into(),
            app_version: env!("CARGO_PKG_VERSION").into(),
            ..Default::default()
        })
        .max_pending_requests(256)
        .build()
        .await?;

    // run_until checks connectivity and awaits terminal shutdown. It does not
    // prompt for login. To consume updates, use start + take_updates + stream_updates,
    // sync the stream's update state, then stop explicitly.
    client.run_until(tokio::signal::ctrl_c()).await?;
    Ok(())
}
