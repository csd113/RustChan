// Plain data structs that map to database rows.

use serde::{Deserialize, Serialize};

// ─── Media type classification ────────────────────────────────────────────────

/// Classifies an uploaded file as image, video, audio, PDF, or a generic download.
/// Stored as a TEXT column in posts ("image", "video", "audio", "pdf", "other").
///
/// The serde `rename_all = "lowercase"` representation **must** stay in sync
/// with `as_str()` / `from_db_str()`.  Add a round-trip unit test whenever a
/// new variant is introduced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaType {
    Image,
    Video,
    Audio,
    Pdf,
    Other,
}

impl MediaType {
    /// Infer `MediaType` from a MIME type string.
    #[must_use]
    pub fn from_mime(mime: &str) -> Self {
        if mime.starts_with("image/") {
            Self::Image
        } else if mime.starts_with("video/") {
            Self::Video
        } else if mime.starts_with("audio/") {
            Self::Audio
        } else if mime == "application/pdf" {
            Self::Pdf
        } else {
            Self::Other
        }
    }

    /// Infer `MediaType` from a file extension (lowercase, no dot).
    /// Used during the backfill migration for pre-existing posts.
    #[cfg(test)]
    #[must_use]
    pub fn from_ext(ext: &str) -> Self {
        match ext {
            "jpg" | "jpeg" | "png" | "gif" | "webp" | "heic" | "heif" | "bmp" | "tiff" | "tif"
            | "svg" => Self::Image,
            "mp4" | "webm" => Self::Video,
            "mp3" | "ogg" | "flac" | "wav" | "m4a" | "aac" | "opus" => Self::Audio,
            "pdf" => Self::Pdf,
            _ => Self::Other,
        }
    }

    /// Serialise to the TEXT value stored in the database.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Video => "video",
            Self::Audio => "audio",
            Self::Pdf => "pdf",
            Self::Other => "other",
        }
    }

    /// Deserialise from the TEXT value stored in the database.
    #[must_use]
    pub fn from_db_str(s: &str) -> Option<Self> {
        match s {
            "image" => Some(Self::Image),
            "video" => Some(Self::Video),
            "audio" => Some(Self::Audio),
            "pdf" => Some(Self::Pdf),
            "other" => Some(Self::Other),
            _ => None,
        }
    }
}

impl std::fmt::Display for MediaType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Board-level access control mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BoardAccessMode {
    #[default]
    Public,
    ViewPassword,
    PostPassword,
}

impl BoardAccessMode {
    /// Serialise to the TEXT value stored in the database.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::ViewPassword => "view_password",
            Self::PostPassword => "post_password",
        }
    }

    /// Deserialise from the TEXT value stored in the database.
    #[must_use]
    pub fn from_db_str(value: &str) -> Option<Self> {
        match value {
            "public" => Some(Self::Public),
            "view_password" => Some(Self::ViewPassword),
            "post_password" => Some(Self::PostPassword),
            _ => None,
        }
    }

    #[must_use]
    pub const fn requires_view_password(self) -> bool {
        matches!(self, Self::ViewPassword)
    }

    #[must_use]
    pub const fn is_password_protected(self) -> bool {
        matches!(self, Self::ViewPassword | Self::PostPassword)
    }

    #[must_use]
    pub const fn requires_unlock_for_posting(self) -> bool {
        self.is_password_protected()
    }

    #[must_use]
    // This alias remains for API clarity and backward-compatible call sites,
    // even though the newer helper is preferred in most code paths.
    #[allow(dead_code)]
    pub const fn requires_post_password(self) -> bool {
        self.requires_unlock_for_posting()
    }
}

impl std::fmt::Display for BoardAccessMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BoardBannerMode {
    #[default]
    Inherit,
    None,
    Override,
}

impl BoardBannerMode {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inherit => "inherit",
            Self::None => "none",
            Self::Override => "override",
        }
    }

    #[must_use]
    pub fn from_db_str(value: &str) -> Option<Self> {
        match value {
            "inherit" => Some(Self::Inherit),
            "none" => Some(Self::None),
            "override" => Some(Self::Override),
            _ => None,
        }
    }
}

impl std::fmt::Display for BoardBannerMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BannerScope {
    Global,
    Board,
    Home,
}

impl BannerScope {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Board => "board",
            Self::Home => "home",
        }
    }

    #[must_use]
    pub fn from_db_str(value: &str) -> Option<Self> {
        match value {
            "global" => Some(Self::Global),
            "board" => Some(Self::Board),
            "home" => Some(Self::Home),
            _ => None,
        }
    }
}

impl std::fmt::Display for BannerScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BannerTargetType {
    None,
    InternalBoard,
    InternalPath,
    ExternalUrl,
}

impl BannerTargetType {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::InternalBoard => "internal_board",
            Self::InternalPath => "internal_path",
            Self::ExternalUrl => "external_url",
        }
    }

    #[must_use]
    pub fn from_db_str(value: &str) -> Option<Self> {
        match value {
            "none" => Some(Self::None),
            "internal_board" => Some(Self::InternalBoard),
            "internal_path" => Some(Self::InternalPath),
            "external_url" => Some(Self::ExternalUrl),
            _ => None,
        }
    }
}

impl std::fmt::Display for BannerTargetType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerPlacement {
    Index,
    Catalog,
}

/// A board, e.g. /tech/ — Technology
#[derive(Debug, Clone, Serialize, Deserialize)]
// This type mirrors serialized or render state, so the boolean count is an intentional tradeoff.
#[allow(clippy::struct_excessive_bools)]
pub struct Board {
    pub id: i64,
    pub display_order: i64,
    pub short_name: String, // "tech" (no slashes)
    pub name: String,       // "Technology"
    pub description: String,
    pub nsfw: bool,
    pub max_threads: i64,
    pub max_archived_threads: i64,
    pub bump_limit: i64,
    pub allow_images: bool,    // per-board image upload toggle (default: true)
    pub allow_video: bool,     // per-board video upload toggle (default: true)
    pub allow_audio: bool,     // per-board audio upload toggle (default: false)
    pub max_image_size: i64,   // per-board image upload size limit in bytes
    pub max_video_size: i64,   // per-board video upload size limit in bytes
    pub max_audio_size: i64,   // per-board audio upload size limit in bytes
    pub allow_pdf: bool,       // per-board PDF upload toggle (default: off)
    pub allow_any_files: bool, // per-board arbitrary file upload toggle (default: off)
    pub allow_tripcodes: bool,
    pub allow_editing: bool, // per-board post editing toggle (default: true)
    pub allow_self_delete: bool, // per-board self-delete toggle (default: true)
    pub edit_window_secs: i64, // legacy board edit-window value; self-actions use the fixed grace window
    pub allow_archive: bool,   // when true, overflow threads are archived instead of deleted
    pub allow_video_embeds: bool, // per-board inline video embed unfurling (default: true)
    pub allow_captcha: bool,   // per-board PoW CAPTCHA on threads and replies (hashcash-style)
    pub show_poster_ids: bool, // per-board thread-local poster IDs in post headers (default: true)
    pub collapse_greentext: bool, // per-board long greentext auto-collapse toggle
    pub post_cooldown_secs: i64, // seconds a user must wait between posts (0 = disabled)
    pub default_theme: String, // blank = inherit site default
    pub banner_mode: BoardBannerMode,
    pub access_mode: BoardAccessMode,
    pub access_password_hash: String,
    pub created_at: i64, // Unix timestamp
}

impl Board {
    #[must_use]
    pub fn max_image_size_bytes(&self) -> usize {
        usize::try_from(self.max_image_size)
            .ok()
            .filter(|value| *value > 0)
            .unwrap_or(crate::config::CONFIG.max_image_size)
    }

    #[must_use]
    pub fn max_video_size_bytes(&self) -> usize {
        usize::try_from(self.max_video_size)
            .ok()
            .filter(|value| *value > 0)
            .unwrap_or(crate::config::CONFIG.max_video_size)
    }

    #[must_use]
    pub fn max_audio_size_bytes(&self) -> usize {
        usize::try_from(self.max_audio_size)
            .ok()
            .filter(|value| *value > 0)
            .unwrap_or(crate::config::CONFIG.max_audio_size)
    }

    #[must_use]
    pub fn max_generic_upload_size_bytes(&self) -> usize {
        self.max_image_size_bytes()
            .max(self.max_video_size_bytes())
            .max(self.max_audio_size_bytes())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BannerAsset {
    pub id: i64,
    pub scope: BannerScope,
    pub board_id: Option<i64>,
    pub board_short: Option<String>,
    pub storage_key: String,
    pub width: i64,
    pub height: i64,
    pub file_size: i64,
    pub enabled: bool,
    pub sort_order: i64,
    pub target_type: BannerTargetType,
    pub target_value: String,
    pub show_on_index: bool,
    pub show_on_catalog: bool,
    pub created_at: i64,
}

/// A configurable UI theme that may be built-in or admin-defined.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Theme {
    pub slug: String,
    pub display_name: String,
    pub description: String,
    pub swatch_hex: String,
    pub enabled: bool,
    pub sort_order: i64,
    pub is_builtin: bool,
    pub custom_css: String,
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
    pub image_count: i64,
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
    /// SHA-256(IP + secret). `None` for gateway-inserted federation posts
    /// which have no inbound client IP.
    pub ip_hash: Option<String>,
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
    /// Present while async media work is queued/running, or after it has failed.
    pub media_processing_state: Option<String>,
    /// Human-readable detail for failed async media processing.
    pub media_processing_error: Option<String>,
}

/// Admin user record
#[derive(Debug, Clone, Serialize)]
pub struct AdminUser {
    pub id: i64,
    pub username: String,
    /// Excluded from Serialize in practice — be careful not to expose this.
    pub password_hash: String,
    pub created_at: i64,
}

/// Active admin session
#[derive(Debug, Clone, Serialize)]
pub struct AdminSession {
    pub id: String,
    pub admin_id: i64,
    pub created_at: i64,
    pub expires_at: i64,
}

/// A banned IP hash
#[derive(Debug, Clone, Serialize)]
pub struct Ban {
    pub id: i64,
    pub ip_hash: String,
    pub reason: Option<String>,
    pub expires_at: Option<i64>,
    pub created_at: i64,
}

/// A word filter rule
#[derive(Debug, Clone, Serialize)]
pub struct WordFilter {
    pub id: i64,
    pub pattern: String,
    pub replacement: String,
}

/// Board with live thread count, used on the home page
#[derive(Debug, Clone, Serialize)]
pub struct BoardStats {
    pub board: Board,
    pub thread_count: i64,
}

/// Summary used on board index: thread + its last few reply counts
#[derive(Debug, Clone, Serialize)]
pub struct ThreadSummary {
    pub thread: Thread,
    /// Latest N replies (for board index preview)
    pub preview_posts: Vec<Post>,
    /// How many replies are hidden (total - preview shown)
    pub omitted: i64,
}

/// A poll attached to a thread's OP
#[derive(Debug, Clone, Serialize)]
pub struct Poll {
    pub id: i64,
    pub thread_id: i64,
    pub question: String,
    pub expires_at: i64,
    pub created_at: i64,
}

/// A single poll option with live vote count (joined from `poll_votes`)
#[derive(Debug, Clone, Serialize)]
pub struct PollOption {
    pub id: i64,
    pub poll_id: i64,
    pub text: String,
    pub position: i64,
    pub vote_count: i64,
}

/// Full poll data passed to templates
#[derive(Debug, Clone, Serialize)]
pub struct PollData {
    pub poll: Poll,
    pub options: Vec<PollOption>,
    pub total_votes: i64,
    /// Which `option_id` this user voted for, if any
    pub user_voted_option: Option<i64>,
    /// true when `expires_at` <= now
    pub is_expired: bool,
}

/// Search query
pub const SEARCH_QUERY_MAX_CHARS: usize = 256;

#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    #[serde(default)]
    pub q: String,
    #[serde(default = "default_page")]
    pub page: i64,
}

const fn default_page() -> i64 {
    1
}

impl Default for SearchQuery {
    fn default() -> Self {
        Self {
            q: String::new(),
            page: default_page(),
        }
    }
}

/// Pagination helper
#[derive(Debug, Clone, Serialize)]
pub struct Pagination {
    pub page: i64,
    pub per_page: i64,
    pub total: i64,
}

impl Pagination {
    /// Create a new Pagination, clamping all values to sane minimums.
    ///
    /// - `page` is clamped to >= 1
    /// - `per_page` is clamped to >= 1 (avoids division by zero)
    /// - `total` is clamped to >= 0
    #[must_use]
    pub fn new(page: i64, per_page: i64, total: i64) -> Self {
        Self {
            page: page.max(1),
            per_page: per_page.max(1),
            total: total.max(0),
        }
    }

    /// Total number of pages. Always returns at least 1 so templates can
    /// safely display "page 1 of 1" even on empty result sets.
    #[must_use]
    pub fn total_pages(&self) -> i64 {
        // per_page is guaranteed >= 1 by new(), but defend against manual
        // construction just in case.
        let pp = self.per_page.max(1);
        let t = self.total.max(0);
        ((t + pp - 1) / pp).max(1)
    }

    #[must_use]
    pub fn offset(&self) -> i64 {
        self.page
            .max(1)
            .saturating_sub(1)
            .saturating_mul(self.per_page.max(1))
    }

    #[must_use]
    pub const fn has_prev(&self) -> bool {
        self.page > 1
    }

    #[must_use]
    pub fn has_next(&self) -> bool {
        self.page < self.total_pages()
    }
}

/// Aggregate site-wide statistics shown on the home page.
#[derive(Debug, Clone, Default, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
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
#[derive(Debug, Clone, Serialize)]
pub struct ReportWithContext {
    pub report: Report,
    pub board_short: String,
    /// First 120 chars of the reported post body for preview
    pub post_preview: String,
    /// IP hash of the post's author (for quick ban from the inbox).
    /// `None` for gateway-inserted federation posts which have no client IP.
    pub post_ip_hash: Option<String>,
}

/// A single entry in the moderation action log
#[derive(Debug, Clone, Serialize)]
pub struct ModLogEntry {
    pub id: i64,
    pub admin_id: i64,
    pub admin_name: String,
    /// E.g. "`delete_post`", "ban", "sticky", "lock", "`resolve_report`"
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
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupBoardSummary {
    pub short_name: String,
    pub name: String,
}

/// Represents a saved backup file on disk (shown in admin panel).
#[derive(Debug, Clone, Serialize)]
pub struct BackupInfo {
    /// Stable saved-backup reference used by admin actions.
    pub backup_ref: String,
    /// Human-readable backup identity shown in the UI.
    pub backup_id: String,
    /// Display name for legacy zip files or default download names.
    pub filename: String,
    /// Total backup size in bytes.
    pub size_bytes: u64,
    /// Human-readable last-modified timestamp (UTC).
    pub modified: String,
    /// Last-modified timestamp as a Unix epoch second when available.
    pub modified_epoch: Option<i64>,
    /// Whether the backup passed the app's structural verification.
    pub verified: bool,
    /// Short note describing verification status or the detected problem.
    pub verification_note: String,
    /// Saved backup scope label.
    pub scope: String,
    /// Storage mode label such as single ZIP, split ZIP, or directory.
    pub mode: String,
    /// Number of ZIP parts when split storage is used.
    pub part_count: u32,
    /// ZIP part filenames relative to the backup parts directory.
    pub part_filenames: Vec<String>,
    /// Whether this full backup includes the Tor hidden service identity files.
    pub contains_tor_hidden_service_keys: bool,
    /// Boards indexed inside the backup when available.
    pub boards: Vec<BackupBoardSummary>,
    /// Absolute server-local backup directory or legacy archive path.
    pub server_path: String,
    /// Manifest path when available.
    pub manifest_path: String,
    /// Whether the backup can be downloaded as a single archive directly.
    pub downloadable_archive: bool,
}

/// A user-submitted ban appeal
#[derive(Debug, Clone, Serialize)]
pub struct BanAppeal {
    pub id: i64,
    pub ip_hash: String,
    pub reason: String,
    pub status: String, // "open" | "dismissed"
    pub created_at: i64,
}

// ─── ChanNet federation snapshot types ───────────────────────────────────────
//
// Defined here (not in chan_net::snapshot) so that src/db/chan_net.rs can
// reference SnapshotPost without creating a layering inversion. chan_net is
// declared in main.rs and is therefore not accessible from the lib crate;
// models.rs is re-exported by lib.rs and is safe to import from anywhere.
//
// chan_net::snapshot re-exports these types so that all existing call-sites
// (snapshot::SnapshotPost, etc.) continue to compile without change.

/// A single board entry in a federation snapshot.
/// `id` is the board's `short_name` (e.g. "tech", "b").
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SnapshotBoard {
    pub id: String,
    pub title: String,
}

/// A single post in a federation snapshot.
///
/// SECURITY: Text content only. File paths, MIME types, thumbnail paths, and
/// binary data must NEVER be added to this struct.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SnapshotPost {
    pub post_id: u64,
    pub board: String,
    pub author: String,
    pub content: String,
    pub timestamp: u64,
}

/// Metadata block written into every federation snapshot ZIP.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SnapshotMetadata {
    pub generated_at: u64,
    pub rustchan_version: String,
    pub post_count: u64,
    pub tx_id: uuid::Uuid,
    pub signature: Option<String>,
    // Delta fields — always None / false in full federation snapshots.
    pub since: Option<u64>,
    pub is_delta: bool,
    pub includes_archive: bool,
}

// ─── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── MediaType serde ↔ DB string parity ────────────────────────────────

    #[test]
    fn media_type_serde_matches_db_str() {
        for mt in [
            MediaType::Image,
            MediaType::Video,
            MediaType::Audio,
            MediaType::Pdf,
            MediaType::Other,
        ] {
            let json =
                serde_json::to_string(&mt).expect("MediaType always serialises to a JSON string");
            let json_str = json.trim_matches('"');
            assert_eq!(
                mt.as_str(),
                json_str,
                "as_str() and serde disagree for {mt:?}"
            );
            assert_eq!(
                MediaType::from_db_str(json_str),
                Some(mt),
                "from_db_str() round-trip failed for {mt:?}"
            );
        }
    }

    #[test]
    fn media_type_display_matches_as_str() {
        for mt in [
            MediaType::Image,
            MediaType::Video,
            MediaType::Audio,
            MediaType::Pdf,
            MediaType::Other,
        ] {
            assert_eq!(format!("{mt}"), mt.as_str());
        }
    }

    #[test]
    fn media_type_from_mime() {
        assert_eq!(MediaType::from_mime("image/png"), MediaType::Image);
        assert_eq!(MediaType::from_mime("video/mp4"), MediaType::Video);
        assert_eq!(MediaType::from_mime("audio/ogg"), MediaType::Audio);
        assert_eq!(MediaType::from_mime("application/pdf"), MediaType::Pdf);
        assert_eq!(MediaType::from_mime("application/json"), MediaType::Other);
    }

    #[test]
    fn media_type_from_ext() {
        assert_eq!(MediaType::from_ext("jpg"), MediaType::Image);
        assert_eq!(MediaType::from_ext("heic"), MediaType::Image);
        assert_eq!(MediaType::from_ext("mp4"), MediaType::Video);
        assert_eq!(MediaType::from_ext("flac"), MediaType::Audio);
        assert_eq!(MediaType::from_ext("pdf"), MediaType::Pdf);
        assert_eq!(MediaType::from_ext("exe"), MediaType::Other);
    }

    #[test]
    fn board_access_mode_serde_matches_db_str() {
        for access_mode in [
            BoardAccessMode::Public,
            BoardAccessMode::ViewPassword,
            BoardAccessMode::PostPassword,
        ] {
            let json = serde_json::to_string(&access_mode)
                .expect("BoardAccessMode always serialises to a JSON string");
            let json_str = json.trim_matches('"');
            assert_eq!(
                access_mode.as_str(),
                json_str,
                "as_str() and serde disagree for {access_mode:?}"
            );
            assert_eq!(
                BoardAccessMode::from_db_str(json_str),
                Some(access_mode),
                "from_db_str() round-trip failed for {access_mode:?}"
            );
        }
    }

    #[test]
    fn board_access_mode_password_helpers_match_existing_post_requirement() {
        assert!(!BoardAccessMode::Public.is_password_protected());
        assert!(!BoardAccessMode::Public.requires_unlock_for_posting());
        assert!(!BoardAccessMode::Public.requires_post_password());

        assert!(BoardAccessMode::ViewPassword.is_password_protected());
        assert!(BoardAccessMode::ViewPassword.requires_unlock_for_posting());
        assert!(BoardAccessMode::ViewPassword.requires_post_password());

        assert!(BoardAccessMode::PostPassword.is_password_protected());
        assert!(BoardAccessMode::PostPassword.requires_unlock_for_posting());
        assert!(BoardAccessMode::PostPassword.requires_post_password());
    }

    #[test]
    fn board_banner_mode_serde_matches_db_str() {
        for banner_mode in [
            BoardBannerMode::Inherit,
            BoardBannerMode::None,
            BoardBannerMode::Override,
        ] {
            let json = serde_json::to_string(&banner_mode)
                .expect("BoardBannerMode always serialises to a JSON string");
            let json_str = json.trim_matches('"');
            assert_eq!(
                banner_mode.as_str(),
                json_str,
                "as_str() and serde disagree for {banner_mode:?}"
            );
            assert_eq!(
                BoardBannerMode::from_db_str(json_str),
                Some(banner_mode),
                "from_db_str() round-trip failed for {banner_mode:?}"
            );
        }
    }

    #[test]
    fn banner_scope_serde_matches_db_str() {
        for scope in [BannerScope::Global, BannerScope::Board, BannerScope::Home] {
            let json =
                serde_json::to_string(&scope).expect("BannerScope always serialises to JSON");
            let json_str = json.trim_matches('"');
            assert_eq!(scope.as_str(), json_str, "serde disagrees for {scope:?}");
            assert_eq!(
                BannerScope::from_db_str(json_str),
                Some(scope),
                "from_db_str() round-trip failed for {scope:?}"
            );
        }
    }

    #[test]
    fn banner_target_type_serde_matches_db_str() {
        for target_type in [
            BannerTargetType::None,
            BannerTargetType::InternalBoard,
            BannerTargetType::InternalPath,
            BannerTargetType::ExternalUrl,
        ] {
            let json = serde_json::to_string(&target_type)
                .expect("BannerTargetType always serialises to JSON");
            let json_str = json.trim_matches('"');
            assert_eq!(
                target_type.as_str(),
                json_str,
                "serde disagrees for {target_type:?}"
            );
            assert_eq!(
                BannerTargetType::from_db_str(json_str),
                Some(target_type),
                "from_db_str() round-trip failed for {target_type:?}"
            );
        }
    }

    #[test]
    fn search_query_default_matches_serde_defaults() {
        let query = SearchQuery::default();
        assert!(query.q.is_empty());
        assert_eq!(query.page, 1);
    }

    // ── Pagination ────────────────────────────────────────────────────────

    #[test]
    fn pagination_clamps_inputs() {
        let p = Pagination::new(0, 0, -5);
        assert_eq!(p.page, 1);
        assert_eq!(p.per_page, 1);
        assert_eq!(p.total, 0);
    }

    #[test]
    fn pagination_total_pages_at_least_one() {
        let p = Pagination::new(1, 10, 0);
        assert_eq!(p.total_pages(), 1);
    }

    #[test]
    fn pagination_total_pages_normal() {
        assert_eq!(Pagination::new(1, 10, 1).total_pages(), 1);
        assert_eq!(Pagination::new(1, 10, 10).total_pages(), 1);
        assert_eq!(Pagination::new(1, 10, 11).total_pages(), 2);
        assert_eq!(Pagination::new(1, 10, 20).total_pages(), 2);
        assert_eq!(Pagination::new(1, 10, 21).total_pages(), 3);
    }

    #[test]
    fn pagination_offset() {
        assert_eq!(Pagination::new(1, 10, 100).offset(), 0);
        assert_eq!(Pagination::new(2, 10, 100).offset(), 10);
        assert_eq!(Pagination::new(3, 25, 100).offset(), 50);
    }

    #[test]
    fn pagination_offset_clamped_for_bad_page() {
        // Even if someone bypasses new() and manually sets page = -1
        let p = Pagination {
            page: -1,
            per_page: 10,
            total: 50,
        };
        assert_eq!(p.offset(), 0);
    }

    #[test]
    fn pagination_has_prev_and_next() {
        let p = Pagination::new(1, 10, 30);
        assert!(!p.has_prev());
        assert!(p.has_next());

        let p = Pagination::new(2, 10, 30);
        assert!(p.has_prev());
        assert!(p.has_next());

        let p = Pagination::new(3, 10, 30);
        assert!(p.has_prev());
        assert!(!p.has_next());
    }

    #[test]
    fn pagination_single_page() {
        let p = Pagination::new(1, 10, 5);
        assert!(!p.has_prev());
        assert!(!p.has_next());
        assert_eq!(p.total_pages(), 1);
    }

    #[test]
    fn pagination_empty_results() {
        let p = Pagination::new(1, 10, 0);
        assert!(!p.has_prev());
        assert!(!p.has_next());
        assert_eq!(p.total_pages(), 1);
        assert_eq!(p.offset(), 0);
    }
}
