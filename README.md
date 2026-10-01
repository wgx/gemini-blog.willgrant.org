# gemini-blog-mirror

Mirrors `https://blog.willgrant.org` (a Jekyll blog) onto the Gemini
network (`gemini://`), hosted on Fly.io.

## How it fits together

```
GitHub Actions (schedule/manual)
   │
   ├─▶ cli  ── fetches feed.xml, diffs against manifest.json,
   │           converts new/changed posts' HTML to Gemtext,
  │           writes blog/posts/*.gmi + blog/index.gmi
   │
  ├─▶ git commit + push  (manifest.json + generated blog/ files)
   │
     └─▶ Fly.io auto-deploys from `main`, building the Docker image
       (generic `server` binary + generated blog/ content)
                              │
                              ▼
                    server (always running on Fly, sleeps when idle)
                    speaks the Gemini protocol over TLS on :1965,
                    serves files from its content root; blog/ is at
                    gemini://<host>/blog/
```

Two crates, one workspace:

* **`cli`** - runs only in CI. Fetches the feed, decides which posts are
  new or changed (via `manifest.json`), converts their HTML to Gemtext,
  and writes `.gmi` files. See `cli/src/gemtext.rs` for the HTML→Gemtext
  conversion rules (headings, extracted links, lists, code blocks,
  tables, images).
* **`server`** - the only thing that actually runs on Fly.io. A small
  `tokio` + `rustls` TCP server that terminates Gemini's TLS itself and
  serves files from `CONTENT_DIR` (the Docker image uses `/app`) from disk.
  It never fetches anything at runtime, which keeps it compatible with
  Fly's scale-to-zero/Firecracker model - there's no cache to warm and no
  state to lose when a machine sleeps.

Because this specific blog's `feed.xml` is an Atom feed with the full
post HTML embedded in `<content>`, the CLI normally never needs to fetch
each post's page separately - one feed request gets everything. (The
code still supports feeds that only ship summaries: `feed.rs` falls back
to fetching `source_url` directly for those.)

Images and other assets referenced by posts (e.g. `<img src="/images/…">`)
are **not** mirrored into `/blog` - only text is. Their links are
rewritten to absolute URLs pointing back at the original blog
(`https://blog.willgrant.org/images/…`) so they still resolve for anyone
reading the mirror.

## Local development

Requires a Rust toolchain (stable, 2021 edition) - Rust 1.80+ for
`std::sync::LazyLock`, which `cli/src/gemtext.rs` uses.

Generate the mirror locally:

```sh
cargo run -p cli -- \
  --feed-url https://blog.willgrant.org/feed.xml \
  --manifest manifest.json \
  --out blog \
  --url-prefix /blog \
  --force \
  --site-title "Blog posts by Will Grant (Gemini mirror)"
```

Run the server against that output:

```sh
CONTENT_DIR=. LISTEN_PORT=1965 cargo run -p server
```

Then point any Gemini client at `gemini://localhost/blog/` (e.g. [Lagrange](
https://gemini.circumlunar.space/clients.html)). The server generates a
throwaway self-signed certificate on every start unless
`GEMINI_TLS_CERT_PEM`/`GEMINI_TLS_KEY_PEM` are set (see
`server/src/tls.rs`), so most clients will show a TOFU (trust-on-first-use)
prompt the first time - that's normal for Gemini, not a bug.

Run the unit tests for the HTML→Gemtext conversion:

```sh
cargo test -p cli
```

## One-time setup on Fly.io

1. `fly launch --no-deploy` (or hand-edit the app name in `fly.toml` -
  it must be globally unique on Fly).
2. Configure Fly.io to auto-deploy this app from the `main` branch.
  The GitHub Actions workflow commits generated Gemtext to `main`; that
  commit triggers the Fly deployment.
3. Optional but recommended: generate a real cert/key pair once (so the
   Gemini TLS certificate is stable across restarts, rather than a fresh
   self-signed one every deploy) and store them as `fly secrets set
   GEMINI_TLS_CERT_PEM=... GEMINI_TLS_KEY_PEM=...`. Without this, the
   server just generates a transient self-signed certificate on boot,
   which is spec-compliant but means repeat visitors' Gemini clients
   will see the pinned certificate change occasionally.
4. Push to `main`, or run the "Sync blog to Gemini" workflow manually
  from the Actions tab.

## Known simplifications / things to revisit

* The HTML→Gemtext converter (`cli/src/gemtext.rs`) handles everything
  seen in this blog's actual feed (headings, paragraphs, links, images,
  lists, blockquotes, `<pre>` code blocks, tables, embedded
  `<video>`/`<source>`), but it's a best-effort mapping, not a full HTML
  renderer - deeply nested or unusual markup may need a new case added
  to `walk_block`.
* `resolve_path` in `server/src/main.rs` does directory-escape
  protection via `canonicalize()` + `starts_with`, but doesn't redirect
  a directory request without a trailing slash - it just 404s. Fine for
  this site's flat `posts/*.gmi` structure; worth revisiting if you add
  nested sections later.
* The GitHub Actions workflow commits generated Gemtext and the manifest
  straight to `main`. If the repo has required status checks on that
  branch, switch it to push to a side branch and open/update a PR instead.
