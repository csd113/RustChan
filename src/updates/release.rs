//! Fixed-source, bounded release metadata. No artifacts are downloaded or executed.

use anyhow::{ensure, Context as _};
use semver::Version;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// Official release list, also available when the API is offline or rate-limited.
pub const RELEASES_URL: &str = "https://github.com/csd113/RustChan/releases/latest";
/// Fixed API source; neither HTTP requests nor release metadata can override it.
const API: &str = "https://api.github.com/repos/csd113/RustChan/releases?per_page=100";
/// Maximum decoded API response, including release notes ignored by deserialization.
const MAX_RESPONSE: usize = 2 * 1024 * 1024;

#[derive(Debug, Clone, Serialize)]
/// Stable version and a locally constructed canonical official release link.
pub struct Release {
    /// Stable semantic version.
    pub version: String,
    /// Validated actual tag, never a supplied URL.
    pub tag: String,
}

impl Release {
    /// Construct the fixed official URL again at the rendering boundary.
    #[must_use]
    pub fn url(&self) -> String {
        let valid = Version::parse(self.tag.strip_prefix('v').unwrap_or(&self.tag))
            .ok()
            .filter(|version| version.pre.is_empty() && version.build.is_empty())
            .is_some_and(|version| {
                self.tag == version.to_string() || self.tag == format!("v{version}")
            });
        if valid {
            format!(
                "https://github.com/csd113/RustChan/releases/tag/{}",
                self.tag
            )
        } else {
            RELEASES_URL.to_owned()
        }
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "result", content = "detail", rename_all = "snake_case")]
/// Result of checking the stable release channel.
pub enum Discovery {
    /// A stable release newer than this build is available.
    Available(Release),
    /// Latest stable release is at or below this build, including prerelease builds ahead of stable.
    UpToDate(Release),
    /// The check failed without changing application operation.
    UnableToCheck(String),
}

#[derive(Debug, Deserialize)]
/// Only the GitHub metadata needed to select a published stable tag.
struct GithubRelease {
    /// Tag parsed as a stable semantic version.
    tag_name: String,
    /// Unpublished drafts are excluded.
    draft: bool,
    /// Prerelease-channel entries are excluded.
    prerelease: bool,
    /// A valid publication timestamp is required.
    published_at: Option<String>,
}

/// Select semantic versions, excluding drafts, prereleases, invalid tags and metadata-only versions.
fn select_release(bytes: &[u8], current: &str) -> anyhow::Result<Discovery> {
    ensure!(
        bytes.len() <= MAX_RESPONSE,
        "release response exceeds size limit"
    );
    let current = Version::parse(current.strip_prefix('v').unwrap_or(current))?;
    let releases: Vec<GithubRelease> = serde_json::from_slice(bytes)?;
    let latest = releases
        .into_iter()
        .filter(|release| !release.draft && !release.prerelease)
        .filter(|release| {
            release
                .published_at
                .as_deref()
                .is_some_and(|date| chrono::DateTime::parse_from_rfc3339(date).is_ok())
        })
        .filter_map(|release| {
            if release.tag_name.len() > 64 {
                return None;
            }
            let version = Version::parse(
                release
                    .tag_name
                    .strip_prefix('v')
                    .unwrap_or(&release.tag_name),
            )
            .ok()?;
            if !version.pre.is_empty() || !version.build.is_empty() {
                return None;
            }
            // Reconstruct the exact tag using the parsed version, never an API-supplied URL.
            let tag = if release.tag_name.starts_with('v') {
                format!("v{version}")
            } else {
                version.to_string()
            };
            (tag == release.tag_name).then_some((version, tag))
        })
        .max_by(|(left, _), (right, _)| left.cmp(right))
        .context("no published stable release found")?;
    let release = Release {
        version: latest.0.to_string(),
        tag: latest.1,
    };
    Ok(if latest.0 > current {
        Discovery::Available(release)
    } else {
        Discovery::UpToDate(release)
    })
}

/// Fetch only metadata from the fixed HTTPS API with redirects disabled and a total deadline.
async fn discover(current: &str) -> anyhow::Result<Discovery> {
    let client = reqwest::Client::builder()
        .user_agent(concat!("RustChan/", env!("CARGO_PKG_VERSION")))
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(15))
        .connect_timeout(Duration::from_secs(3))
        .build()?;
    let mut response = client
        .get(API)
        .header("Accept", "application/vnd.github+json")
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await?;
    ensure!(
        response.status().is_success(),
        "GitHub API returned an error"
    );
    ensure!(
        response
            .content_length()
            .is_none_or(|length| usize::try_from(length).is_ok_and(|length| length <= MAX_RESPONSE)),
        "release response exceeds size limit"
    );
    let mut bytes = Vec::new();
    loop {
        let next_chunk = response.chunk().await?;
        let Some(chunk) = next_chunk else {
            break;
        };
        ensure!(
            chunk.len() <= MAX_RESPONSE.saturating_sub(bytes.len()),
            "release response exceeds size limit"
        );
        bytes.extend_from_slice(&chunk);
    }
    select_release(&bytes, current)
}

/// A failed network check never changes files, installs software, or prevents startup.
pub async fn discover_official(current: &str) -> Discovery {
    match discover(current).await {
        Ok(discovery) => discovery,
        Err(_error) => Discovery::UnableToCheck(
            "Could not check GitHub releases. Try again later, or open the official releases page."
                .to_owned(),
        ),
    }
}

#[cfg(test)]
#[path = "tests/releases.rs"]
mod tests;
