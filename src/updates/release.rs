use anyhow::Context as _;
use ring::signature::{UnparsedPublicKey, ED25519};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::io::Read as _;
use std::time::{Duration, Instant};

/// Fixed official `GitHub` Releases API source.
const API: &str = "https://api.github.com/repos/csd113/RustChan/releases?per_page=100";
/// Fixed official release-download prefix.
const DOWNLOAD: &str = "https://github.com/csd113/RustChan/releases/download/";
/// Maximum compressed and executable artifact size.
pub(super) const MAX_ARTIFACT: u64 = 256 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Signed identity, integrity and migration constraints for one native artifact.
pub struct Manifest {
    /// Exact signed manifest format version.
    pub format: u16,
    /// Stable semantic application version.
    pub version: String,
    /// GitHub release identity bound by the signature.
    pub release_id: u64,
    /// Exact supported compilation target triple.
    pub target: String,
    /// Fixed expected artifact basename.
    pub filename: String,
    /// Expected verified byte length.
    pub size: u64,
    /// Lowercase SHA-256 digest of the exact artifact bytes.
    pub sha256: String,
    /// Digest of the executable inside the archive.
    pub executable_sha256: String,
    /// Expected uncompressed executable byte length.
    pub executable_size: u64,
    /// Expected release schema version after startup.
    pub schema: String,
    /// Oldest schema accepted by this release migration.
    pub minimum_schema: String,
    /// Minimum compatible updater semantic version.
    pub minimum_updater: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
/// Administrator-visible bounded metadata for an official stable release.
pub struct Release {
    /// Opaque snapshot or official release identifier.
    pub id: u64,
    /// Stable semantic application version.
    pub version: String,
    /// Validated release publication timestamp.
    pub published_at: String,
    /// Bounded untrusted release notes, escaped before rendering.
    pub notes: String,
    /// Signature-verified manifest, absent for unusable releases.
    pub manifest: Option<Manifest>,
    /// Expected verified byte length.
    pub size: Option<u64>,
    /// Operator-readable verification outcome.
    pub verification: String,
    /// Whether verification and target/migration checks permit installation.
    pub compatible: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
/// Safe outcome of a stable release check.
pub enum Discovery {
    /// No newer stable release exists.
    UpToDate,
    /// Newer stable release metadata; compatibility still controls installation.
    Available(Box<Release>),
    /// Discovery failed safely without affecting application operation.
    UnableToCheck(String),
}

#[derive(Debug, Deserialize)]
/// Untrusted `GitHub` API release response.
struct GithubRelease {
    /// Official release identifier.
    id: u64,
    /// Untrusted release tag parsed as a stable semantic version.
    tag_name: String,
    /// Exclude unpublished draft releases.
    draft: bool,
    /// Exclude prerelease-channel releases.
    prerelease: bool,
    /// Publication timestamp, absent for unpublished releases.
    published_at: Option<String>,
    /// Untrusted release notes bounded before rendering.
    body: Option<String>,
    /// Release assets requiring unique exact identities.
    assets: Vec<Asset>,
}
#[derive(Debug, Deserialize)]
/// Untrusted `GitHub` API release asset response.
struct Asset {
    /// Untrusted asset basename.
    name: String,
    /// Declared byte length checked against the signed manifest.
    size: u64,
    /// Must equal the constructed official release asset URL.
    browser_download_url: String,
}

#[must_use]
/// Return the official target triple supported for this operating system and CPU.
pub fn platform_target() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") if cfg!(target_env = "gnu") => Some("x86_64-unknown-linux-gnu"),
        ("linux", "aarch64") if cfg!(target_env = "gnu") => Some("aarch64-unknown-linux-gnu"),
        ("macos", "aarch64") => Some("aarch64-apple-darwin"),
        ("windows", "x86_64") if cfg!(target_env = "msvc") => Some("x86_64-pc-windows-msvc"),
        _ => None,
    }
}

/// Parse a stable canonical semantic release version without build metadata.
pub(super) fn stable_version(text: &str) -> anyhow::Result<Version> {
    let version = Version::parse(text.strip_prefix('v').unwrap_or(text))?;
    anyhow::ensure!(
        version.pre.is_empty() && version.build.is_empty(),
        "release version must be stable without build metadata"
    );
    Ok(version)
}

/// Construct a fixed official URL after validating version and basename.
pub(super) fn asset_url(version: &str, name: &str) -> anyhow::Result<String> {
    let version = stable_version(version)?;
    anyhow::ensure!(
        name.len() <= 160
            && !name.is_empty()
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-._".contains(&byte)),
        "invalid release asset name"
    );
    Ok(format!("{DOWNLOAD}v{version}/{name}"))
}

/// Create a bounded TLS client with automatic redirects disabled.
fn agent() -> anyhow::Result<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .user_agent("RustChan-updater/1")
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .connect_timeout(Duration::from_secs(5))
        .build()
        .map_err(Into::into)
}

// GitHub redirects release downloads to its HTTPS release asset CDN. Follow
// each hop ourselves so the HTTP client can never contact an arbitrary redirect target.
/// Fetch bounded artifact bytes only from allowlisted HTTPS release sources.
pub(super) fn fetch(url: &str, limit: u64) -> anyhow::Result<Vec<u8>> {
    fetch_before(url, limit, Instant::now() + Duration::from_secs(30))
}

/// Follow a bounded allowlisted redirect chain within one absolute deadline.
fn fetch_before(url: &str, limit: u64, deadline: Instant) -> anyhow::Result<Vec<u8>> {
    let mut url = url.to_owned();
    let client = agent()?;
    for _ in 0..4 {
        validate_source(&url)?;
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .context("release request timed out")?;
        let mut response = client
            .get(&url)
            .timeout(remaining)
            .header("Accept", "application/vnd.github+json")
            .send()?;
        if response.status().is_redirection() {
            response
                .headers()
                .get("location")
                .context("release redirect has no location")?
                .to_str()?
                .clone_into(&mut url);
            continue;
        }
        anyhow::ensure!(
            response.status().is_success(),
            "GitHub release request failed"
        );
        let mut bytes = Vec::new();
        response.by_ref().take(limit + 1).read_to_end(&mut bytes)?;
        anyhow::ensure!(
            u64::try_from(bytes.len())? <= limit,
            "release response exceeds size limit"
        );
        return Ok(bytes);
    }
    anyhow::bail!("too many release download redirects")
}

/// Reject arbitrary hosts, ports and paths before any outbound request.
pub(super) fn validate_source(url: &str) -> anyhow::Result<()> {
    // Parse with the HTTP library rather than prefix-checking a hostname.
    let uri: axum::http::Uri = url.parse()?;
    anyhow::ensure!(
        uri.scheme_str() == Some("https")
            && uri.port_u16().is_none()
            && uri
                .authority()
                .is_some_and(|authority| !authority.as_str().contains('@')),
        "release source must use HTTPS"
    );
    let permitted = match uri.host() {
        Some("api.github.com") => uri.path() == "/repos/csd113/RustChan/releases",
        Some("github.com") => uri
            .path()
            .starts_with("/csd113/RustChan/releases/download/v"),
        Some("release-assets.githubusercontent.com") => {
            uri.path().starts_with("/github-production-release-asset/")
        }
        _ => false,
    };
    anyhow::ensure!(permitted, "release source is not allowlisted");
    Ok(())
}

/// Compute the lowercase SHA-256 digest of exact bytes.
pub(super) fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Verify `Ed25519` before parsing strict manifest identity and compatibility.
pub(super) fn verify_manifest(
    bytes: &[u8],
    signature: &[u8],
    key: &[u8],
    version: &str,
    release_id: u64,
    target: &str,
) -> anyhow::Result<Manifest> {
    UnparsedPublicKey::new(&ED25519, key)
        .verify(bytes, signature)
        .map_err(|_| anyhow::anyhow!("release signature failed verification"))?;
    let manifest: Manifest = serde_json::from_slice(bytes)?;
    anyhow::ensure!(
        manifest.format == 1
            && manifest.version == stable_version(version)?.to_string()
            && manifest.release_id == release_id
            && manifest.target == target,
        "release manifest identity is incompatible"
    );
    anyhow::ensure!(
        stable_version(&manifest.minimum_updater)? <= stable_version(super::VERSION)?,
        "release requires a newer updater"
    );
    anyhow::ensure!(
        manifest.size > 0
            && manifest.size <= MAX_ARTIFACT
            && manifest.executable_size > 0
            && manifest.executable_size <= MAX_ARTIFACT
            && stable_version(&manifest.minimum_schema)? <= stable_version(&manifest.schema)?
            && manifest.schema == manifest.version,
        "invalid release manifest limits"
    );
    for hash in [&manifest.sha256, &manifest.executable_sha256] {
        anyhow::ensure!(
            hash.len() == 64
                && hash
                    .bytes()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
            "invalid release checksum"
        );
    }
    anyhow::ensure!(
        manifest.filename == format!("rustchan-update-{target}.tar.gz"),
        "unexpected release artifact name"
    );
    Ok(manifest)
}

/// Discover stable releases safely without changing the running application.
#[must_use]
pub fn discover(current: &str, key: Option<&[u8]>) -> Discovery {
    discover_for_schema(current, key, None)
}
/// Check stable releases against the installed semantic schema range.
pub(super) fn discover_for_schema(
    current: &str,
    key: Option<&[u8]>,
    schema: Option<&str>,
) -> Discovery {
    match discover_inner(current, key, schema) {
        Ok(result) => result,
        Err(error) => Discovery::UnableToCheck(format!("Unable to check releases: {error}")),
    }
}
/// Compare schema transition bounds semantically.
pub(super) fn schema_compatible(schema: &str, manifest: &Manifest) -> anyhow::Result<bool> {
    let schema = stable_version(schema)?;
    Ok(schema >= stable_version(&manifest.minimum_schema)?
        && schema <= stable_version(&manifest.schema)?)
}

/// Exclude unstable/unpublished tags and order newer releases semantically.
fn candidates(bytes: &[u8], current: &str) -> anyhow::Result<Vec<GithubRelease>> {
    let current = stable_version(current)?;
    let releases: Vec<GithubRelease> = serde_json::from_slice(bytes)?;
    let mut releases: Vec<_> = releases
        .into_iter()
        .filter(|r| !r.draft && !r.prerelease && r.published_at.is_some())
        .filter_map(|r| stable_version(&r.tag_name).ok().map(|v| (v, r)))
        .filter(|(v, _)| *v > current)
        .collect();
    releases.sort_by(|(a, _), (b, _)| b.cmp(a));
    Ok(releases.into_iter().map(|(_, r)| r).collect())
}
#[cfg(test)]
fn latest(bytes: &[u8], current: &str) -> anyhow::Result<Option<GithubRelease>> {
    Ok(candidates(bytes, current)?.into_iter().next())
}
/// Inspect bounded official release candidates without altering the deployment.
fn discover_inner(
    current: &str,
    key: Option<&[u8]>,
    schema: Option<&str>,
) -> anyhow::Result<Discovery> {
    let deadline = Instant::now() + Duration::from_secs(30);
    let releases = candidates(&fetch_before(API, 2 * 1024 * 1024, deadline)?, current)?;
    let mut latest_unusable = None;
    for release in releases {
        let mut result = inspect_release(release, key, deadline)?;
        if result
            .manifest
            .as_ref()
            .is_some_and(|m| schema.is_some_and(|s| !schema_compatible(s, m).unwrap_or(false)))
        {
            result.compatible = false;
            result.manifest = None;
            result.verification = "Incompatible release: the installed database schema is outside its migration range.".into();
        }
        if result.compatible || key.is_none() {
            return Ok(Discovery::Available(Box::new(result)));
        }
        if latest_unusable.is_none() {
            latest_unusable = Some(result);
        }
        if Instant::now() >= deadline {
            break;
        }
    }
    Ok(latest_unusable.map_or(Discovery::UpToDate, |r| Discovery::Available(Box::new(r))))
}
/// Bound untrusted release-note text without splitting UTF-8 characters.
fn bounded_notes(mut text: String) -> String {
    const LIMIT: usize = 32 * 1024;
    if text.len() > LIMIT {
        let mut boundary = LIMIT;
        while !text.is_char_boundary(boundary) {
            boundary -= 1;
        }
        text.truncate(boundary);
        text.push_str("\n[Release notes truncated; see the official release for the remainder.]");
    }
    text
}
/// Bind asset identities and signed manifest to the selected release.
fn inspect_release(
    release: GithubRelease,
    key: Option<&[u8]>,
    deadline: Instant,
) -> anyhow::Result<Release> {
    let version = stable_version(&release.tag_name)?.to_string();
    let mut result = Release {
        id: release.id,
        version,
        published_at: release
            .published_at
            .and_then(|date| chrono::DateTime::parse_from_rfc3339(&date).ok())
            .map_or_else(
                || "Release date unavailable".into(),
                |date| date.to_rfc3339(),
            ),
        notes: bounded_notes(release.body.unwrap_or_default()),
        manifest: None,
        size: None,
        verification: "No trusted signing key configured; installation disabled.".into(),
        compatible: false,
    };
    let Some(target) = platform_target() else {
        result.verification = "Unsupported operating system or architecture.".into();
        return Ok(result);
    };
    let suffix = match target {
        "x86_64-unknown-linux-gnu" => "linux-x86_64",
        "aarch64-unknown-linux-gnu" => "linux-arm64",
        "aarch64-apple-darwin" => "macos-apple-silicon",
        _ => "windows-x86_64",
    };
    let legacy_name = format!("rustchan-cli-v{}-{suffix}.zip", result.version);
    result.size = release
        .assets
        .iter()
        .find(|asset| {
            asset.name == format!("rustchan-update-{target}.tar.gz") || asset.name == legacy_name
        })
        .map(|asset| asset.size);
    let Some(key) = key else {
        return Ok(result);
    };
    let verified = (|| {
        let name = format!("rustchan-update-{target}.json");
        let find = |name: &str| -> anyhow::Result<&Asset> {
            let matching: Vec<_> = release.assets.iter().filter(|a| a.name == name).collect();
            anyhow::ensure!(
                matching.len() == 1,
                "release is missing a unique compatible artifact"
            );
            let asset = matching.first().context("missing release asset")?;
            anyhow::ensure!(
                asset.browser_download_url == asset_url(&result.version, name)?,
                "disallowed release asset source"
            );
            Ok(asset)
        };
        find(&name)?;
        find(&format!("{name}.sig"))?;
        let manifest = verify_manifest(
            &fetch_before(&asset_url(&result.version, &name)?, 16 * 1024, deadline)?,
            &fetch_before(
                &asset_url(&result.version, &format!("{name}.sig"))?,
                64,
                deadline,
            )?,
            key,
            &result.version,
            result.id,
            target,
        )?;
        let artifact = find(&manifest.filename)?;
        anyhow::ensure!(
            artifact.size == manifest.size,
            "release artifact size does not match manifest"
        );
        Ok::<_, anyhow::Error>(manifest)
    })();
    match verified {
        Ok(manifest) => {
            result.size = Some(manifest.size);
            result.compatible = true;
            result.verification =
                "Ed25519 signature verified; SHA-256 will be checked before installation.".into();
            result.manifest = Some(manifest);
        }
        Err(error) => {
            let detail = error.to_string();
            let label = if detail.contains("compatible") || detail.contains("newer updater") {
                "Incompatible release"
            } else {
                "Release failed verification"
            };
            result.verification = format!("{label}: {error}");
        }
    }
    Ok(result)
}

#[cfg(test)]
/// Signed-manifest and stable-release boundary tests.
mod tests {
    use super::*;
    use ring::signature::{Ed25519KeyPair, KeyPair as _};

    /// Canonical test manifest, bound to one release and platform.
    fn manifest() -> Manifest {
        Manifest {
            format: 1,
            version: "1.6.0".to_owned(),
            release_id: 42,
            target: "x86_64-unknown-linux-gnu".to_owned(),
            filename: "rustchan-update-x86_64-unknown-linux-gnu.tar.gz".to_owned(),
            size: 100,
            sha256: "00".repeat(32),
            executable_sha256: "00".repeat(32),
            executable_size: 128,
            schema: "1.6.0".to_owned(),
            minimum_schema: "1.5.0".to_owned(),
            minimum_updater: "1.5.0".to_owned(),
        }
    }

    /// Sign and verify exact bytes using a fresh disposable Ed25519 key.
    fn verify(value: &serde_json::Value) -> anyhow::Result<Manifest> {
        let seed = Ed25519KeyPair::generate_pkcs8(&ring::rand::SystemRandom::new())
            .map_err(|_error| anyhow::anyhow!("test key generation failed"))?;
        let key = Ed25519KeyPair::from_pkcs8(seed.as_ref())
            .map_err(|_error| anyhow::anyhow!("test key parsing failed"))?;
        let bytes = serde_json::to_vec(value)?;
        verify_manifest(
            &bytes,
            key.sign(&bytes).as_ref(),
            key.public_key().as_ref(),
            "1.6.0",
            42,
            "x86_64-unknown-linux-gnu",
        )
    }

    /// Semantic ordering excludes drafts/prereleases, malformed versions and un-published tags.
    #[test]
    fn semantic_selection_and_prerelease_exclusion() -> anyhow::Result<()> {
        let releases = serde_json::json!([
            {"id":1,"tag_name":"v1.9.0","draft":false,"prerelease":false,"published_at":"2026-09-30T00:00:00Z","assets":[]},
            {"id":2,"tag_name":"v1.10.0","draft":false,"prerelease":false,"published_at":"2026-09-30T00:00:00Z","assets":[]},
            {"id":3,"tag_name":"v2.0.0-rc.1","draft":false,"prerelease":false,"published_at":"2026-09-30T00:00:00Z","assets":[]},
            {"id":4,"tag_name":"v3.0.0","draft":false,"prerelease":true,"published_at":"2026-09-30T00:00:00Z","assets":[]},
            {"id":5,"tag_name":"v4.0.0","draft":true,"prerelease":false,"published_at":"2026-09-30T00:00:00Z","assets":[]}
        ]);
        let bytes = serde_json::to_vec(&releases)?;
        anyhow::ensure!(
            latest(&bytes, "1.5.0")?
                .context("new stable release")?
                .tag_name
                == "v1.10.0",
            "semantic ordering must select 1.10 over 1.9"
        );
        anyhow::ensure!(
            latest(&bytes, "1.10.0")?.is_none(),
            "current release must report no update"
        );
        anyhow::ensure!(
            candidates(b"invalid json", "1.5.0").is_err(),
            "malformed releases must fail safely"
        );
        Ok(())
    }

    /// Signatures, identity, target, limits and unknown fields all fail closed.
    #[test]
    fn signed_manifest_is_strict_and_compatible() -> anyhow::Result<()> {
        let valid = serde_json::to_value(manifest())?;
        anyhow::ensure!(
            verify(&valid).is_ok(),
            "valid compatible manifest must verify"
        );
        for (field, value) in [
            ("format", serde_json::json!(2)),
            ("version", serde_json::json!("1.7.0")),
            ("release_id", serde_json::json!(43)),
            ("target", serde_json::json!("aarch64-unknown-linux-gnu")),
            ("minimum_updater", serde_json::json!("99.0.0")),
            ("minimum_schema", serde_json::json!("9.0.0")),
            ("sha256", serde_json::json!("bad")),
            ("size", serde_json::json!(0)),
            ("filename", serde_json::json!("../../outside")),
            ("unknown", serde_json::json!(true)),
        ] {
            let mut altered = valid.clone();
            altered
                .as_object_mut()
                .context("manifest object")?
                .insert(field.to_owned(), value);
            anyhow::ensure!(
                verify(&altered).is_err(),
                "incompatible field {field} must be rejected"
            );
        }
        anyhow::ensure!(
            verify_manifest(
                b"{}",
                &[0; 64],
                &[0; 32],
                "1.6.0",
                42,
                "x86_64-unknown-linux-gnu"
            )
            .is_err(),
            "invalid signature must be rejected before parsing"
        );
        Ok(())
    }

    /// Schema transition compatibility is semantic, never lexicographic or integer based.
    #[test]
    fn schema_range_and_source_allowlist_fail_closed() -> anyhow::Result<()> {
        let manifest = manifest();
        anyhow::ensure!(
            schema_compatible("1.5.0", &manifest)?,
            "old baseline should be migratable"
        );
        anyhow::ensure!(
            !schema_compatible("1.4.0", &manifest)?,
            "unknown old schema must be refused"
        );
        anyhow::ensure!(
            !schema_compatible("1.7.0", &manifest)?,
            "schema downgrades must be refused"
        );
        for url in [
            "http://github.com/csd113/RustChan/releases/download/v1.6.0/file",
            "https://github.com.evil.test/csd113/RustChan/releases/download/v1.6.0/file",
            "https://github.com/csd113/Other/releases/download/v1.6.0/file",
            "https://127.0.0.1/file",
            "https://api.github.com/repos/csd113/RustChan/issues",
            "https://github.com:8443/csd113/RustChan/releases/download/v1.6.0/file",
        ] {
            anyhow::ensure!(
                validate_source(url).is_err(),
                "untrusted source must be refused: {url}"
            );
        }
        anyhow::ensure!(
            asset_url("1.6.0", "../bad").is_err(),
            "asset traversal must be rejected"
        );
        Ok(())
    }

    /// Check-only discovery recognizes `RustChan`'s existing platform ZIP naming contract.
    #[test]
    fn check_only_recognizes_existing_release_archives() -> anyhow::Result<()> {
        if platform_target().is_none() {
            return Ok(());
        }
        let source: GithubRelease = serde_json::from_value(serde_json::json!({
            "id":42,"tag_name":"v1.6.0","draft":false,"prerelease":false,
            "published_at":"2026-09-30T00:00:00Z","body":"release",
            "assets":[
                {"name":"rustchan-cli-v1.6.0-linux-x86_64.zip","size":1234,"browser_download_url":"https://github.com/csd113/RustChan/releases/download/v1.6.0/fixture.zip"},
                {"name":"rustchan-cli-v1.6.0-linux-arm64.zip","size":1234,"browser_download_url":"https://github.com/csd113/RustChan/releases/download/v1.6.0/fixture.zip"},
                {"name":"rustchan-cli-v1.6.0-macos-apple-silicon.zip","size":1234,"browser_download_url":"https://github.com/csd113/RustChan/releases/download/v1.6.0/fixture.zip"},
                {"name":"rustchan-cli-v1.6.0-windows-x86_64.zip","size":1234,"browser_download_url":"https://github.com/csd113/RustChan/releases/download/v1.6.0/fixture.zip"}
            ]
        }))?;
        let release = inspect_release(source, None, Instant::now() + Duration::from_secs(30))?;
        anyhow::ensure!(
            release.size == Some(1234) && !release.compatible && release.manifest.is_none(),
            "legacy ZIP metadata must be visible without authorizing unsigned installation"
        );
        Ok(())
    }
}
