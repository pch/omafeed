//! Discover and decode website icons into small, validated PNGs for GTK.
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use std::{
    io::Cursor,
    path::{Path, PathBuf},
};

const ICON_LIMIT: usize = 1024 * 1024;
const HEAD_LIMIT: usize = 512 * 1024;
pub fn path(cache: &Path, site_url: &str) -> PathBuf {
    let origin = url::Url::parse(site_url)
        .map(|u| u.origin().ascii_serialization())
        .unwrap_or_default();
    cache
        .join("icons-v2")
        .join(format!("{:x}.png", Sha256::digest(origin.as_bytes())))
}
fn recent(path: &Path, seconds: u64) -> bool {
    std::fs::metadata(path)
        .ok()
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.elapsed().ok())
        .is_some_and(|age| age.as_secs() < seconds)
}
async fn get(
    client: &reqwest::Client,
    url: &str,
    limit: usize,
    prefix: bool,
) -> Option<(String, Vec<u8>)> {
    let response = client
        .get(url)
        .timeout(std::time::Duration::from_secs(10))
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
            if !prefix {
                return None;
            }
            bytes.extend_from_slice(&chunk[..limit - bytes.len()]);
            break;
        }
        bytes.extend_from_slice(&chunk);
        // A large homepage body must not invalidate usable declarations in its head.
        if prefix && bytes.windows(7).any(|w| w.eq_ignore_ascii_case(b"</head>")) {
            break;
        }
    }
    Some((final_url, bytes))
}
pub fn discover(html: &str, base: &str) -> Vec<String> {
    let Ok(mut base) = url::Url::parse(base) else {
        return vec![];
    };
    let doc = scraper::Html::parse_document(html);
    let base_selector = scraper::Selector::parse("base[href]").expect("constant selector");
    if let Some(element) = doc.select(&base_selector).next()
        && let Ok(resolved) = base.join(element.value().attr("href").unwrap_or_default())
        && matches!(resolved.scheme(), "http" | "https")
    {
        base = resolved;
    }
    let selector = scraper::Selector::parse("link[href][rel]").expect("constant selector");
    let mut icons = Vec::new();
    for element in doc.select(&selector) {
        let e = element.value();
        if !e
            .attr("rel")
            .unwrap_or_default()
            .split_ascii_whitespace()
            .any(|r| {
                r.eq_ignore_ascii_case("icon")
                    || r.eq_ignore_ascii_case("apple-touch-icon")
                    || r.eq_ignore_ascii_case("apple-touch-icon-precomposed")
            })
        {
            continue;
        }
        if let Ok(url) = base.join(e.attr("href").unwrap_or_default())
            && matches!(url.scheme(), "http" | "https")
            && !icons.contains(&url.to_string())
        {
            icons.push(url.to_string());
        }
    }
    icons.truncate(8);
    icons
}
/// Decode rather than trusting a file signature. SVG never resolves file/network resources.
pub fn normalize(bytes: &[u8]) -> Option<Vec<u8>> {
    if bytes.is_empty() || bytes.len() > ICON_LIMIT {
        return None;
    }
    let mut reader = image::ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .ok()?;
    let png = if reader.format().is_some() {
        let mut limits = image::Limits::default();
        limits.max_image_width = Some(2048);
        limits.max_image_height = Some(2048);
        limits.max_alloc = Some(32 * 1024 * 1024);
        reader.limits(limits);
        let decoded = reader.decode().ok()?.thumbnail(64, 64);
        let mut output = Cursor::new(Vec::new());
        decoded
            .write_to(&mut output, image::ImageFormat::Png)
            .ok()?;
        output.into_inner()
    } else {
        let options = resvg::usvg::Options {
            image_href_resolver: resvg::usvg::ImageHrefResolver {
                resolve_data: Box::new(|_, _, _| None),
                resolve_string: Box::new(|_, _| None),
            },
            ..Default::default()
        };
        let tree = resvg::usvg::Tree::from_data(bytes, &options).ok()?;
        let size = tree.size();
        let scale = (64.0 / size.width()).min(64.0 / size.height());
        let mut pixels = resvg::tiny_skia::Pixmap::new(64, 64)?;
        let transform = resvg::tiny_skia::Transform::from_scale(scale, scale).post_translate(
            (64.0 - size.width() * scale) / 2.0,
            (64.0 - size.height() * scale) / 2.0,
        );
        resvg::render(&tree, transform, &mut pixels.as_mut());
        // Do not cache an empty SVG (e.g. unsupported text-only content).
        if !pixels.data().as_chunks::<4>().0.iter().any(|p| p[3] > 0) {
            return None;
        }
        pixels.encode_png().ok()?
    };
    Some(png)
}
fn save(target: &Path, bytes: &[u8]) -> bool {
    let Some(parent) = target.parent() else {
        return false;
    };
    if std::fs::create_dir_all(parent).is_err() {
        return false;
    }
    let tmp = target.with_extension("tmp");
    std::fs::write(&tmp, bytes).is_ok() && std::fs::rename(tmp, target).is_ok()
}
pub async fn cache(client: &reqwest::Client, root: &Path, site_url: &str, force: bool) {
    let target = path(root, site_url);
    let failed = target.with_extension("failed");
    if recent(&target, 7 * 86400)
        && std::fs::read(&target)
            .ok()
            .and_then(|b| normalize(&b))
            .is_some()
    {
        return;
    }
    // Upgrade the previous raw-format cache, including Windows ICOs, without waiting a week.
    if !target.exists() {
        let legacy = root
            .join("icons")
            .join(target.file_name().unwrap())
            .with_extension("ico");
        if recent(&legacy, 7 * 86400)
            && let Ok(bytes) = std::fs::read(legacy)
            && let Some(png) = normalize(&bytes)
            && save(&target, &png)
        {
            return;
        }
    }
    if !force && recent(&failed, 86400) {
        return;
    }
    let Ok(mut site) = url::Url::parse(site_url) else {
        return;
    };
    if !matches!(site.scheme(), "http" | "https") {
        return;
    }
    if site.path().ends_with(".xml")
        || site.path().ends_with(".atom")
        || site.path().contains("/feed")
    {
        site.set_path("/");
        site.set_query(None);
    }
    let mut candidates = vec![];
    if let Some((base, html)) = get(client, site.as_str(), HEAD_LIMIT, true).await {
        candidates = discover(&String::from_utf8_lossy(&html), &base);
        if let Ok(final_site) = url::Url::parse(&base) {
            site = final_site;
        }
    }
    // Respect redirects and subdirectory sites, then try common root icon locations.
    for fallback in [
        "favicon.ico",
        "/favicon.ico",
        "/favicon.svg",
        "/favicon.png",
        "/apple-touch-icon.png",
    ] {
        if let Ok(url) = site.join(fallback)
            && !candidates.contains(&url.to_string())
        {
            candidates.push(url.to_string());
        }
    }
    for url in candidates {
        if let Some((_, bytes)) = get(client, &url, ICON_LIMIT, false).await {
            let png = tokio::task::spawn_blocking(move || normalize(&bytes))
                .await
                .ok()
                .flatten();
            if let Some(png) = png
                && save(&target, &png)
            {
                let _ = std::fs::remove_file(failed);
                return;
            }
        }
    }
    if let Some(dir) = target.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let _ = std::fs::write(failed, []);
}
/// Keep the most recently written 32 MiB of normalized icons.
pub fn prune(root: &Path) {
    let Ok(entries) = std::fs::read_dir(root.join("icons-v2")) else {
        return;
    };
    let mut files = entries
        .flatten()
        .filter_map(|e| {
            let p = e.path();
            if p.extension().is_none_or(|e| e != "png") {
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
