//! Text from article HTML: one line for previews and search, or paragraphs for reading.
use super::one_line;

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
    one_line(&text)
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
                let alt = e.attr("alt").map(one_line).unwrap_or_default();
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
            .map(one_line)
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

#[cfg(test)]
mod tests {
    use super::*;

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
