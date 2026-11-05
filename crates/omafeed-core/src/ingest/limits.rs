//! Hosts that asked omafeed to stay away for a while. Reddit, for one, answers a client only
//! once a minute and says so in `X-RateLimit-*` headers rather than with `Retry-After`.
use super::schedule;
use reqwest::{StatusCode, header::HeaderMap};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::SystemTime,
};

/// The host part of `url`, or an empty string.
pub(super) fn host(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_owned))
        .unwrap_or_default()
}

/// Until when each host asked not to be requested, as Unix times. Cheap to clone; clones share
/// one table, so discovery and refreshes heed each other.
#[derive(Clone, Default)]
pub(crate) struct HostLimits(Arc<Mutex<HashMap<String, i64>>>);

impl HostLimits {
    /// Until when the host of `url` asked not to be requested, if that is after `now`.
    pub(crate) fn until(&self, url: &str, now: i64) -> Option<i64> {
        let hosts = self.0.lock().unwrap();
        hosts.get(&host(url)).copied().filter(|&t| t > now)
    }

    /// Note the wait a response asks for, returning it in seconds, for the hosts of `urls`: the
    /// address requested and the one that answered after redirects.
    pub(crate) fn note(
        &self,
        urls: &[&str],
        status: StatusCode,
        headers: &HeaderMap,
    ) -> Option<i64> {
        let wait = requested_wait(status, headers, SystemTime::now())?;
        let until = chrono::Utc::now().timestamp() + wait;
        let mut hosts = self.0.lock().unwrap();
        for url in urls {
            let entry = hosts.entry(host(url)).or_default();
            *entry = (*entry).max(until);
        }
        Some(wait)
    }
}

/// How many seconds a response asks the client to leave its host alone: after a 429 or 503,
/// what `Retry-After` (else `X-RateLimit-Reset`) says; after any other response that reports
/// no requests left (`X-RateLimit-Remaining` under one), what `X-RateLimit-Reset` says.
fn requested_wait(status: StatusCode, headers: &HeaderMap, now: SystemTime) -> Option<i64> {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());
    let unix_now = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs() as i64);
    let reset = header("x-ratelimit-reset").and_then(|v| schedule::rate_limit_reset(v, unix_now));
    if matches!(
        status,
        StatusCode::TOO_MANY_REQUESTS | StatusCode::SERVICE_UNAVAILABLE
    ) {
        return header("retry-after")
            .map(|v| schedule::retry_after(v, now))
            .or(reset);
    }
    let exhausted = header("x-ratelimit-remaining")
        .and_then(|v| v.trim().parse::<f64>().ok())
        .is_some_and(|left| left < 1.0);
    reset.filter(|_| exhausted)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        pairs
            .iter()
            .map(|(k, v)| {
                (
                    reqwest::header::HeaderName::from_static(k),
                    v.parse().unwrap(),
                )
            })
            .collect()
    }

    #[test]
    fn waits_come_from_retry_after_or_rate_limit_headers() {
        let now = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let wait =
            |status, pairs: &[(&'static str, &str)]| requested_wait(status, &headers(pairs), now);
        let reddit = [
            ("x-ratelimit-remaining", "0.0"),
            ("x-ratelimit-reset", "45"),
        ];
        assert_eq!(wait(StatusCode::OK, &reddit), Some(45));
        assert_eq!(wait(StatusCode::TOO_MANY_REQUESTS, &reddit), Some(45));
        let more_left = [("x-ratelimit-remaining", "99"), ("x-ratelimit-reset", "45")];
        assert_eq!(wait(StatusCode::OK, &more_left), None);
        assert_eq!(wait(StatusCode::TOO_MANY_REQUESTS, &more_left), Some(45));
        let both = [("retry-after", "120"), ("x-ratelimit-reset", "45")];
        assert_eq!(wait(StatusCode::SERVICE_UNAVAILABLE, &both), Some(120));
        // A server's 403 asks for nothing, whatever it sends.
        assert_eq!(wait(StatusCode::FORBIDDEN, &[("retry-after", "0")]), None);
        assert_eq!(wait(StatusCode::TOO_MANY_REQUESTS, &[]), None);
    }

    #[test]
    fn a_host_is_held_back_until_its_wait_is_over() {
        let limits = HostLimits::default();
        let now = chrono::Utc::now().timestamp();
        let reddit = headers(&[("x-ratelimit-remaining", "0"), ("x-ratelimit-reset", "60")]);
        let url = "https://www.reddit.com/r/rust/.rss";
        assert_eq!(
            limits.note(&[url, "https://old.reddit.com/"], StatusCode::OK, &reddit),
            Some(60)
        );
        let until = limits
            .until("https://www.reddit.com/r/linux/.rss", now)
            .unwrap();
        assert!((now + 59..=now + 61).contains(&until), "{until}");
        assert_eq!(limits.until("https://reddit.com/r/rust/.rss", now), None);
        assert_eq!(
            limits.until("https://old.reddit.com/r/rust/", now),
            Some(until),
            "redirect"
        );
        assert_eq!(limits.until(url, until), None, "over at its end");
        // A shorter wait noted later does not cut the first one short.
        let soon = headers(&[("retry-after", "5")]);
        limits.note(&[url], StatusCode::TOO_MANY_REQUESTS, &soon);
        assert_eq!(limits.until(url, now), Some(until));
    }
}
