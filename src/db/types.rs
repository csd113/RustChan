use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;

/// Shared `SQLite` connection pool type.
pub type DbPool = Pool<SqliteConnectionManager>;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Values required to insert a post.
pub struct NewPost {
    /// Parent thread identifier.
    pub thread_id: i64,
    /// Owning board identifier.
    pub board_id: i64,
    /// Display name supplied by the poster.
    pub name: String,
    /// Derived tripcode, when one was supplied.
    pub tripcode: Option<String>,
    /// Optional post subject.
    pub subject: Option<String>,
    /// Original post body.
    pub body: String,
    /// Sanitized rendered post body.
    pub body_html: String,
    /// Privacy-preserving source-address hash.
    pub ip_hash: Option<String>,
    /// Stored primary-media path.
    pub file_path: Option<String>,
    /// Original primary-media filename.
    pub file_name: Option<String>,
    /// Primary-media size in bytes.
    pub file_size: Option<i64>,
    /// Stored thumbnail path.
    pub thumb_path: Option<String>,
    /// Primary-media MIME type.
    pub mime_type: Option<String>,
    /// Application media classification.
    pub media_type: Option<String>,
    /// Stored companion-audio path.
    pub audio_file_path: Option<String>,
    /// Original companion-audio filename.
    pub audio_file_name: Option<String>,
    /// Companion-audio size in bytes.
    pub audio_file_size: Option<i64>,
    /// Companion-audio MIME type.
    pub audio_mime_type: Option<String>,
    /// Secret token authorizing self-service deletion.
    pub deletion_token: String,
    /// Whether this post opens its thread.
    pub is_op: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Stored paths and MIME type associated with a deduplicated file hash.
pub struct CachedFile {
    /// Stored primary-file path.
    pub file_path: String,
    /// Stored thumbnail path.
    pub thumb_path: String,
    /// Primary-file MIME type.
    pub mime_type: String,
}
