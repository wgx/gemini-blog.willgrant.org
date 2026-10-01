mod tls;

use anyhow::{Context, Result};
use std::net::{Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use tokio_rustls::TlsAcceptor;

const MAX_REQUEST_BYTES: usize = 1024;

#[tokio::main]
async fn main() -> Result<()> {
    // rustls 0.23 wants an explicit process-wide default crypto provider
    // installed before any ServerConfig is built; we only compile in the
    // "ring" backend, so install that one. Safe to ignore the error case
    // (it only fails if something else already installed a provider).
    let _ = rustls::crypto::ring::default_provider().install_default();

    let port: u16 = std::env::var("LISTEN_PORT")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1965);
    let content_dir: PathBuf = std::env::var("CONTENT_DIR")
        .unwrap_or_else(|_| ".".to_string())
        .into();

    anyhow::ensure!(
        content_dir.is_dir(),
        "CONTENT_DIR '{}' does not exist or is not a directory",
        content_dir.display()
    );
    let content_dir = Arc::new(
        content_dir
            .canonicalize()
            .context("canonicalizing CONTENT_DIR")?,
    );

    let tls_config = tls::build_server_config()?;
    let acceptor = TlsAcceptor::from(tls_config);

    // Use one IPv6 wildcard socket with IPv4-mapped connections enabled.
    // Separate wildcard sockets collide on Linux because [::] is dual-stack
    // by default and already covers 0.0.0.0.
    let listener = TcpListener::bind(SocketAddr::from((Ipv6Addr::UNSPECIFIED, port)))
        .await
        .with_context(|| format!("binding [::]:{port}"))?;

    println!(
        "Gemini server listening on port {port} (IPv4 + IPv6), serving {}",
        content_dir.display()
    );

    let accept_task = accept_loop(listener, acceptor, content_dir);

    tokio::select! {
        res = accept_task => res?,
        _ = tokio::signal::ctrl_c() => {
            println!("shutting down");
        }
    }

    Ok(())
}

async fn accept_loop(
    listener: TcpListener,
    acceptor: TlsAcceptor,
    content_dir: Arc<PathBuf>,
) -> Result<()> {
    loop {
        let (socket, peer) = match listener.accept().await {
            Ok(pair) => pair,
            Err(e) => {
                eprintln!("accept error: {e}");
                continue;
            }
        };
        let acceptor = acceptor.clone();
        let content_dir = content_dir.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(socket, acceptor, content_dir).await {
                eprintln!("connection from {peer} failed: {e}");
            }
        });
    }
}

async fn handle_connection(
    socket: tokio::net::TcpStream,
    acceptor: TlsAcceptor,
    content_dir: Arc<PathBuf>,
) -> Result<()> {
    let mut stream = acceptor.accept(socket).await.context("TLS handshake failed")?;

    let request_line = match read_request_line(&mut stream).await {
        Ok(line) => line,
        Err(e) => {
            let _ = write_status(&mut stream, 59, "Bad Request").await;
            return Err(e);
        }
    };

    let response = route(&request_line, &content_dir).await;
    match response {
        Response::Success { mime, body } => {
            stream
                .write_all(format!("20 {mime}\r\n").as_bytes())
                .await?;
            stream.write_all(&body).await?;
        }
        Response::Status { code, meta } => {
            write_status(&mut stream, code, &meta).await?;
        }
    }
    stream.shutdown().await.ok();
    Ok(())
}

async fn write_status<S: AsyncWriteExt + Unpin>(stream: &mut S, code: u16, meta: &str) -> Result<()> {
    stream.write_all(format!("{code} {meta}\r\n").as_bytes()).await?;
    Ok(())
}

/// Reads a Gemini request: a single `<url><CR><LF>` line, capped at
/// `MAX_REQUEST_BYTES` per the spec, so a client can't make the server
/// buffer an unbounded amount of data before it's even parsed a path.
async fn read_request_line<S: AsyncReadExt + Unpin>(stream: &mut S) -> Result<String> {
    let mut buf = Vec::with_capacity(256);
    let mut byte = [0u8; 1];
    loop {
        let n = stream.read(&mut byte).await.context("reading request")?;
        anyhow::ensure!(n == 1, "connection closed before a complete request line was sent");
        if byte[0] == b'\n' {
            break;
        }
        if byte[0] != b'\r' {
            buf.push(byte[0]);
        }
        anyhow::ensure!(buf.len() <= MAX_REQUEST_BYTES, "request line exceeded {MAX_REQUEST_BYTES} bytes");
    }
    String::from_utf8(buf).context("request line was not valid UTF-8")
}

enum Response {
    Success { mime: String, body: Vec<u8> },
    Status { code: u16, meta: String },
}

async fn route(raw_url: &str, content_dir: &Path) -> Response {
    let parsed = match url::Url::parse(raw_url.trim()) {
        Ok(u) if u.scheme() == "gemini" => u,
        Ok(_) => return Response::Status { code: 59, meta: "Only the gemini:// scheme is supported".into() },
        Err(_) => return Response::Status { code: 59, meta: "Malformed request".into() },
    };

    let decoded_path = percent_encoding::percent_decode_str(parsed.path())
        .decode_utf8_lossy()
        .to_string();

    match resolve_path(content_dir, &decoded_path) {
        Some(file_path) => match tokio::fs::read(&file_path).await {
            Ok(body) => Response::Success { mime: mime_for(&file_path), body },
            Err(_) => Response::Status { code: 51, meta: "Not Found".into() },
        },
        None => Response::Status { code: 51, meta: "Not Found".into() },
    }
}

/// Safely map a request path onto a file under `content_dir`, refusing any
/// path that would escape it (e.g. via `..` segments) and falling back
/// to `index.gmi` for a directory-style request.
fn resolve_path(content_dir: &Path, request_path: &str) -> Option<PathBuf> {
    let trimmed = request_path.trim_start_matches('/');
    let relative = if trimmed.is_empty() || trimmed.ends_with('/') {
        format!("{trimmed}index.gmi")
    } else {
        trimmed.to_string()
    };

    let candidate = content_dir.join(&relative);

    // Canonicalize and check the result is still inside dist_dir - the
    // one thing standing between a client and arbitrary file reads if a
    // `..` slipped through.
    let canonical = candidate.canonicalize().ok()?;
    if canonical.starts_with(content_dir) {
        Some(canonical)
    } else {
        None
    }
}

fn mime_for(path: &Path) -> String {
    match path.extension().and_then(|e| e.to_str()) {
        Some("gmi") | Some("gemini") => "text/gemini; charset=utf-8".to_string(),
        Some("txt") => "text/plain; charset=utf-8".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}
