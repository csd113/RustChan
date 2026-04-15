use crate::config::CONFIG;
use crate::db;
use crate::models::{
    BannerAsset, BannerPlacement, BannerScope, BannerTargetType, Board, BoardBannerMode,
};
use anyhow::{Context, Result};
use image::{imageops::FilterType, DynamicImage, GenericImageView, ImageFormat};
use std::path::{Path, PathBuf};

pub const DISPLAY_WIDTH: u32 = 468;
pub const DISPLAY_HEIGHT: u32 = 60;
pub const MIN_WIDTH: u32 = DISPLAY_WIDTH;
pub const MIN_HEIGHT: u32 = DISPLAY_HEIGHT;
pub const RECOMMENDED_WIDTH: u32 = DISPLAY_WIDTH * 2;
pub const RECOMMENDED_HEIGHT: u32 = DISPLAY_HEIGHT * 2;

#[derive(Debug, Clone)]
pub struct BannerSiteSettings {
    pub allow_external_links: bool,
    pub rotation_interval_minutes: i64,
}

#[derive(Debug, Clone)]
pub struct ResolvedBanner {
    pub asset: BannerAsset,
    pub image_url: String,
    pub href: Option<String>,
    pub alt: String,
}

#[derive(Debug, Clone)]
pub struct BannerSelection {
    pub banner: Option<ResolvedBanner>,
    pub etag_fragment: String,
    pub disable_not_modified_short_circuit: bool,
}

#[must_use]
pub fn runtime_banner_dir() -> PathBuf {
    crate::config::runtime_dir().join("banner")
}

#[must_use]
pub fn global_banner_dir() -> PathBuf {
    runtime_banner_dir().join("global")
}

#[must_use]
pub fn home_banner_dir() -> PathBuf {
    runtime_banner_dir().join("home")
}

#[must_use]
pub fn board_banner_dir(board_short: &str) -> PathBuf {
    PathBuf::from(&CONFIG.upload_dir)
        .join(board_short)
        .join("_banner")
}

#[must_use]
pub fn backup_source_dir() -> PathBuf {
    runtime_banner_dir()
}

#[must_use]
pub fn banner_asset_path(asset: &BannerAsset) -> PathBuf {
    match asset.scope {
        BannerScope::Board => board_banner_dir(asset.board_short.as_deref().unwrap_or_default())
            .join(format!("{}.webp", asset.storage_key)),
        BannerScope::Global => global_banner_dir().join(format!("{}.webp", asset.storage_key)),
        BannerScope::Home => home_banner_dir().join(format!("{}.webp", asset.storage_key)),
    }
}

#[must_use]
pub fn banner_asset_url(asset: &BannerAsset) -> String {
    format!("/banner/assets/{}?v={}", asset.id, asset.created_at)
}

pub fn write_banner_asset(asset: &BannerAsset, bytes: &[u8]) -> Result<(u32, u32, u64)> {
    let img = decode_uploaded_banner(bytes)?;
    let target_path = banner_asset_path(asset);
    if let Some(parent) = target_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create banner directory {}", parent.display()))?;
    }

    let stored_dimensions = if is_animated_gif(bytes) {
        (img.width(), img.height())
    } else {
        let processed = normalize_banner_image(&img);
        let dimensions = (processed.width(), processed.height());
        processed
            .save_with_format(&target_path, ImageFormat::WebP)
            .with_context(|| format!("write {}", target_path.display()))?;
        dimensions
    };

    if is_animated_gif(bytes) {
        write_animated_gif_banner_asset(bytes, &target_path)?;
    }

    let metadata = std::fs::metadata(&target_path)
        .with_context(|| format!("stat {}", target_path.display()))?;
    Ok((stored_dimensions.0, stored_dimensions.1, metadata.len()))
}

pub fn delete_banner_asset_file(asset: &BannerAsset) -> Result<()> {
    let path = banner_asset_path(asset);
    if path.exists() {
        std::fs::remove_file(&path).with_context(|| format!("remove {}", path.display()))?;
    }
    Ok(())
}

pub fn resolve_banner_href(
    asset: &BannerAsset,
    allow_external_links: bool,
    current_path: &str,
) -> Option<String> {
    match asset.target_type {
        BannerTargetType::None => None,
        BannerTargetType::InternalBoard => normalize_internal_board_path(&asset.target_value),
        BannerTargetType::InternalPath => normalize_internal_path(&asset.target_value),
        BannerTargetType::ExternalUrl => {
            if !allow_external_links || normalize_external_url(&asset.target_value).is_none() {
                None
            } else {
                Some(format!(
                    "/banner/external/{}?return_to={}",
                    asset.id,
                    crate::templates::urlencoding_simple(&safe_return_to(current_path))
                ))
            }
        }
    }
}

#[must_use]
pub fn safe_return_to(path: &str) -> String {
    if path.starts_with('/') && !path.starts_with("//") && !path.starts_with("/\\") {
        path.to_string()
    } else {
        "/".to_string()
    }
}

#[must_use]
pub fn normalize_internal_path(path: &str) -> Option<String> {
    let trimmed = path.trim();
    if trimmed.starts_with('/') && !trimmed.starts_with("//") && !trimmed.starts_with("/\\") {
        Some(trimmed.to_string())
    } else {
        None
    }
}

#[must_use]
pub fn normalize_internal_board_path(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_matches('/');
    let valid = !trimmed.is_empty()
        && trimmed.len() <= 8
        && trimmed.bytes().all(|byte| byte.is_ascii_alphanumeric());
    valid.then(|| format!("/{trimmed}/"))
}

#[must_use]
pub fn normalize_external_url(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let parsed = reqwest::Url::parse(trimmed).ok()?;
    matches!(parsed.scheme(), "http" | "https").then(|| parsed.to_string())
}

#[must_use]
pub fn should_show_on_placement(asset: &BannerAsset, placement: BannerPlacement) -> bool {
    match placement {
        BannerPlacement::Index => asset.show_on_index,
        BannerPlacement::Catalog => asset.show_on_catalog,
    }
}

#[must_use]
pub fn choose_active_banner(
    candidates: &[BannerAsset],
    settings: &BannerSiteSettings,
) -> (Option<BannerAsset>, String, bool) {
    if candidates.is_empty() {
        return (None, "none".to_string(), false);
    }
    if candidates.len() == 1 {
        let only_asset = candidates.first().cloned();
        return (
            only_asset.clone(),
            only_asset.as_ref().map_or_else(
                || "none".to_string(),
                |asset| format!("single-{}", asset.id),
            ),
            false,
        );
    }
    if settings.rotation_interval_minutes > 0 {
        let bucket = chrono::Utc::now().timestamp()
            / settings
                .rotation_interval_minutes
                .saturating_mul(60)
                .max(60);
        let index = usize::try_from(bucket.rem_euclid(candidates.len() as i64)).unwrap_or(0);
        let asset = candidates.get(index).cloned();
        let fragment = asset.as_ref().map_or_else(
            || "none".to_string(),
            |item| format!("timer-{bucket}-{}", item.id),
        );
        return (asset, fragment, false);
    }

    let index = (uuid::Uuid::new_v4().as_u128() % candidates.len() as u128) as usize;
    let asset = candidates.get(index).cloned();
    let fragment = asset
        .as_ref()
        .map_or_else(|| "none".to_string(), |item| format!("refresh-{}", item.id));
    (asset, fragment, true)
}

pub fn load_site_banner_settings(conn: &rusqlite::Connection) -> BannerSiteSettings {
    BannerSiteSettings {
        allow_external_links: db::get_banner_external_links_enabled(conn),
        rotation_interval_minutes: db::get_banner_rotation_interval_minutes(conn),
    }
}

pub fn resolve_home_banner(
    conn: &rusqlite::Connection,
    current_path: &str,
) -> Result<BannerSelection> {
    let settings = load_site_banner_settings(conn);
    let assets = db::list_banner_assets_for_scope(conn, BannerScope::Home)?
        .into_iter()
        .filter(|asset| asset.enabled)
        .collect::<Vec<_>>();
    Ok(resolve_from_candidates(
        assets,
        &settings,
        current_path,
        "home banner",
    ))
}

pub fn resolve_board_banner(
    conn: &rusqlite::Connection,
    board: &Board,
    placement: BannerPlacement,
    current_path: &str,
) -> Result<BannerSelection> {
    let settings = load_site_banner_settings(conn);
    let board_assets = db::list_banner_assets_for_board(conn, board.id)?;
    let candidates = match board.banner_mode {
        BoardBannerMode::None => Vec::new(),
        BoardBannerMode::Override => board_assets,
        BoardBannerMode::Inherit => {
            if board_assets.is_empty() {
                db::list_banner_assets_for_scope(conn, BannerScope::Global)?
            } else {
                tracing::info!(
                    target: "banner",
                    board = %board.short_name,
                    "Board has uploaded banners while still set to inherit; using board banners"
                );
                board_assets
            }
        }
    }
    .into_iter()
    .filter(|asset| asset.enabled && should_show_on_placement(asset, placement))
    .collect::<Vec<_>>();

    Ok(resolve_from_candidates(
        candidates,
        &settings,
        current_path,
        &format!("/{}/ banner", board.short_name),
    ))
}

#[must_use]
pub fn render_banner_html(
    selection: &BannerSelection,
    wrapper_class: &str,
    image_class: &str,
) -> String {
    let Some(banner) = &selection.banner else {
        return String::new();
    };
    let image_html = format!(
        r#"<img class="{image_class}" src="{src}" alt="{alt}" width="{width}" height="{height}" loading="eager" data-banner-id="{banner_id}">"#,
        image_class = image_class,
        src = crate::utils::sanitize::escape_html(&banner.image_url),
        alt = crate::utils::sanitize::escape_html(&banner.alt),
        width = DISPLAY_WIDTH,
        height = DISPLAY_HEIGHT,
        banner_id = banner.asset.id,
    );
    let inner = if let Some(href) = &banner.href {
        format!(
            r#"<a class="banner-link" href="{href}">{image}</a>"#,
            href = crate::utils::sanitize::escape_html(href),
            image = image_html
        )
    } else {
        image_html
    };
    format!(r#"<div class="{wrapper_class}">{inner}</div>"#)
}

fn resolve_from_candidates(
    candidates: Vec<BannerAsset>,
    settings: &BannerSiteSettings,
    current_path: &str,
    alt: &str,
) -> BannerSelection {
    let (asset, etag_fragment, disable_not_modified_short_circuit) =
        choose_active_banner(&candidates, settings);
    let banner = asset.map(|asset| ResolvedBanner {
        image_url: banner_asset_url(&asset),
        href: resolve_banner_href(&asset, settings.allow_external_links, current_path),
        alt: alt.to_string(),
        asset,
    });
    BannerSelection {
        banner,
        etag_fragment,
        disable_not_modified_short_circuit,
    }
}

fn write_animated_gif_banner_asset(bytes: &[u8], target_path: &Path) -> Result<()> {
    if !crate::media::ffmpeg::detect_ffmpeg() {
        anyhow::bail!(
            "Animated GIF banners require ffmpeg so they can be converted to animated WebP."
        );
    }
    if !crate::media::ffmpeg::check_webp_encoder() {
        anyhow::bail!("Animated GIF banners require an ffmpeg build with libwebp support.");
    }

    let input_path = animated_gif_temp_path(target_path);
    std::fs::write(&input_path, bytes)
        .with_context(|| format!("write {}", input_path.display()))?;

    let conversion = crate::media::ffmpeg::ffmpeg_image_to_webp(&input_path, target_path)
        .with_context(|| format!("convert animated gif banner {}", input_path.display()));
    let _ = std::fs::remove_file(&input_path);
    conversion
}

fn animated_gif_temp_path(target_path: &Path) -> PathBuf {
    let parent = target_path
        .parent()
        .map_or_else(runtime_banner_dir, Path::to_path_buf);
    parent.join(format!(
        "{}-animated-input.gif",
        uuid::Uuid::new_v4().simple()
    ))
}

fn decode_uploaded_banner(bytes: &[u8]) -> Result<DynamicImage> {
    let img = image::load_from_memory(bytes).context("decode banner image")?;
    let (width, height) = img.dimensions();
    if width < MIN_WIDTH || height < MIN_HEIGHT {
        anyhow::bail!("Banner image must be at least {MIN_WIDTH}x{MIN_HEIGHT} pixels.");
    }
    let width_u64 = u64::from(width);
    let height_u64 = u64::from(height);
    if width_u64.saturating_mul(u64::from(DISPLAY_HEIGHT))
        != height_u64.saturating_mul(u64::from(DISPLAY_WIDTH))
    {
        anyhow::bail!(
            "Banner image must use the exact {}:{} aspect ratio.",
            DISPLAY_WIDTH,
            DISPLAY_HEIGHT
        );
    }
    Ok(img)
}

fn normalize_banner_image(img: &DynamicImage) -> DynamicImage {
    let (width, height) = img.dimensions();
    if width > RECOMMENDED_WIDTH || height > RECOMMENDED_HEIGHT {
        img.resize_exact(RECOMMENDED_WIDTH, RECOMMENDED_HEIGHT, FilterType::Lanczos3)
    } else {
        img.clone()
    }
}

fn is_animated_gif(bytes: &[u8]) -> bool {
    image::guess_format(bytes).ok() == Some(ImageFormat::Gif) && has_multiple_gif_frames(bytes)
}

fn has_multiple_gif_frames(bytes: &[u8]) -> bool {
    let mut frame_markers = 0usize;
    for window in bytes.windows(2) {
        if window == [0x21, 0xF9] {
            frame_markers += 1;
            if frame_markers > 1 {
                return true;
            }
        }
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{has_multiple_gif_frames, is_animated_gif};

    #[test]
    fn detects_multiple_gif_frames() {
        let bytes = b"GIF89a\x21\xF9\x04\x00\x00\x00\x00\x00\x21\xF9\x04\x00\x00\x00\x00\x00";
        assert!(has_multiple_gif_frames(bytes));
        assert!(is_animated_gif(bytes));
    }

    #[test]
    fn ignores_single_frame_gifs() {
        let bytes = b"GIF89a\x21\xF9\x04\x00\x00\x00\x00\x00";
        assert!(!has_multiple_gif_frames(bytes));
        assert!(!is_animated_gif(bytes));
    }
    use super::{normalize_external_url, normalize_internal_board_path, normalize_internal_path};

    #[test]
    fn internal_path_rejects_protocol_relative_values() {
        assert!(normalize_internal_path("//evil.test").is_none());
        assert!(normalize_internal_path("/safe/path").is_some());
    }

    #[test]
    fn internal_board_path_requires_valid_short_name() {
        assert_eq!(
            normalize_internal_board_path("out"),
            Some("/out/".to_string())
        );
        assert!(normalize_internal_board_path("../etc").is_none());
    }

    #[test]
    fn external_url_allows_http_and_https_only() {
        assert!(normalize_external_url("https://example.com").is_some());
        assert!(normalize_external_url("javascript:alert(1)").is_none());
    }
}
