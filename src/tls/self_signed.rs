//! `src/tls/self_signed.rs`
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
/// If a valid, non-expiring cert already exists in `<data_dir>/tls/dev/` it
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
pub fn generate_or_load(data_dir: &Path) -> Result<(Arc<TlsAcceptor>, Arc<ServerConfig>)> {
    let dir = data_dir.join("tls/dev");
    let cert_path = dir.join("self-signed.crt");
    let key_path = dir.join("self-signed.key");
    std::fs::create_dir_all(&dir).map_err(|e| {
        AppError::Tls(format!(
            "failed to create TLS dev directory {}: {e}",
            dir.display()
        ))
    })?;

    if needs_regeneration(&cert_path) {
        tracing::info!("TLS: generating self-signed certificate for {CERT_SANS:?}");
        write_self_signed_cert(&cert_path, &key_path)?;
    } else {
        tracing::debug!(
            "TLS: reusing existing self-signed certificate at {}",
            cert_path.display()
        );
    }
    super::load_pem_as_acceptor(&cert_path, &key_path)
}

// ---------------------------------------------------------------------------
// Generation
// ---------------------------------------------------------------------------
fn write_self_signed_cert(cert_path: &Path, key_path: &Path) -> Result<()> {
    let params = build_cert_params();
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

#[allow(clippy::arithmetic_side_effects)]
fn build_cert_params() -> rcgen::CertificateParams {
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
        params.subject_alt_names.push(san_for(san));
    }

    // Mark as end-entity (not a CA)
    params.is_ca = rcgen::IsCa::NoCa;

    // === REQUIRED FOR MODERN CLIENTS ===
    params.key_usages = vec![
        KeyUsagePurpose::DigitalSignature,
        KeyUsagePurpose::KeyEncipherment,
    ];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ServerAuth];

    params
}

/// Decide the correct [`SanType`] for a raw string: IPv4/6 literals become
/// `IpAddress`, everything else becomes `DnsName`.
#[allow(clippy::unwrap_used)]
fn san_for(s: &str) -> rcgen::SanType {
    s.parse::<std::net::IpAddr>().map_or_else(
        |_| rcgen::SanType::DnsName(s.to_owned().try_into().unwrap()),
        rcgen::SanType::IpAddress,
    )
}

// ---------------------------------------------------------------------------
// Expiry check
// ---------------------------------------------------------------------------
/// Return `true` if the cert file is absent, unreadable, or will expire
/// within [`REGENERATE_BEFORE_DAYS`] days.
fn needs_regeneration(cert_path: &Path) -> bool {
    remaining_validity_days(cert_path).is_none_or(|rem| rem < REGENERATE_BEFORE_DAYS)
}

/// Parse the `notAfter` field of a PEM certificate and return how many whole
/// days remain until expiry, or `None` on any failure.
fn remaining_validity_days(cert_path: &Path) -> Option<u64> {
    use x509_cert::der::Decode;
    let pem_bytes = std::fs::read(cert_path).ok()?;
    // Extract the first certificate from the PEM bundle.
    let (_, pem) = pem_rfc7468::decode_vec(&pem_bytes).ok()?;
    // Parse the DER-encoded certificate to reach the validity fields.
    let cert = x509_cert::Certificate::from_der(&pem).ok()?;
    // `not_after` is stored as an ASN.1 Time; convert via Unix timestamp.
    let not_after: SystemTime = cert.tbs_certificate.validity.not_after.to_system_time();
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
    use std::io::Write;
    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

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
mod tests {
    #![allow(clippy::unwrap_used)]
    #![allow(clippy::expect_used)]
    use super::*;
    use tempfile::TempDir;

    /// Install the `ring` crypto provider process-wide.
    /// Returns `Ok(())` the first time; returns `Err` (harmlessly) if
    /// another test in this process already installed it.
    fn ensure_crypto_provider() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }

    #[test]
    fn generates_cert_on_first_call() {
        ensure_crypto_provider();
        let tmp = TempDir::new().unwrap();
        let cert = tmp.path().join("tls/dev/self-signed.crt");
        let key = tmp.path().join("tls/dev/self-signed.key");
        assert!(!cert.exists());
        generate_or_load(tmp.path()).expect("generate_or_load failed");
        assert!(cert.exists(), "cert file should exist after first call");
        assert!(key.exists(), "key file should exist after first call");
    }

    #[test]
    fn reuses_valid_cert_on_second_call() {
        ensure_crypto_provider();
        let tmp = TempDir::new().unwrap();
        generate_or_load(tmp.path()).unwrap();
        let cert_path = tmp.path().join("tls/dev/self-signed.crt");
        let mtime_1 = std::fs::metadata(&cert_path).unwrap().modified().unwrap();
        // Small sleep to ensure mtime would differ if the file were rewritten.
        std::thread::sleep(std::time::Duration::from_millis(10));
        generate_or_load(tmp.path()).unwrap();
        let mtime_2 = std::fs::metadata(&cert_path).unwrap().modified().unwrap();
        assert_eq!(mtime_1, mtime_2, "valid cert should not be regenerated");
    }

    #[test]
    fn needs_regeneration_returns_true_for_missing_file() {
        assert!(needs_regeneration(Path::new("/nonexistent/path/cert.pem")));
    }

    #[test]
    fn san_for_parses_ipv4() {
        let san = san_for("127.0.0.1");
        assert!(matches!(san, rcgen::SanType::IpAddress(_)));
    }

    #[test]
    fn san_for_parses_dns() {
        let san = san_for("localhost");
        assert!(matches!(san, rcgen::SanType::DnsName(_)));
    }
}
