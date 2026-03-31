// src/db/types.rs

use r2d2::Pool;
use r2d2_sqlite::SqliteConnectionManager;

pub type DbPool = Pool<SqliteConnectionManager>;

#[derive(Clone)]
pub struct NewPost {
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
    pub audio_file_path: Option<String>,
    pub audio_file_name: Option<String>,
    pub audio_file_size: Option<i64>,
    pub audio_mime_type: Option<String>,
    pub deletion_token: String,
    pub is_op: bool,
}

pub struct CachedFile {
    pub file_path: String,
    pub thumb_path: String,
    pub mime_type: String,
}
