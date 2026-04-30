#[cfg(test)]
pub const DEFAULT_NEW_BOARD_ALLOW_AUDIO: bool = false;
#[cfg(test)]
pub const DEFAULT_NEW_BOARD_ALLOW_EDITING: bool = true;
#[cfg(test)]
pub const DEFAULT_NEW_BOARD_ALLOW_SELF_DELETE: bool = true;
#[cfg(test)]
pub const DEFAULT_NEW_BOARD_ALLOW_VIDEO_EMBEDS: bool = true;
#[cfg(test)]
pub const DEFAULT_NEW_BOARD_SHOW_POSTER_IDS: bool = true;

#[must_use]
pub fn sample_board() -> crate::models::Board {
    crate::models::Board {
        id: 1,
        display_order: 1,
        short_name: "test".to_string(),
        name: "Test".to_string(),
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
