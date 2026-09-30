//! `manifest.json` handling.
//!
//! The manifest is the single source of truth the incremental sync diffs
//! against. It is committed to the repo alongside the generated `.gmi`
//! files, so a fresh CI runner (or a fresh clone) can pick up exactly
//! where the last run left off without re-parsing the whole blog.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;

/// One tracked blog post.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PostRecord {
    /// Stable identifier taken from the feed entry (Atom `id` / RSS `guid`).
    pub id: String,
    /// URL-safe filename stem, e.g. `2026-07-23-the-hardest-way-to-make-gif`.
    pub slug: String,
    pub title: String,
    /// Canonical HTML URL on the source blog.
    pub source_url: String,
    /// RFC 3339 timestamp, used purely for change detection and sorting.
    pub updated: String,
    /// RFC 3339 timestamp of first publication, shown in the index.
    pub published: String,
    /// Path of the generated Gemtext file, relative to the `/dist` root,
    /// e.g. `posts/2026-07-23-the-hardest-way-to-make-gif.gmi`.
    pub gmi_path: String,
}

/// The full manifest. Keyed by feed entry id so lookups during diffing are
/// O(1) rather than a linear scan.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Manifest {
    /// Bumped if the on-disk schema ever changes shape.
    #[serde(default = "default_version")]
    pub version: u32,
    #[serde(default)]
    pub posts: BTreeMap<String, PostRecord>,
}

fn default_version() -> u32 {
    1
}

impl Manifest {
    /// Load an existing manifest, or return an empty one if the file does
    /// not exist yet (e.g. the very first run against a new repo).
    pub fn load(path: &Path) -> Result<Manifest> {
        if !path.exists() {
            return Ok(Manifest::default());
        }
        let raw = std::fs::read_to_string(path)
            .with_context(|| format!("reading manifest at {}", path.display()))?;
        let manifest: Manifest = serde_json::from_str(&raw)
            .with_context(|| format!("parsing manifest at {}", path.display()))?;
        Ok(manifest)
    }

    /// Write the manifest back out, pretty-printed so diffs in git stay
    /// readable during code review.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let raw = serde_json::to_string_pretty(self)?;
        std::fs::write(path, raw + "\n")
            .with_context(|| format!("writing manifest to {}", path.display()))?;
        Ok(())
    }

    /// True if `entry_id` is missing from the manifest, or present but with
    /// an older `updated` timestamp than what the feed now reports.
    pub fn needs_regeneration(&self, entry_id: &str, updated: &str) -> bool {
        match self.posts.get(entry_id) {
            None => true,
            Some(existing) => existing.updated != updated,
        }
    }

    /// Posts sorted newest-first by publish date, for index generation.
    pub fn posts_by_recency(&self) -> Vec<&PostRecord> {
        let mut posts: Vec<&PostRecord> = self.posts.values().collect();
        posts.sort_by(|a, b| b.published.cmp(&a.published));
        posts
    }
}
