//! Self-signed development certificate management.
use crate::error::AppError;
use crate::error::Result;
use rustls::ServerConfig;
use std::{path::Path, sync::Arc, time::SystemTime};
use tokio_rustls::TlsAcceptor;

/// Number of days before expiry at which the cert is considered stale and
/// will be regenerated on the next startup.
const REGENERATE_BEFORE_DAYS: u64 = 30;

/// Certificates are valid for this many days when freshly generated.
const CERT_VALIDITY_DAYS: u32 = 365;

/// Names embedded in the self-signed certificate.
const CERT_SANS: &[&str] = &["localhost", "127.0.0.1", "::1"];

// ---------------------------------------------------------------------------
// Public entry point
// ---------------------------------------------------------------------------
/// Return a [`TlsAcceptor`] backed by a self-signed `localhost` certificate.
///
/// If a valid, non-expiring cert already exists in `<data_dir>/runtime/tls/dev/` it
/// is reused; otherwise a fresh one is generated with `rcgen` and written to
/// disk (mode `0600` on Unix).
///
/// **This is intended for local development only.** Never use a self-signed
/// cert in production — configure `[tls.acme]` or `[tls.manual_cert]`
/// instead.
///
/// # Errors
///
/// Returns [`AppError::Tls`] (wrapped in [`crate::Result`]) if directory
/// creation fails, certificate/key generation fails, the private files cannot
/// be written, or the PEM files cannot be loaded into a `TlsAcceptor`.
pub(super) fn generate_or_load(data_dir: &Path) -> Result<(Arc<TlsAcceptor>, Arc<ServerConfig>)> {
    let dir = data_dir.join("runtime/tls/dev");
    let cert_path = dir.join("self-signed.crt");
    let key_path = dir.join("self-signed.key");
    std::fs::create_dir_all(&dir).map_err(|e| {
        AppError::Tls(format!(
            "failed to create TLS dev directory {}: {e}",
            dir.display()
        ))
    })?;

    let should_regenerate = needs_regeneration(&cert_path, &key_path);
    if should_regenerate {
        tracing::info!("TLS: generating self-signed certificate for {CERT_SANS:?}");
        write_self_signed_cert(&cert_path, &key_path)?;
    } else {
        tracing::debug!(
            "TLS: reusing existing self-signed certificate at {}",
            cert_path.display()
        );
    }

    match super::load_pem_as_acceptor(&cert_path, &key_path) {
        Ok(acceptor) => Ok(acceptor),
        Err(error) if !should_regenerate => {
            tracing::warn!(
                "TLS: existing self-signed cert/key pair could not be loaded ({}); regenerating",
                error
            );
            write_self_signed_cert(&cert_path, &key_path)?;
            super::load_pem_as_acceptor(&cert_path, &key_path)
        }
        Err(error) => Err(error),
    }
}

// ---------------------------------------------------------------------------
// Generation
// ---------------------------------------------------------------------------
/// Generates a self-signed certificate and writes its PEM files atomically.
fn write_self_signed_cert(cert_path: &Path, key_path: &Path) -> Result<()> {
    let params = build_cert_params()?;
    let key_pair = rcgen::KeyPair::generate()
        .map_err(|e| AppError::Tls(format!("rcgen key generation failed: {e}")))?;

    // Serialize the private key *before* consuming key_pair into self_signed.
    let key_pem = key_pair.serialize_pem();
    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| AppError::Tls(format!("rcgen self-sign failed: {e}")))?;
    let cert_pem = cert.pem();

    write_private_file(cert_path, cert_pem.as_bytes())?;
    write_private_file(key_path, key_pem.as_bytes())?;

    tracing::info!(
        "TLS: wrote self-signed certificate to {}",
        cert_path.display()
    );
    Ok(())
}

/// Builds the constrained certificate parameters used for development TLS.
fn build_cert_params() -> Result<rcgen::CertificateParams> {
    use rcgen::{
        CertificateParams, DistinguishedName, DnValue, ExtendedKeyUsagePurpose, KeyUsagePurpose,
    };
    use time::OffsetDateTime;

    let mut params = CertificateParams::default();

    // Subject
    let mut dn = DistinguishedName::new();
    dn.push(
        rcgen::DnType::CommonName,
        DnValue::Utf8String("RustHost Dev".into()),
    );
    dn.push(
        rcgen::DnType::OrganizationName,
        DnValue::Utf8String("RustHost".into()),
    );
    params.distinguished_name = dn;

    // Validity window: now → now + CERT_VALIDITY_DAYS
    let now = OffsetDateTime::now_utc();
    let expiry = now + time::Duration::days(i64::from(CERT_VALIDITY_DAYS));
    params.not_before = now;
    params.not_after = expiry;

    // Subject Alternative Names — required for modern browsers / TLS stacks
    for san in CERT_SANS {
        params.subject_alt_names.push(san_for(san)?);
    }

    // Mark as end-entity (not a CA)
    params.is_ca = rcgen::IsCa::NoCa;

    // === REQUIRED FOR MODERN CLIENTS ===
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];

    Ok(params)
}

/// Decide the correct [`SanType`] for a raw string: IPv4/6 literals become
/// `IpAddress`, everything else becomes `DnsName`.
fn san_for(s: &str) -> Result<rcgen::SanType> {
    if let Ok(address) = s.parse::<std::net::IpAddr>() {
        return Ok(rcgen::SanType::IpAddress(address));
    }

    let dns_name = s
        .to_owned()
        .try_into()
        .map_err(|error| AppError::Tls(format!("invalid self-signed DNS name {s:?}: {error}")))?;
    Ok(rcgen::SanType::DnsName(dns_name))
}

// ---------------------------------------------------------------------------
// Expiry check
// ---------------------------------------------------------------------------
/// Return `true` if the cert file is absent, unreadable, or will expire
/// within [`REGENERATE_BEFORE_DAYS`] days.
fn needs_regeneration(cert_path: &Path, key_path: &Path) -> bool {
    !key_path.exists()
        || remaining_validity_days(cert_path).is_none_or(|rem| rem < REGENERATE_BEFORE_DAYS)
}

/// Parse the `notAfter` field of a PEM certificate and return how many whole
/// days remain until expiry, or `None` on any failure.
fn remaining_validity_days(cert_path: &Path) -> Option<u64> {
    use x509_cert::der::Decode as _;
    let pem_bytes = std::fs::read(cert_path).ok()?;
    let (_, pem) = pem_rfc7468::decode_vec(&pem_bytes).ok()?;
    let cert = x509_cert::Certificate::from_der(&pem).ok()?;
    // `not_after` is stored as an ASN.1 Time; convert via Unix timestamp.
    let not_after = cert.tbs_certificate().validity().not_after.to_system_time();
    let remaining = not_after.duration_since(SystemTime::now()).ok()?;
    Some(remaining.as_secs() / 86_400)
}

// ---------------------------------------------------------------------------
// Secure file write
// ---------------------------------------------------------------------------
/// Write `contents` to `path`, creating or truncating the file, and set
/// restrictive permissions (Unix `0600`) so the private key is not world-
/// readable. On non-Unix platforms the write still succeeds but no
/// permission change is attempted.
fn write_private_file(path: &Path, contents: &[u8]) -> Result<()> {
    use std::fs::OpenOptions;
    use std::io::Write as _;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt as _;

    let mut file = OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .open(path)
        .map_err(|e| AppError::Tls(format!("cannot open {} for writing: {e}", path.display())))?;

    #[cfg(unix)]
    {
        file.set_permissions(std::fs::Permissions::from_mode(0o600))
            .map_err(|e| {
                AppError::Tls(format!(
                    "failed to set permissions on {}: {e}",
                    path.display()
                ))
            })?;
    }

    file.write_all(contents)
        .map_err(|e| AppError::Tls(format!("failed to write {}: {e}", path.display())))
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------
#[cfg(test)]
/// Tests for certificate generation, reuse, expiry, and subject names.
mod tests {
    use super::*;
    use anyhow::{Context as _, Result};
    use tempfile::TempDir;

    /// Install the `ring` crypto provider process-wide.
    /// Returns `Ok(())` the first time; returns `Err` (harmlessly) if
    /// another test in this process already installed it.
    fn ensure_crypto_provider() {
        drop(rustls::crypto::ring::default_provider().install_default());
    }

    /// Generates both certificate files on first use.
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions intentionally report violations of first-run certificate generation"
    )]
    fn generates_cert_on_first_call() -> Result<()> {
        ensure_crypto_provider();
        let tmp = TempDir::new().context("create temporary TLS directory")?;
        let cert = tmp.path().join("runtime/tls/dev/self-signed.crt");
        let key = tmp.path().join("runtime/tls/dev/self-signed.key");
        assert!(
            !cert.exists(),
            "certificate must not exist before generation"
        );
        generate_or_load(tmp.path()).context("generate self-signed certificate")?;
        assert!(cert.exists(), "cert file should exist after first call");
        assert!(key.exists(), "key file should exist after first call");
        Ok(())
    }

    /// Reuses an existing valid certificate without rewriting it.
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions intentionally report violations of certificate reuse"
    )]
    fn reuses_valid_cert_on_second_call() -> Result<()> {
        ensure_crypto_provider();
        let tmp = TempDir::new().context("create temporary TLS directory")?;
        generate_or_load(tmp.path()).context("generate self-signed certificate")?;
        let cert_path = tmp.path().join("runtime/tls/dev/self-signed.crt");
        let mtime_1 = std::fs::metadata(&cert_path)
            .context("read initial certificate metadata")?
            .modified()
            .context("read initial certificate modification time")?;
        // Small sleep to ensure mtime would differ if the file were rewritten.
        std::thread::sleep(std::time::Duration::from_millis(10));
        generate_or_load(tmp.path()).context("reload self-signed certificate")?;
        let mtime_2 = std::fs::metadata(&cert_path)
            .context("read reused certificate metadata")?
            .modified()
            .context("read reused certificate modification time")?;
        assert_eq!(mtime_1, mtime_2, "valid cert should not be regenerated");
        Ok(())
    }

    /// Anchors the generated certificate validity window to current UTC time.
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions intentionally report violations of certificate validity bounds"
    )]
    fn generated_cert_validity_uses_current_utc_window() -> Result<()> {
        use x509_cert::der::Decode as _;

        ensure_crypto_provider();
        let tmp = TempDir::new().context("create temporary TLS directory")?;
        let before = SystemTime::now();
        generate_or_load(tmp.path()).context("generate self-signed certificate")?;
        let after = SystemTime::now();

        let cert_path = tmp.path().join("runtime/tls/dev/self-signed.crt");
        let pem_bytes = std::fs::read(cert_path).context("read generated certificate")?;
        let (_, der) = pem_rfc7468::decode_vec(&pem_bytes).context("decode certificate PEM")?;
        let cert = x509_cert::Certificate::from_der(&der).context("decode certificate DER")?;
        let not_before = cert
            .tbs_certificate()
            .validity()
            .not_before
            .to_system_time();
        let not_after = cert.tbs_certificate().validity().not_after.to_system_time();

        assert!(
            not_before <= after,
            "certificate must not begin after generation completes"
        );
        assert!(
            not_after > before,
            "certificate must expire after generation begins"
        );
        let validity = not_after
            .duration_since(not_before)
            .context("certificate validity window must be positive")?;
        assert!(
            validity
                >= std::time::Duration::from_secs(
                    (u64::from(CERT_VALIDITY_DAYS) - 1) * 24 * 60 * 60
                ),
            "certificate validity must cover nearly the configured duration"
        );
        Ok(())
    }

    /// Treats an absent certificate as requiring regeneration.
    #[test]
    fn needs_regeneration_returns_true_for_missing_file() {
        assert!(
            needs_regeneration(
                Path::new("/nonexistent/path/cert.pem"),
                Path::new("/nonexistent/path/key.pem")
            ),
            "missing certificate material must require regeneration"
        );
    }

    /// Regenerates both files when the private key is missing.
    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "assertions intentionally report violations of missing-key recovery"
    )]
    fn missing_key_triggers_regeneration_on_next_startup() -> Result<()> {
        ensure_crypto_provider();
        let tmp = TempDir::new().context("create temporary TLS directory")?;
        generate_or_load(tmp.path()).context("generate self-signed certificate")?;

        let cert_path = tmp.path().join("runtime/tls/dev/self-signed.crt");
        let key_path = tmp.path().join("runtime/tls/dev/self-signed.key");
        let original_cert = std::fs::read(&cert_path).context("read original certificate")?;
        std::fs::remove_file(&key_path).context("remove private key")?;

        generate_or_load(tmp.path()).context("regenerate missing key")?;

        let regenerated_cert = std::fs::read(&cert_path).context("read regenerated certificate")?;
        assert!(key_path.exists(), "missing key should be regenerated");
        assert_ne!(
            original_cert, regenerated_cert,
            "cert should be rewritten when the key file is missing"
        );
        Ok(())
    }

    /// Parses an IPv4 subject alternative name as an IP address.
    #[test]
    fn san_for_parses_ipv4() {
        let san = san_for("127.0.0.1");
        assert!(
            matches!(san, Ok(rcgen::SanType::IpAddress(_))),
            "IPv4 SAN must be represented as an IP address"
        );
    }

    /// Parses a hostname subject alternative name as DNS.
    #[test]
    fn san_for_parses_dns() {
        let san = san_for("localhost");
        assert!(
            matches!(san, Ok(rcgen::SanType::DnsName(_))),
            "hostname SAN must be represented as a DNS name"
        );
    }
}
