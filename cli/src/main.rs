mod feed;
mod gemtext;
mod manifest;
mod slug;

use anyhow::{Context, Result};
use clap::Parser;
use manifest::{Manifest, PostRecord};
use std::path::PathBuf;

/// Ingestion CLI: reads a blog's RSS/Atom feed, diffs it against
/// `manifest.json`, and writes only new/changed posts as Gemtext into
/// `--out`. Designed to run in CI on a schedule and have its output
/// (including the updated manifest) committed back to the repo.
#[derive(Parser, Debug)]
#[command(name = "cli", version, about)]
struct Args {
    /// The blog's RSS or Atom feed URL.
    #[arg(long)]
    feed_url: String,

    /// Path to the manifest.json that tracks what's already been mirrored.
    #[arg(long, default_value = "manifest.json")]
    manifest: PathBuf,

    /// Output directory for generated .gmi files.
    #[arg(long, default_value = "blog")]
    out: PathBuf,

    /// URL path where the output directory is served.
    #[arg(long, default_value = "/blog")]
    url_prefix: String,

    /// Title used at the top of the generated index.gmi.
    #[arg(long, default_value = "Blog Mirror")]
    site_title: String,

    /// Regenerate every tracked post's .gmi file even if the feed's
    /// `updated` timestamp hasn't changed. Useful after changing the
    /// HTML->Gemtext conversion logic itself.
    #[arg(long, default_value_t = false)]
    force: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();
    let url_prefix = normalize_url_prefix(&args.url_prefix);

    let mut manifest = Manifest::load(&args.manifest)
        .with_context(|| format!("loading manifest from {}", args.manifest.display()))?;

    let client = feed::FeedClient::new()?;
    let entries = client
        .fetch_entries(&args.feed_url)
        .await
        .with_context(|| format!("fetching feed {}", args.feed_url))?;

    println!("Fetched {} entries from feed", entries.len());

    let posts_dir = args.out.join("posts");
    std::fs::create_dir_all(&posts_dir)
        .with_context(|| format!("creating output directory {}", posts_dir.display()))?;

    let mut generated = 0usize;
    let mut skipped = 0usize;

    for entry in &entries {
        let updated_str = entry.updated.to_rfc3339();
        let needs_write = args.force || manifest.needs_regeneration(&entry.id, &updated_str);

        if !needs_write {
            skipped += 1;
            continue;
        }

        let post_slug = slug::derive_slug(&entry.source_url, &entry.title);

        // Prefer content already embedded in the feed (typical for Atom
        // feeds like Jekyll's `feed.xml`); only make an extra HTTP
        // request per-post for feeds that ship summaries only.
        let html_body = match &entry.html_body {
            Some(body) if !body.trim().is_empty() => body.clone(),
            _ => client
                .fetch_post_html(&entry.source_url)
                .await
                .with_context(|| format!("fetching full post HTML for {}", entry.source_url))?,
        };

        let gemtext_body = gemtext::html_to_gemtext(&html_body, Some(&entry.source_url));

        let gmi_relative = format!("posts/{post_slug}.gmi");
        let gmi_path = args.out.join(&gmi_relative);

        let page = render_post_page(
            &entry.title,
            &entry.published.to_rfc3339(),
            &gemtext_body,
            &url_prefix,
        );
        std::fs::write(&gmi_path, page)
            .with_context(|| format!("writing {}", gmi_path.display()))?;

        manifest.posts.insert(
            entry.id.clone(),
            PostRecord {
                id: entry.id.clone(),
                slug: post_slug,
                title: entry.title.clone(),
                source_url: entry.source_url.clone(),
                updated: updated_str,
                published: entry.published.to_rfc3339(),
                gmi_path: gmi_relative,
            },
        );

        generated += 1;
        println!("  wrote {}", gmi_path.display());
    }

    // The index is cheap to regenerate and always needs to be, since a
    // brand-new post changes its contents even when every *other* post
    // was unchanged this run.
    write_index(&args.out, &args.site_title, &manifest, &url_prefix)?;

    manifest.save(&args.manifest)?;

    println!("Done: {generated} generated, {skipped} unchanged, {} total tracked", manifest.posts.len());
    Ok(())
}

/// Wrap a converted post body with a small header (title, publish date,
/// a back-link to the index) and a footer back-link, so every page is
/// navigable without relying on the client's history/back button.
fn render_post_page(
    title: &str,
    published_rfc3339: &str,
    body: &str,
    url_prefix: &str,
) -> String {
    let date = published_rfc3339
        .split('T')
        .next()
        .unwrap_or(published_rfc3339);
    format!(
        "# {title}\n\nPublished: {date}\n\n{body}\n=> {url_prefix}/index.gmi Back to all posts\n"
    )
}

fn normalize_url_prefix(prefix: &str) -> String {
    let trimmed = prefix.trim_matches('/');
    if trimmed.is_empty() {
        String::new()
    } else {
        format!("/{trimmed}")
    }
}

/// Regenerate `/index.gmi`, listing every tracked post newest-first.
fn write_index(
    out_dir: &std::path::Path,
    site_title: &str,
    manifest: &Manifest,
    url_prefix: &str,
) -> Result<()> {
    let mut out = String::new();
    out.push_str(&format!("# {site_title}\n\n"));
    out.push_str("A Gemini mirror of the web blog, generated automatically.\n\n");

    for post in manifest.posts_by_recency() {
        let date = post.published.split('T').next().unwrap_or(&post.published);
        out.push_str(&format!(
            "=> {url_prefix}/{} {date} - {}\n",
            post.gmi_path, post.title
        ));
    }

    let index_path = out_dir.join("index.gmi");
    std::fs::write(&index_path, out)
        .with_context(|| format!("writing {}", index_path.display()))?;
    println!("  wrote {}", index_path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{normalize_url_prefix, render_post_page};

    #[test]
    fn normalizes_url_prefix_for_root_and_subdirectory() {
        assert_eq!(normalize_url_prefix("/blog/"), "/blog");
        assert_eq!(normalize_url_prefix("/"), "");
    }

    #[test]
    fn generated_post_backlinks_to_prefixed_index() {
        let page = render_post_page("Title", "2026-10-01T00:00:00Z", "Body", "/blog");
        assert!(page.ends_with("=> /blog/index.gmi Back to all posts\n"));
    }
}
