use crate::config::CONFIG;
use crate::db;
use crate::error::{AppError, Result as AppResult};
use crate::models::{
    BannerAsset, BannerPlacement, BannerScope, BannerTargetType, Board, BoardBannerMode,
};
use anyhow::{Context as _, Result};
use image::{imageops::FilterType, DynamicImage, GenericImageView as _, ImageFormat, ImageReader};
use std::{
    io::Cursor,
    path::{Component, Path, PathBuf},
};

/// Width used when displaying banners in HTML.
pub const DISPLAY_WIDTH: u32 = 468;
/// Height used when displaying banners in HTML.
pub const DISPLAY_HEIGHT: u32 = 60;
/// Minimum accepted source-image width.
pub const MIN_WIDTH: u32 = DISPLAY_WIDTH;
/// Minimum accepted source-image height.
pub const MIN_HEIGHT: u32 = DISPLAY_HEIGHT;
/// Recommended high-density source-image width.
pub const RECOMMENDED_WIDTH: u32 = DISPLAY_WIDTH * 2;
/// Recommended high-density source-image height.
pub const RECOMMENDED_HEIGHT: u32 = DISPLAY_HEIGHT * 2;
/// Maximum accepted source-image width.
pub const MAX_WIDTH: u32 = 4096;
/// Maximum accepted source-image height.
pub const MAX_HEIGHT: u32 = 1024;
/// Maximum accepted decoded source-image area.
pub const MAX_PIXELS: u64 = 4_194_304;
/// Maximum accepted frame count for an animated GIF.
pub const MAX_ANIMATED_GIF_FRAMES: usize = 60;

#[derive(Debug, Clone, Copy)]
/// Dimensions and animation metadata collected before banner decoding.
pub struct BannerImagePreflight {
    /// Source-image width in pixels.
    pub width: u32,
    /// Source-image height in pixels.
    pub height: u32,
    /// Frame count when the source is an animated GIF.
    pub animated_gif_frames: Option<usize>,
    /// Whether the source is an animated WebP image.
    pub animated_webp: bool,
}

#[derive(Debug, Clone)]
/// Target-form values derived from one stored banner target.
pub struct BannerTargetDraft {
    /// Internal-board form value.
    pub board_value: String,
    /// Internal-path form value.
    pub thread_value: String,
    /// External-URL form value.
    pub external_url: String,
}

#[derive(Debug, Clone)]
/// Site-wide settings that influence banner resolution.
pub struct BannerSiteSettings {
    /// Whether banners may link to external HTTP(S) destinations.
    pub allow_external_links: bool,
    /// Rotation interval, or zero for per-request random selection.
    pub rotation_interval_minutes: i64,
}

#[derive(Debug, Clone)]
/// Banner asset with the URLs and alternative text needed for rendering.
pub struct ResolvedBanner {
    /// Persistent banner record.
    pub asset: BannerAsset,
    /// Versioned local URL for the banner image.
    pub image_url: String,
    /// Validated optional link destination.
    pub href: Option<String>,
    /// Alternative text exposed to assistive technology.
    pub alt: String,
}

#[derive(Debug, Clone)]
/// Result of selecting a banner for one response.
pub struct BannerSelection {
    /// Selected banner, or `None` when the placement has no candidates.
    pub banner: Option<ResolvedBanner>,
    /// Stable fragment included in response `ETag` construction.
    pub etag_fragment: String,
    /// Whether random selection prevents a `304 Not Modified` shortcut.
    pub disable_not_modified_short_circuit: bool,
}

#[must_use]
/// Return the runtime directory that owns non-board banner assets.
pub fn runtime_banner_dir() -> PathBuf {
    crate::config::runtime_dir().join("banner")
}

#[must_use]
/// Return the directory containing global fallback banners.
pub fn global_banner_dir() -> PathBuf {
    runtime_banner_dir().join("global")
}

#[must_use]
/// Return the directory containing home-page banners.
pub fn home_banner_dir() -> PathBuf {
    runtime_banner_dir().join("home")
}

#[must_use]
/// Return the banner directory for a board short name.
pub fn board_banner_dir(board_short: &str) -> PathBuf {
    PathBuf::from(&CONFIG.upload_dir)
        .join(board_short)
        .join("_banner")
}

#[must_use]
/// Return the banner directory included in full backups.
pub fn backup_source_dir() -> PathBuf {
    runtime_banner_dir()
}

#[must_use]
/// Map a banner form anchor to the containing collapsible settings section.
pub fn banner_open_section(anchor: &str) -> &str {
    match anchor {
        "global-banners" | "home-banners" => "board-banners",
        _ if anchor.starts_with("board-appearance-") => "board-banners",
        _ => anchor,
    }
}

#[must_use]
/// Build the admin-page anchor for a board's banner settings.
pub fn board_appearance_anchor(board_short: &str) -> String {
    format!("board-appearance-{board_short}")
}

#[must_use]
/// Build the admin-page anchor for a banner scope.
pub fn banner_admin_anchor(scope: BannerScope, board_short: Option<&str>) -> String {
    match scope {
        BannerScope::Global => "global-banners".to_owned(),
        BannerScope::Home => "home-banners".to_owned(),
        BannerScope::Board => board_appearance_anchor(board_short.unwrap_or_default()),
    }
}

/// Resolve the on-disk path for a banner asset.
///
/// # Errors
/// Returns an error if the stored banner key is not canonical.
pub fn banner_asset_path(asset: &BannerAsset) -> Result<PathBuf> {
    let webp_path = banner_storage_path(
        asset.scope,
        asset.board_short.as_deref(),
        &asset.storage_key,
    )?;
    let gif_path = banner_gif_fallback_path(&webp_path);
    if gif_path.exists() && !webp_path.exists() {
        Ok(gif_path)
    } else {
        Ok(webp_path)
    }
}

#[must_use]
/// Infer the HTTP content type of a stored banner path.
pub fn banner_asset_content_type(path: &Path) -> &'static str {
    match path.extension().and_then(|ext| ext.to_str()) {
        Some("gif") => "image/gif",
        _ => "image/webp",
    }
}

/// Build the storage path for a banner asset.
///
/// # Errors
/// Returns an error if the board path is missing or the storage key is invalid.
pub fn banner_storage_path(
    scope: BannerScope,
    board_short: Option<&str>,
    storage_key: &str,
) -> Result<PathBuf> {
    let file_name = banner_storage_file_name(storage_key)?;
    let path = match scope {
        BannerScope::Board => {
            let board_short = board_short
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .ok_or_else(|| anyhow::anyhow!("banner board path is missing"))?;
            board_banner_dir(board_short).join(file_name)
        }
        BannerScope::Global => global_banner_dir().join(file_name),
        BannerScope::Home => home_banner_dir().join(file_name),
    };
    Ok(path)
}

/// Convert a validated storage key into its primary `.webp` file name.
///
/// # Errors
/// Returns an error if the storage key is not canonical.
pub fn banner_storage_file_name(storage_key: &str) -> Result<String> {
    validate_banner_storage_key(storage_key)?;
    Ok(format!("{storage_key}.webp"))
}

/// Check that a banner storage key is a canonical UUID hex string.
///
/// # Errors
/// Returns an error if the key is empty, non-hexadecimal, or not canonical.
pub fn validate_banner_storage_key(storage_key: &str) -> Result<()> {
    let trimmed = storage_key.trim();
    if trimmed.is_empty()
        || trimmed.len() != 32
        || trimmed.bytes().any(|byte| !byte.is_ascii_hexdigit())
    {
        anyhow::bail!("Banner storage key must be a canonical 32-character hexadecimal UUID.");
    }
    let uuid = uuid::Uuid::parse_str(trimmed)
        .context("Banner storage key must be a canonical 32-character hexadecimal UUID.")?;
    let canonical = uuid.simple().to_string();
    if canonical != trimmed {
        anyhow::bail!("Banner storage key must use canonical lowercase hex form.");
    }
    Ok(())
}

/// Validate a banner restore entry path and return its canonical relative form.
///
/// # Errors
/// Returns an error if the entry escapes the banner tree or uses a non-canonical name.
pub fn validate_banner_restore_entry_name(name: &str) -> Result<String> {
    let rel = Path::new(name);
    if rel.is_absolute()
        || rel.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::CurDir | Component::Prefix(_)
            )
        })
    {
        anyhow::bail!("Suspicious banner restore entry path.");
    }

    let mut components = rel.components();
    let scope = components
        .next()
        .and_then(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .ok_or_else(|| anyhow::anyhow!("Banner restore entry name is not valid UTF-8."))?;
    if scope != "global" && scope != "home" {
        anyhow::bail!("Banner restore entries must live under global/ or home/.");
    }
    let file_name = components
        .next()
        .and_then(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .ok_or_else(|| anyhow::anyhow!("Banner restore entry name is not valid UTF-8."))?;
    let extension = Path::new(file_name)
        .extension()
        .and_then(|ext| ext.to_str());
    if components.next().is_some() || !matches!(extension, Some("webp" | "gif")) {
        anyhow::bail!("Banner restore entries must be scoped .webp or .gif files.");
    }

    let stem = Path::new(file_name)
        .file_stem()
        .and_then(|value| value.to_str())
        .ok_or_else(|| anyhow::anyhow!("Banner restore entry name is not valid UTF-8."))?;
    validate_banner_storage_key(stem)?;
    Ok(format!("{scope}/{file_name}"))
}

#[must_use]
/// Build the versioned public image URL for a banner asset.
pub fn banner_asset_url(asset: &BannerAsset) -> String {
    format!("/banner/assets/{}?v={}", asset.id, asset.created_at)
}

#[must_use]
/// Split one stored target into the mutually exclusive admin form fields.
pub fn banner_target_draft(target_type: BannerTargetType, target_value: &str) -> BannerTargetDraft {
    BannerTargetDraft {
        board_value: banner_target_value(
            target_type,
            BannerTargetType::InternalBoard,
            target_value,
        ),
        thread_value: banner_target_value(
            target_type,
            BannerTargetType::InternalPath,
            target_value,
        ),
        external_url: banner_target_value(target_type, BannerTargetType::ExternalUrl, target_value),
    }
}

/// Return `target_value` only when its field type is selected.
fn banner_target_value(
    selected_type: BannerTargetType,
    field_type: BannerTargetType,
    target_value: &str,
) -> String {
    if selected_type == field_type {
        target_value.to_owned()
    } else {
        String::new()
    }
}

#[must_use]
/// Select and normalize the submitted value for the chosen target type.
pub fn select_banner_target_value(
    target_type_raw: &str,
    target_value_raw: Option<&str>,
    target_board_value_raw: Option<&str>,
    target_thread_value_raw: Option<&str>,
    target_external_url_raw: Option<&str>,
) -> String {
    match BannerTargetType::from_db_str(target_type_raw.trim()).unwrap_or(BannerTargetType::None) {
        BannerTargetType::None => String::new(),
        BannerTargetType::InternalBoard => target_board_value_raw
            .unwrap_or_else(|| target_value_raw.unwrap_or_default())
            .trim()
            .to_owned(),
        BannerTargetType::InternalPath => target_thread_value_raw
            .unwrap_or_else(|| target_value_raw.unwrap_or_default())
            .trim()
            .to_owned(),
        BannerTargetType::ExternalUrl => target_external_url_raw
            .unwrap_or_else(|| target_value_raw.unwrap_or_default())
            .trim()
            .to_owned(),
    }
}

/// Parse and validate a banner target selection.
///
/// # Errors
/// Returns an error if the target type or value is invalid for the configured rules.
pub fn parse_banner_target(
    target_type_raw: &str,
    target_value_raw: &str,
    allow_external_links: bool,
) -> AppResult<(BannerTargetType, String)> {
    let target_type = BannerTargetType::from_db_str(target_type_raw.trim())
        .ok_or_else(|| AppError::BadRequest("Invalid banner target type.".into()))?;
    let target_value = target_value_raw.trim();
    match target_type {
        BannerTargetType::None => Ok((BannerTargetType::None, String::new())),
        BannerTargetType::InternalBoard => {
            let board_path = normalize_internal_board_path(target_value).ok_or_else(|| {
                AppError::BadRequest("Internal board link must be a valid board short name.".into())
            })?;
            Ok((
                BannerTargetType::InternalBoard,
                board_path.trim_matches('/').to_owned(),
            ))
        }
        BannerTargetType::InternalPath => {
            let path = normalize_internal_path(target_value).ok_or_else(|| {
                AppError::BadRequest("Internal path must begin with a single '/'.".into())
            })?;
            Ok((BannerTargetType::InternalPath, path))
        }
        BannerTargetType::ExternalUrl => {
            if !allow_external_links {
                return Err(AppError::BadRequest(
                    "External banner links are disabled in site settings.".into(),
                ));
            }
            let url = normalize_external_url(target_value).ok_or_else(|| {
                AppError::BadRequest(
                    "External banner links must use a valid http/https URL.".into(),
                )
            })?;
            Ok((BannerTargetType::ExternalUrl, url))
        }
    }
}

/// Canonicalize and write a banner asset to its final storage path.
///
/// # Errors
/// Returns an error if the image is invalid or cannot be written.
pub fn write_banner_asset(asset: &BannerAsset, bytes: &[u8]) -> Result<(u32, u32, u64)> {
    canonicalize_banner_bytes(bytes, &banner_asset_path(asset)?)
}

/// Validate, normalize, and write banner bytes to disk.
///
/// # Errors
/// Returns an error if the image is malformed, too large, or cannot be written.
pub fn canonicalize_banner_bytes(bytes: &[u8], target_path: &Path) -> Result<(u32, u32, u64)> {
    let preflight = preflight_banner_bytes(bytes)?;
    if let Some(parent) = target_path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create banner directory {}", parent.display()))?;
    }

    let stored_dimensions = if preflight.animated_gif_frames.is_some() {
        write_animated_gif_banner_asset_scaled(
            bytes,
            target_path,
            preflight.width,
            preflight.height,
        )?
    } else if preflight.animated_webp {
        write_animated_webp_banner_asset(bytes, target_path, preflight.width, preflight.height)?
    } else {
        let img = decode_uploaded_banner(bytes, &preflight)?;
        let processed = normalize_banner_image(&img);
        let dimensions = (processed.width(), processed.height());
        processed
            .save_with_format(target_path, ImageFormat::WebP)
            .with_context(|| format!("write {}", target_path.display()))?;
        dimensions
    };

    let stored_path = stored_banner_path_after_write(target_path);
    let metadata = std::fs::metadata(&stored_path)
        .with_context(|| format!("stat {}", stored_path.display()))?;
    Ok((stored_dimensions.0, stored_dimensions.1, metadata.len()))
}

/// Delete a banner asset file if it exists.
///
/// # Errors
/// Returns an error if the path cannot be derived or the file cannot be removed.
pub fn delete_banner_asset_file(asset: &BannerAsset) -> Result<()> {
    let webp_path = banner_storage_path(
        asset.scope,
        asset.board_short.as_deref(),
        &asset.storage_key,
    )?;
    let gif_path = banner_gif_fallback_path(&webp_path);
    for path in [&webp_path, &gif_path] {
        if path.exists() {
            std::fs::remove_file(path).with_context(|| format!("remove {}", path.display()))?;
        }
    }
    Ok(())
}

/// Resolve the target href for a banner, if any.
#[must_use]
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
                    crate::utils::redirect::encode_form_query_component(&safe_return_to(
                        current_path
                    ))
                ))
            }
        }
    }
}

#[must_use]
/// Return an internal fallback path safe to place in `return_to`.
pub fn safe_return_to(path: &str) -> String {
    crate::utils::redirect::safe_internal_path_or(Some(path), "/").to_owned()
}

#[must_use]
/// Normalize a minimally safe absolute internal path.
pub fn normalize_internal_path(path: &str) -> Option<String> {
    let trimmed = path.trim();
    if crate::utils::redirect::is_basic_safe_internal_path(trimmed) {
        Some(trimmed.to_owned())
    } else {
        None
    }
}

#[must_use]
/// Normalize a board short name into its canonical internal board path.
pub fn normalize_internal_board_path(value: &str) -> Option<String> {
    let trimmed = value.trim().trim_matches('/');
    let valid = !trimmed.is_empty()
        && trimmed.len() <= 8
        && trimmed.bytes().all(|byte| byte.is_ascii_alphanumeric());
    valid.then(|| format!("/{trimmed}/"))
}

#[must_use]
/// Normalize an external HTTP(S) URL.
pub fn normalize_external_url(value: &str) -> Option<String> {
    let trimmed = value.trim();
    let parsed = reqwest::Url::parse(trimmed).ok()?;
    matches!(parsed.scheme(), "http" | "https").then(|| parsed.to_string())
}

#[must_use]
/// Return whether an asset is enabled for the requested page placement.
pub const fn should_show_on_placement(asset: &BannerAsset, placement: BannerPlacement) -> bool {
    match placement {
        BannerPlacement::Index => asset.show_on_index,
        BannerPlacement::Catalog => asset.show_on_catalog,
    }
}

#[must_use]
/// Choose an active asset and cache fragment from eligible candidates.
pub fn choose_active_banner(
    candidates: &[BannerAsset],
    settings: &BannerSiteSettings,
) -> (Option<BannerAsset>, String, bool) {
    if candidates.is_empty() {
        return (None, "none".to_owned(), false);
    }
    if candidates.len() == 1 {
        let only_asset = candidates.first().cloned();
        return (
            only_asset.clone(),
            only_asset
                .as_ref()
                .map_or_else(|| "none".to_owned(), |asset| format!("single-{}", asset.id)),
            false,
        );
    }
    if settings.rotation_interval_minutes > 0 {
        let bucket = chrono::Utc::now().timestamp()
            / settings
                .rotation_interval_minutes
                .saturating_mul(60)
                .max(60);
        let len = i64::try_from(candidates.len()).unwrap_or(1);
        let index = usize::try_from(bucket.rem_euclid(len)).unwrap_or(0);
        let asset = candidates.get(index).cloned();
        let fragment = asset.as_ref().map_or_else(
            || "none".to_owned(),
            |item| format!("timer-{bucket}-{}", item.id),
        );
        return (asset, fragment, false);
    }

    let nonce = uuid::Uuid::new_v4().as_u128();
    let len = u128::try_from(candidates.len()).unwrap_or(1);
    let index = usize::try_from(nonce % len).unwrap_or(0);
    let asset = candidates.get(index).cloned();
    let fragment = asset.as_ref().map_or_else(
        || "none".to_owned(),
        |item| format!("refresh-{nonce}-{}", item.id),
    );
    (asset, fragment, true)
}

/// Load site-wide banner behavior settings from the database.
pub fn load_site_banner_settings(conn: &rusqlite::Connection) -> BannerSiteSettings {
    BannerSiteSettings {
        allow_external_links: db::get_banner_external_links_enabled(conn),
        rotation_interval_minutes: db::get_banner_rotation_interval_minutes(conn),
    }
}

/// Resolve the banner used on the home page.
///
/// # Errors
/// Returns an error if the banner query fails.
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
        &assets,
        &settings,
        current_path,
        "home banner",
    ))
}

/// Resolve the banner used on a board page.
///
/// # Errors
/// Returns an error if the banner query fails.
pub fn resolve_board_banner(
    conn: &rusqlite::Connection,
    board: &Board,
    placement: BannerPlacement,
    current_path: &str,
) -> Result<BannerSelection> {
    let settings = load_site_banner_settings(conn);
    let candidates = match board.banner_mode {
        BoardBannerMode::None => Vec::new(),
        BoardBannerMode::Override => db::list_banner_assets_for_board(conn, board.id)?,
        BoardBannerMode::Inherit => db::list_banner_assets_for_scope(conn, BannerScope::Global)?,
    }
    .into_iter()
    .filter(|asset| asset.enabled && should_show_on_placement(asset, placement))
    .collect::<Vec<_>>();

    Ok(resolve_from_candidates(
        &candidates,
        &settings,
        current_path,
        &format!("/{}/ banner", board.short_name),
    ))
}

#[must_use]
/// Render a selected banner as an HTML fragment.
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

/// Resolve one page's eligible banner candidates into render state.
fn resolve_from_candidates(
    candidates: &[BannerAsset],
    settings: &BannerSiteSettings,
    current_path: &str,
    alt: &str,
) -> BannerSelection {
    let (asset, etag_fragment, disable_not_modified_short_circuit) =
        choose_active_banner(candidates, settings);
    let banner = asset.map(|asset| ResolvedBanner {
        image_url: banner_asset_url(&asset),
        href: resolve_banner_href(&asset, settings.allow_external_links, current_path),
        alt: alt.to_owned(),
        asset,
    });
    BannerSelection {
        banner,
        etag_fragment,
        disable_not_modified_short_circuit,
    }
}

/// Store an animated GIF banner, using available runtime encoder capabilities.
fn write_animated_gif_banner_asset_scaled(
    bytes: &[u8],
    target_path: &Path,
    original_width: u32,
    original_height: u32,
) -> Result<(u32, u32)> {
    write_animated_gif_banner_asset_scaled_with_caps(
        bytes,
        target_path,
        original_width,
        original_height,
        crate::media::ffmpeg::detect_ffmpeg(),
        crate::media::ffmpeg::check_webp_encoder(),
    )
}

/// Store an animated GIF as scaled WebP or an unmodified GIF fallback.
fn write_animated_gif_banner_asset_scaled_with_caps(
    bytes: &[u8],
    target_path: &Path,
    original_width: u32,
    original_height: u32,
    ffmpeg_available: bool,
    ffmpeg_webp_available: bool,
) -> Result<(u32, u32)> {
    let (max_width, max_height) = maybe_shrink_dimensions(original_width, original_height);
    if !ffmpeg_available || !ffmpeg_webp_available {
        write_gif_banner_fallback(bytes, target_path)?;
        return Ok((original_width, original_height));
    }

    let input_path = animated_gif_temp_path(target_path);
    std::fs::write(&input_path, bytes)
        .with_context(|| format!("write {}", input_path.display()))?;

    let webp_path = banner_webp_path(target_path);
    let conversion = crate::media::ffmpeg::ffmpeg_image_to_webp_scaled(
        &input_path,
        &webp_path,
        max_width,
        max_height,
    )
    .with_context(|| format!("convert animated gif banner {}", input_path.display()));
    drop(std::fs::remove_file(&input_path));
    match conversion {
        Ok(()) => {
            let gif_path = banner_gif_fallback_path(&webp_path);
            if gif_path.exists() {
                drop(std::fs::remove_file(gif_path));
            }
            Ok((max_width, max_height))
        }
        Err(error) => {
            tracing::warn!("animated GIF banner conversion failed ({error:#}); storing GIF");
            write_gif_banner_fallback(bytes, &webp_path)?;
            Ok((original_width, original_height))
        }
    }
}

/// Store an animated WebP, scaling it when the external encoder is available.
fn write_animated_webp_banner_asset(
    bytes: &[u8],
    target_path: &Path,
    original_width: u32,
    original_height: u32,
) -> Result<(u32, u32)> {
    let (max_width, max_height) = maybe_shrink_dimensions(original_width, original_height);
    let should_scale = (max_width, max_height) != (original_width, original_height);
    if should_scale
        && crate::media::ffmpeg::detect_ffmpeg()
        && crate::media::ffmpeg::check_webp_encoder()
    {
        let input_path = animated_webp_temp_path(target_path);
        std::fs::write(&input_path, bytes)
            .with_context(|| format!("write {}", input_path.display()))?;
        let conversion = crate::media::ffmpeg::ffmpeg_image_to_webp_scaled(
            &input_path,
            target_path,
            max_width,
            max_height,
        )
        .with_context(|| format!("scale animated webp banner {}", input_path.display()));
        drop(std::fs::remove_file(&input_path));
        if conversion.is_ok() {
            return Ok((max_width, max_height));
        }
        if let Err(error) = conversion {
            tracing::warn!("animated WebP banner scaling failed ({error:#}); storing original");
        }
    }

    std::fs::write(target_path, bytes)
        .with_context(|| format!("write {}", target_path.display()))?;
    Ok((original_width, original_height))
}

/// Build a collision-resistant temporary path for GIF conversion input.
fn animated_gif_temp_path(target_path: &Path) -> PathBuf {
    let parent = target_path
        .parent()
        .map_or_else(runtime_banner_dir, Path::to_path_buf);
    parent.join(format!(
        "{}-animated-input.gif",
        uuid::Uuid::new_v4().simple()
    ))
}

/// Build a collision-resistant temporary path for WebP scaling input.
fn animated_webp_temp_path(target_path: &Path) -> PathBuf {
    let parent = target_path
        .parent()
        .map_or_else(runtime_banner_dir, Path::to_path_buf);
    parent.join(format!(
        "{}-animated-input.webp",
        uuid::Uuid::new_v4().simple()
    ))
}

/// Write an animated GIF fallback beside the canonical WebP location.
fn write_gif_banner_fallback(bytes: &[u8], target_path: &Path) -> Result<()> {
    let webp_path = banner_webp_path(target_path);
    let gif_path = banner_gif_fallback_path(&webp_path);
    std::fs::write(&gif_path, bytes).with_context(|| format!("write {}", gif_path.display()))?;
    if webp_path.exists() {
        drop(std::fs::remove_file(webp_path));
    }
    Ok(())
}

/// Normalize any banner path to its canonical WebP variant.
fn banner_webp_path(path: &Path) -> PathBuf {
    if path.extension().and_then(|ext| ext.to_str()) == Some("webp") {
        path.to_path_buf()
    } else {
        path.with_extension("webp")
    }
}

/// Return the GIF fallback sibling of a canonical banner path.
fn banner_gif_fallback_path(path: &Path) -> PathBuf {
    path.with_extension("gif")
}

/// Resolve the file variant that exists after a banner write.
fn stored_banner_path_after_write(target_path: &Path) -> PathBuf {
    if target_path.exists() {
        return target_path.to_path_buf();
    }
    let webp_path = banner_webp_path(target_path);
    let gif_path = banner_gif_fallback_path(&webp_path);
    if gif_path.exists() {
        gif_path
    } else {
        webp_path
    }
}

/// Inspect banner headers and animation metadata before full decoding.
fn preflight_banner_bytes(bytes: &[u8]) -> Result<BannerImagePreflight> {
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|error| {
            anyhow::anyhow!("Banner image must be a supported bitmap file: {error}")
        })?;
    let format = reader
        .format()
        .ok_or_else(|| anyhow::anyhow!("Banner image must be a supported bitmap file."))?;
    let (width, height) = reader
        .into_dimensions()
        .context("read banner image dimensions")?;
    validate_banner_dimensions(width, height)?;
    let animated_gif_frames = if format == ImageFormat::Gif {
        let frames = count_gif_frames(bytes);
        if frames > MAX_ANIMATED_GIF_FRAMES {
            anyhow::bail!(
                "Animated GIF banners must have at most {MAX_ANIMATED_GIF_FRAMES} frames."
            );
        }
        Some(frames)
    } else {
        None
    };
    Ok(BannerImagePreflight {
        width,
        height,
        animated_gif_frames,
        animated_webp: format == ImageFormat::WebP && is_animated_webp(bytes),
    })
}

/// Enforce accepted banner dimensions and decoded-pixel bounds.
fn validate_banner_dimensions(width: u32, height: u32) -> Result<()> {
    if width < MIN_WIDTH || height < MIN_HEIGHT {
        anyhow::bail!("Banner image must be at least {MIN_WIDTH}x{MIN_HEIGHT} pixels.");
    }
    if width > MAX_WIDTH || height > MAX_HEIGHT {
        anyhow::bail!("Banner image is too large for banner use.");
    }
    let pixels = u64::from(width).saturating_mul(u64::from(height));
    if pixels > MAX_PIXELS {
        anyhow::bail!("Banner image exceeds the maximum pixel count.");
    }
    let width_u64 = u64::from(width);
    let height_u64 = u64::from(height);
    if width_u64.saturating_mul(u64::from(DISPLAY_HEIGHT))
        != height_u64.saturating_mul(u64::from(DISPLAY_WIDTH))
    {
        anyhow::bail!(
            "Banner image must use the exact {DISPLAY_WIDTH}:{DISPLAY_HEIGHT} aspect ratio."
        );
    }
    Ok(())
}

/// Decode a static banner after validating its preflight metadata.
fn decode_uploaded_banner(bytes: &[u8], preflight: &BannerImagePreflight) -> Result<DynamicImage> {
    if preflight.animated_gif_frames.is_some() {
        anyhow::bail!("Animated GIF banners are handled separately.");
    }
    if preflight.animated_webp {
        anyhow::bail!("Animated WebP banners are handled separately.");
    }
    let img = image::load_from_memory(bytes).context("decode banner image")?;
    Ok(img)
}

/// Convert a decoded banner to the canonical pixel representation.
fn normalize_banner_image(img: &DynamicImage) -> DynamicImage {
    let (width, height) = img.dimensions();
    if width > RECOMMENDED_WIDTH || height > RECOMMENDED_HEIGHT {
        img.resize_exact(RECOMMENDED_WIDTH, RECOMMENDED_HEIGHT, FilterType::Lanczos3)
    } else {
        img.clone()
    }
}

/// Bound banner dimensions while preserving aspect ratio.
const fn maybe_shrink_dimensions(width: u32, height: u32) -> (u32, u32) {
    if width > RECOMMENDED_WIDTH || height > RECOMMENDED_HEIGHT {
        (RECOMMENDED_WIDTH, RECOMMENDED_HEIGHT)
    } else {
        (width, height)
    }
}

/// Count decodable frames in GIF input, stopping at the configured ceiling.
fn count_gif_frames(bytes: &[u8]) -> usize {
    let mut frame_markers = 0usize;
    for window in bytes.windows(2) {
        if window == [0x21, 0xF9] {
            frame_markers += 1;
        }
    }
    frame_markers.max(1)
}

/// Return whether a RIFF WebP container advertises animation chunks.
fn is_animated_webp(bytes: &[u8]) -> bool {
    if bytes.get(0..4) != Some(b"RIFF") || bytes.get(8..12) != Some(b"WEBP") {
        return false;
    }
    let mut offset = 12usize;
    while offset.saturating_add(8) <= bytes.len() {
        let Some(chunk_type) = bytes.get(offset..offset + 4) else {
            return false;
        };
        let Some(size_bytes) = bytes.get(offset + 4..offset + 8) else {
            return false;
        };
        let Ok(size_bytes) = <[u8; 4]>::try_from(size_bytes) else {
            return false;
        };
        let Ok(chunk_size) = usize::try_from(u32::from_le_bytes(size_bytes)) else {
            return false;
        };
        if chunk_type == b"ANIM" || chunk_type == b"ANMF" {
            return true;
        }
        offset = offset
            .saturating_add(8)
            .saturating_add(chunk_size)
            .saturating_add(chunk_size % 2);
    }
    false
}

#[cfg(test)]
mod tests {
    use super::{
        banner_admin_anchor, banner_asset_content_type, banner_gif_fallback_path,
        banner_open_section, banner_storage_path, banner_target_draft, canonicalize_banner_bytes,
        choose_active_banner, is_animated_webp, normalize_external_url, normalize_internal_path,
        parse_banner_target, resolve_banner_href, safe_return_to,
        validate_banner_restore_entry_name, validate_banner_storage_key,
        write_animated_gif_banner_asset_scaled_with_caps, write_animated_webp_banner_asset,
        DISPLAY_HEIGHT, DISPLAY_WIDTH, MAX_ANIMATED_GIF_FRAMES,
    };
    use crate::models::{BannerAsset, BannerScope, BannerTargetType};
    use anyhow::Result;
    use image::{codecs::gif::GifEncoder, Delay, Frame, ImageBuffer, ImageFormat, Rgba};
    use std::io::Cursor;

    #[test]
    fn validates_canonical_banner_storage_key() {
        assert!(validate_banner_storage_key("0123456789abcdef0123456789abcdef").is_ok());
        assert!(validate_banner_storage_key("../etc").is_err());
        assert!(validate_banner_storage_key("0123456789abcdef0123456789abcdeg").is_err());
    }

    #[test]
    fn banner_storage_path_rejects_traversal() {
        assert!(banner_storage_path(BannerScope::Global, None, "../etc").is_err());
        assert!(banner_storage_path(
            BannerScope::Board,
            Some("tech"),
            "0123456789abcdef0123456789abcdef"
        )
        .is_ok());
    }

    #[test]
    fn restore_entry_name_requires_flat_webp_file() {
        assert!(
            validate_banner_restore_entry_name("global/0123456789abcdef0123456789abcdef.webp")
                .is_ok()
        );
        assert!(
            validate_banner_restore_entry_name("home/0123456789abcdef0123456789abcdef.gif").is_ok()
        );
        assert!(
            validate_banner_restore_entry_name("0123456789abcdef0123456789abcdef.webp").is_err()
        );
        assert!(validate_banner_restore_entry_name("../evil.webp").is_err());
        assert!(validate_banner_restore_entry_name("nested/evil.webp").is_err());
    }

    #[test]
    fn banner_admin_anchor_matches_scope() {
        assert_eq!(
            banner_admin_anchor(BannerScope::Global, None),
            "global-banners"
        );
        assert_eq!(banner_admin_anchor(BannerScope::Home, None), "home-banners");
        assert_eq!(
            banner_admin_anchor(BannerScope::Board, Some("tech")),
            "board-appearance-tech"
        );
        assert_eq!(banner_open_section("global-banners"), "board-banners");
        assert_eq!(
            banner_open_section("board-appearance-tech"),
            "board-banners"
        );
    }

    #[test]
    fn banner_target_draft_only_populates_selected_field() {
        let board = banner_target_draft(BannerTargetType::InternalBoard, "tech");
        assert_eq!(board.board_value, "tech");
        assert!(board.thread_value.is_empty());
        assert!(board.external_url.is_empty());

        let thread = banner_target_draft(BannerTargetType::InternalPath, "/tech/thread/42");
        assert!(thread.board_value.is_empty());
        assert_eq!(thread.thread_value, "/tech/thread/42");
        assert!(thread.external_url.is_empty());
    }

    #[test]
    fn banner_target_validation_rejects_unsafe_redirect_shapes() {
        assert_eq!(safe_return_to("https://evil.example"), "/");
        assert_eq!(safe_return_to("//evil.example/path"), "/");
        assert!(normalize_internal_path("/tech/thread/42").is_some());
        assert!(normalize_internal_path("//evil.example/path").is_none());
        assert!(normalize_external_url("https://example.com/path").is_some());
        assert!(normalize_external_url("javascript:alert(1)").is_none());
        assert!(parse_banner_target(
            BannerTargetType::ExternalUrl.as_str(),
            "https://example.com",
            false,
        )
        .is_err());
    }

    #[test]
    fn disabled_external_banner_link_resolves_to_plain_image() {
        let asset = BannerAsset {
            id: 42,
            scope: BannerScope::Global,
            board_id: None,
            board_short: None,
            storage_key: "0123456789abcdef0123456789abcdef".into(),
            width: i64::from(DISPLAY_WIDTH),
            height: i64::from(DISPLAY_HEIGHT),
            file_size: 1,
            enabled: true,
            sort_order: 1,
            target_type: BannerTargetType::ExternalUrl,
            target_value: "https://example.com".into(),
            show_on_index: true,
            show_on_catalog: true,
            created_at: 1,
        };

        assert!(resolve_banner_href(&asset, false, "/b").is_none());
        assert_eq!(
            resolve_banner_href(&asset, true, "https://evil.example").as_deref(),
            Some("/banner/external/42?return_to=%2F")
        );
    }

    #[test]
    fn zero_rotation_chooses_per_refresh_and_skips_304_short_circuit() {
        let a = BannerAsset {
            id: 1,
            scope: BannerScope::Global,
            board_id: None,
            board_short: None,
            storage_key: "0123456789abcdef0123456789abcdef".into(),
            width: 468,
            height: 60,
            file_size: 1,
            enabled: true,
            sort_order: 1,
            target_type: BannerTargetType::None,
            target_value: String::new(),
            show_on_index: true,
            show_on_catalog: true,
            created_at: 1,
        };
        let b = BannerAsset { id: 2, ..a.clone() };
        let settings = crate::banner::BannerSiteSettings {
            allow_external_links: false,
            rotation_interval_minutes: 0,
        };
        let first = choose_active_banner(&[a.clone(), b.clone()], &settings);
        let second = choose_active_banner(&[a, b], &settings);
        assert!(first.0.is_some());
        assert!(second.0.is_some());
        assert_ne!(first.1, second.1);
        assert!(first.2);
        assert!(second.2);
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn rejects_banner_gif_frame_bomb() -> Result<()> {
        let mut bytes = Vec::new();
        {
            let mut encoder = GifEncoder::new(&mut bytes);
            for _ in 0..=MAX_ANIMATED_GIF_FRAMES {
                let image = ImageBuffer::from_pixel(1, 1, Rgba([0, 0, 0, 255]));
                let frame = Frame::from_parts(image, 0, 0, Delay::from_numer_denom_ms(1, 1));
                encoder.encode_frame(frame)?;
            }
        }
        let target = std::env::temp_dir().join("banner-test.webp");
        assert!(
            canonicalize_banner_bytes(&bytes, &target).is_err(),
            "a GIF over the frame limit must be rejected"
        );
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn animated_gif_banner_falls_back_to_gif_without_ffmpeg() -> Result<()> {
        let mut bytes = Vec::new();
        {
            let mut encoder = GifEncoder::new(&mut bytes);
            for color in [Rgba([0, 0, 0, 255]), Rgba([255, 255, 255, 255])] {
                let image = ImageBuffer::from_pixel(DISPLAY_WIDTH, DISPLAY_HEIGHT, color);
                let frame = Frame::from_parts(image, 0, 0, Delay::from_numer_denom_ms(10, 1));
                encoder.encode_frame(frame)?;
            }
        }
        let target = std::env::temp_dir().join(format!("{}.webp", uuid::Uuid::new_v4().simple()));
        let gif_path = banner_gif_fallback_path(&target);
        let dimensions = write_animated_gif_banner_asset_scaled_with_caps(
            &bytes,
            &target,
            DISPLAY_WIDTH,
            DISPLAY_HEIGHT,
            false,
            false,
        )?;
        assert_eq!(
            dimensions,
            (DISPLAY_WIDTH, DISPLAY_HEIGHT),
            "fallback must preserve source dimensions"
        );
        assert!(
            !target.exists(),
            "the unavailable WebP output must not be published"
        );
        assert_eq!(
            std::fs::read(&gif_path)?,
            bytes,
            "the GIF fallback must preserve the source bytes"
        );
        assert_eq!(
            banner_asset_content_type(&gif_path),
            "image/gif",
            "the fallback must be served as GIF"
        );
        drop(std::fs::remove_file(gif_path));
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn deleting_banner_asset_removes_webp_and_gif_siblings() -> Result<()> {
        let storage_key = uuid::Uuid::new_v4().simple().to_string();
        let asset = BannerAsset {
            id: 1,
            scope: BannerScope::Global,
            board_id: None,
            board_short: None,
            storage_key,
            width: i64::from(DISPLAY_WIDTH),
            height: i64::from(DISPLAY_HEIGHT),
            file_size: 1,
            enabled: true,
            sort_order: 1,
            target_type: BannerTargetType::None,
            target_value: String::new(),
            show_on_index: true,
            show_on_catalog: true,
            created_at: 1,
        };
        let webp_path = banner_storage_path(asset.scope, None, &asset.storage_key)?;
        let gif_path = banner_gif_fallback_path(&webp_path);
        if let Some(parent) = webp_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&webp_path, b"webp")?;
        std::fs::write(&gif_path, b"gif")?;

        super::delete_banner_asset_file(&asset)?;

        assert!(!webp_path.exists(), "WebP sibling must be deleted");
        assert!(!gif_path.exists(), "GIF sibling must be deleted");
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn animated_webp_banner_is_preserved_without_static_decode() -> Result<()> {
        let bytes = b"RIFF\x18\x00\x00\x00WEBPVP8X\n\x00\x00\x00\x02\x00\x00\x00\xd3\x01\x00;\x00\x00ANIM\x00\x00\x00\x00";
        assert!(
            is_animated_webp(bytes),
            "fixture must be recognized as animated WebP"
        );

        let target = std::env::temp_dir().join(format!("{}.webp", uuid::Uuid::new_v4().simple()));
        let dimensions =
            write_animated_webp_banner_asset(bytes, &target, DISPLAY_WIDTH, DISPLAY_HEIGHT)?;
        assert_eq!(
            dimensions,
            (DISPLAY_WIDTH, DISPLAY_HEIGHT),
            "unscaled animation dimensions must be preserved"
        );
        assert_eq!(
            std::fs::read(&target)?,
            bytes,
            "animated WebP bytes must be preserved"
        );
        drop(std::fs::remove_file(target));
        Ok(())
    }

    #[test]
    #[expect(
        clippy::panic_in_result_fn,
        reason = "test assertions intentionally panic on failure"
    )]
    fn rejects_oversized_banner_dimensions() -> Result<()> {
        let image = ImageBuffer::from_pixel(4212, 540, Rgba([1, 2, 3, 255]));
        let mut cursor = Cursor::new(Vec::new());
        {
            image::DynamicImage::ImageRgba8(image).write_to(&mut cursor, ImageFormat::Png)?;
        }
        let bytes = cursor.into_inner();
        let target = std::env::temp_dir().join("banner-test-large.webp");
        assert!(
            canonicalize_banner_bytes(&bytes, &target).is_err(),
            "oversized decoded dimensions must be rejected"
        );
        Ok(())
    }

    #[test]
    fn rejects_malformed_banner_bytes() {
        let target = std::env::temp_dir().join("banner-test-malformed.webp");
        assert!(canonicalize_banner_bytes(b"not an image", &target).is_err());
    }
}
