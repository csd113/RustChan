// src/handlers/admin/backup/types.rs

/// Implements board backup types handler support.
pub(super) mod board_backup_types {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    // This type mirrors serialized or render state, so the boolean count is an intentional tradeoff.
    #[expect(
        clippy::struct_excessive_bools,
        reason = "the fields mirror independent persisted board settings in the backup schema"
    )]
    /// Database row data for board.
    pub(crate) struct BoardRow {
        /// The record identifier.
        pub id: i64,
        /// The short name.
        pub short_name: String,
        /// The name.
        pub name: String,
        /// The description.
        pub description: String,
        /// Whether the NSFW setting is active.
        pub nsfw: bool,
        /// The max threads.
        pub max_threads: i64,
        #[serde(default = "default_max_archived_threads")]
        /// The max archived threads.
        pub max_archived_threads: i64,
        /// The bump limit.
        pub bump_limit: i64,
        #[serde(default = "default_true")]
        /// Whether to allow images.
        pub allow_images: bool,
        #[serde(default = "default_true")]
        /// Whether to allow video.
        pub allow_video: bool,
        #[serde(default)]
        /// Whether to allow audio.
        pub allow_audio: bool,
        #[serde(default)]
        /// Whether to allow PDF.
        pub allow_pdf: bool,
        #[serde(default)]
        /// Whether to allow any files.
        pub allow_any_files: bool,
        #[serde(default = "default_true")]
        /// Whether to allow tripcodes.
        pub allow_tripcodes: bool,
        #[serde(default = "default_edit_window_secs")]
        /// The edit window duration in seconds.
        pub edit_window_secs: i64,
        #[serde(default)]
        /// Whether to allow editing.
        pub allow_editing: bool,
        #[serde(default)]
        /// Whether to allow self delete.
        pub allow_self_delete: bool,
        #[serde(default = "default_true")]
        /// Whether to allow archive.
        pub allow_archive: bool,
        #[serde(default)]
        /// Whether to allow video embeds.
        pub allow_video_embeds: bool,
        #[serde(default)]
        /// Whether to allow captcha.
        pub allow_captcha: bool,
        #[serde(default)]
        /// The show poster identifiers.
        pub show_poster_ids: bool,
        #[serde(default)]
        /// Whether the collapse greentext setting is active.
        pub collapse_greentext: bool,
        #[serde(default)]
        /// The post cooldown duration in seconds.
        pub post_cooldown_secs: i64,
        #[serde(default = "default_banner_mode")]
        /// The banner mode.
        pub banner_mode: String,
        #[serde(default = "default_access_mode")]
        /// The access mode.
        pub access_mode: String,
        #[serde(default)]
        /// The access password hash.
        pub access_password_hash: String,
        /// The created timestamp.
        pub created_at: i64,
    }

    /// Returns the default true.
    const fn default_true() -> bool {
        true
    }

    /// Returns the default edit window secs.
    const fn default_edit_window_secs() -> i64 {
        300
    }

    /// Returns the default max archived threads.
    const fn default_max_archived_threads() -> i64 {
        150
    }

    /// Returns the default access mode.
    fn default_access_mode() -> String {
        "public".to_owned()
    }

    /// Returns the default banner mode.
    fn default_banner_mode() -> String {
        "inherit".to_owned()
    }

    #[derive(Serialize, Deserialize)]
    /// Database row data for thread.
    pub(crate) struct ThreadRow {
        /// The record identifier.
        pub id: i64,
        /// The board identifier.
        pub board_id: i64,
        /// The optional subject.
        pub subject: Option<String>,
        /// The created timestamp.
        pub created_at: i64,
        /// The bumped timestamp.
        pub bumped_at: i64,
        /// Whether the locked setting is active.
        pub locked: bool,
        /// Whether the sticky setting is active.
        pub sticky: bool,
        #[serde(default)]
        /// Whether the archived setting is active.
        pub archived: bool,
        /// The number of replies.
        pub reply_count: i64,
    }

    #[derive(Serialize, Deserialize)]
    /// Database row data for post.
    pub(crate) struct PostRow {
        /// The record identifier.
        pub id: i64,
        /// The thread identifier.
        pub thread_id: i64,
        /// The board identifier.
        pub board_id: i64,
        /// The name.
        pub name: String,
        /// The optional tripcode.
        pub tripcode: Option<String>,
        /// The optional subject.
        pub subject: Option<String>,
        /// The body.
        pub body: String,
        /// The body HTML.
        pub body_html: String,
        /// The optional IP hash.
        pub ip_hash: Option<String>,
        /// The file path.
        pub file_path: Option<String>,
        /// The optional file name.
        pub file_name: Option<String>,
        /// The optional file size.
        pub file_size: Option<i64>,
        /// The thumb path.
        pub thumb_path: Option<String>,
        /// The optional MIME type.
        pub mime_type: Option<String>,
        /// The optional media type.
        pub media_type: Option<String>,
        /// The created timestamp.
        pub created_at: i64,
        /// The deletion token.
        pub deletion_token: String,
        /// Whether this value is op.
        pub is_op: bool,
        /// The optional media processing state.
        pub media_processing_state: Option<String>,
        /// The optional media processing error.
        pub media_processing_error: Option<String>,
    }

    #[derive(Serialize, Deserialize)]
    /// Database row data for poll.
    pub(crate) struct PollRow {
        /// The record identifier.
        pub id: i64,
        /// The thread identifier.
        pub thread_id: i64,
        /// The question.
        pub question: String,
        /// The expires timestamp.
        pub expires_at: i64,
        /// The created timestamp.
        pub created_at: i64,
    }

    #[derive(Serialize, Deserialize)]
    /// Database row data for poll option.
    pub(crate) struct PollOptionRow {
        /// The record identifier.
        pub id: i64,
        /// The poll identifier.
        pub poll_id: i64,
        /// The text.
        pub text: String,
        /// The position.
        pub position: i64,
    }

    #[derive(Serialize, Deserialize)]
    /// Database row data for poll vote.
    pub(crate) struct PollVoteRow {
        /// The record identifier.
        pub id: i64,
        /// The poll identifier.
        pub poll_id: i64,
        /// The option identifier.
        pub option_id: i64,
        /// The IP hash.
        pub ip_hash: String,
    }

    #[derive(Serialize, Deserialize)]
    /// Database row data for file hash.
    pub(crate) struct FileHashRow {
        /// The SHA-256.
        pub sha256: String,
        /// The file path.
        pub file_path: String,
        /// The thumb path.
        pub thumb_path: String,
        /// The MIME type.
        pub mime_type: String,
        /// The created timestamp.
        pub created_at: i64,
    }

    #[derive(Serialize, Deserialize)]
    /// Database row data for banner.
    pub(crate) struct BannerRow {
        /// The storage key.
        pub storage_key: String,
        /// The width.
        pub width: i64,
        /// The height.
        pub height: i64,
        /// The file size.
        pub file_size: i64,
        /// Whether this item is enabled.
        pub enabled: bool,
        /// The sort order.
        pub sort_order: i64,
        /// The target type.
        pub target_type: String,
        /// The target value.
        pub target_value: String,
        /// Whether to show on index.
        pub show_on_index: bool,
        /// Whether to show on catalog.
        pub show_on_catalog: bool,
        /// The created timestamp.
        pub created_at: i64,
    }

    #[derive(Serialize, Deserialize)]
    /// Manifest data for board backup.
    pub(crate) struct BoardBackupManifest {
        /// The version.
        pub version: u32,
        /// The board.
        pub board: BoardRow,
        /// The threads collection.
        pub threads: Vec<ThreadRow>,
        /// The posts collection.
        pub posts: Vec<PostRow>,
        /// The polls collection.
        pub polls: Vec<PollRow>,
        /// The poll options collection.
        pub poll_options: Vec<PollOptionRow>,
        /// The poll votes collection.
        pub poll_votes: Vec<PollVoteRow>,
        /// The file hashes collection.
        pub file_hashes: Vec<FileHashRow>,
        #[serde(default)]
        /// The banners collection.
        pub banners: Vec<BannerRow>,
    }
}
