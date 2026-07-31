use crate::config::CONFIG;
use anyhow::{Context as _, Result};
use image::{imageops::FilterType, DynamicImage, GenericImageView as _, ImageFormat};
use std::path::{Path, PathBuf};

/// Generated favicon files that may be served or backed up.
const GLOBAL_FILENAMES: &[&str] = &[
    "favicon.ico",
    "favicon-16x16.png",
    "favicon-32x32.png",
    "apple-touch-icon.png",
    "android-chrome-192x192.png",
    "android-chrome-512x512.png",
    "version.txt",
];

#[cfg(test)]
static FAVICON_STAGE_WRITE_FAILURE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
#[cfg(test)]
static FAVICON_OLD_CLEANUP_FAILURE: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);
#[cfg(test)]
static FAVICON_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

#[derive(Clone, Copy, Debug)]
/// Storage scope for a generated favicon set.
pub enum FaviconScope<'a> {
    /// Site-wide favicon used as the fallback.
    Global,
    /// Favicon override for the named board.
    Board(&'a str),
}

#[derive(Debug)]
/// Public URL and cache-busting version for a favicon set.
pub struct ResolvedFavicon {
    /// Base URL that contains generated favicon files.
    pub base_url: String,
    /// Opaque version string appended to asset URLs.
    pub version: String,
}

#[must_use]
/// Return the directory containing the global favicon set.
pub fn global_favicon_dir() -> PathBuf {
    crate::config::runtime_favicon_dir()
}

#[must_use]
/// Return the generated-favicon directory for a board.
pub fn board_favicon_dir(board_short: &str) -> PathBuf {
    PathBuf::from(&CONFIG.upload_dir)
        .join(board_short)
        .join("_favicon")
}

#[must_use]
/// Return whether a board currently has a complete favicon override.
pub fn board_has_custom_favicon(board_short: &str) -> bool {
    version_for_scope(FaviconScope::Board(board_short)).is_some()
}

#[must_use]
/// Return whether a complete global favicon set exists.
pub fn global_has_custom_favicon() -> bool {
    version_for_scope(FaviconScope::Global).is_some()
}

#[must_use]
/// Resolve the effective favicon version for a board or the site fallback.
pub fn favicon_version_for_board(board_short: Option<&str>) -> Option<String> {
    board_short
        .and_then(|short| version_for_scope(FaviconScope::Board(short)))
        .or_else(|| version_for_scope(FaviconScope::Global))
}

#[must_use]
/// Render `<link>` elements for the effective favicon set.
pub fn favicon_head_html(board_short: Option<&str>) -> String {
    let resolved = resolve_favicon_for_board(board_short);
    let Some(resolved) = resolved else {
        return String::new();
    };
    let v = &resolved.version;
    format!(
        concat!(
            "<link rel=\"icon\" href=\"{base}/favicon.ico?v={v}\" sizes=\"any\">",
            "<link rel=\"icon\" type=\"image/png\" sizes=\"16x16\" href=\"{base}/favicon-16x16.png?v={v}\">",
            "<link rel=\"icon\" type=\"image/png\" sizes=\"32x32\" href=\"{base}/favicon-32x32.png?v={v}\">",
            "<link rel=\"apple-touch-icon\" sizes=\"180x180\" href=\"{base}/apple-touch-icon.png?v={v}\">",
            "<link rel=\"icon\" type=\"image/png\" sizes=\"192x192\" href=\"{base}/android-chrome-192x192.png?v={v}\">",
            "<link rel=\"icon\" type=\"image/png\" sizes=\"512x512\" href=\"{base}/android-chrome-512x512.png?v={v}\">"
        ),
        base = resolved.base_url,
        v = v,
    )
}

#[must_use]
/// Resolve the effective board-specific or global favicon set.
pub fn resolve_favicon_for_board(board_short: Option<&str>) -> Option<ResolvedFavicon> {
    board_short
        .and_then(|short| resolve_scope(FaviconScope::Board(short)))
        .or_else(|| resolve_scope(FaviconScope::Global))
}

/// Generate and atomically publish the full favicon asset set for a scope.
///
/// # Errors
/// Returns an error if the uploaded image cannot be decoded, is not exactly
/// `512x512`, or any filesystem write or rename operation fails.
pub fn write_favicon_set(scope: FaviconScope<'_>, bytes: &[u8]) -> Result<()> {
    let img = decode_uploaded_favicon(bytes)?;
    let target_dir = scope_dir(scope);
    let stage_dir = staging_dir_for(&target_dir);
    std::fs::create_dir_all(&stage_dir)
        .with_context(|| format!("create favicon staging directory {}", stage_dir.display()))?;
    let mut stage_guard = DirectoryCleanupGuard::new(stage_dir.clone());

    #[cfg(test)]
    maybe_fail_favicon_stage_write()?;
    write_png(
        &img.resize_exact(16, 16, FilterType::Lanczos3),
        &stage_dir.join("favicon-16x16.png"),
    )?;
    write_png(
        &img.resize_exact(32, 32, FilterType::Lanczos3),
        &stage_dir.join("favicon-32x32.png"),
    )?;
    write_png(
        &img.resize_exact(180, 180, FilterType::Lanczos3),
        &stage_dir.join("apple-touch-icon.png"),
    )?;
    write_png(
        &img.resize_exact(192, 192, FilterType::Lanczos3),
        &stage_dir.join("android-chrome-192x192.png"),
    )?;
    write_png(
        &img.resize_exact(512, 512, FilterType::Lanczos3),
        &stage_dir.join("android-chrome-512x512.png"),
    )?;
    img.resize_exact(32, 32, FilterType::Lanczos3)
        .save_with_format(stage_dir.join("favicon.ico"), ImageFormat::Ico)
        .with_context(|| format!("write {}", stage_dir.join("favicon.ico").display()))?;
    std::fs::write(
        stage_dir.join("version.txt"),
        uuid::Uuid::new_v4().to_string(),
    )
    .with_context(|| format!("write {}", stage_dir.join("version.txt").display()))?;

    swap_stage_into_place(&stage_dir, &target_dir)?;
    stage_guard.disarm();
    Ok(())
}

/// Remove a board-specific favicon override so the board falls back to the
/// global favicon.
///
/// # Errors
/// Returns an error if the board favicon directory exists but cannot be
/// removed.
pub fn clear_board_favicon(board_short: &str) -> Result<()> {
    let dir = board_favicon_dir(board_short);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .with_context(|| format!("remove board favicon directory {}", dir.display()))?;
    }
    Ok(())
}

#[must_use]
/// Resolve an allowed public file from the global favicon set.
pub fn global_favicon_file(file_name: &str) -> Option<PathBuf> {
    if !GLOBAL_FILENAMES.contains(&file_name) || file_name == "version.txt" {
        return None;
    }
    let path = global_favicon_dir().join(file_name);
    path.exists().then_some(path)
}

#[must_use]
/// Return the global favicon directory included in full backups.
pub fn global_backup_source_dir() -> PathBuf {
    global_favicon_dir()
}

/// Resolve a favicon scope into its public URL and version.
fn resolve_scope(scope: FaviconScope<'_>) -> Option<ResolvedFavicon> {
    let version = version_for_scope(scope)?;
    let base_url = match scope {
        FaviconScope::Global => String::new(),
        FaviconScope::Board(board_short) => format!("/boards/{board_short}/_favicon"),
    };
    Some(ResolvedFavicon { base_url, version })
}

/// Construct a unique staging-directory sibling of the live target.
fn staging_dir_for(target_dir: &Path) -> PathBuf {
    let parent = target_dir
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf);
    let file_name = target_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("favicon");
    let stage_dir = parent.join(format!(
        ".{file_name}.stage.{}",
        uuid::Uuid::new_v4().simple()
    ));
    stage_dir
}

/// Atomically publish a staged favicon directory and clean the previous set.
fn swap_stage_into_place(stage_dir: &Path, target_dir: &Path) -> Result<()> {
    let previous_dir = target_dir.parent().map_or_else(
        || PathBuf::from(format!("{}.old", target_dir.display())),
        |parent| {
            parent.join(format!(
                ".{}.old.{}",
                target_dir
                    .file_name()
                    .and_then(|name| name.to_str())
                    .unwrap_or("favicon"),
                uuid::Uuid::new_v4().simple()
            ))
        },
    );

    let had_existing_target = target_dir.exists();
    if had_existing_target {
        std::fs::rename(target_dir, &previous_dir).with_context(|| {
            format!(
                "move existing favicon directory {} to {}",
                target_dir.display(),
                previous_dir.display()
            )
        })?;
    }

    match std::fs::rename(stage_dir, target_dir) {
        Ok(()) => {
            if had_existing_target {
                cleanup_previous_favicon_dir(&previous_dir)?;
            }
            Ok(())
        }
        Err(error) => {
            if had_existing_target {
                drop(std::fs::rename(&previous_dir, target_dir));
            }
            drop(std::fs::remove_dir_all(stage_dir));
            Err(anyhow::anyhow!(
                "move staged favicon directory {} to {}: {error}",
                stage_dir.display(),
                target_dir.display()
            ))
        }
    }
}

/// Remove a favicon directory displaced by a successful publication.
fn cleanup_previous_favicon_dir(previous_dir: &Path) -> Result<()> {
    #[cfg(test)]
    maybe_fail_favicon_old_cleanup()?;
    std::fs::remove_dir_all(previous_dir)
        .with_context(|| format!("remove old favicon directory {}", previous_dir.display()))
}

/// Removes a staging directory on early return unless explicitly disarmed.
struct DirectoryCleanupGuard {
    /// Staging path owned by the guard.
    path: PathBuf,
    /// Whether cleanup remains armed.
    active: bool,
}

impl DirectoryCleanupGuard {
    /// Create an armed cleanup guard for `path`.
    const fn new(path: PathBuf) -> Self {
        Self { path, active: true }
    }

    /// Transfer ownership of the path by disabling drop-time cleanup.
    const fn disarm(&mut self) {
        self.active = false;
    }
}

impl Drop for DirectoryCleanupGuard {
    fn drop(&mut self) {
        if self.active && self.path.exists() {
            if let Err(error) = std::fs::remove_dir_all(&self.path) {
                tracing::warn!(
                    path = %self.path.display(),
                    error = %error,
                    "failed to remove favicon staging directory after error"
                );
            }
        }
    }
}

#[cfg(test)]
fn maybe_fail_favicon_stage_write() -> Result<()> {
    let message = FAVICON_STAGE_WRITE_FAILURE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if let Some(message) = message {
        anyhow::bail!("{message}");
    }
    Ok(())
}

#[cfg(test)]
fn maybe_fail_favicon_old_cleanup() -> Result<()> {
    let message = FAVICON_OLD_CLEANUP_FAILURE
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    if let Some(message) = message {
        anyhow::bail!("{message}");
    }
    Ok(())
}

/// Return the storage directory associated with a favicon scope.
fn scope_dir(scope: FaviconScope<'_>) -> PathBuf {
    match scope {
        FaviconScope::Global => global_favicon_dir(),
        FaviconScope::Board(board_short) => board_favicon_dir(board_short),
    }
}

/// Read a non-empty favicon cache-busting version for a scope.
fn version_for_scope(scope: FaviconScope<'_>) -> Option<String> {
    let path = scope_dir(scope).join("version.txt");
    std::fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty())
}

/// Decode an uploaded favicon and enforce the canonical source dimensions.
fn decode_uploaded_favicon(bytes: &[u8]) -> Result<DynamicImage> {
    let img = image::load_from_memory(bytes).context("decode favicon image")?;
    let (width, height) = img.dimensions();
    if width != 512 || height != 512 {
        anyhow::bail!("Favicon image must be exactly 512x512 pixels.");
    }
    Ok(img)
}

/// Encode a generated favicon size as PNG.
fn write_png(image: &DynamicImage, path: &Path) -> Result<()> {
    image
        .save_with_format(path, ImageFormat::Png)
        .with_context(|| format!("write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::{
        board_favicon_dir, write_favicon_set, FaviconScope, FAVICON_OLD_CLEANUP_FAILURE,
        FAVICON_STAGE_WRITE_FAILURE, FAVICON_TEST_LOCK,
    };
    use anyhow::{Context as _, Result};
    use image::ImageFormat;
    use std::path::{Path, PathBuf};

    fn favicon_png_bytes() -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        image::DynamicImage::new_rgba8(512, 512)
            .write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Png)?;
        Ok(bytes)
    }

    fn matching_dirs(parent: &Path, prefix: &str) -> Vec<PathBuf> {
        std::fs::read_dir(parent)
            .ok()
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with(prefix))
            })
            .collect()
    }

    fn reset_failures() {
        *FAVICON_STAGE_WRITE_FAILURE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        *FAVICON_OLD_CLEANUP_FAILURE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn favicon_stage_dir_is_removed_after_mid_write_failure() -> Result<()> {
        let _guard = FAVICON_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_failures();
        let suffix = uuid::Uuid::new_v4()
            .simple()
            .to_string()
            .chars()
            .take(7)
            .collect::<String>();
        let board_short = format!("f{suffix}");
        let target_dir = board_favicon_dir(&board_short);
        let parent = target_dir
            .parent()
            .context("board favicon directory should have a parent")?
            .to_path_buf();
        drop(std::fs::remove_dir_all(&parent));
        std::fs::create_dir_all(&parent)?;
        *FAVICON_STAGE_WRITE_FAILURE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some("injected favicon write failure".to_owned());

        let bytes = favicon_png_bytes()?;
        let error = write_favicon_set(FaviconScope::Board(&board_short), &bytes)
            .err()
            .context("injected write failure should be returned")?;
        assert!(
            error.to_string().contains("injected favicon write failure"),
            "injected failure context must remain visible"
        );
        assert!(
            matching_dirs(&parent, "._favicon.stage.").is_empty(),
            "failed publication must clean its staging directory"
        );
        assert!(
            !target_dir.exists(),
            "failed first publication must not create the live directory"
        );

        *FAVICON_STAGE_WRITE_FAILURE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        reset_failures();
        drop(std::fs::remove_dir_all(&parent));
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn favicon_old_dir_cleanup_failure_is_reported() -> Result<()> {
        let _guard = FAVICON_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_failures();
        let suffix = uuid::Uuid::new_v4()
            .simple()
            .to_string()
            .chars()
            .take(7)
            .collect::<String>();
        let board_short = format!("f{suffix}");
        let target_dir = board_favicon_dir(&board_short);
        let parent = target_dir
            .parent()
            .context("board favicon directory should have a parent")?
            .to_path_buf();
        drop(std::fs::remove_dir_all(&parent));
        let bytes = favicon_png_bytes()?;
        write_favicon_set(FaviconScope::Board(&board_short), &bytes)?;

        *FAVICON_OLD_CLEANUP_FAILURE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) =
            Some("injected old favicon cleanup failure".to_owned());
        let error = write_favicon_set(FaviconScope::Board(&board_short), &bytes)
            .err()
            .context("cleanup failure should be returned")?;
        assert!(
            error
                .to_string()
                .contains("injected old favicon cleanup failure"),
            "cleanup error context must remain visible"
        );
        assert!(
            target_dir.join("version.txt").exists(),
            "replacement must already be published before old cleanup"
        );
        assert!(
            matching_dirs(&parent, "._favicon.stage.").is_empty(),
            "successful publication must consume its staging directory"
        );
        let old_dirs = matching_dirs(&parent, "._favicon.old.");
        assert_eq!(
            old_dirs.len(),
            1,
            "failed old-directory cleanup must leave one recoverable directory"
        );

        *FAVICON_OLD_CLEANUP_FAILURE
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        reset_failures();
        for old_dir in old_dirs {
            std::fs::remove_dir_all(old_dir)?;
        }
        drop(std::fs::remove_dir_all(&parent));
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn favicon_successful_replacement_leaves_no_stage_or_old_dirs() -> Result<()> {
        let _guard = FAVICON_TEST_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        reset_failures();
        let suffix = uuid::Uuid::new_v4()
            .simple()
            .to_string()
            .chars()
            .take(7)
            .collect::<String>();
        let board_short = format!("f{suffix}");
        let target_dir = board_favicon_dir(&board_short);
        let parent = target_dir
            .parent()
            .context("board favicon directory should have a parent")?
            .to_path_buf();
        drop(std::fs::remove_dir_all(&parent));

        let bytes = favicon_png_bytes()?;
        write_favicon_set(FaviconScope::Board(&board_short), &bytes)?;
        write_favicon_set(FaviconScope::Board(&board_short), &bytes)?;

        assert!(
            target_dir.join("favicon.ico").exists(),
            "replacement must publish the favicon set"
        );
        assert!(
            matching_dirs(&parent, "._favicon.stage.").is_empty(),
            "replacement must leave no staging directories"
        );
        assert!(
            matching_dirs(&parent, "._favicon.old.").is_empty(),
            "replacement must leave no old directories"
        );
        reset_failures();
        drop(std::fs::remove_dir_all(&parent));
        Ok(())
    }
}
