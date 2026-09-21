//! Shared, deterministic fixtures for crate-local tests.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};

#[cfg(test)]
/// Default audio-upload setting for a newly constructed test board.
pub const DEFAULT_NEW_BOARD_ALLOW_AUDIO: bool = false;
#[cfg(test)]
/// Default post-editing setting for a newly constructed test board.
pub const DEFAULT_NEW_BOARD_ALLOW_EDITING: bool = true;
#[cfg(test)]
/// Default self-deletion setting for a newly constructed test board.
pub const DEFAULT_NEW_BOARD_ALLOW_SELF_DELETE: bool = true;
#[cfg(test)]
/// Default video-embed setting for a newly constructed test board.
pub const DEFAULT_NEW_BOARD_ALLOW_VIDEO_EMBEDS: bool = true;
#[cfg(test)]
/// Default poster-ID setting for a newly constructed test board.
pub const DEFAULT_NEW_BOARD_SHOW_POSTER_IDS: bool = true;

/// Builds a representative board with stable defaults for tests.
#[must_use]
pub fn sample_board() -> crate::models::Board {
    crate::models::Board {
        id: 1,
        display_order: 1,
        short_name: "test".to_owned(),
        name: "Test".to_owned(),
        description: String::new(),
        nsfw: false,
        max_threads: 100,
        max_archived_threads: 150,
        bump_limit: 500,
        allow_images: true,
        allow_video: true,
        allow_audio: DEFAULT_NEW_BOARD_ALLOW_AUDIO,
        max_image_size: 8 * 1024 * 1024,
        max_video_size: 50 * 1024 * 1024,
        max_audio_size: 150 * 1024 * 1024,
        max_pdf_size: 8 * 1024 * 1024,
        allow_pdf: false,
        allow_any_files: false,
        allow_tripcodes: true,
        allow_editing: DEFAULT_NEW_BOARD_ALLOW_EDITING,
        allow_self_delete: DEFAULT_NEW_BOARD_ALLOW_SELF_DELETE,
        edit_window_secs: 0,
        allow_archive: true,
        allow_video_embeds: DEFAULT_NEW_BOARD_ALLOW_VIDEO_EMBEDS,
        allow_captcha: false,
        show_poster_ids: DEFAULT_NEW_BOARD_SHOW_POSTER_IDS,
        collapse_greentext: false,
        post_cooldown_secs: 0,
        default_theme: String::new(),
        banner_mode: crate::models::BoardBannerMode::Inherit,
        access_mode: crate::models::BoardAccessMode::Public,
        access_password_hash: String::new(),
        created_at: 0,
    }
}

/// Creates a fully initialized application state for handler tests.
#[expect(
    clippy::expect_used,
    reason = "shared test fixture construction must fail immediately when its isolated filesystem or database cannot be initialized"
)]
pub(crate) fn app_state() -> crate::middleware::AppState {
    crate::config::generate_settings_file_if_missing();
    std::fs::create_dir_all(&crate::config::CONFIG.upload_dir).expect("create test upload dir");
    std::fs::create_dir_all(crate::config::full_backups_dir())
        .expect("create test full backup dir");
    std::fs::create_dir_all(crate::config::board_backups_dir())
        .expect("create test board backup dir");
    let pool = crate::db::init_test_pool().expect("test pool");
    if let Ok(conn) = pool.get() {
        drop(crate::db::sync_live_theme_state(&conn));
        crate::templates::set_live_boards(crate::db::get_all_boards(&conn).unwrap_or_default());
    }
    let job_queue = std::sync::Arc::new(crate::workers::JobQueue::new(pool.clone()));
    crate::middleware::AppState {
        db: pool,
        ffmpeg_available: false,
        ffprobe_available: false,
        ffmpeg_webp_available: false,
        ffmpeg_vp9_available: false,
        ffmpeg_vp9_encoder_available: false,
        ffmpeg_opus_available: false,
        pdf_thumbnail_renderer: None,
        job_queue,
        backup_progress: std::sync::Arc::new(crate::middleware::BackupProgress::new()),
        auto_full_backup_settings: crate::middleware::AutoFullBackupSettings::new(
            24,
            1,
            false,
            "directory",
            4 * 1024 * 1024 * 1024,
        ),
        maintenance_gate: crate::middleware::MaintenanceGate::new(),
        media_upload_gate: crate::middleware::MediaUploadGate::new(),
        chan_import_gate: crate::middleware::ChanImportGate::new(),
        db_maintenance_jobs: crate::middleware::DbMaintenanceJobs::new(),
        chan_ledger: None,
        onion_address: std::sync::Arc::new(tokio::sync::RwLock::new(None)),
    }
}

/// Returns deterministic loopback connection metadata for request tests.
pub(crate) fn connect_info() -> axum::extract::ConnectInfo<SocketAddr> {
    axum::extract::ConnectInfo(SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 41000))
}

/// Encodes text fields and an optional file as a multipart test request body.
pub(crate) fn multipart_body(
    fields: &[(&str, &str)],
    file: Option<(&str, &str, &[u8], &str)>,
) -> (String, Vec<u8>) {
    let boundary = "rustchan-test-boundary".to_owned();
    let mut body = Vec::new();

    for (name, value) in fields {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
        );
        body.extend_from_slice(value.as_bytes());
        body.extend_from_slice(b"\r\n");
    }

    if let Some((field_name, filename, contents, content_type)) = file {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(
            format!(
                "Content-Disposition: form-data; name=\"{field_name}\"; filename=\"{filename}\"\r\n"
            )
            .as_bytes(),
        );
        body.extend_from_slice(format!("Content-Type: {content_type}\r\n\r\n").as_bytes());
        body.extend_from_slice(contents);
        body.extend_from_slice(b"\r\n");
    }

    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (boundary, body)
}
