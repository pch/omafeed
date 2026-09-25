//! Small helpers shared by OPML, storage, fetching, and rendering.
use anyhow::{Result, bail};
use std::net::IpAddr;
use url::{Host, Url};

/// Escape text for HTML/XML element content and quoted attributes.
pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

/// Accept only credential-free HTTP(S) URLs, dropping any fragment.
pub fn validate_url(value: &str) -> Result<String> {
    let mut u = Url::parse(value.trim())?;
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() {
        bail!("Feed URL must use HTTP or HTTPS");
    }
    if !u.username().is_empty() || u.password().is_some() {
        bail!("Credentials in feed URLs are not supported");
    }
    u.set_fragment(None);
    Ok(u.to_string())
}

/// Resolve `href` against `base`, keeping it only if it is an HTTP(S) URL.
pub fn resolve_http(base: &str, href: &str) -> Option<String> {
    let url = match Url::parse(base) {
        Ok(base) => base.join(href.trim()).ok()?,
        Err(_) => Url::parse(href.trim()).ok()?,
    };
    matches!(url.scheme(), "http" | "https").then(|| url.to_string())
}

/// True for `localhost` and loopback, private, link-local, or unspecified IP literals.
pub fn is_local_host(url: &Url) -> bool {
    let ip = match url.host() {
        Some(Host::Domain(d)) => return d.eq_ignore_ascii_case("localhost"),
        Some(Host::Ipv4(ip)) => IpAddr::V4(ip),
        Some(Host::Ipv6(ip)) => IpAddr::V6(ip),
        None => return false,
    };
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            v4.is_loopback() || v4.is_private() || v4.is_link_local() || v4.is_unspecified()
        }
        IpAddr::V6(v6) => {
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_hosts_are_detected() {
        for local in [
            "http://localhost/",
            "http://127.0.0.1/",
            "http://10.1.2.3/",
            "http://192.168.1.1/",
            "http://169.254.169.254/",
            "http://[::1]/",
            "http://[fd00::1]/",
        ] {
            assert!(is_local_host(&Url::parse(local).unwrap()), "{local}");
        }
        assert!(!is_local_host(&Url::parse("https://example.org/").unwrap()));
        assert!(!is_local_host(&Url::parse("http://8.8.8.8/").unwrap()));
    }
}
