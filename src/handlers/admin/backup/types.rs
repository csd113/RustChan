// src/handlers/admin/backup/types.rs

pub(super) mod board_backup_types {
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize)]
    #[allow(clippy::struct_excessive_bools)]
    pub struct BoardRow {
        pub id: i64,
        pub short_name: String,
        pub name: String,
        pub description: String,
        pub nsfw: bool,
        pub max_threads: i64,
        #[serde(default = "default_max_archived_threads")]
        pub max_archived_threads: i64,
        pub bump_limit: i64,
        #[serde(default = "default_true")]
        pub allow_images: bool,
        #[serde(default = "default_true")]
        pub allow_video: bool,
        #[serde(default)]
        pub allow_audio: bool,
        #[serde(default)]
        pub allow_any_files: bool,
        #[serde(default = "default_true")]
        pub allow_tripcodes: bool,
        #[serde(default = "default_edit_window_secs")]
        pub edit_window_secs: i64,
        #[serde(default)]
        pub allow_editing: bool,
        #[serde(default = "default_true")]
        pub allow_archive: bool,
        #[serde(default)]
        pub allow_video_embeds: bool,
        #[serde(default)]
        pub allow_captcha: bool,
        #[serde(default)]
        pub show_poster_ids: bool,
        #[serde(default)]
        pub collapse_greentext: bool,
        #[serde(default)]
        pub post_cooldown_secs: i64,
        pub created_at: i64,
    }

    const fn default_true() -> bool {
        true
    }

    const fn default_edit_window_secs() -> i64 {
        300
    }

    const fn default_max_archived_threads() -> i64 {
        150
    }

    #[derive(Serialize, Deserialize)]
    pub struct ThreadRow {
        pub id: i64,
        pub board_id: i64,
        pub subject: Option<String>,
        pub created_at: i64,
        pub bumped_at: i64,
        pub locked: bool,
        pub sticky: bool,
        pub reply_count: i64,
    }

    #[derive(Serialize, Deserialize)]
    pub struct PostRow {
        pub id: i64,
        pub thread_id: i64,
        pub board_id: i64,
        pub name: String,
        pub tripcode: Option<String>,
        pub subject: Option<String>,
        pub body: String,
        pub body_html: String,
        pub ip_hash: Option<String>,
        pub file_path: Option<String>,
        pub file_name: Option<String>,
        pub file_size: Option<i64>,
        pub thumb_path: Option<String>,
        pub mime_type: Option<String>,
        pub media_type: Option<String>,
        pub created_at: i64,
        pub deletion_token: String,
        pub is_op: bool,
    }

    #[derive(Serialize, Deserialize)]
    pub struct PollRow {
        pub id: i64,
        pub thread_id: i64,
        pub question: String,
        pub expires_at: i64,
        pub created_at: i64,
    }

    #[derive(Serialize, Deserialize)]
    pub struct PollOptionRow {
        pub id: i64,
        pub poll_id: i64,
        pub text: String,
        pub position: i64,
    }

    #[derive(Serialize, Deserialize)]
    pub struct PollVoteRow {
        pub id: i64,
        pub poll_id: i64,
        pub option_id: i64,
        pub ip_hash: String,
    }

    #[derive(Serialize, Deserialize)]
    pub struct FileHashRow {
        pub sha256: String,
        pub file_path: String,
        pub thumb_path: String,
        pub mime_type: String,
        pub created_at: i64,
    }

    #[derive(Serialize, Deserialize)]
    pub struct BoardBackupManifest {
        pub version: u32,
        pub board: BoardRow,
        pub threads: Vec<ThreadRow>,
        pub posts: Vec<PostRow>,
        pub polls: Vec<PollRow>,
        pub poll_options: Vec<PollOptionRow>,
        pub poll_votes: Vec<PollVoteRow>,
        pub file_hashes: Vec<FileHashRow>,
    }
}
