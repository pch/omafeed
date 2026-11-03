//! Conditional feed downloads and the server's requests to wait.
use super::{parse::parse_document, schedule};
use crate::{
    http::{self, BODY_LIMIT},
    model::{Download, Feed, Update},
};
use anyhow::{Context, Result};
use reqwest::{Client, StatusCode, header};
use std::time::SystemTime;

#[derive(Debug)]
pub(super) struct RetryDelay(pub(super) i64);

impl std::fmt::Display for RetryDelay {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Server requested retry in {} seconds", self.0)
    }
}

impl std::error::Error for RetryDelay {}

pub async fn download(client: &Client, feed: &Feed) -> Result<Download> {
    Ok(download_from(client, feed).await?.0)
}

/// Download `feed`, also returning the URL it was finally served from, after redirects.
pub(super) async fn download_from(client: &Client, feed: &Feed) -> Result<(Download, String)> {
    let mut request = client.get(&feed.url);
    if let Some(etag) = &feed.etag {
        request = request.header(header::IF_NONE_MATCH, etag);
    }
    if let Some(modified) = &feed.modified {
        request = request.header(header::IF_MODIFIED_SINCE, modified);
    }
    let response = request.send().await.context("Download feed")?;
    let status = response.status();
    let served_from = response.url().to_string();
    if status == StatusCode::NOT_MODIFIED {
        return Ok((Download::NotModified, served_from));
    }
    if matches!(
        status,
        StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
    ) && let Some(value) = response
        .headers()
        .get(header::RETRY_AFTER)
        .and_then(|v| v.to_str().ok())
    {
        return Err(RetryDelay(schedule::retry_after(value, SystemTime::now())).into());
    }
    let response = response.error_for_status()?;
    let header = |name| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_owned)
    };
    let etag = header(header::ETAG);
    let modified = header(header::LAST_MODIFIED);
    let base = response.url().to_string();
    let data = http::read_limited(response, BODY_LIMIT, http::Body::Whole).await?;
    let (entries, site_url) =
        tokio::task::spawn_blocking(move || parse_document(&data, &base)).await??;
    Ok((
        Download::Updated(Update {
            site_url,
            entries,
            etag,
            modified,
        }),
        served_from,
    ))
}
