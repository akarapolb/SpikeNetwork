//! TLS termination for the relay (rustls, ring provider, TLS 1.2+).

use rustls::crypto::ring::default_provider;
use rustls::ServerConfig;
use rustls_pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_rustls::TlsAcceptor;

/// PEM files: a certificate chain (leaf first) and its private key.
#[derive(Clone, Debug)]
pub struct TlsFiles {
    pub cert: PathBuf,
    pub key: PathBuf,
}

/// Loaded once at startup; renew certificates by restarting (queues are durable).
pub fn acceptor(f: &TlsFiles) -> io::Result<TlsAcceptor> {
    let certs = CertificateDer::pem_file_iter(&f.cert)
        .map_err(io::Error::other)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(io::Error::other)?;
    if certs.is_empty() {
        return Err(io::Error::other(format!(
            "no certificates found in {}",
            f.cert.display()
        )));
    }
    let key = PrivateKeyDer::from_pem_file(&f.key).map_err(io::Error::other)?;
    let cfg = ServerConfig::builder_with_provider(Arc::new(default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(io::Error::other)?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(io::Error::other)?;
    Ok(TlsAcceptor::from(Arc::new(cfg)))
}
