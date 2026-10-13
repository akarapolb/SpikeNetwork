//! How the client reaches a relay: `wss://` with certificate verification, or
//! plain `ws://` only to loopback unless explicitly allowed.
//!
//! End-to-end encryption already hides message content from the relay and from
//! the network. TLS additionally hides device ids, timing and sizes from
//! on-path observers, and keeps the relay connection from being tampered with.

use anyhow::{anyhow, bail, Context, Result};
use rustls::crypto::ring::default_provider;
use rustls::{ClientConfig, RootCertStore};
use rustls_pki_types::{pem::PemObject, CertificateDer, ServerName};
use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::Uri;
use tokio_tungstenite::WebSocketStream;

#[derive(Clone, Debug, Default)]
pub struct TransportOpts {
    /// Extra trusted CA certificate (PEM), e.g. for a self-hosted relay. Added to the built-in roots.
    pub ca_file: Option<PathBuf>,
    /// Permit plain `ws://` to non-loopback hosts. Content stays end-to-end
    /// encrypted, but metadata and the connection itself are exposed.
    pub allow_insecure: bool,
}

#[derive(Clone, Debug)]
pub struct Target {
    pub url: String,
    pub opts: TransportOpts,
}

impl From<&str> for Target {
    fn from(url: &str) -> Self {
        Target {
            url: url.into(),
            opts: TransportOpts::default(),
        }
    }
}
impl From<&String> for Target {
    fn from(url: &String) -> Self {
        url.as_str().into()
    }
}
impl From<String> for Target {
    fn from(url: String) -> Self {
        url.as_str().into()
    }
}

pub trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}

pub type Ws = WebSocketStream<Box<dyn Io>>;

fn is_loopback_host(host: &str) -> bool {
    let h = host.trim_matches(['[', ']']);
    h.eq_ignore_ascii_case("localhost") || h.parse::<IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

pub async fn dial(t: &Target) -> Result<Ws> {
    let uri: Uri = t
        .url
        .parse()
        .with_context(|| format!("invalid relay URL '{}'", t.url))?;
    let host = uri
        .host()
        .ok_or_else(|| anyhow!("relay URL has no host"))?
        .to_string();
    let tls = match uri.scheme_str() {
        Some("wss") => true,
        Some("ws") => false,
        _ => bail!("relay URL must start with wss:// (or ws:// for loopback)"),
    };
    if !tls && !is_loopback_host(&host) && !t.opts.allow_insecure {
        bail!(
            "refusing plain ws:// to {host}: use wss://, or pass --allow-insecure-transport \
             (message content stays encrypted, but metadata and the connection are exposed)"
        );
    }
    let port = uri.port_u16().unwrap_or(if tls { 443 } else { 80 });
    let tcp = TcpStream::connect((host.trim_matches(['[', ']']), port))
        .await
        .with_context(|| format!("connecting to {host}:{port}"))?;
    tcp.set_nodelay(true).ok();

    let io: Box<dyn Io> = if tls {
        let name = ServerName::try_from(host.trim_matches(['[', ']']).to_string())
            .map_err(|_| anyhow!("invalid TLS server name '{host}'"))?;
        let stream = TlsConnector::from(client_config(&t.opts)?)
            .connect(name, tcp)
            .await
            .with_context(|| {
                format!("TLS handshake with {host} failed (certificate not trusted?)")
            })?;
        Box::new(stream)
    } else {
        Box::new(tcp)
    };
    let (ws, _) =
        tokio_tungstenite::client_async(t.url.as_str().into_client_request()?, io).await?;
    Ok(ws)
}

fn client_config(opts: &TransportOpts) -> Result<Arc<ClientConfig>> {
    let mut roots = RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    if let Some(ca) = &opts.ca_file {
        let mut added = 0;
        for cert in CertificateDer::pem_file_iter(ca)
            .with_context(|| format!("reading {}", ca.display()))?
        {
            roots.add(cert?).context("invalid CA certificate")?;
            added += 1;
        }
        if added == 0 {
            bail!("no certificates found in {}", ca.display());
        }
    }
    let cfg = ClientConfig::builder_with_provider(Arc::new(default_provider()))
        .with_safe_default_protocol_versions()?
        .with_root_certificates(roots)
        .with_no_client_auth();
    Ok(Arc::new(cfg))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_detection() {
        for h in ["localhost", "127.0.0.1", "127.9.9.9", "[::1]", "LOCALHOST"] {
            assert!(is_loopback_host(h), "{h}");
        }
        for h in [
            "example.com",
            "10.0.0.5",
            "192.168.1.2",
            "0.0.0.0",
            "[2001:db8::1]",
            "localhost.evil.com",
        ] {
            assert!(!is_loopback_host(h), "{h}");
        }
    }
}
