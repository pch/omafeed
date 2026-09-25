use crate::{db::Article, settings::Palette, util::escape};
use base64::{Engine, engine::general_purpose::STANDARD as BASE64};
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
h1 { font: 600 32px/1.2 system-ui; margin: 14px 0 12px }
h2, h3, h4 { line-height: 1.35 }
a { color: {accent} }
.meta { font: 13px/1.6 system-ui; opacity: .7 }
.source { display: inline-flex; align-items: center; gap: 8px; font: 600 14px/1.4 system-ui; color: inherit; text-decoration: none; opacity: .8 }
a.source:hover { opacity: 1 }
.source img { width: 20px; height: 20px; border-radius: 4px }
img, video { max-width: 100%; height: auto }
pre { overflow: auto; padding: 18px; background: color-mix(in srgb, currentColor 7%, transparent); border-radius: 8px; font-size: .85em }
code { font-family: monospace }
blockquote { border-left: 3px solid {accent}; margin-left: 0; padding-left: 20px; opacity: .85 }
table { display: block; overflow: auto; border-collapse: collapse }
td, th { padding: 6px; border: 1px solid #8885 }
hr { border: 0; border-top: 1px solid #8885; margin: 28px 0 }
figure { margin: 20px 0 }
";

/// Where an article comes from, shown above its title.
pub struct Source<'a> {
    /// Website to open when the feed name is clicked; empty for none.
    pub url: &'a str,
    /// Cached favicon PNG, embedded as a `data:` URI.
    pub icon: Option<&'a [u8]>,
}

fn source_html(name: &str, source: &Source) -> String {
    let icon = source
        .icon
        .map(|png| {
            format!(
                "<img src=\"data:image/png;base64,{}\" alt=\"\">",
                BASE64.encode(png)
            )
        })
        .unwrap_or_default();
    let name = escape(name);
    match crate::util::validate_url(source.url) {
        Ok(url) => format!(
            "<a class=\"source\" href=\"{}\">{icon}{name}</a>",
            escape(&url)
        ),
        Err(_) => format!("<span class=\"source\">{icon}{name}</span>"),
    }
}

/// Build the reader document. Article HTML must already be sanitized; palette
/// colors are validated hex values, so they are safe to interpolate into CSS.
pub fn document(a: &Article, source: &Source, p: &Palette, font: u32, images: bool) -> String {
    // `data:` only admits the embedded favicon; sanitized article HTML cannot use it.
    let img_policy = if images {
        "data: https: http:"
    } else {
        "data:"
    };
    let csp = format!(
        "default-src 'none'; style-src 'unsafe-inline'; img-src {img_policy}; base-uri 'none'; form-action 'none'"
    );
    let style = STYLE
        .replace("{scheme}", if p.dark { "dark" } else { "light" })
        .replace("{bg}", &p.background)
        .replace("{fg}", &p.foreground)
        .replace("{accent}", &p.accent)
        .replace("{font}", &font.to_string());
    let mut byline = Vec::new();
    if !a.author.is_empty() {
        byline.push(escape(&a.author));
    }
    if let Some(date) = chrono::DateTime::from_timestamp(a.published, 0) {
        byline.push(date.format("%B %-d, %Y").to_string());
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
            "<header>{source}<h1>{title}</h1><div class=\"meta\">{byline}</div></header>",
            "<hr>{body}</main></body></html>"
        ),
        csp = csp,
        style = style,
        source = source_html(&a.feed_title, source),
        title = escape(&a.title),
        byline = byline.join(" · "),
        body = a.html,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_shows_linked_source_with_embedded_icon() {
        let a = Article {
            feed_title: "Tom & Jerry".into(),
            title: "Hello".into(),
            ..Default::default()
        };
        let p = Palette::load(crate::settings::Theme::Dark);
        let source = Source {
            url: "https://example.org/",
            icon: Some(b"png"),
        };
        let html = document(&a, &source, &p, 18, false);
        assert!(html.contains(r#"<a class="source" href="https://example.org/"><img src="data:image/png;base64,cG5n" alt="">Tom &amp; Jerry</a>"#));
        assert!(html.contains("img-src data:;"));
        let unsafe_url = Source {
            url: "javascript:alert(1)",
            icon: None,
        };
        let html = document(&a, &unsafe_url, &p, 18, false);
        assert!(html.contains(r#"<span class="source">Tom &amp; Jerry</span>"#));
    }

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
