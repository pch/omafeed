use crate::{db::Article, opml::escape, settings::Palette};
use std::collections::HashSet;
pub fn sanitize(html: &str, base: &str) -> String {
    let mut clean = ammonia::Builder::default();
    clean.url_schemes(HashSet::from(["http", "https", "mailto"]));
    if let Ok(base) = url::Url::parse(base) {
        clean.url_relative(ammonia::UrlRelative::RewriteWithBase(base));
    }
    clean.clean(html).to_string()
}
pub fn plain(html: &str) -> String {
    let safe = ammonia::clean(html);
    scraper::Html::parse_fragment(&safe)
        .root_element()
        .text()
        .collect::<Vec<_>>()
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}
pub fn document(a: &Article, p: &Palette, font: u32, images: bool) -> String {
    let img_policy = if images { "https: http:" } else { "'none'" };
    format!(
        r#"<!doctype html><html><head><meta charset="utf-8"><meta name="viewport" content="width=device-width"><meta http-equiv="Content-Security-Policy" content="default-src 'none'; style-src 'unsafe-inline'; img-src {img_policy}; base-uri 'none'; form-action 'none'"><style>
:root{{color-scheme:{scheme}}}body{{margin:0;background:{bg};color:{fg};font:{font}px/1.75 Georgia,serif;overflow-wrap:anywhere}}main{{max-width:720px;margin:auto;padding:40px 40px 100px}}h1{{font:600 32px/1.2 system-ui;margin:16px 0}}h2,h3,h4{{line-height:1.35}}a{{color:{accent}}}.meta{{font:13px/1.6 system-ui;opacity:.7}}img,video{{max-width:100%;height:auto}}pre{{overflow:auto;padding:18px;background:color-mix(in srgb,currentColor 7%,transparent);border-radius:8px;font-size:.85em}}code{{font-family:monospace}}blockquote{{border-left:3px solid {accent};margin-left:0;padding-left:20px;opacity:.85}}table{{display:block;overflow:auto;border-collapse:collapse}}td,th{{padding:6px;border:1px solid #8885}}hr{{border:0;border-top:1px solid #8885;margin:28px 0}}figure{{margin:20px 0}}</style></head><body><main><div class="meta">{feed} · {date}</div><h1>{title}</h1><div class="meta">{author} · <a href="{url}">Open original ↗</a></div><hr>{body}</main></body></html>"#,
        scheme = if p.dark { "dark" } else { "light" },
        bg = p.background,
        fg = p.foreground,
        accent = p.accent,
        feed = escape(&a.feed_title),
        date = chrono::DateTime::from_timestamp(a.published, 0)
            .map(|d| d.format("%B %-d, %Y").to_string())
            .unwrap_or_default(),
        title = escape(&a.title),
        author = escape(&a.author),
        url = escape(&a.url),
        body = a.html
    )
}
