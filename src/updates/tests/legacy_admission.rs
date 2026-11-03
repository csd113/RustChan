//! Older recovery state must remain intact and continue excluding uncertain writers.

use super::admit;
use anyhow::{ensure, Result};
use std::path::Path;

fn fixture(data: &Path) -> Result<()> {
    let legacy = data.join(".software-updates");
    crate::config::ensure_private_dir(&legacy.join("writers"))?;
    for (name, bytes) in [
        ("admission.lock", b"".as_slice()),
        ("data-writers.lock", b"".as_slice()),
        ("origin.json", br#"{"format":1,"phase":"ready"}"#.as_slice()),
        (
            "status.json",
            br#"{"phase":"idle","installed":"1.7.1"}"#.as_slice(),
        ),
    ] {
        crate::config::write_private_file(&legacy.join(name), bytes)?;
    }
    Ok(())
}

#[test]
fn clean_legacy_installation_preserves_journals_and_holds_writer_exclusion() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let data = temporary.path().canonicalize()?;
    fixture(&data)?;
    let path = data.join(".software-updates/status.json");
    let before = std::fs::read(&path)?;
    let admission = admit(&data)?;
    let parallel_new_cli = admit(&data)?;
    drop(parallel_new_cli);
    let competing = std::fs::File::options()
        .read(true)
        .write(true)
        .open(data.join(".software-updates/data-writers.lock"))?;
    ensure!(competing.try_lock().is_err());
    let older_root = std::fs::File::options()
        .read(true)
        .write(true)
        .open(data.join(".software-updates/admission.lock"))?;
    ensure!(older_root.try_lock().is_err());
    ensure!(std::fs::read(path)? == before);
    drop(admission);
    competing.try_lock()?;
    drop(competing);
    older_root.try_lock()?;
    drop(older_root);
    std::fs::remove_file(data.join(".software-updates/origin.json"))?;
    let _protected_old_install = admit(&data)?;
    Ok(())
}

#[test]
fn unfinished_legacy_updates_and_uncertain_writers_fail_closed() -> Result<()> {
    let temporary = tempfile::tempdir()?;
    let data = temporary.path().canonicalize()?;
    fixture(&data)?;
    let legacy = data.join(".software-updates");
    for phase in ["activating", "failed_manual_intervention", "unknown"] {
        let bytes = serde_json::to_vec(&serde_json::json!({"phase": phase}))?;
        std::fs::write(legacy.join("status.json"), &bytes)?;
        ensure!(admit(&data).is_err());
        ensure!(std::fs::read(legacy.join("status.json"))? == bytes);
    }
    std::fs::write(
        legacy.join("status.json"),
        br#"{"phase":"idle","operation":"unknown"}"#,
    )?;
    ensure!(admit(&data).is_err());
    std::fs::write(legacy.join("status.json"), br#"{"phase":"idle"}"#)?;
    let id = uuid::Uuid::new_v4();
    #[cfg(target_os = "linux")]
    let boot = super::kernel_boot()?;
    #[cfg(not(target_os = "linux"))]
    let boot = uuid::Uuid::new_v4();
    let writer = serde_json::json!({"format": 1_u32, "id": id, "boot": boot, "role": "server", "phase": "running",
        "parent": {"pid": 1_i32, "start_tick": 1_u64}, "guardian": {"pid": 2_i32, "start_tick": 2_u64}});
    crate::config::write_private_file(
        &legacy.join("writers").join(format!("{id}.json")),
        &serde_json::to_vec(&writer)?,
    )?;
    ensure!(admit(&data).is_err());
    Ok(())
}
