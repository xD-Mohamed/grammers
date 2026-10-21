//! Small example-only helpers; no credentials are compiled into binaries.
use grammers_client::Client;

pub type Result<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

pub fn env(name: &str) -> Result<String> {
    std::env::var(name)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            format!("Set {name} in the runtime environment before running this example").into()
        })
}

pub fn api_id() -> Result<i32> {
    let id = env("TG_ID")?
        .parse::<i32>()
        .map_err(|_| "TG_ID must be a positive i32")?;
    if id <= 0 {
        return Err("TG_ID must be positive".into());
    }
    Ok(id)
}

// Always await shutdown, including when the operation fails. Keep the first error.
pub async fn finish(client: &Client, result: Result) -> Result {
    let stopped = client.stop().await;
    if let (Err(_), Err(error)) = (&result, &stopped) {
        eprintln!("Shutdown also failed: {error}");
    }
    result?;
    stopped?;
    Ok(())
}
