// models.rs — plain data structs that map 1:1 to database rows.
// No ORM magic; fields match column names for easy rusqlite mapping.

use serde::{Deserialize, Serialize};

/// A board, e.g. /tech/ — Technology
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Board {
    pub id: i64,
    pub short_name: String,  // "tech" (no slashes)
    pub name: String,        // "Technology"
    pub description: String,
    pub nsfw: bool,
    pub max_threads: i64,
    pub bump_limit: i64,
    pub created_at: i64,     // Unix timestamp
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
    pub body_html: String,   // pre-rendered HTML (greentext, links, >>refs)
    pub ip_hash: String,
    pub file_path: Option<String>,
    pub file_name: Option<String>,
    pub file_size: Option<i64>,
    pub thumb_path: Option<String>,
    pub mime_type: Option<String>,
    pub created_at: i64,
    pub deletion_token: String,
    pub is_op: bool,
}

/// Admin user record
#[derive(Debug, Clone)]
pub struct AdminUser {
    pub id: i64,
    pub username: String,
    pub password_hash: String,
    pub created_at: i64,
}

/// Active admin session
#[derive(Debug, Clone)]
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
pub struct PostForm {
    pub name: String,
    pub subject: String,
    pub body: String,
    pub deletion_token: String,
    // File fields are handled separately in multipart parsing
}

/// Form data for deleting a post
#[derive(Debug, Deserialize)]
pub struct DeleteForm {
    pub post_id: i64,
    pub deletion_token: String,
    pub board: String,
}

/// Admin ban form
#[derive(Debug, Deserialize)]
pub struct BanForm {
    pub ip_hash: String,
    pub reason: String,
    pub duration_hours: Option<i64>,
}

/// Admin login form
#[derive(Debug, Deserialize)]
pub struct LoginForm {
    pub username: String,
    pub password: String,
}

/// Search query
#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    pub q: String,
    #[serde(default = "default_page")]
    pub page: i64,
}

fn default_page() -> i64 { 1 }

/// Pagination helper
#[derive(Debug, Clone)]
pub struct Pagination {
    pub page: i64,
    pub per_page: i64,
    pub total: i64,
}

impl Pagination {
    pub fn new(page: i64, per_page: i64, total: i64) -> Self {
        Self { page, per_page, total }
    }
    pub fn total_pages(&self) -> i64 {
        (self.total + self.per_page - 1) / self.per_page
    }
    pub fn offset(&self) -> i64 {
        (self.page - 1) * self.per_page
    }
    pub fn has_prev(&self) -> bool { self.page > 1 }
    pub fn has_next(&self) -> bool { self.page < self.total_pages() }
}
