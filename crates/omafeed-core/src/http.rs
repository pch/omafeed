//! HTTP client configuration and size-limited body reads.
use anyhow::{Result, bail};
use futures_util::StreamExt;
use reqwest::{Client, Response};
use std::time::Duration;

/// Maximum decompressed size of a feed or web page.
pub const BODY_LIMIT: usize = 10 * 1024 * 1024;

pub fn client() -> Result<Client> {
    Ok(Client::builder()
        .user_agent(concat!(
            "Omafeed/",
            env!("CARGO_PKG_VERSION"),
            " (+https://github.com/pch/omafeed)"
        ))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::limited(8))
        .build()?)
}

/// Read the whole body, failing once it exceeds `limit` bytes.
pub async fn read_limited(response: Response, limit: usize) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk?;
        if data.len() + chunk.len() > limit {
            bail!(
                "Response exceeds {} MB decompressed limit",
                limit / 1024 / 1024
            );
        }
        data.extend_from_slice(&chunk);
    }
    Ok(data)
}
