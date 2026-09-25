use crate::{db::Library, util::escape};
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
    fn outlines(lib: &Library, parent: Option<i64>, out: &mut String) {
        for f in lib.folders.iter().filter(|f| f.parent == parent) {
            out.push_str(&format!("<outline text=\"{}\">\n", escape(&f.name)));
            outlines(lib, Some(f.id), out);
            out.push_str("</outline>\n");
        }
        for f in lib.feeds.iter().filter(|f| f.folder == parent) {
            out.push_str(&format!(
                "<outline type=\"rss\" text=\"{}\" xmlUrl=\"{}\" htmlUrl=\"{}\"/>\n",
                escape(&f.title),
                escape(&f.url),
                escape(&f.site_url)
            ));
        }
    }
    let mut out = String::from(concat!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n",
        "<opml version=\"2.0\"><head><title>Omafeed subscriptions</title></head><body>\n",
    ));
    outlines(lib, None, &mut out);
    out.push_str("</body></opml>\n");
    out
}
