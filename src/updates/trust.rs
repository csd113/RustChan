//! Built-in official release verification identity; never contains signing secrets.
//!
//! Reviewed from the official v1.6.0 and v1.6.5 release public-key assets. The
//! keys match, and the v1.6.5 manifest signature verifies with the earlier key.
//! Rotation requires a reviewed source/release transition, never a download.

/// `Ed25519` public key for official `RustChan` native release manifests.
const OFFICIAL_PUBLIC_KEY: &str =
    "9c003b7595894937addf316cc16f9aef5672903ba91acc309d2af05033988a7c";

/// Decode the fixed release verification key embedded in every source build.
pub(super) fn official_public_key() -> anyhow::Result<Vec<u8>> {
    let key = hex::decode(OFFICIAL_PUBLIC_KEY)?;
    anyhow::ensure!(key.len() == 32, "invalid embedded release verification key");
    Ok(key)
}
