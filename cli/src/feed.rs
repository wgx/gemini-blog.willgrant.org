//! Fetches the blog's RSS/Atom feed and normalizes each entry into a
//! `RawPost` the rest of the pipeline can work with, regardless of whether
//! the source feed was RSS 2.0 or Atom.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};

/// One feed entry, before it has been diffed against the manifest or
/// converted to Gemtext.
#[derive(Debug, Clone)]
pub struct RawPost {
    pub id: String,
    pub title: String,
    pub source_url: String,
    pub published: DateTime<Utc>,
    pub updated: DateTime<Utc>,
    /// Full HTML body, if the feed embedded it (common for Atom feeds,
    /// e.g. Jekyll's `feed.xml`). `None` for feeds that only ship a
    /// summary/excerpt, in which case the caller must fetch `source_url`
    /// itself to get the full post HTML.
    pub html_body: Option<String>,
}

pub struct FeedClient {
    http: reqwest::Client,
}

impl FeedClient {
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent("gemini-blog-mirror/0.1 (+https://github.com/; static Gemini mirror generator)")
            .build()?;
        Ok(Self { http })
    }

    /// Download and parse the feed at `feed_url`, returning entries newest
    /// first (feed-rs already normalizes RSS/Atom ordering quirks).
    pub async fn fetch_entries(&self, feed_url: &str) -> Result<Vec<RawPost>> {
        let bytes = self
            .http
            .get(feed_url)
            .send()
            .await
            .with_context(|| format!("requesting feed {feed_url}"))?
            .error_for_status()
            .with_context(|| format!("feed {feed_url} returned an error status"))?
            .bytes()
            .await
            .context("reading feed response body")?;

        let feed = feed_rs::parser::parse(&bytes[..])
            .with_context(|| format!("parsing feed XML from {feed_url}"))?;

        let mut posts = Vec::with_capacity(feed.entries.len());
        for entry in feed.entries {
            let Some(source_url) = entry
                .links
                .iter()
                .find(|l| l.rel.as_deref().unwrap_or("alternate") == "alternate")
                .or_else(|| entry.links.first())
                .map(|l| l.href.clone())
            else {
                // An entry with no link at all can't be mirrored anywhere
                // useful; skip it rather than fail the whole run.
                continue;
            };

            let title = entry
                .title
                .map(|t| t.content)
                .unwrap_or_else(|| "Untitled post".to_string());

            let published = entry.published.or(entry.updated).unwrap_or_else(Utc::now);
            let updated = entry.updated.or(entry.published).unwrap_or(published);

            let html_body = entry
                .content
                .and_then(|c| c.body)
                .or_else(|| entry.summary.map(|s| s.content));

            posts.push(RawPost {
                id: entry.id,
                title,
                source_url,
                published,
                updated,
                html_body,
            });
        }

        // Newest first.
        posts.sort_by(|a, b| b.published.cmp(&a.published));
        Ok(posts)
    }

    /// Fetch a post's full HTML page directly. Used as a fallback for
    /// feeds that only publish a summary/excerpt rather than full content
    /// (the spec's "fetch full HTML for changed/new URLs" step).
    pub async fn fetch_post_html(&self, url: &str) -> Result<String> {
        let text = self
            .http
            .get(url)
            .send()
            .await
            .with_context(|| format!("requesting post page {url}"))?
            .error_for_status()?
            .text()
            .await
            .with_context(|| format!("reading post page body from {url}"))?;
        Ok(text)
    }
}
