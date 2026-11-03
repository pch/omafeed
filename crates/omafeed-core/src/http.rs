//! HTTP client configuration and size-limited body reads.
//!
//! Addresses the user enters may be on the local network, so local feeds keep working. A site
//! on the internet must not lead Omafeed to the local network: redirects are checked here, and
//! links found in fetched content (discovered feeds, icons) by their callers, with
//! [`crate::util::may_request`]. Only `localhost` and IP literals count as local; names are not
//! resolved first, so a public name that resolves to a local address is not caught.
use anyhow::{Result, bail};
use futures_util::StreamExt;
use reqwest::{Client, Response};
use std::time::Duration;

/// Maximum decompressed size of a feed or web page.
pub const BODY_LIMIT: usize = 10 * 1024 * 1024;

/// Redirects followed before a request fails, as in `reqwest`'s default policy.
const MAX_REDIRECTS: usize = 8;

#[derive(Debug)]
enum RedirectError {
    TooMany,
    ToLocal,
}

impl std::fmt::Display for RedirectError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            RedirectError::TooMany => "too many redirects",
            RedirectError::ToLocal => "a site on the internet may not redirect to a local address",
        })
    }
}

impl std::error::Error for RedirectError {}

/// Follow up to [`MAX_REDIRECTS`] redirects, but never from a remote host to a local one.
fn redirect(attempt: reqwest::redirect::Attempt) -> reqwest::redirect::Action {
    if attempt.previous().len() > MAX_REDIRECTS {
        return attempt.error(RedirectError::TooMany);
    }
    let allowed = attempt
        .previous()
        .last()
        .is_some_and(|from| crate::util::may_request(from, attempt.url()));
    if allowed {
        attempt.follow()
    } else {
        attempt.error(RedirectError::ToLocal)
    }
}

pub fn client() -> Result<Client> {
    Ok(builder().build()?)
}

/// The client's configuration, for tests that add their own name resolution.
pub(crate) fn builder() -> reqwest::ClientBuilder {
    Client::builder()
        .user_agent(concat!(
            "Omafeed/",
            env!("CARGO_PKG_VERSION"),
            " (+https://github.com/pch/omafeed)"
        ))
        .connect_timeout(Duration::from_secs(10))
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::custom(redirect))
}

/// How much of a body [`read_limited`] reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Body {
    /// All of it, failing once it exceeds the limit.
    Whole,
    /// A web page up to the end of its `<head>`, cut at the limit instead of failing, so that a
    /// large page does not lose the declarations in its head.
    Head,
}

/// Read a response body of at most `limit` bytes.
pub(crate) async fn read_limited(response: Response, limit: usize, body: Body) -> Result<Vec<u8>> {
    let mut data = Vec::new();
    let mut chunks = response.bytes_stream();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk?;
        if data.len() + chunk.len() > limit {
            if body == Body::Head {
                data.extend_from_slice(&chunk[..limit - data.len()]);
                break;
            }
            bail!(
                "Response exceeds {} MB decompressed limit",
                limit / 1024 / 1024
            );
        }
        // Only search the new data (plus a tag-sized overlap) for the end of <head>.
        let search_from = data.len().saturating_sub(6);
        data.extend_from_slice(&chunk);
        if body == Body::Head
            && data[search_from..]
                .windows(7)
                .any(|w| w.eq_ignore_ascii_case(b"</head>"))
        {
            break;
        }
    }
    Ok(data)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::net::SocketAddr;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Answer every request with `response`, returning the address and the request lines.
    pub(crate) async fn server(
        response: impl Fn(&str) -> String + Send + 'static,
    ) -> (SocketAddr, std::sync::Arc<std::sync::Mutex<Vec<String>>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let log = seen.clone();
        tokio::spawn(async move {
            loop {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = Vec::new();
                while !request.windows(4).any(|w| w == b"\r\n\r\n") {
                    let mut bytes = [0; 1024];
                    let n = socket.read(&mut bytes).await.unwrap();
                    if n == 0 {
                        break;
                    }
                    request.extend_from_slice(&bytes[..n]);
                }
                let request = String::from_utf8_lossy(&request);
                let line = request.lines().next().unwrap_or_default().to_owned();
                let reply = response(&line);
                log.lock().unwrap().push(line);
                let _ = socket.write_all(reply.as_bytes()).await;
            }
        });
        (addr, seen)
    }

    pub(crate) fn ok(content_type: &str, body: &str) -> String {
        format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
    }

    fn redirect_to(location: &str) -> String {
        format!(
            "HTTP/1.1 302 Found\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        )
    }

    const OK: &str = "HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok";

    /// A client that reaches the local test servers under names that do not look local.
    pub(crate) fn client(remote: &[(&str, SocketAddr)]) -> Client {
        remote
            .iter()
            .fold(builder(), |b, (name, addr)| b.resolve(name, *addr))
            .build()
            .unwrap()
    }

    #[tokio::test]
    async fn remote_sites_may_not_redirect_to_local_addresses() {
        let (target, hits) = server(|_| OK.into()).await;
        let local = format!("http://{target}/secret");
        let (site, _) = server(move |_| redirect_to(&local)).await;
        let client = client(&[("remote.test", site)]);
        let error = client
            .get(format!("http://remote.test:{}/", site.port()))
            .send()
            .await
            .map_err(anyhow::Error::from)
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("may not redirect to a local address"),
            "{error:#}"
        );
        assert!(
            hits.lock().unwrap().is_empty(),
            "the local server was reached"
        );
    }

    #[tokio::test]
    async fn local_and_remote_to_remote_redirects_are_followed() {
        let (target, hits) = server(|_| OK.into()).await;
        // A local site may send its readers elsewhere on the local network.
        let local = format!("http://{target}/feed");
        let (site, _) = server(move |_| redirect_to(&local)).await;
        let client = client(&[("remote.test", site), ("other.test", target)]);
        let response = client.get(format!("http://{site}/")).send().await.unwrap();
        assert_eq!(response.status(), 200);
        // Remote to remote is ordinary.
        let (hop, _) =
            server(move |_| redirect_to(&format!("http://other.test:{}/moved", target.port())))
                .await;
        let client = super::tests::client(&[("remote.test", hop), ("other.test", target)]);
        let response = client
            .get(format!("http://remote.test:{}/", hop.port()))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        assert_eq!(hits.lock().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn bodies_are_read_whole_or_up_to_the_end_of_their_head() {
        let page = format!(
            "<html><head><title>T</title></head><body>{}</body></html>",
            "x".repeat(5000)
        );
        let (site, _) = server(move |_| ok("text/html", &page)).await;
        let url = format!("http://{site}/");
        let get = || async { client(&[]).get(&url).send().await.unwrap() };
        let whole = read_limited(get().await, 100_000, Body::Whole)
            .await
            .unwrap();
        assert!(whole.ends_with(b"</html>"));
        let error = read_limited(get().await, 100, Body::Whole)
            .await
            .unwrap_err();
        assert_eq!(
            error.to_string(),
            "Response exceeds 0 MB decompressed limit"
        );
        let head = read_limited(get().await, 100, Body::Head).await.unwrap();
        assert_eq!(head.len(), 100, "cut at the limit");
        let head = read_limited(get().await, 100_000, Body::Head)
            .await
            .unwrap();
        let text = String::from_utf8(head).unwrap();
        assert!(text.contains("</head>"));
    }

    #[tokio::test]
    async fn redirect_loops_still_stop() {
        let (site, hits) = server(|_| redirect_to("/again")).await;
        let error = client(&[])
            .get(format!("http://{site}/"))
            .send()
            .await
            .map_err(anyhow::Error::from)
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("too many redirects"),
            "{error:#}"
        );
        assert_eq!(hits.lock().unwrap().len(), MAX_REDIRECTS + 1);
    }
}
