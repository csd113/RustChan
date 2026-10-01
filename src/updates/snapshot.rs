//! Complete stopped-service snapshots of startup-mutable media and private runtime state.

#[cfg(unix)]
use anyhow::Context as _;
use serde::{Deserialize, Serialize};
use sha2::Digest as _;
#[cfg(unix)]
use std::{
    fs,
    os::unix::fs::{MetadataExt as _, PermissionsExt as _},
    path::Path,
};

/// A verified file or directory in an updater-owned rollback snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    /// Relative path restricted to boards/ or runtime/.
    pub path: String,
    /// Whether the entry is a directory rather than a regular file.
    pub directory: bool,
    /// Original owner for restoration.
    pub uid: u32,
    /// Original group for restoration.
    pub gid: u32,
    /// Original ordinary permission bits (special bits are rejected).
    pub mode: u32,
    /// Byte length for regular files.
    pub size: u64,
    /// SHA-256 for regular files; empty for directories.
    pub sha256: String,
}

/// Estimate snapshot and restoration space without copying live files.
pub(super) fn estimated_bytes(data: &Path) -> anyhow::Result<u64> {
    let mut pending = vec![data.join("boards"), data.join("runtime")];
    let owner = fs::metadata(data)?.uid();
    let mut count = 0_usize;
    let mut bytes = 0_u64;
    while let Some(path) = pending.pop() {
        count += 1;
        anyhow::ensure!(
            count <= 1_000_000 && path.strip_prefix(data)?.components().count() <= 32,
            "persistent backup exceeds safety limits"
        );
        let meta = fs::symlink_metadata(&path)?;
        anyhow::ensure!(
            (meta.is_dir() || meta.is_file())
                && meta.uid() == owner
                && meta.mode() & 0o7000 == 0
                && (meta.is_dir() || meta.nlink() == 1)
                && meta.mode() & 0o400 != 0
                && (!meta.is_dir() || meta.mode() & 0o300 == 0o300),
            "unsafe or inaccessible persistent backup entry"
        );
        if meta.is_dir() {
            for entry in fs::read_dir(path)? {
                pending.push(entry?.path());
            }
        } else {
            bytes = bytes
                .checked_add(meta.len())
                .context("persistent backup size overflow")?;
        }
    }
    Ok(bytes)
}

/// Copy only fixed persistent roots after the web service has stopped.
#[cfg(unix)]
pub(super) fn create(data: &Path, backup: &Path) -> anyhow::Result<(String, u64)> {
    let owner = fs::symlink_metadata(data)?.uid();
    let mut entries = Vec::new();
    for root in ["boards", "runtime"] {
        copy_tree(
            data,
            &data.join(root),
            &backup.join("persistent"),
            owner,
            &mut entries,
        )?;
    }
    verify(backup, &entries)?;
    let bytes = serde_json::to_vec(&entries)?;
    anyhow::ensure!(
        bytes.len() <= 64 * 1024 * 1024,
        "persistent inventory exceeds limit"
    );
    let manifest = backup.join("persistent.json");
    fs::write(&manifest, &bytes)?;
    fs::File::open(&manifest)?.sync_all()?;
    Ok((
        hex::encode(sha2::Sha256::digest(&bytes)),
        entries.iter().map(|entry| entry.size).sum(),
    ))
}

/// Bound recursion and reject links, special files and foreign-owned persistent state.
#[cfg(unix)]
fn copy_tree(
    data: &Path,
    source: &Path,
    destination: &Path,
    web_uid: u32,
    entries: &mut Vec<Entry>,
) -> anyhow::Result<()> {
    let relative = source.strip_prefix(data)?;
    anyhow::ensure!(
        relative.components().count() <= 32 && entries.len() < 1_000_000,
        "persistent backup exceeds safety limits"
    );
    let meta = fs::symlink_metadata(source)?;
    anyhow::ensure!(
        (meta.is_dir() || meta.is_file())
            && !meta.file_type().is_symlink()
            && meta.uid() == web_uid
            && meta.mode() & 0o7000 == 0
            && (meta.is_dir() || meta.nlink() == 1),
        "unsafe persistent backup entry"
    );
    let target = destination.join(relative);
    let (size, sha256) = if meta.is_dir() {
        fs::create_dir_all(&target)?;
        (0, String::new())
    } else {
        fs::copy(source, &target)?;
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600))?;
        fs::File::open(&target)?.sync_all()?;
        (meta.len(), hash(&target)?)
    };
    entries.push(Entry {
        path: relative
            .to_str()
            .context("invalid persistent backup path")?
            .to_owned(),
        directory: meta.is_dir(),
        uid: meta.uid(),
        gid: meta.gid(),
        mode: meta.mode() & 0o777,
        size,
        sha256,
    });
    if meta.is_dir() {
        let mut children = fs::read_dir(source)?.collect::<Result<Vec<_>, _>>()?;
        children.sort_by_key(fs::DirEntry::file_name);
        for child in children {
            copy_tree(data, &child.path(), destination, web_uid, entries)?;
        }
        fs::File::open(&target)?.sync_all()?;
    }
    Ok(())
}

/// Validate the entire snapshot before changing any live persistent directory.
#[cfg(unix)]
fn verify(backup: &Path, entries: &[Entry]) -> anyhow::Result<()> {
    let mut paths = std::collections::HashSet::new();
    for entry in entries {
        let path = Path::new(&entry.path);
        let mut components = path.components();
        anyhow::ensure!(
            matches!(components.next(), Some(std::path::Component::Normal(name)) if name == "boards" || name == "runtime")
                && components.all(|part| matches!(part, std::path::Component::Normal(_)))
                && paths.insert(entry.path.as_str())
                && entry.mode & !0o777 == 0,
            "invalid persistent backup manifest"
        );
        let source = backup.join("persistent").join(path);
        for ancestor in source.ancestors().take_while(|p| *p != backup) {
            anyhow::ensure!(
                !fs::symlink_metadata(ancestor)?.file_type().is_symlink(),
                "linked persistent backup entry"
            );
        }
        let meta = fs::symlink_metadata(&source)?;
        anyhow::ensure!(
            if entry.directory {
                meta.is_dir()
            } else {
                meta.is_file()
                    && meta.nlink() == 1
                    && meta.len() == entry.size
                    && hash(&source)? == entry.sha256
            },
            "persistent backup failed verification"
        );
    }
    for root in ["boards", "runtime"] {
        anyhow::ensure!(paths.contains(root), "persistent backup is incomplete");
    }
    Ok(())
}

/// Verify immutable inventory and every payload before restoring any database/configuration.
pub(super) fn verify_saved(backup: &Path, expected_hash: &str) -> anyhow::Result<()> {
    load(backup, expected_hash)?;
    Ok(())
}

/// Bound and authenticate the entire persisted file inventory.
fn load(backup: &Path, expected_hash: &str) -> anyhow::Result<Vec<Entry>> {
    let path = backup.join("persistent.json");
    anyhow::ensure!(
        fs::symlink_metadata(&path)?.is_file()
            && fs::metadata(&path)?.len() <= 64 * 1024 * 1024
            && hash(&path)? == expected_hash,
        "persistent inventory failed verification"
    );
    let entries: Vec<Entry> = serde_json::from_slice(&fs::read(path)?)?;
    verify(backup, &entries)?;
    Ok(entries)
}

/// Rebuild and atomically replace each fixed tree; journal remains `RollingBack` until all finish.
/// Replaying after any interruption repeats restoration from immutable snapshots.
#[cfg(unix)]
pub(super) fn restore(backup: &Path, data: &Path, expected_hash: &str) -> anyhow::Result<()> {
    let entries = load(backup, expected_hash)?;
    let owner = fs::metadata(data)?.uid();
    anyhow::ensure!(
        entries.iter().all(|entry| entry.uid == owner),
        "persistent backup owner mismatch"
    );
    for root in ["boards", "runtime"] {
        let stage = tempfile::Builder::new()
            .prefix(".update-restore-")
            .tempdir_in(data)?;
        for entry in entries
            .iter()
            .filter(|entry| Path::new(&entry.path).starts_with(root))
        {
            let relative = Path::new(&entry.path).strip_prefix(root)?;
            let target = stage.path().join(relative);
            if entry.directory {
                fs::create_dir_all(&target)?;
            } else {
                fs::copy(backup.join("persistent").join(&entry.path), &target)?;
                fs::File::open(&target)?.sync_all()?;
            }
        }
        // Children first: restore owner-only permissions after materializing the whole tree.
        for entry in entries
            .iter()
            .rev()
            .filter(|entry| Path::new(&entry.path).starts_with(root))
        {
            let target = stage
                .path()
                .join(Path::new(&entry.path).strip_prefix(root)?);
            std::os::unix::fs::chown(&target, Some(entry.uid), Some(entry.gid))?;
            fs::set_permissions(&target, fs::Permissions::from_mode(entry.mode))?;
            fs::File::open(&target)?.sync_all()?;
        }
        let live = data.join(root);
        let displaced = data.join(format!(".update-displaced-{root}"));
        if displaced.exists() {
            reject_links(&displaced)?;
            fs::remove_dir_all(&displaced)?;
        }
        if live.exists() {
            reject_links(&live)?;
            fs::rename(&live, &displaced)?;
        }
        fs::rename(stage.keep(), &live)?;
        fs::File::open(data)?.sync_all()?;
        if displaced.exists() {
            fs::remove_dir_all(&displaced)?;
        }
    }
    Ok(())
}

/// Refuse symlink/special-file trees before deletion or displacement.
#[cfg(unix)]
fn reject_links(path: &Path) -> anyhow::Result<()> {
    let mut pending = vec![(path.to_path_buf(), 0_usize)];
    let mut count = 0_usize;
    while let Some((path, depth)) = pending.pop() {
        count += 1;
        anyhow::ensure!(
            depth <= 32 && count <= 1_000_000,
            "persistent tree exceeds safety limits"
        );
        let meta = fs::symlink_metadata(&path)?;
        anyhow::ensure!(
            meta.is_dir() || meta.is_file(),
            "unsafe live persistent entry"
        );
        if meta.is_dir() {
            for entry in fs::read_dir(path)? {
                pending.push((entry?.path(), depth + 1));
            }
        }
    }
    Ok(())
}

/// Stream a persistent-file digest without buffering large media.
#[cfg(unix)]
fn hash(path: &Path) -> anyhow::Result<String> {
    use sha2::{Digest as _, Sha256};
    use std::io::Read as _;
    let mut reader = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 16 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(buffer.get(..count).context("invalid file read length")?);
    }
    Ok(hex::encode(hash.finalize()))
}
