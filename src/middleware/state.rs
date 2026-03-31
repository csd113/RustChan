// src/middleware/state.rs

#[derive(Clone)]
pub struct AppState {
    pub db: crate::db::DbPool,
    pub ffmpeg_available: bool,
    pub ffmpeg_webp_available: bool,
    pub job_queue: std::sync::Arc<crate::workers::JobQueue>,
    pub backup_progress: std::sync::Arc<crate::middleware::BackupProgress>,
    pub chan_ledger:
        Option<std::sync::Arc<parking_lot::Mutex<std::collections::HashSet<uuid::Uuid>>>>,
    pub onion_address: std::sync::Arc<tokio::sync::RwLock<Option<String>>>,
}
