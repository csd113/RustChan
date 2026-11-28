//! The shared document must retain provenance for native same-origin form posts.

#[test]
fn native_forms_retain_same_origin_referrers() -> anyhow::Result<()> {
    let page = super::base_layout("Test", None, "", "", &[], None, None, false, "/");
    anyhow::ensure!(super::REFERRER_POLICY == "same-origin");
    anyhow::ensure!(page.contains("<meta name=\"referrer\" content=\"same-origin\">"));
    anyhow::ensure!(!page.contains("content=\"no-referrer\""));
    Ok(())
}
