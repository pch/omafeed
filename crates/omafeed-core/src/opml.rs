use crate::{
    model::{Library, TreeItem},
    util::escape,
};
use anyhow::{Result, bail};
use serde::Deserialize;

const MAX_SIZE: usize = 5 * 1024 * 1024;

#[derive(Debug, Deserialize)]
pub struct Document {
    pub body: Body,
}

#[derive(Debug, Deserialize)]
pub struct Body {
    #[serde(rename = "outline", default)]
    pub outlines: Vec<Outline>,
}

#[derive(Debug, Deserialize)]
pub struct Outline {
    #[serde(rename = "@text", default)]
    pub text: String,
    #[serde(rename = "@title", default)]
    pub title: String,
    #[serde(rename = "@xmlUrl")]
    pub url: Option<String>,
    #[serde(rename = "@htmlUrl")]
    pub site_url: Option<String>,
    #[serde(rename = "outline", default)]
    pub children: Vec<Outline>,
}

impl Outline {
    pub fn name(&self) -> &str {
        if self.title.is_empty() {
            &self.text
        } else {
            &self.title
        }
    }
}

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
        "<opml version=\"2.0\"><head><title>Omafeed subscriptions</title></head><body>\n",
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
