use anyhow::{Context as _, Result};
use std::path::{Component, Path, PathBuf};

/// Validate a runtime path fragment before joining it to a trusted root.
///
/// # Errors
/// Returns an error if the path is absolute, contains a parent-directory
/// component, or contains a platform path prefix.
pub fn validate_runtime_relative_path(path: &str) -> Result<&Path> {
    let rel = Path::new(path);
    if rel.as_os_str().is_empty() {
        anyhow::bail!("Runtime path is empty.");
    }
    for component in rel.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                anyhow::bail!("Runtime path escapes its configured root.");
            }
        }
    }
    Ok(rel)
}

/// Lexical prefix checks do not catch symlink escapes. Canonicalize both sides
/// so runtime paths stay inside their root.
///
/// # Errors
/// Returns an error if either path cannot be canonicalized or the resolved
/// child is outside the resolved root.
pub fn canonical_child_of(root: &Path, path: &Path) -> Result<PathBuf> {
    let root = root
        .canonicalize()
        .with_context(|| format!("Canonicalize runtime root {}", root.display()))?;
    let path = path
        .canonicalize()
        .with_context(|| format!("Canonicalize runtime path {}", path.display()))?;
    if !path.starts_with(&root) {
        anyhow::bail!("Runtime path escapes its configured root.");
    }
    Ok(path)
}

/// Validate and resolve an existing regular file below a trusted root.
///
/// # Errors
/// Returns an error if the relative path is unsafe, any existing component is a
/// symlink, the file is missing or not regular, or the resolved path is outside
/// the resolved root.
pub fn existing_regular_file_child(root: &Path, relative_path: &str) -> Result<PathBuf> {
    let rel = validate_runtime_relative_path(relative_path)?;
    let root = root
        .canonicalize()
        .with_context(|| format!("Canonicalize runtime root {}", root.display()))?;
    let path = root.join(rel);
    reject_symlink_components(&path)?;
    let path = path
        .canonicalize()
        .with_context(|| format!("Canonicalize runtime path {}", path.display()))?;
    if !path.starts_with(&root) {
        anyhow::bail!("Runtime path escapes its configured root.");
    }
    assert_regular_file_no_symlink(&path)?;
    Ok(path)
}

/// Validate the existing parent of a not-yet-created runtime child.
///
/// # Errors
/// Returns an error if the parent is missing, symlinked, cannot be
/// canonicalized, or resolves outside the configured root.
pub fn canonical_parent_for_new_child(root: &Path, path: &Path) -> Result<PathBuf> {
    let parent = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Runtime path has no parent."))?;
    let root = root
        .canonicalize()
        .with_context(|| format!("Canonicalize runtime root {}", root.display()))?;
    let parent = parent
        .canonicalize()
        .with_context(|| format!("Canonicalize runtime parent {}", parent.display()))?;
    if !parent.starts_with(&root) {
        anyhow::bail!("Runtime destination escapes its configured root.");
    }
    Ok(parent)
}

/// Reject any existing symlink component in a path without following it.
fn reject_symlink_components(path: &Path) -> Result<()> {
    let mut cursor = PathBuf::new();
    for component in path.components() {
        cursor.push(component.as_os_str());
        let metadata = std::fs::symlink_metadata(&cursor)
            .with_context(|| format!("Inspect runtime path component {}", cursor.display()))?;
        if metadata.file_type().is_symlink() {
            anyhow::bail!("Runtime path component is a symlink.");
        }
    }
    Ok(())
}

/// Validate that a runtime path is a plain file, not a symlink or hardlink.
///
/// # Errors
/// Returns an error if the path is missing, is not a regular file, is a
/// symlink, or has more than one hardlink on Unix.
pub fn assert_regular_file_no_symlink(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("Inspect runtime file {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        anyhow::bail!("Runtime file is a symlink.");
    }
    if !metadata.file_type().is_file() {
        anyhow::bail!("Runtime path is not a regular file.");
    }
    reject_hardlinked_file_if_unix(&metadata)?;
    Ok(())
}

/// Validate that a runtime path is a directory and not a symlink.
///
/// # Errors
/// Returns an error if the path is missing, is not a directory, or is a symlink.
pub fn assert_dir_no_symlink(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)
        .with_context(|| format!("Inspect runtime directory {}", path.display()))?;
    if metadata.file_type().is_symlink() {
        anyhow::bail!("Runtime directory is a symlink.");
    }
    if !metadata.file_type().is_dir() {
        anyhow::bail!("Runtime path is not a directory.");
    }
    Ok(())
}

#[cfg(unix)]
fn reject_hardlinked_file_if_unix(metadata: &std::fs::Metadata) -> Result<()> {
    use std::os::unix::fs::MetadataExt as _;
    if metadata.nlink() != 1 {
        anyhow::bail!("Runtime file has multiple hard links.");
    }
    Ok(())
}

#[cfg(not(unix))]
fn reject_hardlinked_file_if_unix(_metadata: &std::fs::Metadata) -> Result<()> {
    // Portable Rust has no cross-platform link-count API; keep non-Unix builds
    // best-effort while preserving the symlink and canonical-root checks.
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn regular_file_check_rejects_symlink_and_hardlink() {
        use std::os::unix::fs as unix_fs;

        let dir = tempfile::tempdir().expect("tempdir");
        let target = dir.path().join("target.txt");
        let link = dir.path().join("link.txt");
        std::fs::write(&target, b"ok").expect("write target");
        unix_fs::symlink(&target, &link).expect("symlink");
        assert!(assert_regular_file_no_symlink(&link).is_err());

        let hardlink = dir.path().join("hardlink.txt");
        std::fs::hard_link(&target, &hardlink).expect("hardlink");
        assert!(assert_regular_file_no_symlink(&target).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn canonical_child_rejects_symlinked_parent_escape() {
        use std::os::unix::fs as unix_fs;

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("root");
        let outside = dir.path().join("outside");
        std::fs::create_dir_all(&root).expect("root");
        std::fs::create_dir_all(&outside).expect("outside");
        std::fs::write(outside.join("secret.txt"), b"secret").expect("secret");
        unix_fs::symlink(&outside, root.join("alias")).expect("symlink");

        assert!(canonical_child_of(&root, &root.join("alias/secret.txt")).is_err());
    }

    #[test]
    fn existing_regular_file_child_accepts_valid_relative_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("root");
        std::fs::create_dir_all(root.join("board")).expect("create board");
        std::fs::write(root.join("board/file.txt"), b"ok").expect("write file");

        let path = existing_regular_file_child(&root, "board/file.txt").expect("valid file");

        assert_eq!(
            path,
            root.join("board/file.txt")
                .canonicalize()
                .expect("canonical file")
        );
    }

    #[cfg(unix)]
    #[test]
    fn existing_regular_file_child_rejects_final_symlink() {
        use std::os::unix::fs as unix_fs;

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("root");
        let outside = dir.path().join("outside.txt");
        std::fs::create_dir_all(&root).expect("create root");
        std::fs::write(&outside, b"secret").expect("write outside file");
        unix_fs::symlink(&outside, root.join("link.txt")).expect("symlink");

        assert!(existing_regular_file_child(&root, "link.txt").is_err());
    }
}
