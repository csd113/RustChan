//! Certificate management: validate local material before staging HTTPS changes.

use super::{BTreeMap, Config, Environment, InputKind, SettingDefinition};
use anyhow::{ensure, Context as _};
use std::{fmt::Write as _, path::Path, sync::Arc};

/// All twelve inventoried TLS leaves, using their authoritative TOML paths.
pub static SETTINGS: &[SettingDefinition] = &[
    SettingDefinition { application: crate::config::admin::ApplicationMode::Restart, key: "tls.enabled", label: "Native HTTPS listener", environment: "", kind: InputKind::Boolean, value: |c| c.tls.enabled.to_string(), help: "Enable built-in HTTPS after choosing a certificate source below. Reverse-proxy HTTPS does not require this listener." },
    SettingDefinition { application: crate::config::admin::ApplicationMode::Restart, key: "tls.require_https", label: "Require native HTTPS", environment: "", kind: InputKind::Boolean, value: |c| c.tls.require_https.to_string(), help: "Disables public plaintext application access when native HTTPS is enabled. Verify the HTTPS listener is reachable before restarting." },
    SettingDefinition { application: crate::config::admin::ApplicationMode::Restart, key: "tls.port", label: "HTTPS port", environment: "", kind: InputKind::Number(1, 65535), value: |c| c.tls.port.to_string(), help: "Must differ from the primary HTTP and redirect ports. Your service launcher and firewall must allow this port." },
    SettingDefinition { application: crate::config::admin::ApplicationMode::Restart, key: "tls.redirect_http", label: "HTTP redirect listener", environment: "", kind: InputKind::Boolean, value: |c| c.tls.redirect_http.to_string(), help: "Adds a separate listener that redirects to HTTPS; requires a configured public hostname." },
    SettingDefinition { application: crate::config::admin::ApplicationMode::Restart, key: "tls.http_port", label: "HTTP redirect port", environment: "", kind: InputKind::Number(1, 65535), value: |c| c.tls.http_port.to_string(), help: "Separate from the primary application listener. Default: 8080." },
    SettingDefinition { application: crate::config::admin::ApplicationMode::Restart, key: "tls.acme.enabled", label: "Automatic ACME certificates", environment: "", kind: InputKind::Boolean, value: |c| c.tls.acme.enabled.to_string(), help: "Requires a tls-acme build, public DNS and reachable HTTPS for TLS-ALPN-01 validation. Issuance happens after restart; use staging first." },
    SettingDefinition { application: crate::config::admin::ApplicationMode::Restart, key: "tls.acme.domains", label: "ACME domain names", environment: "", kind: InputKind::List, value: |c| c.tls.acme.domains.join(", "), help: "DNS names only, one per line or separated by commas. These must resolve to your public HTTPS listener." },
    SettingDefinition { application: crate::config::admin::ApplicationMode::Restart, key: "tls.acme.email", label: "ACME contact email (optional)", environment: "", kind: InputKind::OptionalText, value: |c| c.tls.acme.email.clone().unwrap_or_default(), help: "Optional account contact. Leave blank to omit. No ACME request is sent by this form." },
    SettingDefinition { application: crate::config::admin::ApplicationMode::Restart, key: "tls.acme.staging", label: "ACME staging directory", environment: "", kind: InputKind::Boolean, value: |c| c.tls.acme.staging.to_string(), help: "Staging certificates are for testing and are not trusted by browsers. Preserve your current selection until public issuance is ready." },
    SettingDefinition { application: crate::config::admin::ApplicationMode::Restart, key: "tls.acme.cache_dir", label: "ACME private cache directory", environment: "", kind: InputKind::OptionalText, value: |c| c.tls.acme.cache_dir.clone(), help: "Absolute path or path relative to the data directory. Leave blank for the loader default. Contains account credentials; restrict service-user access." },
    SettingDefinition { application: crate::config::admin::ApplicationMode::Restart, key: "tls.manual_cert.cert_path", label: "Manual PEM certificate-chain path", environment: "", kind: InputKind::OptionalText, value: |c| c.tls.manual_cert.as_ref().map_or_else(String::new, |m| m.cert_path.clone()), help: "Install the PEM files on the service host first. Absolute path or relative to the active data directory; chain and key are checked before saving. No file contents are displayed." },
    SettingDefinition { application: crate::config::admin::ApplicationMode::Restart, key: "tls.manual_cert.key_path", label: "Manual PEM private-key path", environment: "", kind: InputKind::OptionalText, value: |c| c.tls.manual_cert.as_ref().map_or_else(String::new, |m| m.key_path.clone()), help: "Select both certificate and key, or leave both blank to remove the manual source. Manual certificates take priority over ACME. Renew files offline, then restart to reload." },
];

/// Save one complete certificate workflow, validating saved and effective listeners.
///
/// # Errors
/// Rejects invalid configuration, unavailable build features and unusable PEM files.
pub fn save(form: &BTreeMap<String, String>) -> anyhow::Result<()> {
    let mut settings_form = form.clone();
    let confirmed = settings_form
        .remove("confirm_https_access")
        .is_some_and(|value| value == "1");
    confirm_cutover(&settings_form, confirmed, &super::CONFIG)?;
    let updates = super::parse_settings_form(SETTINGS, &settings_form)?;
    ensure!(
        updates
            .get("tls.manual_cert.cert_path")
            .is_some_and(Option::is_some)
            == updates
                .get("tls.manual_cert.key_path")
                .is_some_and(Option::is_some),
        "Choose both manual certificate and private-key paths, or leave both blank"
    );
    let _guard = super::SETTINGS_WRITE_LOCK.lock();
    super::save_root_at(
        &super::super::settings_file_path(),
        &updates,
        &Environment::Process,
        validate,
    )
}

/// Require a verified, already running TLS port before disabling plaintext access.
fn confirm_cutover(
    form: &BTreeMap<String, String>,
    confirmed: bool,
    active: &Config,
) -> anyhow::Result<()> {
    if form.get("tls.require_https").map(String::as_str) == Some("true")
        && !active.tls.require_https
    {
        ensure!(confirmed, "Confirm that you have verified native HTTPS access before removing plaintext application access");
        ensure!(active.tls.enabled && form.get("tls.port").is_some_and(|port| port == &active.tls.port.to_string()), "First enable and restart native HTTPS, verify access, then require HTTPS without changing its port");
    }
    Ok(())
}

/// Validate certificate-source selection without creating files or contacting ACME.
pub(super) fn validate(config: &Config) -> anyhow::Result<()> {
    super::validate_network(config)?;
    let tls = &config.tls;
    ensure!(
        !tls.require_https || tls.enabled,
        "Require HTTPS needs the native HTTPS listener"
    );
    ensure!(
        !tls.redirect_http || (tls.enabled && !config.public_hosts.is_empty()),
        "HTTP redirects require native HTTPS and a public hostname"
    );
    if let Some(email) = &tls.acme.email {
        ensure!(
            email.len() <= 254
                && email.split_once('@').is_some_and(
                    |(local, host)| !local.is_empty() && super::valid_public_host(host)
                )
                && !email.chars().any(char::is_whitespace),
            "invalid ACME contact email"
        );
    }
    for domain in &tls.acme.domains {
        ensure!(
            super::valid_public_host(domain)
                && domain.parse::<std::net::IpAddr>().is_err()
                && domain.contains('.'),
            "ACME requires a public DNS name"
        );
    }
    if !tls.acme.cache_dir.is_empty() {
        validate_path(&tls.acme.cache_dir)?;
    }
    if let Some(manual) = &tls.manual_cert {
        validate_path(&manual.cert_path)?;
        validate_path(&manual.key_path)?;
        validate_pair(
            &super::super::data_dir().join(&manual.cert_path),
            &super::super::data_dir().join(&manual.key_path),
        )?;
    } else if tls.enabled && tls.acme.enabled {
        ensure!(
            cfg!(feature = "tls-acme"),
            "this build does not support ACME"
        );
        ensure!(
            !tls.acme.domains.is_empty(),
            "ACME requires at least one domain"
        );
        ensure!(!config.tor_only, "ACME is unavailable in Tor-only mode");
        ensure!(
            tls.acme.domains.iter().all(|domain| config
                .public_hosts
                .iter()
                .any(|host| host.eq_ignore_ascii_case(domain))),
            "every ACME domain must be configured in Network public hostnames"
        );
    } else if tls.enabled {
        ensure!(
            cfg!(feature = "tls-self-signed"),
            "this build needs a manual certificate or ACME source"
        );
    }
    Ok(())
}

/// Accept local absolute or data-relative paths, rejecting traversal before reads.
fn validate_path(path: &str) -> anyhow::Result<()> {
    ensure!(
        !path.is_empty() && path.len() <= 4096 && !path.chars().any(char::is_control),
        "certificate/cache path must be nonempty and contain no control characters"
    );
    ensure!(
        !Path::new(path)
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir)),
        "certificate/cache paths must not contain parent traversal"
    );
    Ok(())
}

/// Bound material reads, parse PEM, and ask rustls to verify key compatibility.
fn validate_pair(cert_path: &Path, key_path: &Path) -> anyhow::Result<()> {
    use rustls_pki_types::{pem::PemObject as _, CertificateDer, PrivateKeyDer};
    let cert = read_material(cert_path)?;
    let key = read_material(key_path)?;
    let certificates = CertificateDer::pem_reader_iter(&mut cert.as_slice())
        .collect::<Result<Vec<_>, _>>()
        .context("invalid certificate PEM")?;
    ensure!(!certificates.is_empty(), "certificate chain is empty");
    let key =
        PrivateKeyDer::from_pem_reader(&mut key.as_slice()).context("invalid private-key PEM")?;
    rustls::ServerConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .context("TLS protocol configuration")?
        .with_no_client_auth()
        .with_single_cert(certificates, key)
        .context("certificate and key do not form a usable pair")?;
    Ok(())
}

/// Read an already-open regular file with a strict allocation limit.
fn read_material(path: &Path) -> anyhow::Result<Vec<u8>> {
    use std::io::Read as _;
    let file = std::fs::File::open(path).context("cannot open certificate/key file")?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file() && metadata.len() <= 1_048_576,
        "certificate/key must be a regular file no larger than 1 MiB"
    );
    let mut bytes = Vec::new();
    file.take(1_048_577).read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() <= 1_048_576,
        "certificate/key grew beyond 1 MiB"
    );
    Ok(bytes)
}

/// Rewrite nested TLS tables one level at a time, verifying the full semantic result.
pub(super) fn rewrite_tls(
    content: &str,
    updates: &BTreeMap<String, Option<toml::Value>>,
) -> anyhow::Result<String> {
    let mut output = content.to_owned();
    let mut groups: BTreeMap<&str, BTreeMap<String, Option<toml::Value>>> = BTreeMap::new();
    for (path, value) in updates {
        let (table, key) = path
            .rsplit_once('.')
            .context("TLS needs dotted setting paths")?;
        ensure!(
            matches!(table, "tls" | "tls.acme" | "tls.manual_cert"),
            "unknown TLS table"
        );
        groups
            .entry(table)
            .or_default()
            .insert(key.to_owned(), value.clone());
    }
    for (table, fields) in groups {
        output = rewrite_table(&output, table, &fields)?;
    }
    Ok(output)
}

/// Locate real headers by parsing them; ignore header-like lines in scalar values.
fn table_range(content: &str, table: &str) -> anyhow::Result<Option<std::ops::Range<usize>>> {
    let spans: BTreeMap<String, toml::Spanned<toml::Value>> = toml::from_str(content)?;
    let mut offset = 0;
    let mut found = None;
    for line in content.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        if !line.trim_start().starts_with('[')
            || spans
                .values()
                .any(|s| !s.get_ref().is_table() && s.span().contains(&start))
        {
            continue;
        }
        let marker = format!("{line}\n__admin_header_marker = true");
        let parsed: toml::Value = toml::from_str(&marker)
            .context("unsupported TLS table header; edit settings.toml offline")?;
        let matching = super::file_value(&parsed, table)
            .is_some_and(|v| v.get("__admin_header_marker").is_some());
        if matching {
            found = Some(start..offset);
        } else if let Some(range) = &mut found {
            range.end = start;
            return Ok(found);
        }
    }
    if let Some(range) = &mut found {
        range.end = content.len();
    }
    Ok(found)
}

/// Reuse the exact root scalar writer within one table body; fail closed on exotic TOML.
fn rewrite_table(
    content: &str,
    table: &str,
    updates: &BTreeMap<String, Option<toml::Value>>,
) -> anyhow::Result<String> {
    let before: toml::Value = toml::from_str(content)?;
    let mut expected = before.clone();
    let mut target = expected
        .as_table_mut()
        .context("settings root must be a table")?;
    for part in table.split('.') {
        target = target
            .entry(part.to_owned())
            .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
            .as_table_mut()
            .context("TLS settings must use tables")?;
    }
    for (key, value) in updates {
        if let Some(value) = value {
            target.insert(key.clone(), value.clone());
        } else {
            target.remove(key);
        }
    }
    let remove_manual = table == "tls.manual_cert" && target.is_empty();
    if remove_manual {
        expected
            .get_mut("tls")
            .and_then(toml::Value::as_table_mut)
            .context("missing TLS")?
            .remove("manual_cert");
    }
    let mut output = content.to_owned();
    if let Some(range) = table_range(content, table)? {
        let section = content.get(range.clone()).context("invalid TLS range")?;
        let header_end = section.find('\n').map_or(section.len(), |p| p + 1);
        let body = section.get(header_end..).context("invalid TLS header")?;
        // Parsing the isolated body ensures no nested/inline table gets silently overwritten.
        let parsed: BTreeMap<String, toml::Spanned<toml::Value>> =
            toml::from_str(body).context("unsupported TLS table layout; edit offline")?;
        ensure!(
            parsed.values().all(|v| !v.get_ref().is_table()),
            "use explicit TLS table headers to preserve nested configuration"
        );
        let rewritten = super::rewrite_root_settings(body, updates)?;
        let original_header = section
            .get(..header_end)
            .context("invalid UTF-8 TLS header")?;
        let header = if remove_manual {
            format!("# {original_header}")
        } else {
            original_header.to_owned()
        };
        output.replace_range(range, &format!("{header}{rewritten}"));
    } else if !remove_manual {
        ensure!(
            super::file_value(&before, table).is_none(),
            "inline or dotted TLS tables require offline editing"
        );
        write!(output, "\n[{table}]\n")?;
        for (key, value) in updates {
            if let Some(value) = value {
                writeln!(output, "{key} = {value}")?;
            }
        }
    }
    let actual: toml::Value =
        toml::from_str(&output).context("TLS rewrite failed; no changes saved")?;
    ensure!(
        actual == expected,
        "TLS rewrite changed unrelated settings; no changes saved"
    );
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn form(content: &str) -> anyhow::Result<BTreeMap<String, String>> {
        let config = super::super::resolve_file(content, &Environment::Values(&BTreeMap::new()))?;
        Ok(SETTINGS
            .iter()
            .map(|f| (f.key.to_owned(), (f.value)(&config)))
            .collect())
    }

    #[test]
    fn plaintext_cutover_requires_confirmation_and_an_existing_tls_listener() -> anyhow::Result<()>
    {
        let empty = BTreeMap::new();
        let environment = Environment::Values(&empty);
        let mut active = super::super::resolve_file("enable_tor_support = false\n", &environment)?;
        let fields = BTreeMap::from([
            ("tls.require_https".to_owned(), "true".to_owned()),
            ("tls.port".to_owned(), "8443".to_owned()),
        ]);
        ensure!(
            confirm_cutover(&fields, false, &active).is_err(),
            "cutover requires explicit confirmation"
        );
        ensure!(
            confirm_cutover(&fields, true, &active).is_err(),
            "confirmation cannot substitute for a started listener"
        );
        active.tls.enabled = true;
        active.tls.port = 8443;
        confirm_cutover(&fields, true, &active)?;
        active.tls.port = 9443;
        ensure!(
            confirm_cutover(&fields, true, &active).is_err(),
            "cutover must not move the verified port"
        );
        Ok(())
    }

    #[test]
    fn tls_roundtrip_preserves_comments_and_clears_optional_sources() -> anyhow::Result<()> {
        let before = "# keep root\nport = 3000\n[tls] # listener\nenabled = false # keep inline\n[tls.acme]\nstaging = true # test CA\ndomains = [\n 'example.test', # keep note\n]\n[unrelated]\nvalue = 42\n";
        let mut form = form(before)?;
        form.insert("tls.port".into(), "9443".into());
        let updates = super::super::parse_settings_form(SETTINGS, &form)?;
        let after = rewrite_tls(before, &updates)?;
        for comment in [
            "# keep root",
            "# listener",
            "# keep inline",
            "# test CA",
            "value = 42",
        ] {
            ensure!(after.contains(comment), "lost unrelated comment/value");
        }
        let config = super::super::resolve_file(&after, &Environment::Values(&BTreeMap::new()))?;
        ensure!(
            config.tls.port == 9443 && config.tls.acme.staging,
            "actual loader must read TLS edits"
        );
        ensure!(
            config.tls.manual_cert.is_none(),
            "blank manual source must stay absent"
        );
        validate(&config)?;
        Ok(())
    }

    #[test]
    fn invalid_tls_changes_preserve_the_entire_file() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("settings.toml");
        let before = "enable_tor_support = false\n[tls]\nenabled = false\n";
        std::fs::write(&path, before)?;
        for (key, value) in [
            ("tls.require_https", "true"),
            ("tls.manual_cert.cert_path", "missing.pem"),
            ("tls.acme.email", "invalid"),
            ("tls.acme.cache_dir", "../outside"),
        ] {
            let mut form = form(before)?;
            form.insert(key.into(), value.into());
            let updates = super::super::parse_settings_form(SETTINGS, &form)?;
            ensure!(
                super::super::save_root_at(
                    &path,
                    &updates,
                    &Environment::Values(&BTreeMap::new()),
                    validate
                )
                .is_err(),
                "invalid TLS must fail"
            );
            ensure!(
                std::fs::read_to_string(&path)? == before,
                "failed TLS change must preserve bytes"
            );
        }
        Ok(())
    }

    #[test]
    fn malformed_or_oversized_material_is_rejected() -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let cert = dir.path().join("cert.pem");
        let key = dir.path().join("key.pem");
        std::fs::write(&cert, "invalid certificate")?;
        std::fs::write(&key, "private fixture")?;
        ensure!(
            validate_pair(&cert, &key).is_err(),
            "invalid material must fail"
        );
        std::fs::write(&cert, vec![b'x'; 1_048_577])?;
        ensure!(
            read_material(&cert).is_err(),
            "size limit must fail before parsing"
        );
        Ok(())
    }
    #[cfg(feature = "tls-self-signed")]
    #[test]
    fn manual_certificate_preflight_accepts_matching_keys_and_rejects_mismatch(
    ) -> anyhow::Result<()> {
        let dir = tempfile::tempdir()?;
        let key = rcgen::KeyPair::generate()?;
        let cert =
            rcgen::CertificateParams::new(vec!["localhost".to_owned()])?.self_signed(&key)?;
        let cert_path = dir.path().join("cert.pem");
        let key_path = dir.path().join("key.pem");
        std::fs::write(&cert_path, cert.pem())?;
        std::fs::write(&key_path, key.serialize_pem())?;
        validate_pair(&cert_path, &key_path)?;
        std::fs::write(&key_path, rcgen::KeyPair::generate()?.serialize_pem())?;
        ensure!(
            validate_pair(&cert_path, &key_path).is_err(),
            "a mismatched key must not be staged"
        );
        Ok(())
    }
}
