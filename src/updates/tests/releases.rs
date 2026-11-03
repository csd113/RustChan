//! Stable-channel ordering and safe manual links; no network or artifact downloads.

use super::{select_release, Discovery, Release, RELEASES_URL};
use anyhow::{ensure, Result};
use serde_json::json;

fn release(tag: &str, draft: bool, prerelease: bool) -> serde_json::Value {
    json!({"tag_name": tag, "draft": draft, "prerelease": prerelease,
        "published_at": "2026-11-03T00:00:00Z", "html_url": "javascript:untrusted()"})
}

#[test]
fn stable_checks_compare_semantic_versions_and_ignore_unpublished_releases() -> Result<()> {
    let bytes = serde_json::to_vec(&json!([
        release("v1.7.4", false, false),
        release("v1.7.5", false, false),
        release("v9.0.0", true, false),
        release("v8.0.0-rc.1", false, true),
        release("v7.0.0-rc.1", false, false),
        release("v6.0.0+build", false, false)
    ]))?;
    for current in ["1.7.1", "1.7.5-rc.1"] {
        ensure!(
            matches!(select_release(&bytes, current)?, Discovery::Available(ref release) if release.version == "1.7.5")
        );
    }
    for current in ["1.7.5", "1.8.0-rc.1", "1.10.0"] {
        ensure!(
            matches!(select_release(&bytes, current)?, Discovery::UpToDate(ref release) if release.version == "1.7.5")
        );
    }
    Ok(())
}

#[test]
fn invalid_responses_and_untrusted_tags_never_become_download_links() -> Result<()> {
    ensure!(select_release(b"not JSON", "1.7.1").is_err());
    ensure!(select_release(b"[]", "1.7.1").is_err());
    let bytes = serde_json::to_vec(&json!([
        release("v1.7.5/../../foreign", false, false),
        release("https://foreign.test/v9.0.0", false, false),
        release("v1.7.5", false, false)
    ]))?;
    let selected = select_release(&bytes, "1.7.1")?;
    ensure!(
        matches!(selected, Discovery::Available(ref release) if release.url() == "https://github.com/csd113/RustChan/releases/tag/v1.7.5")
    );
    let invalid_cache = Release {
        version: "1.7.5".to_owned(),
        tag: "javascript:untrusted()".to_owned(),
    };
    ensure!(invalid_cache.url() == RELEASES_URL);
    Ok(())
}
