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
    "caption",
    "center",
    "dd",
    "details",
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
    "hgroup",
    "hr",
    "li",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "summary",
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
    let is_block = |node: &scraper::Node| {
        node.as_element()
            .is_some_and(|e| BLOCKS.contains(&e.name()))
    };
    // A block separates words where it ends as well as where it starts: `<summary>A</summary>B`.
    // Closes are found as in `readable`, without recursion.
    let mut open = Vec::new();
    for node in doc.root_element().descendants() {
        while open.last().is_some_and(|top| node.parent() != Some(*top)) {
            if open.pop().is_some_and(|closed| is_block(closed.value())) {
                text.push(' ');
            }
        }
        match node.value() {
            scraper::Node::Text(t) => text.push_str(t),
            scraper::Node::Element(_) => {
                if is_block(node.value()) {
                    text.push(' ');
                }
                open.push(node);
            }
            _ => {}
        }
    }
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Text for reading, not searching: paragraphs stay separate, and headings, lists, quotes and
/// code keep a Markdown-like shape.
///
/// ```text
/// ## Heading          paragraphs are separated by a blank line
/// - bullet            "1." numbers ordered lists; nested lists are indented
/// > quote             fenced ``` blocks keep code exactly as written
/// ```
///
/// Links keep their text but not their address, and images appear as `[image: alt]`.
pub fn readable(html: &str) -> String {
    let safe = ammonia::clean(html);
    let doc = scraper::Html::parse_fragment(&safe);
    let mut out = Readable::default();
    // Nodes arrive in document order; a node whose parent is not the innermost open element
    // means the elements above it have closed. Walking with a stack instead of recursion keeps
    // a hostile, deeply nested feed from overflowing the stack.
    let mut open = Vec::new();
    for node in doc.root_element().descendants() {
        while let Some(top) = open.last() {
            if node.parent().is_some_and(|p| p == *top) {
                break;
            }
            if let Some(closed) = open.pop()
                && let scraper::Node::Element(e) = closed.value()
            {
                out.close(e.name());
            }
        }
        match node.value() {
            scraper::Node::Element(e) => {
                out.open(e);
                open.push(node);
            }
            scraper::Node::Text(t) => out.text(t),
            _ => {}
        }
    }
    while let Some(node) = open.pop() {
        if let scraper::Node::Element(e) = node.value() {
            out.close(e.name());
        }
    }
    out.finish()
}

/// A list item being read: its later blocks line up under its first.
struct Item {
    /// Spaces as wide as the item's marker, indentation included.
    pad: String,
    /// Quote depth outside the item; quotes inside it go after the marker.
    quote: usize,
    /// The top-level list it belongs to, so its blocks stay tight.
    list: usize,
}

#[derive(Default)]
struct Readable {
    /// Finished blocks, and the list each item belongs to. Items of one list sit on
    /// consecutive lines; everything else is separated by a blank line.
    blocks: Vec<(String, Option<usize>)>,
    /// Counts top-level lists, so two lists in a row stay separate.
    list_count: usize,
    /// Text of the block being built; `\n` marks a `<br>`.
    inline: String,
    quote: usize,
    /// Open lists: `None` for bullets, `Some(n)` for numbered lists that have reached `n`.
    lists: Vec<Option<i64>>,
    /// Open list items, innermost last.
    items: Vec<Item>,
    /// Prefix for the next block, and whether it starts a list item.
    marker: Option<(String, bool)>,
    /// Raw text of the `<pre>` being read.
    pre: Option<String>,
}

impl Readable {
    fn open(&mut self, e: &scraper::node::Element) {
        if let Some(code) = &mut self.pre {
            // Some highlighters end code lines with `<br>` instead of a newline.
            if e.name() == "br" {
                code.push('\n');
            }
            return;
        }
        match e.name() {
            "pre" => {
                self.flush();
                self.pre = Some(String::new());
            }
            "br" => self.inline.push('\n'),
            "hr" => {
                self.flush();
                self.blocks.push(("---".into(), None));
            }
            "img" => {
                let alt = e.attr("alt").map(str_line).unwrap_or_default();
                if !alt.is_empty() {
                    self.inline.push_str(&format!(" [image: {alt}] "));
                }
            }
            "ul" | "ol" => {
                self.flush();
                if self.lists.is_empty() {
                    self.list_count += 1;
                }
                // `<ol start="4">` continues numbering from an earlier list.
                let start = e.attr("start").and_then(|s| s.trim().parse::<i64>().ok());
                self.lists
                    .push((e.name() == "ol").then(|| start.unwrap_or(1).saturating_sub(1)));
            }
            "li" => {
                self.flush();
                let indent = "  ".repeat(self.lists.len().saturating_sub(1));
                let prefix = match self.lists.last_mut() {
                    Some(Some(n)) => {
                        *n = n.saturating_add(1);
                        format!("{indent}{n}. ")
                    }
                    _ => format!("{indent}- "),
                };
                self.items.push(Item {
                    pad: " ".repeat(prefix.chars().count()),
                    quote: self.quote,
                    list: self.list_count,
                });
                self.marker = Some((prefix, true));
            }
            "blockquote" => {
                self.flush();
                self.quote += 1;
            }
            "td" | "th" => {
                if !self.inline.trim().is_empty() {
                    self.inline.push_str(" | ");
                }
            }
            name @ ("h1" | "h2" | "h3" | "h4" | "h5" | "h6") => {
                self.flush();
                let level = usize::from(name.as_bytes()[1] - b'0');
                let heading = format!("{} ", "#".repeat(level));
                // A heading that starts a list item keeps the item's marker: `- ## Title`.
                self.marker = Some(match self.marker.take() {
                    Some((item, true)) => (item + &heading, true),
                    _ => (heading, false),
                });
            }
            name if BLOCKS.contains(&name) => self.flush(),
            _ => {}
        }
    }

    fn close(&mut self, name: &str) {
        if self.pre.is_some() {
            if name == "pre" {
                self.end_pre();
            }
            return;
        }
        match name {
            "br" | "hr" | "img" | "td" | "th" => {}
            "ul" | "ol" => {
                self.flush();
                self.lists.pop();
            }
            "blockquote" => {
                self.flush();
                self.quote = self.quote.saturating_sub(1);
            }
            "li" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.flush();
                // An empty item or heading must not lend its marker to the next block.
                self.marker = None;
                if name == "li" {
                    self.items.pop();
                }
            }
            name if BLOCKS.contains(&name) => self.flush(),
            _ => {}
        }
    }

    fn text(&mut self, text: &str) {
        match &mut self.pre {
            Some(code) => code.push_str(text),
            // Line breaks inside HTML text are ordinary spaces; only `<br>` breaks a line.
            None => self.inline.push_str(&text.replace(['\n', '\r', '\t'], " ")),
        }
    }

    /// Add a finished block, with the pending heading or list marker when `marked`. Inside a
    /// list item the block lines up under the item's text, and quotes opened inside the item
    /// go after its marker: `- > quoted`.
    fn place(&mut self, lines: &[String], marked: bool) {
        let (prefix, starts_item) = if marked {
            self.marker.take().unwrap_or_default()
        } else {
            Default::default()
        };
        let (outer, pad, inner, list) = match self.items.last() {
            Some(item) => {
                let first = match (starts_item, prefix.is_empty()) {
                    (true, _) => prefix,
                    (false, true) => item.pad.clone(),
                    (false, false) => format!("{}{prefix}", item.pad),
                };
                let pad = " ".repeat(first.chars().count());
                let inner = self.quote.saturating_sub(item.quote);
                ((item.quote, first), pad, inner, Some(item.list))
            }
            None => {
                let pad = " ".repeat(prefix.chars().count());
                (
                    (self.quote, prefix),
                    pad,
                    0,
                    starts_item.then_some(self.list_count),
                )
            }
        };
        let (outer, first) = outer;
        let (outer, inner) = ("> ".repeat(outer), "> ".repeat(inner));
        let text = lines
            .iter()
            .enumerate()
            .map(|(i, l)| {
                let lead = if i == 0 { &first } else { &pad };
                let line = format!("{outer}{lead}{inner}{l}");
                // A blank code line gets no dangling indentation.
                if l.is_empty() {
                    line.trim_end().to_owned()
                } else {
                    line
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        self.blocks.push((text, list));
    }

    /// Turn the text collected so far into a block, with its heading or list marker.
    fn flush(&mut self) {
        let raw = std::mem::take(&mut self.inline);
        let lines: Vec<String> = raw
            .split('\n')
            .map(str_line)
            .filter(|l| !l.is_empty())
            .collect();
        // With no text yet, keep the marker: `<li><p>text</p></li>` opens the paragraph
        // before the item has any text. Closing the item or heading discards it.
        if lines.is_empty() {
            return;
        }
        self.place(&lines, true);
    }

    fn end_pre(&mut self) {
        let code = self.pre.take().unwrap_or_default();
        let code = code.trim_matches(['\n', '\r']).trim_end();
        if code.trim().is_empty() {
            return;
        }
        // A fence longer than any run of backticks in the code cannot be closed by the code.
        let longest = code
            .split(|c| c != '`')
            .map(str::len)
            .max()
            .unwrap_or_default();
        let fence = "`".repeat(longest.max(2) + 1);
        let lines: Vec<String> = [fence.as_str(), code, fence.as_str()]
            .iter()
            .flat_map(|part| part.lines())
            .map(str::to_owned)
            .collect();
        // Code in a list item belongs to the item; elsewhere it never takes a heading marker.
        self.place(&lines, !self.items.is_empty());
    }

    fn finish(mut self) -> String {
        if self.pre.is_some() {
            self.end_pre();
        }
        self.flush();
        let mut out = String::new();
        for (i, (text, item)) in self.blocks.iter().enumerate() {
            if i > 0 {
                let tight = item.is_some() && *item == self.blocks[i - 1].1;
                out.push_str(if tight { "\n" } else { "\n\n" });
            }
            out.push_str(text);
        }
        out
    }
}

/// One line with runs of whitespace collapsed to a single space.
fn str_line(text: &str) -> String {
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
    fn readable_keeps_paragraphs_apart_and_inline_elements_together() {
        assert_eq!(
            readable("<p>One</p><p>Two <b>bold</b>, <a href=\"x\">link</a>.</p>"),
            "One\n\nTwo bold, link."
        );
        assert_eq!(
            readable("Intro text<p>Para</p>tail"),
            "Intro text\n\nPara\n\ntail"
        );
        assert_eq!(
            readable("<p>line one<br>line two</p>"),
            "line one\nline two"
        );
        // Newlines inside HTML text are only spaces.
        assert_eq!(readable("<p>one\ntwo\n\n  three</p>"), "one two three");
    }

    #[test]
    fn readable_says_which_lines_are_headings() {
        assert_eq!(
            readable("<h1>Title</h1><p>Body</p><h3>Sub <em>part</em></h3><p>More</p>"),
            "# Title\n\nBody\n\n### Sub part\n\nMore"
        );
        // An empty heading must not lend its marker to the next paragraph.
        assert_eq!(readable("<p></p><h2></h2><p>x</p>"), "x");
    }

    #[test]
    fn readable_lists_are_tight_nested_and_kept_separate() {
        assert_eq!(
            readable(
                "<p>Intro</p><ul><li>a</li><li>b<ul><li>c</li></ul></li></ul>\
                 <ol><li>x</li><li>y</li></ol><ol><li>again</li></ol><p>End</p>"
            ),
            "Intro\n\n- a\n- b\n  - c\n\n1. x\n2. y\n\n1. again\n\nEnd"
        );
        // Newsletters often wrap item text in a paragraph.
        assert_eq!(
            readable("<ul>\n  <li>\n    <p>spaced</p>\n  </li>\n  <li><p>two</p></li>\n</ul>"),
            "- spaced\n- two"
        );
        // An empty item leaves no marker behind.
        assert_eq!(readable("<ul><li></li></ul><p>after</p>"), "after");
    }

    #[test]
    fn readable_marks_quotes_and_keeps_code_as_written() {
        assert_eq!(
            readable("<blockquote><p>Wise words</p><p>More</p></blockquote><p>After</p>"),
            "> Wise words\n\n> More\n\nAfter"
        );
        assert_eq!(
            readable("<pre><code>fn main() {\n    println!(\"hi\");\n}\n</code></pre>"),
            "```\nfn main() {\n    println!(\"hi\");\n}\n```"
        );
        // Code containing a fence gets a longer one.
        assert_eq!(readable("<pre>a ``` b</pre>"), "````\na ``` b\n````");
        assert_eq!(readable("<pre>   \n</pre><p>x</p>"), "x");
    }

    #[test]
    fn readable_names_images_and_rules_and_flattens_tables() {
        assert_eq!(
            readable(
                "<p>Look <img src=\"https://e.x/a.png\" alt=\"A chart\"> here</p><hr><p>Next</p>"
            ),
            "Look [image: A chart] here\n\n---\n\nNext"
        );
        assert_eq!(readable("<p><img src=\"https://e.x/a.png\"></p>"), "");
        assert_eq!(
            readable("<table><tr><th>A</th><th>B</th></tr><tr><td>1</td><td>2</td></tr></table>"),
            "A | B\n\n1 | 2"
        );
    }

    #[test]
    fn readable_keeps_line_breaks_in_code() {
        assert_eq!(
            readable("<pre>let a = 1;<br>let b = 2;</pre>"),
            "```\nlet a = 1;\nlet b = 2;\n```"
        );
    }

    #[test]
    fn readable_separates_details_and_other_blocks() {
        assert_eq!(
            readable("<details><summary>Changes</summary>Fixed bug</details><center>a</center>b"),
            "Changes\n\nFixed bug\n\na\n\nb"
        );
        assert_eq!(
            plain("<details><summary>Changes</summary>Fixed</details><nav>x</nav>y"),
            "Changes Fixed x y"
        );
    }

    #[test]
    fn readable_numbers_from_the_list_start() {
        assert_eq!(
            readable("<ol start=\"4\"><li>four</li><li>five</li></ol>"),
            "4. four\n5. five"
        );
        assert_eq!(readable("<ol start=\"x\"><li>one</li></ol>"), "1. one");
    }

    #[test]
    fn readable_keeps_everything_in_a_list_item_inside_it() {
        assert_eq!(
            readable("<ul><li><p>a</p><p>b</p></li><li>c</li></ul><p>after</p>"),
            "- a\n  b\n- c\n\nafter"
        );
        assert_eq!(
            readable("<ul><li><blockquote>q</blockquote></li></ul>"),
            "- > q"
        );
        assert_eq!(
            readable("<blockquote><ul><li>x</li></ul></blockquote>"),
            "> - x"
        );
        assert_eq!(
            readable("<ul><li><h3>H</h3>body</li></ul>"),
            "- ### H\n  body"
        );
        assert_eq!(
            readable("<ol><li>run<pre>make\n\nmake install</pre>then done</li></ol>"),
            "1. run\n   ```\n   make\n\n   make install\n   ```\n   then done"
        );
    }

    #[test]
    fn readable_is_safe_and_total() {
        assert_eq!(readable("<script>alert(1)</script><p>hi</p>"), "hi");
        assert_eq!(readable(""), "");
        assert_eq!(readable("  \n "), "");
        assert_eq!(readable("Tom &amp; Jerry &lt;3"), "Tom & Jerry <3");
        // Deep nesting must not overflow the stack or lose the text.
        let deep = format!("{}deep{}", "<div>".repeat(3000), "</div>".repeat(3000));
        assert_eq!(readable(&deep), "deep");
    }

    #[test]
    fn readable_loses_no_words_that_plain_finds() {
        let html = "<p>Listen on <a href=\"#\">Spotify</a>, <em>Apple</em>.</p><p>Next<br>line</p>";
        assert_eq!(plain(html), "Listen on Spotify, Apple. Next line");
        assert_eq!(
            readable(html).split_whitespace().collect::<Vec<_>>(),
            plain(html).split_whitespace().collect::<Vec<_>>()
        );
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
