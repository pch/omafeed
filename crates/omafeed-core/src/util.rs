//! Small helpers shared by OPML, storage, fetching, and rendering.
use anyhow::Result;
use sha2::{Digest, Sha256};
use std::net::IpAddr;
use url::{Host, Url};

/// SHA-256 as lowercase, zero-padded hex for persistent IDs and cache keys.
pub(crate) fn sha256_hex(data: &[u8]) -> String {
    Sha256::digest(data)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

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
    let invalid = |message: String| crate::Error::Validation(message);
    let mut u = Url::parse(value.trim()).map_err(|e| invalid(e.to_string()))?;
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() {
        return Err(invalid("Feed URL must use HTTP or HTTPS".into()).into());
    }
    if !u.username().is_empty() || u.password().is_some() {
        return Err(invalid("Credentials in feed URLs are not supported".into()).into());
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

/// True for `localhost` (and names below it) and loopback, private, link-local, or unspecified
/// IP literals. Other names are not resolved, so one that resolves to a local address passes.
pub fn is_local_host(url: &Url) -> bool {
    let ip = match url.host() {
        Some(Host::Domain(d)) => {
            let d = d.strip_suffix('.').unwrap_or(d).to_ascii_lowercase();
            return d == "localhost" || d.ends_with(".localhost");
        }
        Some(Host::Ipv4(ip)) => IpAddr::V4(ip),
        Some(Host::Ipv6(ip)) => IpAddr::V6(ip),
        None => return false,
    };
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            // 100.64.0.0/10 is carrier-grade NAT, often a VPN's own network.
            let [a, b, ..] = v4.octets();
            let shared = a == 100 && (b & 0xc0) == 64;
            v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_unspecified()
                || shared
        }
        IpAddr::V6(v6) => {
            // fec0::/10 is the deprecated site-local range, still routed on some networks.
            let site_local = (v6.segments()[0] & 0xffc0) == 0xfec0;
            v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_unique_local()
                || v6.is_unicast_link_local()
                || site_local
        }
    }
}

/// Whether a request to `target` may be made because of content or a redirect from `source`.
/// Addresses the user typed may be local, but a site on the internet must not point Omafeed at
/// the local network: only content that itself came from a local host may.
pub(crate) fn may_request(source: &Url, target: &Url) -> bool {
    !is_local_host(target) || is_local_host(source)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_local_content_may_lead_to_local_hosts() {
        let url = |u: &str| Url::parse(u).unwrap();
        let remote = url("https://example.org/feed");
        let local = url("http://127.0.0.1:8080/feed");
        assert!(may_request(&remote, &url("https://other.example.net/")));
        assert!(!may_request(&remote, &url("http://127.0.0.1/")));
        assert!(!may_request(&remote, &url("http://localhost:631/")));
        assert!(!may_request(&remote, &url("http://192.168.1.1/admin")));
        assert!(!may_request(&remote, &url("http://[::ffff:10.0.0.1]/")));
        assert!(may_request(&local, &url("http://127.0.0.1:9000/icon.png")));
        assert!(may_request(&local, &url("http://localhost/")));
        assert!(may_request(&local, &remote));
    }

    #[test]
    fn local_hosts_are_detected() {
        for local in [
            "http://localhost/",
            "http://LOCALHOST./",
            "http://printer.localhost/",
            "http://0.0.0.0/",
            "http://127.0.0.1/",
            "http://10.1.2.3/",
            "http://192.168.1.1/",
            "http://169.254.169.254/",
            "http://[::1]/",
            "http://[fd00::1]/",
            "http://[fe80::1]/",
            "http://[fec0::1]/",
            "http://[feff::1]/",
            "http://100.64.0.1/",
            "http://100.127.255.254/",
            "http://[::ffff:192.168.0.1]/",
            "http://2130706433/",
            "http://0x7f.1/",
        ] {
            assert!(is_local_host(&Url::parse(local).unwrap()), "{local}");
        }
        assert!(!is_local_host(&Url::parse("https://example.org/").unwrap()));
        assert!(!is_local_host(&Url::parse("http://8.8.8.8/").unwrap()));
        assert!(!is_local_host(
            &Url::parse("http://localhost.example.org/").unwrap()
        ));
        for remote in [
            "http://100.63.255.255/",
            "http://100.128.0.1/",
            "http://[ff00::1]/",
            "http://[2001:db8::1]/",
        ] {
            assert!(!is_local_host(&Url::parse(remote).unwrap()), "{remote}");
        }
    }
}
