//! `src/tls/mod.rs`

// The `tls` module is intentionally private in the crate root (`mod tls;` instead of `pub mod tls;`).
// The only `pub(crate)` helper below must remain crate-visible so that:
//   • `self_signed` submodule can call it
//   • tests (unit + integration) can call it from outside the `tls` module
//
// This triggers *two* lints:
//   1. rustc's `unreachable_pub`
//   2. Clippy's `redundant_pub_crate` (the exact error you are seeing via `cargo clippy`)
//
// Both are suppressed at module level — this is the idiomatic, zero-overhead fix used across the Rust ecosystem
// for internal helpers that need crate-wide visibility while living in a private module.
#![allow(unreachable_pub)]
// Public re-exports here match the module layout and keep paths stable for callers.
#![allow(clippy::redundant_pub_crate)]

#[cfg(feature = "tls-acme")]
pub mod acme;
#[cfg(feature = "tls-self-signed")]
pub mod self_signed;

use std::{path::Path, sync::Arc};
use tokio_rustls::TlsAcceptor;

use crate::error::Result;
use crate::{
    config::{ManualCertConfig, TlsConfig},
    error::AppError,
};

/// A TLS acceptor that is either a static [`TlsAcceptor`] (manual cert or
/// self-signed) or an [`AcmeAcceptor`] (Let's Encrypt / rustls-acme).
///
/// The ACME variant cannot be represented as a plain [`TlsAcceptor`] because
/// the underlying certificate is rotated dynamically by the background renewal
/// loop, which requires the `AcmeAcceptor` handle to remain live.
pub enum Acceptor {
    /// Static certificate — manual PEM files or a self-signed dev cert.
    ///
    /// Both the [`TlsAcceptor`] (for manual accept loops) and the underlying
    /// [`rustls::ServerConfig`] (required by `axum-server`'s
    /// `RustlsConfig::from_config`) are stored together so callers don't need
    /// to reach inside `TlsAcceptor` to retrieve the config.
    Static(Arc<TlsAcceptor>, Arc<rustls::ServerConfig>),
    /// Let's Encrypt certificate managed by `rustls-acme`.
    ///
    /// The `ServerConfig` is stored alongside the acceptor because
    /// `futures_rustls::server::StartHandshake::into_stream` requires it to
    /// complete the handshake. It is built with `ResolvesServerCertAcme` as
    /// the certificate resolver so that newly-issued/renewed certificates are
    /// picked up automatically without restarting the server.
    #[cfg(feature = "tls-acme")]
    Acme(Arc<rustls_acme::AcmeAcceptor>, Arc<rustls::ServerConfig>),
}

/// Construct an [`Acceptor`] from the provided [`TlsConfig`], or return
/// `None` if TLS is disabled.
///
/// Resolution order:
/// 1. `tls.enabled = false` → `None` (HTTP-only, no change to existing behaviour)
/// 2. `[tls.manual_cert]` → load PEM files from disk
/// 3. `[tls.acme]` → Let's Encrypt via `rustls-acme` (spawns renewal loop)
/// 4. fallback → auto-generate a `localhost` self-signed dev cert via `rcgen`
///
/// # Errors
///
/// Returns [`crate::AppError::Tls`] if:
/// - A manual certificate path cannot be read or parsed.
/// - The ACME config is invalid (empty domain list, IP address as domain, etc.).
/// - The ACME cache directory cannot be created.
/// - The self-signed certificate cannot be generated or written to disk.
pub fn build_acceptor(cfg: &TlsConfig, data_dir: &Path) -> Result<Option<Acceptor>> {
    if !cfg.enabled {
        return Ok(None);
    }
    if let Some(manual) = &cfg.manual_cert {
        tracing::info!(target: "tls", "TLS: loading manual certificate");
        let (acceptor, server_cfg) = load_manual_cert(manual, data_dir)?;
        return Ok(Some(Acceptor::Static(acceptor, server_cfg)));
    }
    if cfg.acme.enabled {
        tracing::info!(target: "tls", "TLS: starting ACME / Let's Encrypt provisioning");
        #[cfg(feature = "tls-acme")]
        {
            let (acme_acceptor, server_cfg) = acme::build_acme_acceptor(&cfg.acme, data_dir)?;
            return Ok(Some(Acceptor::Acme(acme_acceptor, server_cfg)));
        }
        #[cfg(not(feature = "tls-acme"))]
        {
            return Err(AppError::Tls(
                "ACME requested in config but binary was built without the tls-acme feature. \
                 Rebuild with: cargo build --features tls-acme"
                    .to_string(),
            ));
        }
    }
    tracing::info!(target: "tls", "TLS: no cert configured — generating self-signed dev certificate");
    #[cfg(feature = "tls-self-signed")]
    {
        let (acceptor, server_cfg) = self_signed::generate_or_load(data_dir)?;
        Ok(Some(Acceptor::Static(acceptor, server_cfg)))
    }
    #[cfg(not(feature = "tls-self-signed"))]
    {
        Err(AppError::Tls(
            "TLS is enabled but no certificate source is available. Configure [tls.manual_cert] \
             or rebuild with a certificate feature such as `tls-self-signed` or `tls-acme`."
                .to_string(),
        ))
    }
}

// ---------------------------------------------------------------------------
// Manual cert loader (shared by mod.rs; also called from tests)
// ---------------------------------------------------------------------------

/// Load a PEM certificate chain + private key from disk and wrap them in a
/// [`TlsAcceptor`] and [`rustls::ServerConfig`]. Paths in
/// [`ManualCertConfig`] are resolved relative to `data_dir` so that configs
/// remain portable.
fn load_manual_cert(
    cfg: &ManualCertConfig,
    data_dir: &Path,
) -> Result<(Arc<TlsAcceptor>, Arc<rustls::ServerConfig>)> {
    let cert_path = data_dir.join(&cfg.cert_path);
    let key_path = data_dir.join(&cfg.key_path);
    tracing::debug!(
        target: "tls",
        "TLS: loading cert from {} and key from {}",
        cert_path.display(),
        key_path.display()
    );
    load_pem_as_acceptor(&cert_path, &key_path)
}

// ---------------------------------------------------------------------------
// Shared PEM → TlsAcceptor helper
// (also used by self_signed after writing the cert files)
// ---------------------------------------------------------------------------

/// Parse a PEM certificate chain and a PEM private key from disk and produce
/// a [`TlsAcceptor`] and the underlying [`rustls::ServerConfig`] it was built
/// from.
///
/// Both are returned so that callers can pass the `ServerConfig` directly to
/// `axum-server`'s `RustlsConfig::from_config` without needing to reach inside
/// the `TlsAcceptor`.
///
/// rustls defaults are intentionally left untouched — TLS 1.2+ and a safe
/// cipher list are enforced automatically; no overrides required.
pub(crate) fn load_pem_as_acceptor(
    cert_path: &Path,
    key_path: &Path,
) -> Result<(Arc<TlsAcceptor>, Arc<rustls::ServerConfig>)> {
    use rustls_pki_types::{pem::PemObject as _, CertificateDer, PrivateKeyDer};

    // --- certificate chain ---------------------------------------------------
    let cert_pem = std::fs::read(cert_path)
        .map_err(|e| AppError::Tls(format!("failed to read cert {}: {e}", cert_path.display())))?;
    let certs: Vec<CertificateDer<'static>> =
        CertificateDer::pem_reader_iter(&mut cert_pem.as_slice())
            .collect::<std::result::Result<Vec<_>, _>>()
            .map_err(|e| AppError::Tls(format!("failed to parse cert PEM: {e}")))?;
    if certs.is_empty() {
        return Err(AppError::Tls(format!(
            "no certificates found in {}",
            cert_path.display()
        )));
    }

    // --- private key ---------------------------------------------------------
    let key_pem = std::fs::read(key_path)
        .map_err(|e| AppError::Tls(format!("failed to read key {}: {e}", key_path.display())))?;
    let key: PrivateKeyDer<'static> = PrivateKeyDer::from_pem_reader(&mut key_pem.as_slice())
        .map_err(|e| AppError::Tls(format!("failed to parse key PEM: {e}")))?;

    // --- ServerConfig --------------------------------------------------------
    let server_cfg = Arc::new(
        rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(certs, key)
            .map_err(|e| AppError::Tls(format!("invalid certificate/key pair: {e}")))?,
    );
    let acceptor = Arc::new(TlsAcceptor::from(Arc::clone(&server_cfg)));
    Ok((acceptor, server_cfg))
}
