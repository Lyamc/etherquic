//! Pure-Rust TLS setup: rustls-rustcrypto CryptoProvider, no ring / no aws-lc / no cc.

use std::fs::File;
use std::io::{BufReader, Cursor, Read};
use std::path::Path;
use std::sync::Arc;

use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName, UnixTime};
use rustls::{ClientConfig, DigitallySignedStruct, Error as TlsError, RootCertStore, ServerConfig};

/// Install the process-wide rustls-rustcrypto provider. Safe to call more than once.
pub fn install_pure_crypto() {
    if rustls::crypto::CryptoProvider::get_default().is_some() {
        return;
    }
    let _ = rustls::crypto::CryptoProvider::install_default(rustls_rustcrypto::provider());
}

fn provider() -> Arc<rustls::crypto::CryptoProvider> {
    install_pure_crypto();
    rustls::crypto::CryptoProvider::get_default()
        .expect("pure crypto provider installed")
        .clone()
}

/// ALPN identifier for this experimental transport.
pub const ALPN_ETHERQUIC: &[u8] = b"etherquic";

pub fn load_server_config(cert_pem: &Path, key_pem: &Path) -> Result<ServerConfig, TlsError> {
    let provider = provider();
    let certs = load_certs(cert_pem).map_err(io_to_tls)?;
    let key = load_private_key(key_pem).map_err(io_to_tls)?;

    let mut cfg = ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| TlsError::General(e.to_string()))?
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| TlsError::General(e.to_string()))?;
    cfg.alpn_protocols = vec![ALPN_ETHERQUIC.to_vec()];
    Ok(cfg)
}

pub fn load_client_config(skip_verify: bool) -> Result<ClientConfig, TlsError> {
    let provider = provider();
    let roots = RootCertStore::empty();
    let mut cfg = ClientConfig::builder_with_provider(provider.clone())
        .with_protocol_versions(&[&rustls::version::TLS13])
        .map_err(|e| TlsError::General(e.to_string()))?
        .with_root_certificates(roots)
        .with_no_client_auth();
    cfg.alpn_protocols = vec![ALPN_ETHERQUIC.to_vec()];
    if skip_verify {
        cfg.dangerous()
            .set_certificate_verifier(Arc::new(SkipServerVerification(provider)));
    }
    Ok(cfg)
}

fn load_certs(path: &Path) -> std::io::Result<Vec<CertificateDer<'static>>> {
    let file = File::open(path)?;
    let mut reader = BufReader::new(file);
    rustls_pemfile::certs(&mut reader).collect()
}

fn load_private_key(path: &Path) -> std::io::Result<PrivateKeyDer<'static>> {
    let mut file = File::open(path)?;
    let mut buf = Vec::new();
    file.read_to_end(&mut buf)?;
    rustls_pemfile::private_key(&mut Cursor::new(&buf))?
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidData, "missing private key"))
}

fn io_to_tls(err: std::io::Error) -> TlsError {
    TlsError::General(err.to_string())
}

#[derive(Debug)]
struct SkipServerVerification(Arc<rustls::crypto::CryptoProvider>);

impl ServerCertVerifier for SkipServerVerification {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp: &[u8],
        _now: UnixTime,
    ) -> Result<ServerCertVerified, TlsError> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, TlsError> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}
