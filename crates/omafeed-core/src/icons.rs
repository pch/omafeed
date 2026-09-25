//! Best-effort website icon discovery. Icons live in a bounded, disposable cache.
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

pub fn path(cache: &Path, site_url: &str) -> PathBuf {
    let origin = url::Url::parse(site_url)
        .map(|u| u.origin().ascii_serialization())
        .unwrap_or_default();
    cache
        .join("icons")
        .join(format!("{:x}.ico", Sha256::digest(origin.as_bytes())))
}
fn recent(path: &Path, seconds: u64) -> bool {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age.as_secs() < seconds)
}
async fn get(client: &reqwest::Client, url: &str, limit: usize) -> Option<(String, Vec<u8>)> {
    let response = client
        .get(url)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await
        .ok()?;
    if !response.status().is_success() {
        return None;
    }
    let final_url = response.url().to_string();
    let mut chunks = response.bytes_stream();
    let mut bytes = Vec::new();
    while let Some(chunk) = chunks.next().await {
        let chunk = chunk.ok()?;
        if bytes.len() + chunk.len() > limit {
            return None;
        }
        bytes.extend_from_slice(&chunk);
    }
    Some((final_url, bytes))
}
pub fn discover(html: &str, base: &str) -> Vec<String> {
    let Ok(base) = url::Url::parse(base) else {
        return vec![];
    };
    let doc = scraper::Html::parse_document(html);
    let selector = scraper::Selector::parse("link[href][rel]").expect("constant selector");
    let mut icons = Vec::new();
    for element in doc.select(&selector) {
        let e = element.value();
        let rel = e.attr("rel").unwrap_or_default();
        if !rel
            .split_ascii_whitespace()
            .any(|r| r.eq_ignore_ascii_case("icon") || r.eq_ignore_ascii_case("apple-touch-icon"))
        {
            continue;
        }
        if e.attr("type") == Some("image/svg+xml") {
            continue;
        }
        if let Ok(url) = base.join(e.attr("href").unwrap_or_default())
            && matches!(url.scheme(), "http" | "https")
            && !url.path().ends_with(".svg")
        {
            let url = url.to_string();
            if !icons.contains(&url) {
                icons.push(url);
            }
        }
    }
    icons.truncate(3);
    icons
}
fn raster(bytes: &[u8]) -> bool {
    bytes.starts_with(b"\x00\x00\x01\x00")
        || bytes.starts_with(b"\x89PNG\r\n\x1a\n")
        || bytes.starts_with(b"\xff\xd8\xff")
        || bytes.starts_with(b"GIF8")
        || bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP")
}
pub async fn cache(client: &reqwest::Client, root: &Path, site_url: &str) {
    let target = path(root, site_url);
    let failed = target.with_extension("failed");
    if recent(&target, 7 * 86400) || recent(&failed, 86400) {
        return;
    }
    let Ok(mut site) = url::Url::parse(site_url) else {
        return;
    };
    if !matches!(site.scheme(), "http" | "https") {
        return;
    }
    // Feed metadata normally supplies a homepage. A feed URL fallback uses its origin.
    if site.path().ends_with(".xml")
        || site.path().ends_with(".atom")
        || site.path().contains("/feed")
    {
        site.set_path("/");
        site.set_query(None);
    }
    let mut candidates = if let Some((base, html)) = get(client, site.as_str(), 512 * 1024).await {
        discover(&String::from_utf8_lossy(&html), &base)
    } else {
        vec![]
    };
    if let Ok(fallback) = site.join("/favicon.ico") {
        let fallback = fallback.to_string();
        if !candidates.contains(&fallback) {
            candidates.push(fallback);
        }
    }
    if let Some(dir) = target.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    for url in candidates {
        if let Some((_, bytes)) = get(client, &url, 256 * 1024).await
            && raster(&bytes)
        {
            let temp = target.with_extension("tmp");
            if std::fs::write(&temp, bytes).is_ok() && std::fs::rename(temp, &target).is_ok() {
                let _ = std::fs::remove_file(failed);
                return;
            }
        }
    }
    // Cache misses too, so sites without icons are not probed every refresh.
    let _ = std::fs::write(failed, []);
}
/// Keep the most recently written 32 MiB of icons. Metadata files are tiny.
pub fn prune(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root.join("icons")) else {
        return;
    };
    let mut files = entries
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension().is_none_or(|e| e != "ico") {
                return None;
            }
            let m = e.metadata().ok()?;
            Some((p, m.len(), m.modified().ok()?))
        })
        .collect::<Vec<_>>();
    files.sort_by_key(|(_, _, time)| std::cmp::Reverse(*time));
    let mut total = 0;
    for (path, size, _) in files {
        total += size;
        if total > 32 * 1024 * 1024 {
            let _ = std::fs::remove_file(path);
        }
    }
}
