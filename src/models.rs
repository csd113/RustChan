//! Plain data structures that map database rows and application view models.

use serde::{Deserialize, Serialize};

// Media type classification
/// Classifies an uploaded file as image, video, audio, PDF, or a generic download.
/// Stored as a TEXT column in posts ("image", "video", "audio", "pdf", "other").
///
/// The serde `rename_all = "lowercase"` representation **must** stay in sync
/// with `as_str()` / `from_db_str()`. Keep serialization and database values aligned
/// when introducing a variant.
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

/// A board, e.g. /tech/ — Technology.
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

/// A thread (the OP post + its replies share this record for metadata).
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
    /// Latest post time, including replies that do not bump the thread.
    pub last_post_at: i64,
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

/// A single post (OP or reply).
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
    /// SHA-256(IP + secret). `None` when the post has no inbound client IP.
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

/// Admin user record.
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

/// Active admin session.
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

/// A banned IP hash.
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

/// A word filter rule.
#[derive(Debug, Clone, Serialize)]
pub struct WordFilter {
    /// Database primary key.
    pub id: i64,
    /// Pattern matched against submitted text.
    pub pattern: String,
    /// Text substituted for each match.
    pub replacement: String,
}

/// Board with live thread count, used on the home page.
#[derive(Debug, Clone, Serialize)]
pub struct BoardStats {
    /// Board configuration and identity.
    pub board: Board,
    /// Current number of active threads.
    pub thread_count: i64,
}

/// Summary used on board index: thread + its last few reply counts.
#[derive(Debug, Clone, Serialize)]
pub struct ThreadSummary {
    /// Thread metadata and opening-post preview fields.
    pub thread: Thread,
    /// Latest N replies (for board index preview).
    pub preview_posts: Vec<Post>,
    /// How many replies are hidden (total - preview shown).
    pub omitted: i64,
}

/// A poll attached to a thread's OP.
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

/// A single poll option with live vote count (joined from `poll_votes`).
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

/// Full poll data passed to templates.
#[derive(Debug, Clone, Serialize)]
pub struct PollData {
    /// Poll metadata.
    pub poll: Poll,
    /// Choices in display order.
    pub options: Vec<PollOption>,
    /// Sum of votes across all choices.
    pub total_votes: i64,
    /// Which `option_id` this user voted for, if any.
    pub user_voted_option: Option<i64>,
    /// true when `expires_at` <= now.
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

/// Pagination helper.
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
        // Computing the ceiling from quotient/remainder avoids overflowing
        // when a legitimate total approaches i64::MAX. Both operands are
        // nonnegative and the divisor is strictly positive.
        t.div_euclid(pp)
            .saturating_add(i64::from(t.rem_euclid(pp) != 0))
            .max(1)
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
    /// Total posts ever made.
    pub total_posts: i64,
    /// Total image files ever uploaded.
    pub total_images: i64,
    /// Total video files ever uploaded.
    pub total_videos: i64,
    /// Total audio files ever uploaded.
    pub total_audio: i64,
    /// Total bytes of currently stored files (still on disk).
    pub active_bytes: i64,
}

/// A user-filed report against a post.
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

/// Report enriched with context from joined tables (used in admin inbox).
#[derive(Debug, Clone, Serialize)]
pub struct ReportWithContext {
    /// Underlying report record.
    pub report: Report,
    /// Short name of the board containing the reported post.
    pub board_short: String,
    /// First 120 chars of the reported post body for preview.
    pub post_preview: String,
    /// IP hash of the post's author (for quick ban from the inbox).
    /// `None` when the post has no inbound client IP.
    pub post_ip_hash: Option<String>,
}

/// A single entry in the moderation action log.
#[derive(Debug, Clone, Serialize)]
pub struct ModLogEntry {
    /// Database primary key.
    pub id: i64,
    /// Identifier of the administrator that performed the action.
    pub admin_id: i64,
    /// Administrator username captured for display.
    pub admin_name: String,
    /// E.g. "`delete_post`", "ban", "sticky", "lock", "`resolve_report`".
    pub action: String,
    /// "post" | "thread" | "board" | "ban" | "report".
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

/// A user-submitted ban appeal.
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
