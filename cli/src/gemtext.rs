//! Converts a blog post's HTML body into `text/gemini` (Gemtext).
//!
//! Gemtext is deliberately dumb: every line is one of a handful of line
//! types (text, link, heading, list item, quote, or inside a preformatted
//! toggle block) and there is no inline markup at all. That mismatch with
//! HTML is the whole reason this module exists - notably:
//!
//!   * inline `<a href>` links cannot stay inline. Per the Gemini spec a
//!     `=>` line is the *entire* line, so every link found inside a
//!     paragraph is pulled out and appended as its own `=> url label`
//!     line immediately after the paragraph text that contained it.
//!   * inline emphasis (`<strong>`, `<em>`, `<code>`, ...) has no Gemtext
//!     equivalent, so it is flattened to plain text.
//!   * `<pre>`/`<code>` blocks and `<table>`s are wrapped in a ``` ```
//!     preformatting toggle so at least their layout survives.

use ego_tree::NodeRef;
use scraper::{ElementRef, Html, Node};
use url::Url;

/// Convert a full or partial HTML document into Gemtext.
///
/// `base_url`, when given, is used to resolve any relative `href`/`src`
/// attributes (e.g. `/images/foo.gif`) into absolute URLs pointing back
/// at the original web blog - since this pipeline mirrors *text* content
/// only, images and other assets stay hosted on the source site rather
/// than being copied into `/dist`. Pass the post's own canonical URL as
/// the base so relative links resolve exactly the way a browser would
/// have resolved them on the original page.
pub fn html_to_gemtext(html: &str, base_url: Option<&str>) -> String {
    let document = Html::parse_fragment(html);
    let mut ctx = Context {
        base_url: base_url.and_then(|u| Url::parse(u).ok()),
        ..Context::default()
    };
    walk_children(document.tree.root(), &mut ctx);
    ctx.finish()
}

/// Accumulates output lines and tracks a little bit of state (whether
/// we're inside a preformatted block, and how deeply nested list items
/// are) while walking the DOM.
#[derive(Default)]
struct Context {
    lines: Vec<String>,
    list_depth: usize,
    base_url: Option<Url>,
}

impl Context {
    fn push_text_line(&mut self, s: &str) {
        let s = collapse_whitespace(s);
        if !s.is_empty() {
            self.lines.push(s);
        }
    }

    fn push_blank(&mut self) {
        if !matches!(self.lines.last().map(String::as_str), Some("") | None) {
            self.lines.push(String::new());
        }
    }

    fn push_link(&mut self, url: &str, label: &str) {
        let label = collapse_whitespace(label);
        let url = self.resolve(url.trim());
        if url.is_empty() {
            return;
        }
        if label.is_empty() {
            self.lines.push(format!("=> {url}"));
        } else {
            self.lines.push(format!("=> {url} {label}"));
        }
    }

    /// Resolve a possibly-relative URL against `base_url`. Absolute URLs
    /// (anything with its own scheme, e.g. `https://...`, `mailto:...`)
    /// pass through `Url::join` unchanged, per the standard URL-joining
    /// rules, so it's always safe to call this on every link/image src.
    fn resolve(&self, raw: &str) -> String {
        if raw.is_empty() {
            return String::new();
        }
        match &self.base_url {
            Some(base) => base.join(raw).map(|u| u.to_string()).unwrap_or_else(|_| raw.to_string()),
            None => raw.to_string(),
        }
    }

    fn push_heading(&mut self, level: u8, text: &str) {
        let text = collapse_whitespace(text);
        if text.is_empty() {
            return;
        }
        // Gemtext only has three heading levels; anything past h3 still
        // gets flagged as a heading rather than silently becoming a
        // plain paragraph.
        let marker = match level {
            1 => "#",
            2 => "##",
            _ => "###",
        };
        self.push_blank();
        self.lines.push(format!("{marker} {text}"));
        self.push_blank();
    }

    fn push_list_item(&mut self, text: &str) {
        let text = collapse_whitespace(text);
        if !text.is_empty() {
            self.lines.push(format!("* {text}"));
        }
    }

    fn push_quote_line(&mut self, text: &str) {
        let text = collapse_whitespace(text);
        if !text.is_empty() {
            self.lines.push(format!("> {text}"));
        }
    }

    fn push_preformatted(&mut self, alt: Option<&str>, body: &str) {
        self.push_blank();
        self.lines.push(format!("```{}", alt.unwrap_or("")));
        for line in body.lines() {
            self.lines.push(line.trim_end().to_string());
        }
        self.lines.push("```".to_string());
        self.push_blank();
    }

    fn finish(mut self) -> String {
        while self.lines.last().map(String::as_str) == Some("") {
            self.lines.pop();
        }
        let mut out = self.lines.join("\n");
        out.push('\n');
        out
    }
}

fn collapse_whitespace(s: &str) -> String {
    s.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Walk every child of `node`, dispatching block-level elements to their
/// own handling and letting stray inline/text content at the top level
/// fall back to being treated as an implicit paragraph.
fn walk_children(node: NodeRef<'_, Node>, ctx: &mut Context) {
    for child in node.children() {
        walk_block(child, ctx);
    }
}

fn walk_block(node: NodeRef<'_, Node>, ctx: &mut Context) {
    match node.value() {
        Node::Element(el) => {
            let tag = el.name();
            match tag {
                "script" | "style" | "noscript" | "head" | "nav" | "form" | "button" | "iframe" => {
                    // Never mirrored: scripts/styles are unsafe/irrelevant,
                    // nav/forms are page chrome rather than post content.
                }
                "h1" => ctx.push_heading(1, &inline_text(node)),
                "h2" => ctx.push_heading(2, &inline_text(node)),
                "h3" | "h4" | "h5" | "h6" => ctx.push_heading(3, &inline_text(node)),
                "p" | "div" | "section" | "article" | "figure" | "figcaption" | "header" | "footer" => {
                    emit_inline_block(node, ctx);
                    walk_only_block_children(node, ctx);
                }
                "a" => {
                    // A bare block-level anchor (rare, but some templates
                    // wrap an image or heading in one) - treat as inline.
                    emit_inline_block(node, ctx);
                }
                "ul" | "ol" => {
                    ctx.push_blank();
                    ctx.list_depth += 1;
                    for li in node.children() {
                        if let Node::Element(le) = li.value() {
                            if le.name() == "li" {
                                let (text, links) = inline_text_and_links(li);
                                ctx.push_list_item(&text);
                                for (url, label) in links {
                                    ctx.push_link(&url, &label);
                                }
                                // Nested block content (e.g. a nested <ul>)
                                // inside the <li> is rendered after it.
                                walk_only_block_children(li, ctx);
                            }
                        }
                    }
                    ctx.list_depth -= 1;
                    ctx.push_blank();
                }
                "blockquote" => {
                    ctx.push_blank();
                    let text = block_plain_text(node);
                    for line in text.lines() {
                        ctx.push_quote_line(line);
                    }
                    ctx.push_blank();
                }
                "pre" => {
                    let code = node_text(node);
                    ctx.push_preformatted(None, &code);
                }
                "table" => {
                    let rendered = render_table_as_text(node);
                    ctx.push_preformatted(Some("table"), &rendered);
                }
                "img" => {
                    let src = el.attr("src").unwrap_or_default();
                    let alt = el.attr("alt").unwrap_or("image");
                    ctx.push_blank();
                    ctx.push_link(src, alt);
                }
                "video" | "audio" => {
                    // <source src="..."> children carry the actual media
                    // URL; surface each as a link since Gemini has no
                    // native media embedding.
                    for src_node in ElementRef::wrap(node)
                        .into_iter()
                        .flat_map(|e| e.select(&SOURCE_SELECTOR))
                    {
                        if let Some(src) = src_node.value().attr("src") {
                            ctx.push_link(src, tag);
                        }
                    }
                    if let Some(src) = el.attr("src") {
                        ctx.push_link(src, tag);
                    }
                }
                "hr" => {
                    ctx.push_blank();
                    ctx.lines.push("---".to_string());
                    ctx.push_blank();
                }
                "br" => {
                    // Handled by the inline-text collapsing of whichever
                    // block wraps this <br>; nothing to do standalone.
                }
                _ => {
                    // Unknown/other elements: recurse into children so we
                    // don't lose content nested inside wrapper tags we
                    // don't specifically handle (e.g. <span>, <main>).
                    walk_children(node, ctx);
                }
            }
        }
        Node::Text(text) => {
            // Stray top-level text not wrapped in a block element.
            ctx.push_text_line(text);
        }
        _ => {}
    }
}

/// Recurse into a container's children looking only for further
/// block-level elements (lists, blockquotes, pre, tables, nested divs) -
/// used after a paragraph's own inline text/links have already been
/// emitted, to avoid printing that same text twice.
fn walk_only_block_children(node: NodeRef<'_, Node>, ctx: &mut Context) {
    for child in node.children() {
        if let Node::Element(el) = child.value() {
            if matches!(
                el.name(),
                "ul" | "ol" | "blockquote" | "pre" | "table" | "div" | "section" | "figure" | "hr"
                    | "video" | "audio" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "header"
                    | "footer" | "article" | "figcaption"
            ) {
                walk_block(child, ctx);
            }
        }
    }
}

/// Emit a block's *direct* inline content (text + links), collapsing
/// runs of inline elements into a single paragraph, followed immediately
/// by every link that paragraph contained (per the Gemini "one thing per
/// line" rule).
fn emit_inline_block(node: NodeRef<'_, Node>, ctx: &mut Context) {
    let (text, links) = inline_text_and_links(node);
    if text.is_empty() && links.is_empty() {
        return;
    }
    ctx.push_blank();
    if !text.is_empty() {
        ctx.push_text_line(&text);
    }
    for (url, label) in links {
        ctx.push_link(&url, &label);
    }
    ctx.push_blank();
}

/// Collect the plain-text content of a node's *inline* descendants
/// (skipping nested block-level children, which the caller handles
/// separately), plus every `<a href>` found along the way, in document
/// order.
fn inline_text_and_links(node: NodeRef<'_, Node>) -> (String, Vec<(String, String)>) {
    let mut text = String::new();
    let mut links = Vec::new();
    collect_inline(node, &mut text, &mut links, true);
    (collapse_whitespace(&text), links)
}

fn inline_text(node: NodeRef<'_, Node>) -> String {
    let mut text = String::new();
    let mut links = Vec::new();
    collect_inline(node, &mut text, &mut links, true);
    collapse_whitespace(&text)
}

fn collect_inline(
    node: NodeRef<'_, Node>,
    text: &mut String,
    links: &mut Vec<(String, String)>,
    is_root: bool,
) {
    for child in node.children() {
        match child.value() {
            Node::Text(t) => {
                text.push_str(t);
                text.push(' ');
            }
            Node::Element(el) => match el.name() {
                "script" | "style" => {}
                "br" => text.push('\n'),
                "a" => {
                    let href = el.attr("href").unwrap_or_default().to_string();
                    let label = {
                        let mut t = String::new();
                        let mut inner_links = Vec::new();
                        collect_inline(child, &mut t, &mut inner_links, false);
                        collapse_whitespace(&t)
                    };
                    if !href.is_empty() {
                        text.push_str(&label);
                        text.push(' ');
                        links.push((href, label));
                    } else {
                        text.push_str(&label);
                        text.push(' ');
                    }
                }
                "img" => {
                    // Images are surfaced as their own `=>` link line
                    // (with the alt text as the label) rather than being
                    // inlined as text - Gemtext has no way to embed an
                    // image inline anyway, and this is what most Gemini
                    // clients expect.
                    let src = el.attr("src").unwrap_or_default().to_string();
                    let alt = el.attr("alt").unwrap_or("image").to_string();
                    if !src.is_empty() {
                        links.push((src, alt));
                    }
                }
                // Genuine block-level tags stop inline collection at the
                // root call (the caller walks them separately via
                // `walk_only_block_children`), but if we're already
                // inside an inline run (e.g. a <span> wrapping a <div>,
                // which does happen in the wild) just flatten it too.
                "div" | "p" | "ul" | "ol" | "li" | "blockquote" | "table" | "pre" | "figure"
                | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "video" | "audio"
                    if is_root =>
                {
                    // stop: handled by the block walker
                }
                _ => collect_inline(child, text, links, is_root),
            },
            _ => {}
        }
    }
}

/// Plain text of a block and *all* its descendants, block-level or not -
/// used for blockquotes, where nested paragraphs are common but Gemtext
/// only has a single flat quote-line type anyway.
fn block_plain_text(node: NodeRef<'_, Node>) -> String {
    let mut out = String::new();
    fn walk(node: NodeRef<'_, Node>, out: &mut String) {
        for child in node.children() {
            match child.value() {
                Node::Text(t) => out.push_str(t),
                Node::Element(el) if matches!(el.name(), "script" | "style") => {}
                Node::Element(el) if el.name() == "p" || el.name() == "br" => {
                    walk(child, out);
                    out.push('\n');
                }
                Node::Element(_) => walk(child, out),
                _ => {}
            }
        }
    }
    walk(node, &mut out);
    out.lines()
        .map(collapse_whitespace)
        .filter(|l| !l.is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Raw text of a `<pre>`/`<code>` block, preserving internal line breaks
/// (unlike everywhere else, whitespace here is significant).
fn node_text(node: NodeRef<'_, Node>) -> String {
    let mut out = String::new();
    fn walk(node: NodeRef<'_, Node>, out: &mut String) {
        for child in node.children() {
            match child.value() {
                Node::Text(t) => out.push_str(t),
                Node::Element(_) => walk(child, out),
                _ => {}
            }
        }
    }
    walk(node, &mut out);
    out.trim_matches('\n').to_string()
}

/// Extremely simple table -> text rendering: one line per row, cells
/// joined with " | ". Good enough to preserve the information inside a
/// preformatted block; Gemtext has no real table concept.
fn render_table_as_text(node: NodeRef<'_, Node>) -> String {
    let element = match ElementRef::wrap(node) {
        Some(e) => e,
        None => return String::new(),
    };
    let mut rows = Vec::new();
    for row in element.select(&TR_SELECTOR) {
        let cells: Vec<String> = row
            .select(&CELL_SELECTOR)
            .map(|c| collapse_whitespace(&c.text().collect::<String>()))
            .collect();
        rows.push(cells.join(" | "));
    }
    rows.join("\n")
}

// Built once and reused rather than re-parsing the CSS selector string on
// every call.
static SOURCE_SELECTOR: std::sync::LazyLock<scraper::Selector> =
    std::sync::LazyLock::new(|| scraper::Selector::parse("source").unwrap());
static TR_SELECTOR: std::sync::LazyLock<scraper::Selector> =
    std::sync::LazyLock::new(|| scraper::Selector::parse("tr").unwrap());
static CELL_SELECTOR: std::sync::LazyLock<scraper::Selector> =
    std::sync::LazyLock::new(|| scraper::Selector::parse("td, th").unwrap());

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_headings_paragraphs_and_links() {
        let html = r#"
            <h2>Step 1: Acquire photons</h2>
            <p>For this step I'll be using the <a href="https://example.com/action">ActionSampler</a>, a plastic camera.</p>
        "#;
        let gmi = html_to_gemtext(html, None);
        assert!(gmi.contains("## Step 1: Acquire photons"));
        assert!(gmi.contains("=> https://example.com/action ActionSampler"));
        assert!(gmi.contains("For this step I'll be using the ActionSampler , a plastic camera."));
    }

    #[test]
    fn converts_lists() {
        let html = "<ul><li>Developer tank and reel</li><li>Ilford Ilfosol 3 developer</li></ul>";
        let gmi = html_to_gemtext(html, None);
        assert!(gmi.contains("* Developer tank and reel"));
        assert!(gmi.contains("* Ilford Ilfosol 3 developer"));
    }

    #[test]
    fn wraps_code_blocks_in_preformatted_toggle() {
        let html = "<pre><code>magick -delay 25 -loop 0 1.png 2.png\n</code></pre>";
        let gmi = html_to_gemtext(html, None);
        assert!(gmi.contains("```"));
        assert!(gmi.contains("magick -delay 25 -loop 0 1.png 2.png"));
    }

    #[test]
    fn strips_scripts_and_styles() {
        let html = "<p>Hello</p><script>alert(1)</script><style>body{color:red}</style>";
        let gmi = html_to_gemtext(html, None);
        assert!(gmi.contains("Hello"));
        assert!(!gmi.contains("alert"));
        assert!(!gmi.contains("color:red"));
    }
}
