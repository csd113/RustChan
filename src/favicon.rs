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

#[derive(Clone, Copy)]
pub enum FaviconScope<'a> {
    Global,
    Board(&'a str),
}

pub struct ResolvedFavicon {
    pub base_url: String,
    pub version: String,
}

pub fn global_favicon_dir() -> PathBuf {
    data_dir().join("favicon")
}

pub fn board_favicon_dir(board_short: &str) -> PathBuf {
    PathBuf::from(&CONFIG.upload_dir)
        .join(board_short)
        .join("_favicon")
}

pub fn board_has_custom_favicon(board_short: &str) -> bool {
    version_for_scope(FaviconScope::Board(board_short)).is_some()
}

pub fn global_has_custom_favicon() -> bool {
    version_for_scope(FaviconScope::Global).is_some()
}

pub fn favicon_head_html(board_short: Option<&str>) -> String {
    let resolved = board_short
        .and_then(|short| resolve_scope(FaviconScope::Board(short)))
        .or_else(|| resolve_scope(FaviconScope::Global));
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

pub fn write_favicon_set(scope: FaviconScope<'_>, bytes: &[u8]) -> Result<()> {
    let img = decode_uploaded_favicon(bytes)?;
    let target_dir = scope_dir(scope);
    let stage_dir = staging_dir_for(&target_dir)?;
    std::fs::create_dir_all(&stage_dir)
        .with_context(|| format!("create favicon staging directory {}", stage_dir.display()))?;

    write_png(
        img.resize_exact(16, 16, FilterType::Lanczos3),
        &stage_dir.join("favicon-16x16.png"),
    )?;
    write_png(
        img.resize_exact(32, 32, FilterType::Lanczos3),
        &stage_dir.join("favicon-32x32.png"),
    )?;
    write_png(
        img.resize_exact(180, 180, FilterType::Lanczos3),
        &stage_dir.join("apple-touch-icon.png"),
    )?;
    write_png(
        img.resize_exact(192, 192, FilterType::Lanczos3),
        &stage_dir.join("android-chrome-192x192.png"),
    )?;
    write_png(
        img.resize_exact(512, 512, FilterType::Lanczos3),
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
    Ok(())
}

pub fn clear_board_favicon(board_short: &str) -> Result<()> {
    let dir = board_favicon_dir(board_short);
    if dir.exists() {
        std::fs::remove_dir_all(&dir)
            .with_context(|| format!("remove board favicon directory {}", dir.display()))?;
    }
    Ok(())
}

pub fn global_favicon_file(file_name: &str) -> Option<PathBuf> {
    if !GLOBAL_FILENAMES.contains(&file_name) || file_name == "version.txt" {
        return None;
    }
    let path = global_favicon_dir().join(file_name);
    path.exists().then_some(path)
}

pub fn global_backup_source_dir() -> PathBuf {
    global_favicon_dir()
}

fn data_dir() -> PathBuf {
    PathBuf::from(&CONFIG.database_path)
        .parent()
        .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
}

fn resolve_scope(scope: FaviconScope<'_>) -> Option<ResolvedFavicon> {
    let version = version_for_scope(scope)?;
    let base_url = match scope {
        FaviconScope::Global => String::new(),
        FaviconScope::Board(board_short) => format!("/boards/{board_short}/_favicon"),
    };
    Some(ResolvedFavicon { base_url, version })
}

fn staging_dir_for(target_dir: &Path) -> Result<PathBuf> {
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
    Ok(stage_dir)
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
                let _ = std::fs::remove_dir_all(&previous_dir);
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

fn write_png(image: DynamicImage, path: &Path) -> Result<()> {
    image
        .save_with_format(path, ImageFormat::Png)
        .with_context(|| format!("write {}", path.display()))
}
