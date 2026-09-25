use crate::{db::Article, settings::Palette, util::escape};
use std::collections::HashSet;

/// Sanitize feed HTML, resolving relative URLs against `base`.
pub fn sanitize(html: &str, base: &str) -> String {
    let mut clean = ammonia::Builder::default();
    clean.url_schemes(HashSet::from(["http", "https", "mailto"]));
    if let Ok(base) = url::Url::parse(base) {
        clean.url_relative(ammonia::UrlRelative::RewriteWithBase(base));
    }
    clean.clean(html).to_string()
}

/// Elements that separate words; inline elements (links, emphasis) do not.
const BLOCKS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "br",
    "dd",
    "div",
    "dl",
    "dt",
    "figcaption",
    "figure",
    "footer",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "li",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "td",
    "th",
    "tr",
    "ul",
];

/// Plain text with collapsed whitespace, for titles, previews, and search.
pub fn plain(html: &str) -> String {
    let safe = ammonia::clean(html);
    let doc = scraper::Html::parse_fragment(&safe);
    let mut text = String::new();
    for node in doc.root_element().descendants() {
        match node.value() {
            scraper::Node::Text(t) => text.push_str(t),
            scraper::Node::Element(e) if BLOCKS.contains(&e.name()) => text.push(' '),
            _ => {}
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

const STYLE: &str = "
:root { color-scheme: {scheme} }
body { margin: 0; background: {bg}; color: {fg}; font: {font}px/1.65 system-ui, sans-serif; overflow-wrap: anywhere }
main { max-width: 720px; margin: auto; padding: 40px 40px 100px }
h1 { font: 600 32px/1.2 system-ui; margin: 16px 0 }
h2, h3, h4 { line-height: 1.35 }
a { color: {accent} }
.meta { font: 13px/1.6 system-ui; opacity: .7 }
img, video { max-width: 100%; height: auto }
pre { overflow: auto; padding: 18px; background: color-mix(in srgb, currentColor 7%, transparent); border-radius: 8px; font-size: .85em }
code { font-family: monospace }
blockquote { border-left: 3px solid {accent}; margin-left: 0; padding-left: 20px; opacity: .85 }
table { display: block; overflow: auto; border-collapse: collapse }
td, th { padding: 6px; border: 1px solid #8885 }
hr { border: 0; border-top: 1px solid #8885; margin: 28px 0 }
figure { margin: 20px 0 }
";

/// Build the reader document. Article HTML must already be sanitized; palette
/// colors are validated hex values, so they are safe to interpolate into CSS.
pub fn document(a: &Article, p: &Palette, font: u32, images: bool) -> String {
    let img_policy = if images { "https: http:" } else { "'none'" };
    let csp = format!(
        "default-src 'none'; style-src 'unsafe-inline'; img-src {img_policy}; base-uri 'none'; form-action 'none'"
    );
    let style = STYLE
        .replace("{scheme}", if p.dark { "dark" } else { "light" })
        .replace("{bg}", &p.background)
        .replace("{fg}", &p.foreground)
        .replace("{accent}", &p.accent)
        .replace("{font}", &font.to_string());
    let date = chrono::DateTime::from_timestamp(a.published, 0)
        .map(|d| d.format("%B %-d, %Y").to_string())
        .unwrap_or_default();
    let mut byline = Vec::new();
    if !a.author.is_empty() {
        byline.push(escape(&a.author));
    }
    if !a.url.is_empty() {
        byline.push(format!(
            "<a href=\"{}\">Open original ↗</a>",
            escape(&a.url)
        ));
    }
    format!(
        concat!(
            "<!doctype html><html><head><meta charset=\"utf-8\">",
            "<meta name=\"viewport\" content=\"width=device-width\">",
            "<meta http-equiv=\"Content-Security-Policy\" content=\"{csp}\">",
            "<style>{style}</style></head><body><main>",
            "<div class=\"meta\">{feed} · {date}</div><h1>{title}</h1>",
            "<div class=\"meta\">{byline}</div><hr>{body}</main></body></html>"
        ),
        csp = csp,
        style = style,
        feed = escape(&a.feed_title),
        date = date,
        title = escape(&a.title),
        byline = byline.join(" · "),
        body = a.html,
    )
}

#[cfg(test)]
mod tests {
    use super::plain;

    #[test]
    fn plain_text_spaces_blocks_but_not_inline_elements() {
        assert_eq!(
            plain(
                "<p>Listen on <a href=\"#\">Spotify</a>, <em>Apple</em>.</p><p>Next<br>line</p><ul><li>a</li><li>b</li></ul>"
            ),
            "Listen on Spotify, Apple. Next line a b"
        );
    }
}
