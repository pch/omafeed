//! OPML: reading subscription lists from other readers, and writing the library as one.
use crate::{
    model::{Library, TreeItem},
    util::escape,
};
use anyhow::{Result, bail};
use serde::Deserialize;

const MAX_SIZE: usize = 5 * 1024 * 1024;

/// An OPML document; only its body matters.
#[derive(Debug, Deserialize)]
pub struct Document {
    /// The `<body>`.
    pub body: Body,
}

/// The `<body>` of an OPML document.
#[derive(Debug, Deserialize)]
pub struct Body {
    /// Its top-level `<outline>`s.
    #[serde(rename = "outline", default)]
    pub outlines: Vec<Outline>,
}

/// An `<outline>`: a feed if it has an `xmlUrl`, else a folder of the outlines inside it.
#[derive(Debug, Deserialize)]
pub struct Outline {
    /// The `text` attribute.
    #[serde(rename = "@text", default)]
    pub text: String,
    /// The `title` attribute, which some readers write instead of `text`.
    #[serde(rename = "@title", default)]
    pub title: String,
    /// The feed's address (`xmlUrl`).
    #[serde(rename = "@xmlUrl")]
    pub url: Option<String>,
    /// The feed's website (`htmlUrl`).
    #[serde(rename = "@htmlUrl")]
    pub site_url: Option<String>,
    /// The outlines inside a folder.
    #[serde(rename = "outline", default)]
    pub children: Vec<Outline>,
}

impl Outline {
    /// Its name: the title, or the text if the title is empty.
    pub fn name(&self) -> &str {
        if self.title.is_empty() {
            &self.text
        } else {
            &self.title
        }
    }
}

/// Read an OPML document of at most 5 MB.
pub fn parse(text: &str) -> Result<Document> {
    if text.len() > MAX_SIZE {
        bail!("OPML exceeds 5 MB");
    }
    Ok(quick_xml::de::from_str(text)?)
}

/// Serialize the library as OPML 2.0, preserving the folder hierarchy.
pub fn export(lib: &Library) -> String {
    let mut out = String::from(concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        "<opml version=\"2.0\"><head><title>omafeed subscriptions</title></head><body>\n",
    ));
    for item in lib.tree() {
        match item {
            TreeItem::Folder { folder, .. } => {
                out.push_str(&format!("<outline text=\"{}\">\n", escape(&folder.name)));
            }
            TreeItem::FolderEnd(_) => out.push_str("</outline>\n"),
            TreeItem::Feed { feed, .. } => out.push_str(&format!(
                "<outline type=\"rss\" text=\"{}\" xmlUrl=\"{}\" htmlUrl=\"{}\"/>\n",
                escape(&feed.title),
                escape(&feed.url),
                escape(&feed.site_url)
            )),
        }
    }
    out.push_str("</body></opml>\n");
    out
}
