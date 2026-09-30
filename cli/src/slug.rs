//! Turns a post's source URL (or, failing that, its title) into a stable,
//! filesystem- and URL-safe slug.

use url::Url;

/// Prefer deriving the slug straight from the post's URL path - Jekyll and
/// most other blog engines already bake a stable, unique, human-readable
/// slug into the URL (e.g. `/2026/07/23/the-hardest-way-to-make-gif.html`)
/// so reusing it means our `.gmi` filenames stay stable across runs even
/// if a post's title is edited later. Falls back to slugifying the title
/// if the URL can't be parsed or has no useful path.
pub fn derive_slug(source_url: &str, title: &str) -> String {
    if let Ok(parsed) = Url::parse(source_url) {
        let segments: Vec<&str> = parsed
            .path_segments()
            .map(|s| s.filter(|seg| !seg.is_empty()).collect())
            .unwrap_or_default();
        if !segments.is_empty() {
            let mut parts: Vec<String> = Vec::new();
            for seg in &segments {
                // Date path segments (`2026`, `07`, `23`) get folded into
                // the slug so files stay sorted and unique; the trailing
                // `.html`/`.htm` extension is dropped since we're
                // regenerating this as `.gmi`.
                let cleaned = seg.trim_end_matches(".html").trim_end_matches(".htm");
                if !cleaned.is_empty() {
                    parts.push(slugify(cleaned));
                }
            }
            let joined = parts.join("-");
            if !joined.is_empty() {
                return joined;
            }
        }
    }
    slugify(title)
}

/// Lowercase, ASCII-only, hyphen-separated slug from arbitrary text.
pub fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut last_was_dash = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            last_was_dash = false;
        } else if !last_was_dash && !out.is_empty() {
            out.push('-');
            last_was_dash = true;
        }
    }
    while out.ends_with('-') {
        out.pop();
    }
    if out.is_empty() {
        "post".to_string()
    } else {
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn derives_slug_from_jekyll_style_url() {
        let slug = derive_slug(
            "https://blog.willgrant.org/2026/07/23/the-hardest-way-to-make-gif.html",
            "The Hardest Way to Make a GIF",
        );
        assert_eq!(slug, "2026-07-23-the-hardest-way-to-make-gif");
    }

    #[test]
    fn falls_back_to_title_slug() {
        let slug = derive_slug("not a url", "Hello, World! 2026");
        assert_eq!(slug, "hello-world-2026");
    }
}
