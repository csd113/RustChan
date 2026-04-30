use crate::config::CONFIG;
use anyhow::{Context, Result};
use image::{imageops::FilterType, DynamicImage, GenericImageView, ImageFormat};
use std::path::{Path, PathBuf};

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

#[derive(Clone, Copy)]
pub enum FaviconScope<'a> {
    Global,
    Board(&'a str),
}

pub struct ResolvedFavicon {
    pub base_url: String,
    pub version: String,
}

#[must_use]
pub fn global_favicon_dir() -> PathBuf {
    crate::config::runtime_favicon_dir()
}

#[must_use]
pub fn board_favicon_dir(board_short: &str) -> PathBuf {
    PathBuf::from(&CONFIG.upload_dir)
        .join(board_short)
        .join("_favicon")
}

#[must_use]
pub fn board_has_custom_favicon(board_short: &str) -> bool {
    version_for_scope(FaviconScope::Board(board_short)).is_some()
}

#[must_use]
pub fn global_has_custom_favicon() -> bool {
    version_for_scope(FaviconScope::Global).is_some()
}

#[must_use]
pub fn favicon_version_for_board(board_short: Option<&str>) -> Option<String> {
    board_short
        .and_then(|short| version_for_scope(FaviconScope::Board(short)))
        .or_else(|| version_for_scope(FaviconScope::Global))
}

#[must_use]
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
pub fn global_favicon_file(file_name: &str) -> Option<PathBuf> {
    if !GLOBAL_FILENAMES.contains(&file_name) || file_name == "version.txt" {
        return None;
    }
    let path = global_favicon_dir().join(file_name);
    path.exists().then_some(path)
}

#[must_use]
pub fn global_backup_source_dir() -> PathBuf {
    global_favicon_dir()
}

fn resolve_scope(scope: FaviconScope<'_>) -> Option<ResolvedFavicon> {
    let version = version_for_scope(scope)?;
    let base_url = match scope {
        FaviconScope::Global => String::new(),
        FaviconScope::Board(board_short) => format!("/boards/{board_short}/_favicon"),
    };
    Some(ResolvedFavicon { base_url, version })
}

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
                let _ = std::fs::rename(&previous_dir, target_dir);
            }
            let _ = std::fs::remove_dir_all(stage_dir);
            Err(anyhow::anyhow!(
                "move staged favicon directory {} to {}: {error}",
                stage_dir.display(),
                target_dir.display()
            ))
        }
    }
}

fn cleanup_previous_favicon_dir(previous_dir: &Path) -> Result<()> {
    #[cfg(test)]
    maybe_fail_favicon_old_cleanup()?;
    std::fs::remove_dir_all(previous_dir)
        .with_context(|| format!("remove old favicon directory {}", previous_dir.display()))
}

struct DirectoryCleanupGuard {
    path: PathBuf,
    active: bool,
}

impl DirectoryCleanupGuard {
    const fn new(path: PathBuf) -> Self {
        Self { path, active: true }
    }

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
        .expect("favicon stage write failure mutex")
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
        .expect("favicon old cleanup failure mutex")
        .clone();
    if let Some(message) = message {
        anyhow::bail!("{message}");
    }
    Ok(())
}

fn scope_dir(scope: FaviconScope<'_>) -> PathBuf {
    match scope {
        FaviconScope::Global => global_favicon_dir(),
        FaviconScope::Board(board_short) => board_favicon_dir(board_short),
    }
}

fn version_for_scope(scope: FaviconScope<'_>) -> Option<String> {
    let path = scope_dir(scope).join("version.txt");
    std::fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn decode_uploaded_favicon(bytes: &[u8]) -> Result<DynamicImage> {
    let img = image::load_from_memory(bytes).context("decode favicon image")?;
    let (width, height) = img.dimensions();
    if width != 512 || height != 512 {
        anyhow::bail!("Favicon image must be exactly 512x512 pixels.");
    }
    Ok(img)
}

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
    use image::ImageFormat;

    fn favicon_png_bytes() -> Vec<u8> {
        let mut bytes = Vec::new();
        image::DynamicImage::new_rgba8(512, 512)
            .write_to(&mut std::io::Cursor::new(&mut bytes), ImageFormat::Png)
            .expect("encode favicon png");
        bytes
    }

    fn matching_dirs(parent: &std::path::Path, prefix: &str) -> Vec<std::path::PathBuf> {
        std::fs::read_dir(parent)
            .ok()
            .into_iter()
            .flat_map(std::iter::Iterator::flatten)
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
            .expect("stage failure mutex") = None;
        *FAVICON_OLD_CLEANUP_FAILURE
            .lock()
            .expect("old cleanup failure mutex") = None;
    }

    #[test]
    fn favicon_stage_dir_is_removed_after_mid_write_failure() {
        let _guard = FAVICON_TEST_LOCK.lock().expect("favicon test lock");
        reset_failures();
        let board_short = format!("f{}", &uuid::Uuid::new_v4().simple().to_string()[..7]);
        let target_dir = board_favicon_dir(&board_short);
        let parent = target_dir.parent().expect("target parent").to_path_buf();
        let _ = std::fs::remove_dir_all(&parent);
        std::fs::create_dir_all(&parent).expect("create parent");
        *FAVICON_STAGE_WRITE_FAILURE
            .lock()
            .expect("stage failure mutex") = Some("injected favicon write failure".to_string());

        let error = write_favicon_set(FaviconScope::Board(&board_short), &favicon_png_bytes())
            .expect_err("injected failure");
        assert!(error.to_string().contains("injected favicon write failure"));
        assert!(matching_dirs(&parent, "._favicon.stage.").is_empty());
        assert!(!target_dir.exists());

        *FAVICON_STAGE_WRITE_FAILURE
            .lock()
            .expect("stage failure mutex") = None;
        reset_failures();
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn favicon_old_dir_cleanup_failure_is_reported() {
        let _guard = FAVICON_TEST_LOCK.lock().expect("favicon test lock");
        reset_failures();
        let board_short = format!("f{}", &uuid::Uuid::new_v4().simple().to_string()[..7]);
        let target_dir = board_favicon_dir(&board_short);
        let parent = target_dir.parent().expect("target parent").to_path_buf();
        let _ = std::fs::remove_dir_all(&parent);
        write_favicon_set(FaviconScope::Board(&board_short), &favicon_png_bytes())
            .expect("initial favicon write");

        *FAVICON_OLD_CLEANUP_FAILURE
            .lock()
            .expect("old cleanup failure mutex") =
            Some("injected old favicon cleanup failure".to_string());
        let error = write_favicon_set(FaviconScope::Board(&board_short), &favicon_png_bytes())
            .expect_err("cleanup failure should be visible");
        assert!(error
            .to_string()
            .contains("injected old favicon cleanup failure"));
        assert!(target_dir.join("version.txt").exists());
        assert!(matching_dirs(&parent, "._favicon.stage.").is_empty());
        let old_dirs = matching_dirs(&parent, "._favicon.old.");
        assert_eq!(old_dirs.len(), 1);

        *FAVICON_OLD_CLEANUP_FAILURE
            .lock()
            .expect("old cleanup failure mutex") = None;
        reset_failures();
        for old_dir in old_dirs {
            std::fs::remove_dir_all(old_dir).expect("cleanup old dir");
        }
        let _ = std::fs::remove_dir_all(&parent);
    }

    #[test]
    fn favicon_successful_replacement_leaves_no_stage_or_old_dirs() {
        let _guard = FAVICON_TEST_LOCK.lock().expect("favicon test lock");
        reset_failures();
        let board_short = format!("f{}", &uuid::Uuid::new_v4().simple().to_string()[..7]);
        let target_dir = board_favicon_dir(&board_short);
        let parent = target_dir.parent().expect("target parent").to_path_buf();
        let _ = std::fs::remove_dir_all(&parent);

        write_favicon_set(FaviconScope::Board(&board_short), &favicon_png_bytes())
            .expect("initial write");
        write_favicon_set(FaviconScope::Board(&board_short), &favicon_png_bytes())
            .expect("replacement write");

        assert!(target_dir.join("favicon.ico").exists());
        assert!(matching_dirs(&parent, "._favicon.stage.").is_empty());
        assert!(matching_dirs(&parent, "._favicon.old.").is_empty());
        reset_failures();
        let _ = std::fs::remove_dir_all(&parent);
    }
}
