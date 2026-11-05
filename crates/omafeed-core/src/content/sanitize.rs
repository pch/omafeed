//! Making feed HTML safe to store and show.
use std::collections::HashSet;

/// Sanitize feed HTML, resolving relative URLs against `base`.
pub fn sanitize(html: &str, base: &str) -> String {
    let mut clean = ammonia::Builder::default();
    clean.url_schemes(HashSet::from(["http", "https", "mailto"]));
    if let Ok(base) = url::Url::parse(base) {
        clean.url_relative(ammonia::UrlRelative::RewriteWithBase(base));
    }
    // Quotes and lists take their direction from their own text, because the reader draws the
    // quote bar and the bullets on the side the element starts, and CSS cannot make that side
    // follow the text inside. A feed's own `dir` attributes are dropped: the browser skips
    // elements that carry one when it works out a quote's direction, so a quote of
    // `<p dir="rtl">` text would come out left to right.
    for tag in ["blockquote", "ul", "ol"] {
        clean.set_tag_attribute_value(tag, "dir", "auto");
    }
    clean.clean(html).to_string()
}

#[cfg(test)]
mod tests {
    use super::sanitize;

    #[test]
    fn quotes_and_lists_follow_their_own_text_direction() {
        let html = sanitize(
            r#"<blockquote dir="ltr"><p dir="rtl">مرحبا</p></blockquote><ul><li>One</li></ul><ol><li>واحد</li></ol>"#,
            "",
        );
        assert_eq!(
            html,
            r#"<blockquote dir="auto"><p>مرحبا</p></blockquote><ul dir="auto"><li>One</li></ul><ol dir="auto"><li>واحد</li></ol>"#
        );
    }
}
