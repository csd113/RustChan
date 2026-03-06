// models.rs — plain data structs that map 1:1 to database rows.
// No ORM magic; fields match column names for easy rusqlite mapping.

use serde::{Deserialize, Serialize};

// ─── Media type classification ────────────────────────────────────────────────

/// Classifies an uploaded file as image, video, or audio.
/// Stored as a TEXT column in posts ("image", "video", "audio").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaType {
    Image,
    Video,
    Audio,
}

impl MediaType {
    /// Infer MediaType from a MIME type string.
    pub fn from_mime(mime: &str) -> Option<Self> {
        if mime.starts_with("image/") {
            Some(MediaType::Image)
        } else if mime.starts_with("video/") {
            Some(MediaType::Video)
        } else if mime.starts_with("audio/") {
            Some(MediaType::Audio)
        } else {
            None
        }
    }

    /// Infer MediaType from a file extension (lowercase, no dot).
    /// Used during the backfill migration for pre-existing posts.
    #[allow(dead_code)]
    pub fn from_ext(ext: &str) -> Option<Self> {
        match ext {
            "jpg" | "jpeg" | "png" | "gif" | "webp" => Some(MediaType::Image),
            "mp4" | "webm" => Some(MediaType::Video),
            "mp3" | "ogg" | "flac" | "wav" | "m4a" | "aac" | "opus" => Some(MediaType::Audio),
            _ => None,
        }
    }

    /// Serialise to the TEXT value stored in the database.
    pub fn as_str(&self) -> &'static str {
        match self {
            MediaType::Image => "image",
            MediaType::Video => "video",
            MediaType::Audio => "audio",
        }
    }

    /// Deserialise from the TEXT value stored in the database.
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "image" => Some(MediaType::Image),
            "video" => Some(MediaType::Video),
            "audio" => Some(MediaType::Audio),
            _ => None,
        }
    }
}

/// A board, e.g. /tech/ — Technology
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Board {
    pub id: i64,
    pub short_name: String, // "tech" (no slashes)
    pub name: String,       // "Technology"
    pub description: String,
    pub nsfw: bool,
    pub max_threads: i64,
    pub bump_limit: i64,
    pub allow_images: bool, // per-board image upload toggle (default: true)
    pub allow_video: bool,  // per-board video upload toggle (default: true)
    pub allow_audio: bool,  // per-board audio upload toggle (default: true)
    pub allow_tripcodes: bool,
    pub created_at: i64, // Unix timestamp
}

/// A thread (the OP post + its replies share this record for metadata)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thread {
    pub id: i64,
    pub board_id: i64,
    pub subject: Option<String>,
    pub created_at: i64,
    pub bumped_at: i64,
    pub locked: bool,
    pub sticky: bool,
    pub archived: bool,
    pub reply_count: i64,
    // Joined from posts (OP's body/image for catalog previews)
    pub op_body: Option<String>,
    pub op_file: Option<String>,
    pub op_thumb: Option<String>,
    pub op_name: Option<String>,
    pub op_tripcode: Option<String>,
    pub op_id: Option<i64>,
}

/// A single post (OP or reply)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Post {
    pub id: i64,
    pub thread_id: i64,
    pub board_id: i64,
    pub name: String,
    pub tripcode: Option<String>,
    pub subject: Option<String>,
    pub body: String,
    pub body_html: String, // pre-rendered HTML (greentext, links, >>refs)
    pub ip_hash: String,
    pub file_path: Option<String>,
    pub file_name: Option<String>,
    pub file_size: Option<i64>,
    pub thumb_path: Option<String>,
    pub mime_type: Option<String>,
    /// Explicit media classification — set on all new posts; backfilled for old ones.
    pub media_type: Option<MediaType>,
    /// Secondary audio file for image+audio combo posts (audio path only).
    pub audio_file_path: Option<String>,
    pub audio_file_name: Option<String>,
    pub audio_file_size: Option<i64>,
    pub audio_mime_type: Option<String>,
    pub created_at: i64,
    pub deletion_token: String,
    pub is_op: bool,
    /// Set when the post body has been edited; None means never edited.
    pub edited_at: Option<i64>,
}

/// Admin user record
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct AdminUser {
    pub id: i64,
    pub username: String,
    pub password_hash: String,
    pub created_at: i64,
}

/// Active admin session
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct AdminSession {
    pub id: String,
    pub admin_id: i64,
    pub created_at: i64,
    pub expires_at: i64,
}

/// A banned IP hash
#[derive(Debug, Clone)]
pub struct Ban {
    pub id: i64,
    pub ip_hash: String,
    pub reason: Option<String>,
    pub expires_at: Option<i64>,
    #[allow(dead_code)]
    pub created_at: i64,
}

/// A word filter rule
#[derive(Debug, Clone)]
pub struct WordFilter {
    pub id: i64,
    pub pattern: String,
    pub replacement: String,
}

/// Board with live thread count, used on the home page
#[derive(Debug, Clone)]
pub struct BoardStats {
    pub board: Board,
    pub thread_count: i64,
}

/// Summary used on board index: thread + its last few reply counts
#[derive(Debug, Clone)]
pub struct ThreadSummary {
    pub thread: Thread,
    /// Latest N replies (for board index preview)
    pub preview_posts: Vec<Post>,
    /// How many replies are hidden (total - preview shown)
    pub omitted: i64,
}

/// Form data for posting a new thread or reply (parsed from multipart)
#[derive(Debug, Default)]
#[allow(dead_code)]
pub struct PostForm {
    pub name: String,
    pub subject: String,
    pub body: String,
    pub deletion_token: String,
    // File fields are handled separately in multipart parsing
}

/// Form data for deleting a post
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct DeleteForm {
    pub post_id: i64,
    pub deletion_token: String,
    pub board: String,
}

/// Admin ban form
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct BanForm {
    pub ip_hash: String,
    pub reason: String,
    pub duration_hours: Option<i64>,
}

/// Admin login form
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct LoginForm {
    pub username: String,
    pub password: String,
}

/// A poll attached to a thread's OP
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Poll {
    pub id: i64,
    pub thread_id: i64,
    pub question: String,
    pub expires_at: i64,
    pub created_at: i64,
}

/// A single poll option with live vote count (joined from poll_votes)
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct PollOption {
    pub id: i64,
    pub poll_id: i64,
    pub text: String,
    pub position: i64,
    pub vote_count: i64,
}

/// Full poll data passed to templates
#[derive(Debug, Clone)]
pub struct PollData {
    pub poll: Poll,
    pub options: Vec<PollOption>,
    pub total_votes: i64,
    /// Which option_id this user voted for, if any
    pub user_voted_option: Option<i64>,
    /// true when expires_at <= now
    pub is_expired: bool,
}

/// Search query
#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub q: String,
    #[serde(default = "default_page")]
    pub page: i64,
}

fn default_page() -> i64 {
    1
}

/// Pagination helper
#[derive(Debug, Clone)]
pub struct Pagination {
    pub page: i64,
    pub per_page: i64,
    pub total: i64,
}

impl Pagination {
    pub fn new(page: i64, per_page: i64, total: i64) -> Self {
        Self {
            page,
            per_page,
            total,
        }
    }
    pub fn total_pages(&self) -> i64 {
        if self.per_page <= 0 {
            return 1;
        }
        (self.total.saturating_add(self.per_page - 1)) / self.per_page
    }
    pub fn offset(&self) -> i64 {
        (self.page - 1).max(0).saturating_mul(self.per_page)
    }
    pub fn has_prev(&self) -> bool {
        self.page > 1
    }
    pub fn has_next(&self) -> bool {
        self.page < self.total_pages()
    }
}

/// Aggregate site-wide statistics shown on the home page.
#[derive(Debug, Clone, Default)]
pub struct SiteStats {
    /// Total posts ever made
    pub total_posts: i64,
    /// Total image files ever uploaded
    pub total_images: i64,
    /// Total video files ever uploaded
    pub total_videos: i64,
    /// Total audio files ever uploaded
    pub total_audio: i64,
    /// Total bytes of currently stored files (still on disk)
    pub active_bytes: i64,
}

/// A user-filed report against a post
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Report {
    pub id: i64,
    pub post_id: i64,
    pub thread_id: i64,
    pub board_id: i64,
    pub reason: String,
    pub reporter_hash: String,
    pub status: String, // "open" | "resolved"
    pub created_at: i64,
    pub resolved_at: Option<i64>,
    pub resolved_by: Option<i64>,
}

/// Report enriched with context from joined tables (used in admin inbox)
#[derive(Debug, Clone)]
pub struct ReportWithContext {
    pub report: Report,
    pub board_short: String,
    /// First 120 chars of the reported post body for preview
    pub post_preview: String,
    /// IP hash of the post's author (for quick ban from the inbox)
    pub post_ip_hash: String,
}

/// A single entry in the moderation action log
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct ModLogEntry {
    pub id: i64,
    pub admin_id: i64,
    pub admin_name: String,
    /// E.g. "delete_post", "ban", "sticky", "lock", "resolve_report"
    pub action: String,
    /// "post" | "thread" | "board" | "ban" | "report"
    pub target_type: String,
    pub target_id: Option<i64>,
    pub board_short: String,
    /// Human-readable extra context (reason, post body preview, etc.)
    pub detail: String,
    pub created_at: i64,
}

/// Represents a saved backup file on disk (shown in admin panel).
#[derive(Debug, Clone)]
pub struct BackupInfo {
    /// Filename only (no directory path).
    pub filename: String,
    /// Size of the file in bytes.
    pub size_bytes: u64,
    /// Human-readable last-modified timestamp (UTC).
    pub modified: String,
}
