#[cfg(test)]
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
        allow_audio: true,
        allow_any_files: false,
        allow_tripcodes: true,
        allow_editing: false,
        edit_window_secs: 0,
        allow_archive: true,
        allow_video_embeds: false,
        allow_captcha: false,
        show_poster_ids: false,
        collapse_greentext: false,
        post_cooldown_secs: 0,
        default_theme: String::new(),
        created_at: 0,
    }
}
