#![expect(
    unused_crate_dependencies,
    reason = "Cargo exposes server dependencies to this renderer integration test; only the shared library and fixture serialization are used"
)]
//! Generate exact server-rendered native forms for cross-platform browser transition tests.

/// Export native forms from the production renderer, without enabling native installation in the server.
#[test]
fn native_update_form_fixture() -> anyhow::Result<()> {
    let installed = env!("CARGO_PKG_VERSION");
    let mut candidate = semver::Version::parse(installed)?;
    candidate.minor += 1;
    candidate.patch = 0;
    let candidate = candidate.to_string();
    let value = serde_json::json!({
        "phase":"idle", "installed":installed, "discovery":{"available":{
            "id":42,"version":candidate,"published_at":"2026-09-30T00:00:00Z","notes":"fixture",
            "manifest":{"format":1,"version":candidate,"release_id":42,"target":"x86_64-unknown-linux-gnu",
                "filename":"rustchan-update-x86_64-unknown-linux-gnu.tar.gz","size":128,"sha256":"00".repeat(32),
                "executable_sha256":"00".repeat(32),"executable_size":128,"schema":candidate,"minimum_schema":"1.5.0","minimum_updater":"1.5.0"},
            "size":128,"verification":"Ed25519 signature verified; package hash will be checked before installation.","compatible":true}},
        "approval":"00000000-0000-4000-8000-000000000042", "checked_at":"2026-09-30T00:00:00Z",
        "job":null,"administrator":null,"previous_version":null,"target_version":null,"message":"Ready for installation.",
        "backup":null,"backups":[],"updated_at":"2026-09-30T00:00:00Z"
    });
    let status: chan::updates::Status = serde_json::from_value(value)?;
    let html =
        chan::templates::admin::render_software_updates(&status, "FIXTURE_CSRF", true, false);
    anyhow::ensure!(
        html.contains("id=\"admin-update-install\""),
        "verified native release must display installation form"
    );
    anyhow::ensure!(
        html.contains("current_password") && html.contains("pattern=\"INSTALL\""),
        "native form must require reauthentication and explicit confirmation"
    );
    let dir =
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("output/playwright/update-fixtures");
    std::fs::create_dir_all(&dir)?;
    std::fs::write(dir.join("native-available.html"), html)?;
    Ok(())
}
