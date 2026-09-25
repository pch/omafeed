use anyhow::{Result, bail};
use serde::Deserialize;

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
    if text.len() > 5 * 1024 * 1024 {
        bail!("OPML exceeds 5 MB");
    }
    Ok(quick_xml::de::from_str(text)?)
}
pub fn validate_url(value: &str) -> Result<String> {
    let mut u = url::Url::parse(value.trim())?;
    if !matches!(u.scheme(), "http" | "https") || u.host_str().is_none() {
        bail!("Feed URL must use HTTP or HTTPS");
    }
    if !u.username().is_empty() || u.password().is_some() {
        bail!("Credentials in feed URLs are not supported");
    }
    u.set_fragment(None);
    Ok(u.to_string())
}
pub fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
