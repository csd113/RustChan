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

#[cfg(test)]
/// Verification fixtures from the published v1.6.5 Linux ARM64 release.
mod tests {
    /// A real release signed before this source change establishes key provenance;
    /// modified manifest bytes and a modified identity must fail verification.
    #[test]
    fn embedded_identity_verifies_published_release_and_rejects_tampering() -> anyhow::Result<()> {
        let manifest = br#"{"executable_sha256":"866307892b59e3173e4aede618018e81d77618acb55bc1a5d580425d52d05029","executable_size":32438312,"filename":"rustchan-update-aarch64-unknown-linux-gnu.tar.gz","format":1,"minimum_schema":"1.5.0","minimum_updater":"1.6.0","release_id":402674971,"schema":"1.6.5","sha256":"9d8b838fbcf960f6892c3d8f0fff84ac6d10420f825c10d2b552d72a9991cff0","size":14098404,"target":"aarch64-unknown-linux-gnu","version":"1.6.5"}"#;
        let signature = hex::decode("aaa8317848280ac49ee6cc90f2c4c518c0aa78b81503d4ec88202016932b3f2b766cfd5780f2de4e208a589c84d38d9c9d71aac82e7c500dbd1e52e00ca31d08")?;
        let key = super::official_public_key()?;
        let verify = |bytes: &[u8], identity: &[u8]| {
            super::super::release::verify_manifest(
                bytes,
                &signature,
                identity,
                "1.6.5",
                402_674_971,
                "aarch64-unknown-linux-gnu",
            )
        };
        anyhow::ensure!(verify(manifest, &key)?.version == "1.6.5");
        let mut changed = manifest.to_vec();
        *changed
            .get_mut(1)
            .ok_or_else(|| anyhow::anyhow!("empty release fixture"))? ^= 1;
        anyhow::ensure!(verify(&changed, &key).is_err());
        let mut wrong_key = key;
        *wrong_key
            .first_mut()
            .ok_or_else(|| anyhow::anyhow!("empty verification key"))? ^= 1;
        anyhow::ensure!(verify(manifest, &wrong_key).is_err());
        Ok(())
    }
}
