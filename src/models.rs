//! Plain data structures that map database rows and application view models.

use serde::{Deserialize, Serialize};

// Media type classification
/// Classifies an uploaded file as image, video, audio, PDF, or a generic download.
/// Stored as a TEXT column in posts ("image", "video", "audio", "pdf", "other").
///
/// The serde `rename_all = "lowercase"` representation **must** stay in sync
/// with `as_str()` / `from_db_str()`.  Add a round-trip unit test whenever a
/// new variant is introduced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MediaType {
    /// A still or animated image.
    Image,
    /// A video file.
    Video,
    /// An audio file.
    Audio,
    /// A Portable Document Format file.
    Pdf,
    /// Any supported file that does not belong to a richer media category.
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
            "mp4" | "webm" | "mkv" => Self::Video,
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
    /// Anyone may view and post to the board.
    #[default]
    Public,
    /// A password is required to view the board.
    ViewPassword,
    /// A password is required to post to the board.
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
    /// Returns whether viewing the board requires its configured password.
    pub const fn requires_view_password(self) -> bool {
        matches!(self, Self::ViewPassword)
    }

    #[must_use]
    /// Returns whether either viewing or posting is password protected.
    pub const fn is_password_protected(self) -> bool {
        matches!(self, Self::ViewPassword | Self::PostPassword)
    }

    #[must_use]
    /// Returns whether posting requires a prior board-password unlock.
    pub const fn requires_unlock_for_posting(self) -> bool {
        self.is_password_protected()
    }

    #[must_use]
    /// Returns whether posting requires the board password.
    ///
    /// This alias remains for API clarity and backward-compatible call sites.
    pub const fn requires_post_password(self) -> bool {
        self.requires_unlock_for_posting()
    }
}

impl std::fmt::Display for BoardAccessMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Selects how a board obtains its banner assets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum BoardBannerMode {
    /// Use banners inherited from the site-wide configuration.
    #[default]
    Inherit,
    /// Do not show banners on this board.
    None,
    /// Use only banners configured specifically for this board.
    Override,
}

impl BoardBannerMode {
    /// Returns the database representation of this banner mode.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Inherit => "inherit",
            Self::None => "none",
            Self::Override => "override",
        }
    }

    /// Parses a banner mode from its database representation.
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

/// Identifies where a banner asset is eligible to appear.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BannerScope {
    /// The banner is available across the site.
    Global,
    /// The banner belongs to one board.
    Board,
    /// The banner is restricted to the home page.
    Home,
}

impl BannerScope {
    /// Returns the database representation of this scope.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::Board => "board",
            Self::Home => "home",
        }
    }

    /// Parses a scope from its database representation.
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

/// Describes how clicking a banner resolves its destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BannerTargetType {
    /// The banner has no link.
    None,
    /// The banner links to a board identified by its short name.
    InternalBoard,
    /// The banner links to a site-local path.
    InternalPath,
    /// The banner links to an external URL.
    ExternalUrl,
}

impl BannerTargetType {
    /// Returns the database representation of this target type.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::InternalBoard => "internal_board",
            Self::InternalPath => "internal_path",
            Self::ExternalUrl => "external_url",
        }
    }

    /// Parses a target type from its database representation.
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

/// Identifies a page location that can display a banner.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BannerPlacement {
    /// The board index page.
    Index,
    /// The board catalog page.
    Catalog,
}

/// A board, e.g. /tech/ — Technology
#[derive(Debug, Clone, Serialize, Deserialize)]
// This type mirrors serialized or render state, so the boolean count is an intentional tradeoff.
#[expect(
    clippy::struct_excessive_bools,
    reason = "the row model intentionally mirrors independent board feature flags"
)]
pub struct Board {
    /// Database primary key.
    pub id: i64,
    /// Relative display order on the board list.
    pub display_order: i64,
    /// URL-safe board identifier without surrounding slashes, such as `tech`.
    pub short_name: String,
    /// Human-readable board name, such as `Technology`.
    pub name: String,
    /// Short board description shown to visitors.
    pub description: String,
    /// Whether the board is designated not safe for work.
    pub nsfw: bool,
    /// Maximum number of active threads retained on the board.
    pub max_threads: i64,
    /// Maximum number of archived threads retained on the board.
    pub max_archived_threads: i64,
    /// Reply count after which a thread no longer bumps.
    pub bump_limit: i64,
    /// Whether image uploads are accepted.
    pub allow_images: bool,
    /// Whether video uploads are accepted.
    pub allow_video: bool,
    /// Whether audio uploads are accepted.
    pub allow_audio: bool,
    /// Board-specific image size limit in bytes.
    pub max_image_size: i64,
    /// Board-specific video size limit in bytes.
    pub max_video_size: i64,
    /// Board-specific audio size limit in bytes.
    pub max_audio_size: i64,
    /// Board-specific PDF size limit in bytes.
    pub max_pdf_size: i64,
    /// Whether PDF uploads are accepted.
    pub allow_pdf: bool,
    /// Whether generic file uploads are accepted.
    pub allow_any_files: bool,
    /// Whether posters may use tripcodes.
    pub allow_tripcodes: bool,
    /// Whether posters may edit their posts.
    pub allow_editing: bool,
    /// Whether posters may delete their posts.
    pub allow_self_delete: bool,
    /// Legacy edit window in seconds; self-actions use the fixed grace window.
    pub edit_window_secs: i64,
    /// Whether overflow threads are archived instead of deleted.
    pub allow_archive: bool,
    /// Whether supported video links are expanded inline.
    pub allow_video_embeds: bool,
    /// Whether new threads and replies require a CAPTCHA.
    pub allow_captcha: bool,
    /// Whether post headers show thread-local poster identifiers.
    pub show_poster_ids: bool,
    /// Whether long greentext blocks are collapsed automatically.
    pub collapse_greentext: bool,
    /// Required delay between posts in seconds, or zero when disabled.
    pub post_cooldown_secs: i64,
    /// Theme slug used by default, or an empty string to inherit the site theme.
    pub default_theme: String,
    /// Banner selection behavior for the board.
    pub banner_mode: BoardBannerMode,
    /// Password-protection behavior for the board.
    pub access_mode: BoardAccessMode,
    /// Encoded hash for the board access password.
    pub access_password_hash: String,
    /// Creation time as a Unix timestamp.
    pub created_at: i64,
}

impl Board {
    /// Returns the effective image-upload size limit in bytes.
    #[must_use]
    pub fn max_image_size_bytes(&self) -> usize {
        usize::try_from(self.max_image_size)
            .ok()
            .filter(|value| *value > 0)
            .unwrap_or(crate::config::CONFIG.max_image_size)
    }

    /// Returns the effective video-upload size limit in bytes.
    #[must_use]
    pub fn max_video_size_bytes(&self) -> usize {
        usize::try_from(self.max_video_size)
            .ok()
            .filter(|value| *value > 0)
            .unwrap_or(crate::config::CONFIG.max_video_size)
    }

    /// Returns the effective audio-upload size limit in bytes.
    #[must_use]
    pub fn max_audio_size_bytes(&self) -> usize {
        usize::try_from(self.max_audio_size)
            .ok()
            .filter(|value| *value > 0)
            .unwrap_or(crate::config::CONFIG.max_audio_size)
    }

    /// Returns the effective PDF-upload size limit in bytes.
    #[must_use]
    pub fn max_pdf_size_bytes(&self) -> usize {
        usize::try_from(self.max_pdf_size)
            .ok()
            .filter(|value| *value > 0)
            .unwrap_or(crate::config::CONFIG.max_image_size)
    }

    /// Returns the largest effective upload limit across all supported file types.
    #[must_use]
    pub fn max_generic_upload_size_bytes(&self) -> usize {
        self.max_image_size_bytes()
            .max(self.max_video_size_bytes())
            .max(self.max_audio_size_bytes())
            .max(self.max_pdf_size_bytes())
    }
}

/// Stored banner metadata and its display/link configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BannerAsset {
    /// Database primary key.
    pub id: i64,
    /// Pages on which the banner is eligible to appear.
    pub scope: BannerScope,
    /// Owning board identifier for board-scoped banners.
    pub board_id: Option<i64>,
    /// Owning board short name, when joined for display.
    pub board_short: Option<String>,
    /// Relative key used to load the banner from storage.
    pub storage_key: String,
    /// Banner width in pixels.
    pub width: i64,
    /// Banner height in pixels.
    pub height: i64,
    /// Stored file size in bytes.
    pub file_size: i64,
    /// Whether the banner is eligible for selection.
    pub enabled: bool,
    /// Relative order used during deterministic selection.
    pub sort_order: i64,
    /// Kind of navigation performed when the banner is clicked.
    pub target_type: BannerTargetType,
    /// Board name, local path, or URL interpreted according to `target_type`.
    pub target_value: String,
    /// Whether the banner may appear on board index pages.
    pub show_on_index: bool,
    /// Whether the banner may appear on board catalog pages.
    pub show_on_catalog: bool,
    /// Creation time as a Unix timestamp.
    pub created_at: i64,
}

/// A configurable UI theme that may be built-in or admin-defined.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Theme {
    /// Stable identifier used by configuration, markup, and CSS selectors.
    pub slug: String,
    /// Human-readable name shown in theme selectors.
    pub display_name: String,
    /// Short description of the theme.
    pub description: String,
    /// Representative color used for previews.
    pub swatch_hex: String,
    /// Whether visitors may select the theme.
    pub enabled: bool,
    /// Relative display order.
    pub sort_order: i64,
    /// Whether the theme ships with `RustChan`.
    pub is_builtin: bool,
    /// Administrator-provided CSS for custom themes.
    pub custom_css: String,
}

/// A thread (the OP post + its replies share this record for metadata)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Thread {
    /// Database primary key and public thread number.
    pub id: i64,
    /// Identifier of the board containing the thread.
    pub board_id: i64,
    /// Optional subject from the opening post.
    pub subject: Option<String>,
    /// Creation time as a Unix timestamp.
    pub created_at: i64,
    /// Last bump time as a Unix timestamp.
    pub bumped_at: i64,
    /// Whether new replies are prohibited.
    pub locked: bool,
    /// Whether the thread remains ahead of non-sticky threads.
    pub sticky: bool,
    /// Whether the thread has moved out of the active index.
    pub archived: bool,
    /// Number of replies after the opening post.
    pub reply_count: i64,
    /// Number of posts with image attachments.
    pub image_count: i64,
    /// Opening-post body joined for catalog previews.
    pub op_body: Option<String>,
    /// Opening-post media path joined for catalog previews.
    pub op_file: Option<String>,
    /// Opening-post thumbnail path joined for catalog previews.
    pub op_thumb: Option<String>,
    /// Opening-post name joined for catalog previews.
    pub op_name: Option<String>,
    /// Opening-post tripcode joined for catalog previews.
    pub op_tripcode: Option<String>,
    /// Opening-post identifier joined for catalog previews.
    pub op_id: Option<i64>,
}

/// A single post (OP or reply)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Post {
    /// Database primary key and public post number.
    pub id: i64,
    /// Identifier of the containing thread.
    pub thread_id: i64,
    /// Identifier of the containing board.
    pub board_id: i64,
    /// Poster display name.
    pub name: String,
    /// Optional generated tripcode.
    pub tripcode: Option<String>,
    /// Optional post subject.
    pub subject: Option<String>,
    /// Original plain-text post body.
    pub body: String,
    /// Sanitized, pre-rendered post-body HTML.
    pub body_html: String,
    /// SHA-256(IP + secret). `None` for gateway-inserted federation posts
    /// which have no inbound client IP.
    pub ip_hash: Option<String>,
    /// Relative path to the primary attachment.
    pub file_path: Option<String>,
    /// Original filename of the primary attachment.
    pub file_name: Option<String>,
    /// Size of the primary attachment in bytes.
    pub file_size: Option<i64>,
    /// Relative path to the generated thumbnail.
    pub thumb_path: Option<String>,
    /// MIME type of the primary attachment.
    pub mime_type: Option<String>,
    /// Explicit media classification — set on all new posts; backfilled for old ones.
    pub media_type: Option<MediaType>,
    /// Secondary audio file for image+audio combo posts (audio path only).
    pub audio_file_path: Option<String>,
    /// Original filename of the secondary audio attachment.
    pub audio_file_name: Option<String>,
    /// Size of the secondary audio attachment in bytes.
    pub audio_file_size: Option<i64>,
    /// MIME type of the secondary audio attachment.
    pub audio_mime_type: Option<String>,
    /// Creation time as a Unix timestamp.
    pub created_at: i64,
    /// Opaque token authorizing poster-initiated actions.
    pub deletion_token: String,
    /// Whether this post opens its thread.
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
    /// Database primary key.
    pub id: i64,
    /// Unique administrator login name.
    pub username: String,
    /// Excluded from Serialize in practice — be careful not to expose this.
    pub password_hash: String,
    /// Account creation time as a Unix timestamp.
    pub created_at: i64,
}

/// Active admin session
#[derive(Debug, Clone, Serialize)]
pub struct AdminSession {
    /// Opaque session identifier stored in the authentication cookie.
    pub id: String,
    /// Identifier of the authenticated administrator.
    pub admin_id: i64,
    /// Session creation time as a Unix timestamp.
    pub created_at: i64,
    /// Session expiration time as a Unix timestamp.
    pub expires_at: i64,
}

/// A banned IP hash
#[derive(Debug, Clone, Serialize)]
pub struct Ban {
    /// Database primary key.
    pub id: i64,
    /// Privacy-preserving hash identifying the banned address.
    pub ip_hash: String,
    /// Optional moderator-provided explanation.
    pub reason: Option<String>,
    /// Expiration time as a Unix timestamp, or `None` for a permanent ban.
    pub expires_at: Option<i64>,
    /// Creation time as a Unix timestamp.
    pub created_at: i64,
}

/// A word filter rule
#[derive(Debug, Clone, Serialize)]
pub struct WordFilter {
    /// Database primary key.
    pub id: i64,
    /// Pattern matched against submitted text.
    pub pattern: String,
    /// Text substituted for each match.
    pub replacement: String,
}

/// Board with live thread count, used on the home page
#[derive(Debug, Clone, Serialize)]
pub struct BoardStats {
    /// Board configuration and identity.
    pub board: Board,
    /// Current number of active threads.
    pub thread_count: i64,
}

/// Summary used on board index: thread + its last few reply counts
#[derive(Debug, Clone, Serialize)]
pub struct ThreadSummary {
    /// Thread metadata and opening-post preview fields.
    pub thread: Thread,
    /// Latest N replies (for board index preview)
    pub preview_posts: Vec<Post>,
    /// How many replies are hidden (total - preview shown)
    pub omitted: i64,
}

/// A poll attached to a thread's OP
#[derive(Debug, Clone, Serialize)]
pub struct Poll {
    /// Database primary key.
    pub id: i64,
    /// Identifier of the thread that owns the poll.
    pub thread_id: i64,
    /// Question displayed above the choices.
    pub question: String,
    /// Expiration time as a Unix timestamp.
    pub expires_at: i64,
    /// Creation time as a Unix timestamp.
    pub created_at: i64,
}

/// A single poll option with live vote count (joined from `poll_votes`)
#[derive(Debug, Clone, Serialize)]
pub struct PollOption {
    /// Database primary key.
    pub id: i64,
    /// Identifier of the owning poll.
    pub poll_id: i64,
    /// Choice text displayed to voters.
    pub text: String,
    /// Relative display order.
    pub position: i64,
    /// Current number of recorded votes.
    pub vote_count: i64,
}

/// Full poll data passed to templates
#[derive(Debug, Clone, Serialize)]
pub struct PollData {
    /// Poll metadata.
    pub poll: Poll,
    /// Choices in display order.
    pub options: Vec<PollOption>,
    /// Sum of votes across all choices.
    pub total_votes: i64,
    /// Which `option_id` this user voted for, if any
    pub user_voted_option: Option<i64>,
    /// true when `expires_at` <= now
    pub is_expired: bool,
}

/// Maximum number of Unicode scalar values accepted in a search query.
pub const SEARCH_QUERY_MAX_CHARS: usize = 256;

/// Query-string parameters accepted by the search page.
#[derive(Debug, Deserialize)]
pub struct SearchQuery {
    /// User-supplied search text.
    #[serde(default)]
    pub q: String,
    /// One-based result page.
    #[serde(default = "default_page")]
    pub page: i64,
}

/// Returns the first result page for Serde defaults.
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
    /// Current one-based page.
    pub page: i64,
    /// Maximum number of records on a page.
    pub per_page: i64,
    /// Total number of matching records.
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

    /// Returns the zero-based record offset for the current page.
    #[must_use]
    pub fn offset(&self) -> i64 {
        self.page
            .max(1)
            .saturating_sub(1)
            .saturating_mul(self.per_page.max(1))
    }

    /// Returns whether a page exists before the current one.
    #[must_use]
    pub const fn has_prev(&self) -> bool {
        self.page > 1
    }

    /// Returns whether a page exists after the current one.
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
    /// Database primary key.
    pub id: i64,
    /// Identifier of the reported post.
    pub post_id: i64,
    /// Identifier of the thread containing the post.
    pub thread_id: i64,
    /// Identifier of the board containing the post.
    pub board_id: i64,
    /// Reporter-provided explanation.
    pub reason: String,
    /// Privacy-preserving hash identifying the reporter.
    pub reporter_hash: String,
    /// Workflow state, currently `open` or `resolved`.
    pub status: String,
    /// Creation time as a Unix timestamp.
    pub created_at: i64,
    /// Resolution time as a Unix timestamp.
    pub resolved_at: Option<i64>,
    /// Administrator identifier that resolved the report.
    pub resolved_by: Option<i64>,
}

/// Report enriched with context from joined tables (used in admin inbox)
#[derive(Debug, Clone, Serialize)]
pub struct ReportWithContext {
    /// Underlying report record.
    pub report: Report,
    /// Short name of the board containing the reported post.
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
    /// Database primary key.
    pub id: i64,
    /// Identifier of the administrator that performed the action.
    pub admin_id: i64,
    /// Administrator username captured for display.
    pub admin_name: String,
    /// E.g. "`delete_post`", "ban", "sticky", "lock", "`resolve_report`"
    pub action: String,
    /// "post" | "thread" | "board" | "ban" | "report"
    pub target_type: String,
    /// Optional identifier of the affected record.
    pub target_id: Option<i64>,
    /// Short name of the affected board.
    pub board_short: String,
    /// Human-readable extra context (reason, post body preview, etc.)
    pub detail: String,
    /// Action time as a Unix timestamp.
    pub created_at: i64,
}

/// Represents a saved backup file on disk (shown in admin panel).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BackupBoardSummary {
    /// Stable board identifier without surrounding slashes.
    pub short_name: String,
    /// Human-readable board name.
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
    /// Database primary key.
    pub id: i64,
    /// Privacy-preserving hash identifying the appellant.
    pub ip_hash: String,
    /// Appellant-provided explanation.
    pub reason: String,
    /// Workflow state, currently `open` or `dismissed`.
    pub status: String,
    /// Creation time as a Unix timestamp.
    pub created_at: i64,
}

// ChanNet federation snapshot types
// These live in the shared model layer so database code does not depend on the
// handler-only `chan_net` module.

/// A single board entry in a federation snapshot.
/// `id` is the board's `short_name` (e.g. "tech", "b").
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SnapshotBoard {
    /// Board short name used as the portable identifier.
    pub id: String,
    /// Human-readable board title.
    pub title: String,
}

/// A single post in a federation snapshot.
///
/// SECURITY: Text content only. File paths, MIME types, thumbnail paths, and
/// binary data must NEVER be added to this struct.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SnapshotPost {
    /// Public post number.
    pub post_id: u64,
    /// Short name of the post's board.
    pub board: String,
    /// Display name included in the snapshot.
    pub author: String,
    /// Plain-text post content.
    pub content: String,
    /// Creation time as a Unix timestamp.
    pub timestamp: u64,
}

/// Metadata block written into every federation snapshot ZIP.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SnapshotMetadata {
    /// Snapshot generation time as a Unix timestamp.
    pub generated_at: u64,
    /// `RustChan` version that produced the snapshot.
    pub rustchan_version: String,
    /// Number of posts contained in the snapshot.
    pub post_count: u64,
    /// Unique identifier for the snapshot transaction.
    pub tx_id: uuid::Uuid,
    /// Optional detached signature over the snapshot.
    pub signature: Option<String>,
    /// Starting timestamp for a delta snapshot, absent for a full snapshot.
    pub since: Option<u64>,
    /// Whether the snapshot contains only changes since a prior point.
    pub is_delta: bool,
    /// Whether archived threads are included.
    pub includes_archive: bool,
}

// Tests
#[cfg(test)]
mod tests {
    use super::*;

    // MediaType serde ↔ DB string parity
    #[test]
    fn media_type_serde_matches_db_str() {
        for mt in [
            MediaType::Image,
            MediaType::Video,
            MediaType::Audio,
            MediaType::Pdf,
            MediaType::Other,
        ] {
            let db_value = mt.as_str();
            let expected_json = format!("\"{db_value}\"");
            assert!(
                matches!(
                    serde_json::to_string(&mt).as_deref(),
                    Ok(json) if json == expected_json
                ),
                "as_str() and serde disagree for {mt:?}"
            );
            assert_eq!(
                MediaType::from_db_str(db_value),
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
            assert_eq!(
                format!("{mt}"),
                mt.as_str(),
                "Display must match the database representation for {mt:?}"
            );
        }
    }

    #[test]
    fn media_type_from_mime() {
        assert_eq!(
            MediaType::from_mime("image/png"),
            MediaType::Image,
            "image MIME types must be classified as images"
        );
        assert_eq!(
            MediaType::from_mime("video/mp4"),
            MediaType::Video,
            "video MIME types must be classified as videos"
        );
        assert_eq!(
            MediaType::from_mime("audio/ogg"),
            MediaType::Audio,
            "audio MIME types must be classified as audio"
        );
        assert_eq!(
            MediaType::from_mime("application/pdf"),
            MediaType::Pdf,
            "the PDF MIME type must be classified as a PDF"
        );
        assert_eq!(
            MediaType::from_mime("application/json"),
            MediaType::Other,
            "unrecognized MIME types must use the generic classification"
        );
    }

    #[test]
    fn media_type_from_ext() {
        assert_eq!(
            MediaType::from_ext("jpg"),
            MediaType::Image,
            "JPG files must be classified as images"
        );
        assert_eq!(
            MediaType::from_ext("heic"),
            MediaType::Image,
            "HEIC files must be classified as images"
        );
        assert_eq!(
            MediaType::from_ext("mp4"),
            MediaType::Video,
            "MP4 files must be classified as videos"
        );
        assert_eq!(
            MediaType::from_ext("flac"),
            MediaType::Audio,
            "FLAC files must be classified as audio"
        );
        assert_eq!(
            MediaType::from_ext("pdf"),
            MediaType::Pdf,
            "PDF extensions must use the PDF classification"
        );
        assert_eq!(
            MediaType::from_ext("exe"),
            MediaType::Other,
            "unrecognized extensions must use the generic classification"
        );
    }

    #[test]
    fn board_access_mode_serde_matches_db_str() {
        for access_mode in [
            BoardAccessMode::Public,
            BoardAccessMode::ViewPassword,
            BoardAccessMode::PostPassword,
        ] {
            let db_value = access_mode.as_str();
            let expected_json = format!("\"{db_value}\"");
            assert!(
                matches!(
                    serde_json::to_string(&access_mode).as_deref(),
                    Ok(json) if json == expected_json
                ),
                "as_str() and serde disagree for {access_mode:?}"
            );
            assert_eq!(
                BoardAccessMode::from_db_str(db_value),
                Some(access_mode),
                "from_db_str() round-trip failed for {access_mode:?}"
            );
        }
    }

    #[test]
    fn board_access_mode_password_helpers_match_existing_post_requirement() {
        assert!(
            !BoardAccessMode::Public.is_password_protected(),
            "public boards must not be password protected"
        );
        assert!(
            !BoardAccessMode::Public.requires_unlock_for_posting(),
            "public boards must not require a posting unlock"
        );
        assert!(
            !BoardAccessMode::Public.requires_post_password(),
            "the compatibility helper must preserve public-board behavior"
        );

        assert!(
            BoardAccessMode::ViewPassword.is_password_protected(),
            "view-password boards must be password protected"
        );
        assert!(
            BoardAccessMode::ViewPassword.requires_unlock_for_posting(),
            "view-password access must unlock posting too"
        );
        assert!(
            BoardAccessMode::ViewPassword.requires_post_password(),
            "the compatibility helper must preserve view-password behavior"
        );

        assert!(
            BoardAccessMode::PostPassword.is_password_protected(),
            "post-password boards must be password protected"
        );
        assert!(
            BoardAccessMode::PostPassword.requires_unlock_for_posting(),
            "post-password boards must require a posting unlock"
        );
        assert!(
            BoardAccessMode::PostPassword.requires_post_password(),
            "the compatibility helper must preserve post-password behavior"
        );
    }

    #[test]
    fn board_banner_mode_serde_matches_db_str() {
        for banner_mode in [
            BoardBannerMode::Inherit,
            BoardBannerMode::None,
            BoardBannerMode::Override,
        ] {
            let db_value = banner_mode.as_str();
            let expected_json = format!("\"{db_value}\"");
            assert!(
                matches!(
                    serde_json::to_string(&banner_mode).as_deref(),
                    Ok(json) if json == expected_json
                ),
                "as_str() and serde disagree for {banner_mode:?}"
            );
            assert_eq!(
                BoardBannerMode::from_db_str(db_value),
                Some(banner_mode),
                "from_db_str() round-trip failed for {banner_mode:?}"
            );
        }
    }

    #[test]
    fn banner_scope_serde_matches_db_str() {
        for scope in [BannerScope::Global, BannerScope::Board, BannerScope::Home] {
            let db_value = scope.as_str();
            let expected_json = format!("\"{db_value}\"");
            assert!(
                matches!(
                    serde_json::to_string(&scope).as_deref(),
                    Ok(json) if json == expected_json
                ),
                "serde disagrees for {scope:?}"
            );
            assert_eq!(
                BannerScope::from_db_str(db_value),
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
            let db_value = target_type.as_str();
            let expected_json = format!("\"{db_value}\"");
            assert!(
                matches!(
                    serde_json::to_string(&target_type).as_deref(),
                    Ok(json) if json == expected_json
                ),
                "serde disagrees for {target_type:?}"
            );
            assert_eq!(
                BannerTargetType::from_db_str(db_value),
                Some(target_type),
                "from_db_str() round-trip failed for {target_type:?}"
            );
        }
    }

    #[test]
    fn search_query_default_matches_serde_defaults() {
        let query = SearchQuery::default();
        assert!(query.q.is_empty(), "default search text must be empty");
        assert_eq!(query.page, 1, "default searches must start on page one");
    }

    // Pagination
    #[test]
    fn pagination_clamps_inputs() {
        let p = Pagination::new(0, 0, -5);
        assert_eq!(p.page, 1, "page must be clamped to one");
        assert_eq!(p.per_page, 1, "page size must be clamped to one");
        assert_eq!(p.total, 0, "total records must be clamped to zero");
    }

    #[test]
    fn pagination_total_pages_at_least_one() {
        let p = Pagination::new(1, 10, 0);
        assert_eq!(
            p.total_pages(),
            1,
            "empty result sets must still render as one page"
        );
    }

    #[test]
    fn pagination_total_pages_normal() {
        assert_eq!(
            Pagination::new(1, 10, 1).total_pages(),
            1,
            "one record must fit on one page"
        );
        assert_eq!(
            Pagination::new(1, 10, 10).total_pages(),
            1,
            "a full page must not create an extra page"
        );
        assert_eq!(
            Pagination::new(1, 10, 11).total_pages(),
            2,
            "one record beyond the page size must create a second page"
        );
        assert_eq!(
            Pagination::new(1, 10, 20).total_pages(),
            2,
            "two full pages must remain two pages"
        );
        assert_eq!(
            Pagination::new(1, 10, 21).total_pages(),
            3,
            "one record beyond two pages must create a third page"
        );
    }

    #[test]
    fn pagination_offset() {
        assert_eq!(
            Pagination::new(1, 10, 100).offset(),
            0,
            "the first page must begin at offset zero"
        );
        assert_eq!(
            Pagination::new(2, 10, 100).offset(),
            10,
            "the second ten-record page must begin at offset ten"
        );
        assert_eq!(
            Pagination::new(3, 25, 100).offset(),
            50,
            "the third twenty-five-record page must begin at offset fifty"
        );
    }

    #[test]
    fn pagination_offset_clamped_for_bad_page() {
        // Even if someone bypasses new() and manually sets page = -1
        let p = Pagination {
            page: -1,
            per_page: 10,
            total: 50,
        };
        assert_eq!(p.offset(), 0, "invalid negative pages must clamp to zero");
    }

    #[test]
    fn pagination_has_prev_and_next() {
        let p = Pagination::new(1, 10, 30);
        assert!(!p.has_prev(), "the first page must not have a predecessor");
        assert!(
            p.has_next(),
            "the first of three pages must have a successor"
        );

        let p = Pagination::new(2, 10, 30);
        assert!(p.has_prev(), "the second page must have a predecessor");
        assert!(
            p.has_next(),
            "the second of three pages must have a successor"
        );

        let p = Pagination::new(3, 10, 30);
        assert!(p.has_prev(), "the third page must have a predecessor");
        assert!(!p.has_next(), "the final page must not have a successor");
    }

    #[test]
    fn pagination_single_page() {
        let p = Pagination::new(1, 10, 5);
        assert!(!p.has_prev(), "a single page must not have a predecessor");
        assert!(!p.has_next(), "a single page must not have a successor");
        assert_eq!(p.total_pages(), 1, "five records must fit on one page");
    }

    #[test]
    fn pagination_empty_results() {
        let p = Pagination::new(1, 10, 0);
        assert!(
            !p.has_prev(),
            "an empty result page must not have a predecessor"
        );
        assert!(
            !p.has_next(),
            "an empty result page must not have a successor"
        );
        assert_eq!(
            p.total_pages(),
            1,
            "empty results must still expose one display page"
        );
        assert_eq!(p.offset(), 0, "empty results must begin at offset zero");
    }
}
