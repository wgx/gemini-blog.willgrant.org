//! TLS setup for the Gemini server.
//!
//! Gemini mandates TLS but (unlike HTTPS) has no CA-based trust model -
//! clients are expected to use "trust on first use" (TOFU) and simply
//! pin whatever certificate they first see for a given host. That means
//! a self-signed certificate is perfectly normal and expected here; it's
//! just nicer for repeat visitors if the certificate stays *stable*
//! across restarts, which is why loading a persisted cert/key from Fly
//! secrets is preferred when available, falling back to generating a
//! fresh one on the fly (pun intended) otherwise.

use anyhow::{Context, Result};
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use std::sync::Arc;

/// Build a `rustls::ServerConfig` for the Gemini TLS listener.
///
/// Looks for PEM-encoded cert/key material in `GEMINI_TLS_CERT_PEM` and
/// `GEMINI_TLS_KEY_PEM` (e.g. set via `fly secrets set`); if either is
/// missing, generates a fresh self-signed certificate for
/// `GEMINI_HOSTNAME` (default `localhost`) instead.
pub fn build_server_config() -> Result<Arc<rustls::ServerConfig>> {
    let (cert_chain, key) = match (
        std::env::var("GEMINI_TLS_CERT_PEM"),
        std::env::var("GEMINI_TLS_KEY_PEM"),
    ) {
        (Ok(cert_pem), Ok(key_pem)) => {
            eprintln!("tls: using certificate supplied via GEMINI_TLS_CERT_PEM/GEMINI_TLS_KEY_PEM");
            load_pem(&cert_pem, &key_pem)?
        }
        _ => {
            let hostname =
                std::env::var("GEMINI_HOSTNAME").unwrap_or_else(|_| "localhost".to_string());
            eprintln!(
                "tls: no GEMINI_TLS_CERT_PEM/GEMINI_TLS_KEY_PEM set; generating a transient \
                 self-signed certificate for '{hostname}' (Gemini clients use trust-on-first-use, \
                 so this is normal, but the cert will differ on every restart unless you set \
                 those two secrets)"
            );
            generate_self_signed(&hostname)?
        }
    };

    let mut config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key)
        .context("building rustls ServerConfig from certificate/key")?;

    // Gemini has no ALPN convention of its own; leaving this empty is
    // correct and avoids picking a protocol id meant for HTTP.
    config.alpn_protocols.clear();

    Ok(Arc::new(config))
}

fn load_pem(cert_pem: &str, key_pem: &str) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let certs = rustls_pemfile::certs(&mut cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .context("parsing PEM certificate chain")?;
    anyhow::ensure!(!certs.is_empty(), "GEMINI_TLS_CERT_PEM contained no certificates");

    let key = rustls_pemfile::private_key(&mut key_pem.as_bytes())
        .context("parsing PEM private key")?
        .context("GEMINI_TLS_KEY_PEM contained no private key")?;

    Ok((certs, key))
}

fn generate_self_signed(
    hostname: &str,
) -> Result<(Vec<CertificateDer<'static>>, PrivateKeyDer<'static>)> {
    let subject_alt_names = vec![hostname.to_string()];
    let generated = rcgen::generate_simple_self_signed(subject_alt_names)
        .context("generating self-signed certificate")?;

    let cert_der = CertificateDer::from(generated.cert.der().to_vec());
    let key_der = PrivateKeyDer::try_from(generated.signing_key.serialize_der())
        .map_err(|e| anyhow::anyhow!("encoding generated private key: {e}"))?;

    Ok((vec![cert_der], key_der))
}
