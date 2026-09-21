//! Bounded managed-media auditing and conservative orphan reconciliation.
//!
//! The reconciler treats database lifecycle rows as semantic claims rather
//! than trusting arbitrary strings. A malformed or incomplete reference scan
//! deliberately disables orphan repair for that pass.

#![cfg_attr(
    not(test),
    expect(
        clippy::missing_docs_in_private_items,
        reason = "the public audit contract is fully documented; private snapshot and walker fields mirror that contract and remain module-local"
    )
)]
#![expect(
    clippy::manual_let_else,
    clippy::single_match_else,
    reason = "explicit result matches keep entry-local quarantine side effects adjacent to the failing filesystem operation"
)]

use anyhow::{Context as _, Result};
use rusqlite::{params, OptionalExtension as _};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ffi::OsStr;
use std::io::Read as _;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const DEFAULT_EXAMPLE_LIMIT: usize = 32;
const HASH_BUFFER_BYTES: usize = 8 * 1024;

static FILES_SCANNED_TOTAL: AtomicU64 = AtomicU64::new(0);
static REFERENCES_SCANNED_TOTAL: AtomicU64 = AtomicU64::new(0);
static MISSING_REFERENCES_TOTAL: AtomicU64 = AtomicU64::new(0);
static SAFE_ORPHAN_BYTES_TOTAL: AtomicU64 = AtomicU64::new(0);
static AMBIGUOUS_FILES_TOTAL: AtomicU64 = AtomicU64::new(0);
static REPAIRS_TOTAL: AtomicU64 = AtomicU64::new(0);
static REPAIR_CONFLICTS_TOTAL: AtomicU64 = AtomicU64::new(0);
static INCOMPLETE_SCANS_TOTAL: AtomicU64 = AtomicU64::new(0);

/// Whether a reconciliation pass may schedule narrowly proven repairs.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ReconcileMode {
    /// Inspect and report without changing database rows or files.
    #[default]
    Audit,
    /// Revalidate and durably schedule only explicitly supported repairs.
    Repair,
}

/// Bounded work limits for one reconciliation pass.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReconcileLimits {
    /// Maximum filesystem entries classified in one pass.
    pub files_per_pass: usize,
    /// Maximum authoritative database rows loaded into one snapshot.
    pub database_rows_per_pass: usize,
    /// Maximum bytes read while verifying digests in one pass.
    pub hash_bytes_per_pass: u64,
    /// Maximum automatic repair attempts in one pass.
    pub repairs_per_pass: usize,
    /// Maximum examples retained in the report.
    pub examples_per_pass: usize,
}

impl Default for ReconcileLimits {
    fn default() -> Self {
        Self {
            files_per_pass: 512,
            database_rows_per_pass: 16_384,
            hash_bytes_per_pass: 64 * 1024 * 1024,
            repairs_per_pass: 32,
            examples_per_pass: DEFAULT_EXAMPLE_LIMIT,
        }
    }
}

impl ReconcileLimits {
    /// Clamp zero and excessively large values to operationally bounded limits.
    #[must_use]
    pub fn bounded(self) -> Self {
        Self {
            files_per_pass: self.files_per_pass.clamp(1, 10_000),
            database_rows_per_pass: self.database_rows_per_pass.clamp(1, 250_000),
            hash_bytes_per_pass: self.hash_bytes_per_pass.clamp(1, 4 * 1024 * 1024 * 1024),
            repairs_per_pass: self.repairs_per_pass.clamp(1, 1_000),
            examples_per_pass: self.examples_per_pass.clamp(1, 256),
        }
    }
}

/// Resume point for lexically ordered managed-filesystem enumeration.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconcileCursor {
    /// Opaque lexical key of the last filesystem entry inspected.
    pub after_managed_key: Option<String>,
}

/// Known storage category of an audited managed entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaCategory {
    /// Primary upload or an installed media output in a board root.
    Original,
    /// Image, video, document, or placeholder thumbnail.
    Thumbnail,
    /// Audio waveform derived from a post source.
    Waveform,
    /// Deterministic output created by a video media job.
    TranscodedOutput,
    /// Artifact below the durable upload staging root.
    UploadStage,
    /// Recognized temporary upload artifact.
    KnownTemporary,
    /// Entry whose managed-media category cannot be established.
    Unknown,
}

/// Deterministic result assigned to every inspected reference or entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditClassification {
    /// Active reference exists and the file passes safety validation.
    Healthy,
    /// Valid lifecycle work temporarily protects the path.
    LifecycleInProgress,
    /// A valid durable intent already schedules the path for deletion.
    ScheduledDeletion,
    /// A primary post file is missing without a proven prune transition.
    MissingPrimary,
    /// A thumbnail referenced by an active post is missing.
    MissingThumbnail,
    /// An audio waveform referenced by an active post is missing.
    MissingWaveform,
    /// A deterministic media-job output is missing.
    MissingDeterministicOutput,
    /// A staged source is missing while its required destination is installed.
    MissingStageInstalledDestination,
    /// A pruned post intentionally retains a missing original path identity.
    IntentionallyPrunedOriginal,
    /// Missing derived media can be reconstructed from a valid source.
    RecoverableMissingDerived,
    /// An active reference is missing and no unambiguous recovery exists.
    UnrecoverableActiveReference,
    /// A regular board-owned file has proven reference absence.
    SafeOrphanCandidate,
    /// Apparent reference absence cannot be proven safe.
    AmbiguousOrphan,
    /// A hash row points to a missing file and no lifecycle needs it.
    StaleHashMissingFile,
    /// A hash row is the only remaining claim on an installed file.
    StaleHashUnreferenced,
    /// An active primary path has no corresponding hash metadata.
    MissingHashMetadata,
    /// Stored digest metadata disagrees with the installed regular file.
    DigestConflict,
    /// Multiple digest rows conflict for one physical managed path.
    ConflictingHashMetadata,
    /// Metadata or a lifecycle owner names another board's path.
    CrossBoardMetadata,
    /// A job no longer has valid work or cleanup responsibility.
    ObsoleteJob,
    /// A job payload cannot be decoded or trusted.
    MalformedJob,
    /// A cleanup intent conflicts with an active reference.
    IntentConflictsWithActiveReference,
    /// A completed intent can be removed without further filesystem change.
    CompletedIntent,
    /// A lifecycle intent is internally inconsistent.
    IntentInconsistency,
    /// An intent payload cannot be decoded or trusted.
    MalformedIntent,
    /// An intent names an external or otherwise unsafe path.
    UnsafeExternalIntent,
    /// A symbolic link was found inside managed storage.
    UnsafeSymlink,
    /// A multiply linked file was found inside managed storage.
    UnsafeHardLink,
    /// An unexpected directory was found inside managed storage.
    UnexpectedDirectory,
    /// A device, socket, FIFO, or other special entry was found.
    UnsafeSpecialEntry,
    /// A path is invalid, non-normal, non-UTF-8, or crosses a filesystem.
    UnsafePath,
    /// A filesystem entry is associated with the wrong or unknown board.
    CrossBoardPath,
    /// An item could not be completely inspected.
    ScanError,
}

impl AuditClassification {
    /// Whether this classification represents a missing authoritative path.
    const fn is_missing(self) -> bool {
        matches!(
            self,
            Self::MissingPrimary
                | Self::MissingThumbnail
                | Self::MissingWaveform
                | Self::MissingDeterministicOutput
                | Self::UnrecoverableActiveReference
        )
    }
}

/// Conservative operator or automatic action associated with a classification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecommendedAction {
    /// No change is recommended.
    None,
    /// Retain the file or row and inspect it manually.
    PreserveAndReview,
    /// Permit durable orphan cleanup only after write-transaction revalidation.
    ScheduleDurableDeletion,
    /// Remove hash metadata only after transactional revalidation.
    RemoveStaleHash,
    /// Resolve a terminal job only when no output cleanup remains.
    ResolveObsoleteJob,
    /// Remove an intent whose intended state is already fully established.
    RemoveCompletedIntent,
    /// Recreate derived media through the existing durable job lifecycle.
    EnqueueDerivedMedia,
}

/// One bounded, path-safe example retained in an audit report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditExample {
    /// Assigned classification.
    pub classification: AuditClassification,
    /// Best-known media category.
    pub category: MediaCategory,
    /// Owning board short name when it is trusted.
    pub board: Option<String>,
    /// Board-relative identifier or a redacted path fingerprint.
    pub managed_id: String,
    /// Post, hash, job, or intent identity when applicable.
    pub owner: Option<String>,
    /// File bytes represented by this example.
    pub bytes: u64,
    /// Recommended action.
    pub recommended_action: RecommendedAction,
}

/// Outcome counters for automatic repair attempts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct RepairSummary {
    /// Orphan files durably scheduled for deletion.
    pub files_scheduled: u64,
    /// Bytes represented by scheduled orphan cleanup intents.
    pub bytes_scheduled: u64,
    /// Unambiguously stale hash rows removed.
    pub stale_hash_rows_removed: u64,
    /// Obsolete terminal job rows removed.
    pub obsolete_jobs_removed: u64,
    /// Proven-complete filesystem intents removed.
    pub completed_intents_removed: u64,
    /// Repairs skipped because identity or references changed.
    pub revalidation_conflicts: u64,
    /// Repairs that failed without stopping unrelated candidates.
    pub failures: u64,
}

impl RepairSummary {
    /// Total mutations or durable filesystem schedules completed.
    #[must_use]
    pub const fn completed(self) -> u64 {
        self.files_scheduled
            .saturating_add(self.stale_hash_rows_removed)
            .saturating_add(self.obsolete_jobs_removed)
            .saturating_add(self.completed_intents_removed)
    }
}

/// Deterministic summary of one bounded audit or repair pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuditReport {
    /// Random identifier used to correlate logs from this pass.
    pub scan_id: String,
    /// Unix timestamp when the pass started.
    pub started_at: i64,
    /// Unix timestamp when the pass completed.
    pub completed_at: i64,
    /// Number of configured boards whose roots were examined.
    pub boards_examined: u64,
    /// Number of filesystem entries classified.
    pub paths_examined: u64,
    /// Number of authoritative database rows and path references inspected.
    pub references_examined: u64,
    /// Total file bytes by classification.
    pub bytes_by_classification: BTreeMap<AuditClassification, u64>,
    /// Item counts by classification.
    pub counts: BTreeMap<AuditClassification, u64>,
    /// Item counts by known media category.
    pub categories: BTreeMap<MediaCategory, u64>,
    /// Bounded representative examples.
    pub examples: Vec<AuditExample>,
    /// Redacted, bounded scan errors.
    pub errors: Vec<String>,
    /// Whether the database generation stayed unchanged through enumeration.
    pub transactionally_stable: bool,
    /// Whether any database or filesystem portion was truncated or failed.
    pub incomplete: bool,
    /// Cursor for the next filesystem page, or the default cursor after wrap.
    pub next_cursor: ReconcileCursor,
    /// Repair outcomes; always zero in audit mode.
    pub repairs: RepairSummary,
}

impl AuditReport {
    /// Create an empty report for one scan.
    fn new(scan_id: String, started_at: i64) -> Self {
        Self {
            scan_id,
            started_at,
            completed_at: started_at,
            boards_examined: 0,
            paths_examined: 0,
            references_examined: 0,
            bytes_by_classification: BTreeMap::new(),
            counts: BTreeMap::new(),
            categories: BTreeMap::new(),
            examples: Vec::new(),
            errors: Vec::new(),
            transactionally_stable: false,
            incomplete: false,
            next_cursor: ReconcileCursor::default(),
            repairs: RepairSummary::default(),
        }
    }

    fn record(
        &mut self,
        classification: AuditClassification,
        category: MediaCategory,
        bytes: u64,
        example: Option<AuditExample>,
        example_limit: usize,
    ) {
        *self.counts.entry(classification).or_default() = self
            .counts
            .get(&classification)
            .copied()
            .unwrap_or_default()
            .saturating_add(1);
        *self.categories.entry(category).or_default() = self
            .categories
            .get(&category)
            .copied()
            .unwrap_or_default()
            .saturating_add(1);
        *self
            .bytes_by_classification
            .entry(classification)
            .or_default() = self
            .bytes_by_classification
            .get(&classification)
            .copied()
            .unwrap_or_default()
            .saturating_add(bytes);
        if self.examples.len() < example_limit {
            if let Some(example) = example {
                self.examples.push(example);
            }
        }
    }

    fn error(&mut self, message: impl Into<String>, example_limit: usize) {
        self.incomplete = true;
        if self.errors.len() < example_limit {
            self.errors.push(message.into());
        }
    }
}

/// Process-wide reconciliation metrics exposed without path labels.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReconcileMetrics {
    /// Filesystem entries examined since process start.
    pub files_scanned_total: u64,
    /// Database references examined since process start.
    pub references_scanned_total: u64,
    /// Missing authoritative references observed since process start.
    pub missing_references_total: u64,
    /// Safe orphan bytes observed since process start.
    pub safe_orphan_bytes_total: u64,
    /// Ambiguous files observed since process start.
    pub ambiguous_files_total: u64,
    /// Successful repair actions since process start.
    pub repairs_total: u64,
    /// Repairs cancelled by revalidation since process start.
    pub repair_conflicts_total: u64,
    /// Incomplete reconciliation passes since process start.
    pub incomplete_scans_total: u64,
}

/// Return a relaxed snapshot of process-wide reconciliation metrics.
#[must_use]
pub fn metrics_snapshot() -> ReconcileMetrics {
    ReconcileMetrics {
        files_scanned_total: FILES_SCANNED_TOTAL.load(Ordering::Relaxed),
        references_scanned_total: REFERENCES_SCANNED_TOTAL.load(Ordering::Relaxed),
        missing_references_total: MISSING_REFERENCES_TOTAL.load(Ordering::Relaxed),
        safe_orphan_bytes_total: SAFE_ORPHAN_BYTES_TOTAL.load(Ordering::Relaxed),
        ambiguous_files_total: AMBIGUOUS_FILES_TOTAL.load(Ordering::Relaxed),
        repairs_total: REPAIRS_TOTAL.load(Ordering::Relaxed),
        repair_conflicts_total: REPAIR_CONFLICTS_TOTAL.load(Ordering::Relaxed),
        incomplete_scans_total: INCOMPLETE_SCANS_TOTAL.load(Ordering::Relaxed),
    }
}

/// Resolve the internal operational repair switch into a reconciliation mode.
#[must_use]
pub fn configured_mode() -> ReconcileMode {
    if crate::config::CONFIG.media_reconcile_repair_enabled {
        ReconcileMode::Repair
    } else {
        ReconcileMode::Audit
    }
}

/// Build bounded reconciliation limits from internal environment configuration.
#[must_use]
pub fn configured_limits() -> ReconcileLimits {
    ReconcileLimits {
        files_per_pass: crate::config::CONFIG.media_reconcile_files_per_pass,
        database_rows_per_pass: crate::config::CONFIG.media_reconcile_database_rows_per_pass,
        hash_bytes_per_pass: crate::config::CONFIG.media_reconcile_hash_bytes_per_pass,
        repairs_per_pass: crate::config::CONFIG.media_reconcile_repairs_per_pass,
        examples_per_pass: DEFAULT_EXAMPLE_LIMIT,
    }
    .bounded()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReferenceRole {
    Active,
    Temporary,
    Metadata,
    ScheduledDeletion,
    TerminalMissing,
}

impl ReferenceRole {
    const fn protects_from_orphan_cleanup(self) -> bool {
        matches!(self, Self::Active | Self::Temporary | Self::Metadata)
    }
}

#[derive(Debug, Clone)]
struct PathClaim {
    role: ReferenceRole,
    category: MediaCategory,
    owner: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ReferenceSource {
    Post,
    Hash,
    Job,
    Intent,
}

#[derive(Debug, Clone)]
struct ReferenceRecord {
    source: ReferenceSource,
    path: String,
    board: Option<String>,
    category: MediaCategory,
    role: ReferenceRole,
    owner: String,
    expected_digest: Option<String>,
    recoverable_source: Option<String>,
}

#[derive(Debug, Clone)]
struct AuditFinding {
    classification: AuditClassification,
    category: MediaCategory,
    board: Option<String>,
    managed_id: String,
    owner: Option<String>,
}

#[derive(Debug, Clone)]
struct HashRepairCandidate {
    digest: String,
    file_path: String,
    thumb_path: String,
    mime_type: String,
    classification: AuditClassification,
}

#[derive(Debug, Clone)]
struct JobRepairCandidate {
    id: i64,
    job_type: String,
    payload: String,
}

#[derive(Debug, Clone)]
struct IntentRepairCandidate {
    id: String,
    kind: String,
    payload_json: String,
}

#[derive(Debug, Clone)]
struct PostMediaState {
    board: String,
    file_path: Option<String>,
    media_state: String,
}

type PostReferenceRow = (
    i64,
    i64,
    String,
    Option<String>,
    Option<String>,
    Option<String>,
    String,
    String,
);

#[derive(Debug, Default)]
struct ReferenceModel {
    boards_by_id: HashMap<i64, String>,
    boards: BTreeSet<String>,
    case_conflict_boards: HashSet<String>,
    posts: HashMap<i64, PostMediaState>,
    claims: HashMap<String, Vec<PathClaim>>,
    records: Vec<ReferenceRecord>,
    findings: Vec<AuditFinding>,
    hash_candidates: Vec<HashRepairCandidate>,
    job_candidates: Vec<JobRepairCandidate>,
    intent_candidates: Vec<IntentRepairCandidate>,
    scheduled_boards: HashSet<String>,
    database_rows_scanned: usize,
    data_version: i64,
    incomplete: bool,
    global_ambiguity: bool,
}

impl ReferenceModel {
    fn add_record(&mut self, record: ReferenceRecord) {
        if record.role.protects_from_orphan_cleanup()
            || record.role == ReferenceRole::ScheduledDeletion
        {
            self.claims
                .entry(record.path.clone())
                .or_default()
                .push(PathClaim {
                    role: record.role,
                    category: record.category,
                    owner: record.owner.clone(),
                });
        }
        self.records.push(record);
    }

    fn required_claims(&self, path: &str) -> impl Iterator<Item = &PathClaim> + use<'_> {
        self.claims
            .get(path)
            .into_iter()
            .flat_map(|claims| claims.iter())
            .filter(|claim| claim.role.protects_from_orphan_cleanup())
    }

    fn scheduled_claim(&self, path: &str) -> Option<&PathClaim> {
        self.claims.get(path).and_then(|claims| {
            claims
                .iter()
                .find(|claim| claim.role == ReferenceRole::ScheduledDeletion)
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ManagedPath {
    relative: String,
    board: String,
    category: MediaCategory,
}

#[derive(Debug, Clone)]
struct InventoryEntry {
    absolute: PathBuf,
    relative: Option<String>,
    sort_key: String,
    board: Option<String>,
    category: MediaCategory,
    entry_kind: InventoryEntryKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InventoryEntryKind {
    File,
    Symlink,
    #[cfg(unix)]
    HardLink,
    UnexpectedDirectory,
    Special,
    UnsafePath,
    CrossBoard,
    ScanError,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileIdentity {
    size: u64,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

impl FileIdentity {
    fn from_metadata(metadata: &std::fs::Metadata) -> Self {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            Self {
                size: metadata.len(),
                device: metadata.dev(),
                inode: metadata.ino(),
            }
        }
        #[cfg(not(unix))]
        {
            Self {
                size: metadata.len(),
            }
        }
    }
}

#[derive(Debug, Clone)]
struct OrphanCandidate {
    relative: String,
    board: String,
    category: MediaCategory,
    identity: FileIdentity,
    digest: Option<String>,
}

#[derive(Debug, Default)]
struct HashBudget {
    remaining: u64,
}

impl HashBudget {
    const fn new(bytes: u64) -> Self {
        Self { remaining: bytes }
    }

    fn hash_if_bounded(&mut self, path: &Path, size: u64) -> Result<Option<String>> {
        if size > self.remaining {
            return Ok(None);
        }
        let digest = sha256_regular_file(path)?;
        self.remaining = self.remaining.saturating_sub(size);
        Ok(Some(digest))
    }
}

fn is_valid_board_short(board: &str) -> bool {
    !board.is_empty() && board.len() <= 8 && board.bytes().all(|byte| byte.is_ascii_alphanumeric())
}

fn parse_managed_path(
    value: &str,
    expected_board: Option<&str>,
    boards: &BTreeSet<String>,
) -> Option<ManagedPath> {
    if value.trim().is_empty() || value.contains('\\') {
        return None;
    }
    let path = Path::new(value);
    if path.is_absolute() {
        return None;
    }
    let components = path
        .components()
        .map(|component| match component {
            Component::Normal(value) => value.to_str(),
            Component::CurDir
            | Component::ParentDir
            | Component::RootDir
            | Component::Prefix(_) => None,
        })
        .collect::<Option<Vec<_>>>()?;
    let board = *components.first()?;
    if !is_valid_board_short(board)
        || !boards.contains(board)
        || expected_board.is_some_and(|expected| expected != board)
    {
        return None;
    }
    let category = match components.as_slice() {
        [_, file] if !file.is_empty() && !file.starts_with('.') => MediaCategory::Original,
        [_, "thumbs", file] if !file.is_empty() && !file.starts_with('.') => {
            MediaCategory::Thumbnail
        }
        _ => return None,
    };
    Some(ManagedPath {
        relative: components.join("/"),
        board: board.to_owned(),
        category,
    })
}

fn path_category_from_claims(
    model: &ReferenceModel,
    path: &str,
    fallback: MediaCategory,
) -> MediaCategory {
    model
        .claims
        .get(path)
        .and_then(|claims| {
            claims.iter().find_map(|claim| {
                matches!(
                    claim.category,
                    MediaCategory::Waveform | MediaCategory::TranscodedOutput
                )
                .then_some(claim.category)
            })
        })
        .unwrap_or(fallback)
}

fn redacted_identifier(value: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(value.as_bytes());
    let digest = hex::encode(hasher.finalize());
    format!("redacted:{}", digest.get(..16).unwrap_or(&digest))
}

fn trusted_or_redacted_id(
    value: &str,
    expected_board: Option<&str>,
    boards: &BTreeSet<String>,
) -> String {
    parse_managed_path(value, expected_board, boards)
        .map_or_else(|| redacted_identifier(value), |path| path.relative)
}

fn sha256_regular_file(path: &Path) -> Result<String> {
    crate::utils::fs_security::assert_regular_file_no_symlink(path)
        .context("reconciliation digest input failed safety validation")?;
    let mut file = std::fs::File::open(path).context("open reconciliation digest input")?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; HASH_BUFFER_BYTES];
    loop {
        let read = file
            .read(&mut buffer)
            .context("read reconciliation digest input")?;
        if read == 0 {
            break;
        }
        let bytes = buffer
            .get(..read)
            .ok_or_else(|| anyhow::anyhow!("digest read exceeded its fixed buffer"))?;
        hasher.update(bytes);
    }
    Ok(hex::encode(hasher.finalize()))
}

fn is_not_found(error: &anyhow::Error) -> bool {
    error
        .chain()
        .find_map(|source| source.downcast_ref::<std::io::Error>())
        .is_some_and(|source| source.kind() == std::io::ErrorKind::NotFound)
}

fn inspected_regular_file(root: &Path, relative: &str) -> Result<Option<(PathBuf, FileIdentity)>> {
    match crate::utils::fs_security::existing_regular_file_child(root, relative) {
        Ok(path) => {
            let metadata = std::fs::symlink_metadata(&path)
                .with_context(|| format!("inspect managed identity {relative:?}"))?;
            Ok(Some((path, FileIdentity::from_metadata(&metadata))))
        }
        Err(error) if is_not_found(&error) => Ok(None),
        Err(error) => Err(error),
    }
}

const fn recommended_action(classification: AuditClassification) -> RecommendedAction {
    match classification {
        AuditClassification::SafeOrphanCandidate => RecommendedAction::ScheduleDurableDeletion,
        AuditClassification::StaleHashMissingFile | AuditClassification::StaleHashUnreferenced => {
            RecommendedAction::RemoveStaleHash
        }
        AuditClassification::ObsoleteJob => RecommendedAction::ResolveObsoleteJob,
        AuditClassification::CompletedIntent => RecommendedAction::RemoveCompletedIntent,
        AuditClassification::RecoverableMissingDerived => RecommendedAction::EnqueueDerivedMedia,
        AuditClassification::Healthy
        | AuditClassification::LifecycleInProgress
        | AuditClassification::ScheduledDeletion
        | AuditClassification::IntentionallyPrunedOriginal => RecommendedAction::None,
        AuditClassification::MissingPrimary
        | AuditClassification::MissingThumbnail
        | AuditClassification::MissingWaveform
        | AuditClassification::MissingDeterministicOutput
        | AuditClassification::MissingStageInstalledDestination
        | AuditClassification::UnrecoverableActiveReference
        | AuditClassification::AmbiguousOrphan
        | AuditClassification::MissingHashMetadata
        | AuditClassification::DigestConflict
        | AuditClassification::ConflictingHashMetadata
        | AuditClassification::CrossBoardMetadata
        | AuditClassification::MalformedJob
        | AuditClassification::IntentConflictsWithActiveReference
        | AuditClassification::IntentInconsistency
        | AuditClassification::MalformedIntent
        | AuditClassification::UnsafeExternalIntent
        | AuditClassification::UnsafeSymlink
        | AuditClassification::UnsafeHardLink
        | AuditClassification::UnexpectedDirectory
        | AuditClassification::UnsafeSpecialEntry
        | AuditClassification::UnsafePath
        | AuditClassification::CrossBoardPath
        | AuditClassification::ScanError => RecommendedAction::PreserveAndReview,
    }
}

fn reference_example(
    model: &ReferenceModel,
    record: &ReferenceRecord,
    classification: AuditClassification,
    bytes: u64,
) -> AuditExample {
    AuditExample {
        classification,
        category: record.category,
        board: record.board.clone(),
        managed_id: trusted_or_redacted_id(&record.path, record.board.as_deref(), &model.boards),
        owner: Some(record.owner.clone()),
        bytes,
        recommended_action: recommended_action(classification),
    }
}

#[derive(Debug, Clone)]
enum ParsedJob {
    Media {
        post_id: i64,
        board: String,
        source: String,
        output: String,
        category: MediaCategory,
    },
    ThreadPrune {
        board_id: i64,
    },
    SpamCheck {
        post_id: i64,
    },
}

type MediaJobKey = (String, i64, String, String, String);

fn media_job_key(job_type: &str, parsed: &ParsedJob) -> Option<MediaJobKey> {
    let ParsedJob::Media {
        post_id,
        board,
        source,
        output,
        ..
    } = parsed
    else {
        return None;
    };
    Some((
        job_type.to_owned(),
        *post_id,
        board.clone(),
        source.clone(),
        output.clone(),
    ))
}

#[derive(Debug)]
struct UnsafeIntentPath;

impl std::fmt::Display for UnsafeIntentPath {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("intent path is outside its trusted managed boundary")
    }
}

impl std::error::Error for UnsafeIntentPath {}

fn unsafe_intent_path(message: &'static str) -> anyhow::Error {
    anyhow::Error::new(UnsafeIntentPath).context(message)
}

fn is_unsafe_intent_path(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|source| source.downcast_ref::<UnsafeIntentPath>().is_some())
}

fn parse_job(job_type: &str, payload: &str) -> Result<ParsedJob> {
    let value: serde_json::Value =
        serde_json::from_str(payload).context("decode background job payload")?;
    let tag = value
        .get("t")
        .and_then(serde_json::Value::as_str)
        .context("background job payload has no tag")?;
    let data = value
        .get("d")
        .and_then(serde_json::Value::as_object)
        .context("background job payload has no data object")?;
    match (job_type, tag) {
        ("video_transcode", "VideoTranscode") => {
            let post_id = data
                .get("post_id")
                .and_then(serde_json::Value::as_i64)
                .context("video job has no post id")?;
            let board = data
                .get("board_short")
                .and_then(serde_json::Value::as_str)
                .context("video job has no board")?
                .to_owned();
            let source = data
                .get("file_path")
                .and_then(serde_json::Value::as_str)
                .context("video job has no source")?
                .to_owned();
            let source_path = Path::new(&source);
            let stem = source_path
                .file_stem()
                .and_then(OsStr::to_str)
                .filter(|stem| !stem.is_empty())
                .context("video job source has no file stem")?;
            let extension = source_path
                .extension()
                .and_then(OsStr::to_str)
                .context("video job source has no extension")?;
            let output_name = match extension.to_ascii_lowercase().as_str() {
                "webm" => format!("{stem}.vp9.webm"),
                "mp4" | "mkv" => format!("{stem}.webm"),
                _ => anyhow::bail!("video job source extension is unsupported"),
            };
            Ok(ParsedJob::Media {
                post_id,
                board: board.clone(),
                source,
                output: format!("{board}/{output_name}"),
                category: MediaCategory::TranscodedOutput,
            })
        }
        ("audio_waveform", "AudioWaveform") => {
            let post_id = data
                .get("post_id")
                .and_then(serde_json::Value::as_i64)
                .context("waveform job has no post id")?;
            let board = data
                .get("board_short")
                .and_then(serde_json::Value::as_str)
                .context("waveform job has no board")?
                .to_owned();
            let source = data
                .get("file_path")
                .and_then(serde_json::Value::as_str)
                .context("waveform job has no source")?
                .to_owned();
            let stem = Path::new(&source)
                .file_stem()
                .and_then(OsStr::to_str)
                .filter(|stem| !stem.is_empty())
                .context("waveform job source has no file stem")?
                .to_owned();
            Ok(ParsedJob::Media {
                post_id,
                board: board.clone(),
                source,
                output: format!("{board}/thumbs/{stem}.png"),
                category: MediaCategory::Waveform,
            })
        }
        ("thread_prune", "ThreadPrune") => Ok(ParsedJob::ThreadPrune {
            board_id: data
                .get("board_id")
                .and_then(serde_json::Value::as_i64)
                .context("thread prune job has no board id")?,
        }),
        ("spam_check", "SpamCheck") => Ok(ParsedJob::SpamCheck {
            post_id: data
                .get("post_id")
                .and_then(serde_json::Value::as_i64)
                .context("spam job has no post id")?,
        }),
        _ => anyhow::bail!("background job type and payload tag disagree"),
    }
}

const fn remaining_rows(model: &mut ReferenceModel, limit: usize) -> usize {
    let remaining = limit.saturating_sub(model.database_rows_scanned);
    if remaining == 0 {
        model.incomplete = true;
    }
    remaining
}

fn bounded_sql_limit(remaining: usize) -> i64 {
    i64::try_from(remaining.saturating_add(1)).unwrap_or(i64::MAX)
}

fn mark_cross_board_or_malformed(
    model: &mut ReferenceModel,
    raw_path: &str,
    expected_board: Option<&str>,
    owner: String,
    category: MediaCategory,
) {
    if let Some(path) = parse_managed_path(raw_path, None, &model.boards) {
        model.add_record(ReferenceRecord {
            source: ReferenceSource::Post,
            path: path.relative.clone(),
            board: Some(path.board.clone()),
            category,
            role: ReferenceRole::Temporary,
            owner: owner.clone(),
            expected_digest: None,
            recoverable_source: None,
        });
        model.findings.push(AuditFinding {
            classification: AuditClassification::CrossBoardMetadata,
            category,
            board: Some(path.board),
            managed_id: path.relative,
            owner: Some(owner),
        });
    } else {
        model.global_ambiguity = true;
        model.findings.push(AuditFinding {
            classification: AuditClassification::UnsafePath,
            category,
            board: expected_board.map(str::to_owned),
            managed_id: redacted_identifier(raw_path),
            owner: Some(owner),
        });
    }
}

fn collect_reference_model(
    conn: &rusqlite::Connection,
    upload_root: &Path,
    row_limit: usize,
) -> Result<ReferenceModel> {
    conn.execute_batch("BEGIN")
        .context("begin managed-media reference snapshot")?;
    let result = collect_reference_model_in_transaction(conn, upload_root, row_limit);
    match result {
        Ok(model) => {
            conn.execute_batch("COMMIT")
                .context("commit managed-media reference snapshot")?;
            Ok(model)
        }
        Err(error) => {
            drop(conn.execute_batch("ROLLBACK"));
            Err(error)
        }
    }
}

fn collect_reference_model_in_transaction(
    conn: &rusqlite::Connection,
    upload_root: &Path,
    row_limit: usize,
) -> Result<ReferenceModel> {
    let mut model = ReferenceModel {
        data_version: conn.query_row("PRAGMA data_version", [], |row| row.get(0))?,
        ..ReferenceModel::default()
    };
    collect_boards(conn, &mut model, row_limit)?;
    collect_posts(conn, &mut model, row_limit)?;
    collect_hashes(conn, &mut model, row_limit)?;
    collect_jobs(conn, &mut model, row_limit)?;
    collect_intents(conn, upload_root, &mut model, row_limit)?;
    Ok(model)
}

fn collect_boards(
    conn: &rusqlite::Connection,
    model: &mut ReferenceModel,
    row_limit: usize,
) -> Result<()> {
    let remaining = remaining_rows(model, row_limit);
    if remaining == 0 {
        return Ok(());
    }
    let mut statement =
        conn.prepare("SELECT id, short_name FROM boards ORDER BY id ASC LIMIT ?1")?;
    let mut rows = statement
        .query_map([bounded_sql_limit(remaining)], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.len() > remaining {
        rows.truncate(remaining);
        model.incomplete = true;
    }
    model.database_rows_scanned = model.database_rows_scanned.saturating_add(rows.len());
    let mut casefold = HashMap::<String, String>::new();
    for (board_id, board) in rows {
        if !is_valid_board_short(&board) {
            model.global_ambiguity = true;
            model.findings.push(AuditFinding {
                classification: AuditClassification::CrossBoardMetadata,
                category: MediaCategory::Unknown,
                board: None,
                managed_id: redacted_identifier(&board),
                owner: Some(format!("board:{board_id}")),
            });
            continue;
        }
        let folded = board.to_ascii_lowercase();
        if let Some(other) = casefold.insert(folded.clone(), board.clone()) {
            if other != board {
                model.case_conflict_boards.insert(other.clone());
                model.case_conflict_boards.insert(board.clone());
                model.global_ambiguity = true;
            }
        }
        model.boards_by_id.insert(board_id, board.clone());
        model.boards.insert(board);
    }
    Ok(())
}

fn collect_posts(
    conn: &rusqlite::Connection,
    model: &mut ReferenceModel,
    row_limit: usize,
) -> Result<()> {
    let remaining = remaining_rows(model, row_limit);
    if remaining == 0 {
        return Ok(());
    }
    let mut statement = conn.prepare(
        "SELECT p.id, p.board_id, b.short_name, p.file_path, p.thumb_path,
                p.audio_file_path, COALESCE(p.mime_type, ''),
                COALESCE(p.media_processing_state, '')
         FROM posts p JOIN boards b ON b.id = p.board_id
         ORDER BY p.id ASC LIMIT ?1",
    )?;
    let mut rows = statement
        .query_map([bounded_sql_limit(remaining)], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<PostReferenceRow>>>()?;
    if rows.len() > remaining {
        rows.truncate(remaining);
        model.incomplete = true;
    }
    model.database_rows_scanned = model.database_rows_scanned.saturating_add(rows.len());
    for (post_id, board_id, board, file, thumb, audio, mime, state) in rows {
        if model.boards_by_id.get(&board_id) != Some(&board) {
            model.global_ambiguity = true;
        }
        model.posts.insert(
            post_id,
            PostMediaState {
                board: board.clone(),
                file_path: file.clone(),
                media_state: state.clone(),
            },
        );
        if let Some(path) = file {
            let role = if state == crate::db::MEDIA_ORIGINAL_PRUNED {
                ReferenceRole::TerminalMissing
            } else if matches!(
                state.as_str(),
                crate::db::MEDIA_PROCESSING_PENDING | crate::db::MEDIA_ORIGINAL_PRUNE_PENDING
            ) {
                ReferenceRole::Temporary
            } else {
                ReferenceRole::Active
            };
            add_post_path(model, post_id, &board, &path, MediaCategory::Original, role);
        }
        if let Some(path) = audio {
            let role = if state == crate::db::MEDIA_ORIGINAL_PRUNED {
                ReferenceRole::TerminalMissing
            } else {
                ReferenceRole::Active
            };
            add_post_path(model, post_id, &board, &path, MediaCategory::Original, role);
        }
        if let Some(path) = thumb {
            let category = if mime.starts_with("audio/") {
                MediaCategory::Waveform
            } else {
                MediaCategory::Thumbnail
            };
            add_post_path(
                model,
                post_id,
                &board,
                &path,
                category,
                ReferenceRole::Active,
            );
        }
    }
    Ok(())
}

fn add_post_path(
    model: &mut ReferenceModel,
    post_id: i64,
    board: &str,
    raw_path: &str,
    category: MediaCategory,
    role: ReferenceRole,
) {
    let owner = format!("post:{post_id}");
    let Some(path) = parse_managed_path(raw_path, Some(board), &model.boards) else {
        mark_cross_board_or_malformed(model, raw_path, Some(board), owner, category);
        return;
    };
    model.add_record(ReferenceRecord {
        source: ReferenceSource::Post,
        path: path.relative,
        board: Some(path.board),
        category,
        role,
        owner,
        expected_digest: None,
        recoverable_source: None,
    });
}

fn collect_hashes(
    conn: &rusqlite::Connection,
    model: &mut ReferenceModel,
    row_limit: usize,
) -> Result<()> {
    let remaining = remaining_rows(model, row_limit);
    if remaining == 0 {
        return Ok(());
    }
    let mut statement = conn.prepare(
        "SELECT sha256, file_path, thumb_path, mime_type
         FROM file_hashes ORDER BY sha256 ASC LIMIT ?1",
    )?;
    let mut rows = statement
        .query_map([bounded_sql_limit(remaining)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.len() > remaining {
        rows.truncate(remaining);
        model.incomplete = true;
    }
    model.database_rows_scanned = model.database_rows_scanned.saturating_add(rows.len());
    let mut digests_by_path = BTreeMap::<String, BTreeSet<String>>::new();
    for (digest, file_path, thumb_path, mime_type) in rows {
        let owner = format!("hash:{}", digest.get(..12).unwrap_or(&digest));
        if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            model.global_ambiguity = true;
            model.findings.push(AuditFinding {
                classification: AuditClassification::DigestConflict,
                category: MediaCategory::Original,
                board: None,
                managed_id: redacted_identifier(&file_path),
                owner: Some(owner),
            });
            continue;
        }
        let Some(file) = parse_managed_path(&file_path, None, &model.boards) else {
            mark_cross_board_or_malformed(model, &file_path, None, owner, MediaCategory::Original);
            continue;
        };
        digests_by_path
            .entry(file.relative.clone())
            .or_default()
            .insert(digest.clone());
        model.add_record(ReferenceRecord {
            source: ReferenceSource::Hash,
            path: file.relative.clone(),
            board: Some(file.board.clone()),
            category: MediaCategory::Original,
            role: ReferenceRole::Metadata,
            owner: owner.clone(),
            expected_digest: Some(digest.clone()),
            recoverable_source: None,
        });
        if !thumb_path.trim().is_empty() {
            if let Some(thumb) = parse_managed_path(&thumb_path, Some(&file.board), &model.boards) {
                model.add_record(ReferenceRecord {
                    source: ReferenceSource::Hash,
                    path: thumb.relative,
                    board: Some(thumb.board),
                    category: MediaCategory::Thumbnail,
                    role: ReferenceRole::Metadata,
                    owner: owner.clone(),
                    expected_digest: None,
                    recoverable_source: None,
                });
            } else {
                mark_cross_board_or_malformed(
                    model,
                    &thumb_path,
                    Some(&file.board),
                    owner.clone(),
                    MediaCategory::Thumbnail,
                );
            }
        }
        model.hash_candidates.push(HashRepairCandidate {
            digest,
            file_path: file.relative,
            thumb_path,
            mime_type,
            classification: AuditClassification::Healthy,
        });
    }
    for (path, digests) in digests_by_path {
        if digests.len() > 1 {
            model.global_ambiguity = true;
            model.findings.push(AuditFinding {
                classification: AuditClassification::ConflictingHashMetadata,
                category: MediaCategory::Original,
                board: path.split('/').next().map(str::to_owned),
                managed_id: path,
                owner: Some("file_hashes:conflict".to_owned()),
            });
        }
    }
    Ok(())
}

fn collect_jobs(
    conn: &rusqlite::Connection,
    model: &mut ReferenceModel,
    row_limit: usize,
) -> Result<()> {
    let remaining = remaining_rows(model, row_limit);
    if remaining == 0 {
        return Ok(());
    }
    let mut statement = conn.prepare(
        "SELECT id, job_type, payload, status
         FROM background_jobs ORDER BY id ASC LIMIT ?1",
    )?;
    let mut rows = statement
        .query_map([bounded_sql_limit(remaining)], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.len() > remaining {
        rows.truncate(remaining);
        model.incomplete = true;
    }
    model.database_rows_scanned = model.database_rows_scanned.saturating_add(rows.len());
    let completed_media_jobs = rows
        .iter()
        .filter(|(_, _, _, status)| status == "done")
        .filter_map(|(_, job_type, payload, _)| {
            parse_job(job_type, payload)
                .ok()
                .and_then(|parsed| media_job_key(job_type, &parsed))
        })
        .collect::<BTreeSet<_>>();
    for (job_id, job_type, payload, status) in rows {
        let owner = format!("job:{job_id}");
        let parsed = match parse_job(&job_type, &payload) {
            Ok(parsed) => parsed,
            Err(_) => {
                model.global_ambiguity = true;
                model.findings.push(AuditFinding {
                    classification: AuditClassification::MalformedJob,
                    category: MediaCategory::Unknown,
                    board: None,
                    managed_id: redacted_identifier(&payload),
                    owner: Some(owner),
                });
                continue;
            }
        };
        match parsed {
            ParsedJob::Media {
                post_id,
                board,
                source,
                output,
                category,
            } => {
                let superseded = status != "done"
                    && completed_media_jobs.contains(&(
                        job_type.clone(),
                        post_id,
                        board.clone(),
                        source.clone(),
                        output.clone(),
                    ));
                collect_media_job(
                    model, job_id, job_type, payload, &status, post_id, board, &source, &output,
                    category, superseded,
                );
            }
            ParsedJob::ThreadPrune { board_id } => {
                let classification = if model.boards_by_id.contains_key(&board_id) {
                    AuditClassification::LifecycleInProgress
                } else {
                    AuditClassification::ObsoleteJob
                };
                model.findings.push(AuditFinding {
                    classification,
                    category: MediaCategory::Unknown,
                    board: model.boards_by_id.get(&board_id).cloned(),
                    managed_id: "thread-prune".to_owned(),
                    owner: Some(owner),
                });
            }
            ParsedJob::SpamCheck { post_id } => {
                let classification = if model.posts.contains_key(&post_id) {
                    AuditClassification::Healthy
                } else {
                    AuditClassification::ObsoleteJob
                };
                model.findings.push(AuditFinding {
                    classification,
                    category: MediaCategory::Unknown,
                    board: model.posts.get(&post_id).map(|post| post.board.clone()),
                    managed_id: "spam-check".to_owned(),
                    owner: Some(owner),
                });
            }
        }
    }
    Ok(())
}

#[expect(
    clippy::too_many_arguments,
    reason = "the decoded durable job identity is kept explicit at the trust boundary"
)]
fn collect_media_job(
    model: &mut ReferenceModel,
    job_id: i64,
    job_type: String,
    payload: String,
    status: &str,
    post_id: i64,
    board: String,
    source: &str,
    output: &str,
    category: MediaCategory,
    superseded: bool,
) {
    let owner = format!("job:{job_id}");
    let source_path = parse_managed_path(source, Some(&board), &model.boards);
    let output_path = parse_managed_path(output, Some(&board), &model.boards);
    let (Some(source_path), Some(output_path)) = (source_path, output_path) else {
        model.global_ambiguity = true;
        model.findings.push(AuditFinding {
            classification: AuditClassification::MalformedJob,
            category,
            board: is_valid_board_short(&board).then_some(board),
            managed_id: redacted_identifier(&payload),
            owner: Some(owner),
        });
        return;
    };
    let post = model.posts.get(&post_id);
    let compatible = post.is_some_and(|post| {
        post.board == board
            && post
                .file_path
                .as_deref()
                .is_some_and(|path| path == source || path == output)
    });
    let active = matches!(status, "pending" | "running");
    let classification = if superseded {
        AuditClassification::ObsoleteJob
    } else if compatible && active {
        AuditClassification::LifecycleInProgress
    } else if compatible {
        AuditClassification::Healthy
    } else {
        AuditClassification::ObsoleteJob
    };
    let role = if active || !compatible {
        ReferenceRole::Temporary
    } else {
        ReferenceRole::Active
    };
    model.add_record(ReferenceRecord {
        source: ReferenceSource::Job,
        path: source_path.relative.clone(),
        board: Some(board.clone()),
        category: MediaCategory::Original,
        role,
        owner: owner.clone(),
        expected_digest: None,
        recoverable_source: None,
    });
    model.add_record(ReferenceRecord {
        source: ReferenceSource::Job,
        path: output_path.relative.clone(),
        board: Some(board.clone()),
        category,
        role,
        owner: owner.clone(),
        expected_digest: None,
        recoverable_source: Some(source_path.relative),
    });
    model.findings.push(AuditFinding {
        classification,
        category,
        board: Some(board),
        managed_id: output_path.relative,
        owner: Some(owner),
    });
    if !active && !compatible {
        model.job_candidates.push(JobRepairCandidate {
            id: job_id,
            job_type,
            payload,
        });
    }
}

fn collect_intents(
    conn: &rusqlite::Connection,
    upload_root: &Path,
    model: &mut ReferenceModel,
    row_limit: usize,
) -> Result<()> {
    let remaining = remaining_rows(model, row_limit);
    if remaining == 0 {
        return Ok(());
    }
    let mut statement = conn.prepare(
        "SELECT id, kind, payload_json
         FROM pending_fs_ops ORDER BY created_at ASC, id ASC LIMIT ?1",
    )?;
    let mut rows = statement
        .query_map([bounded_sql_limit(remaining)], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if rows.len() > remaining {
        rows.truncate(remaining);
        model.incomplete = true;
    }
    model.database_rows_scanned = model.database_rows_scanned.saturating_add(rows.len());
    for (id, kind, payload_json) in rows {
        let result = collect_intent(model, upload_root, &id, &kind, &payload_json);
        if let Err(error) = result {
            model.global_ambiguity = true;
            let classification = if is_unsafe_intent_path(&error) {
                AuditClassification::UnsafeExternalIntent
            } else {
                AuditClassification::MalformedIntent
            };
            model.findings.push(AuditFinding {
                classification,
                category: MediaCategory::Unknown,
                board: None,
                managed_id: redacted_identifier(&payload_json),
                owner: Some(format!("intent:{id}")),
            });
        }
    }
    Ok(())
}

fn collect_intent(
    model: &mut ReferenceModel,
    upload_root: &Path,
    id: &str,
    kind: &str,
    payload_json: &str,
) -> Result<()> {
    match kind {
        crate::pending_fs::DELETE_FILES_KIND => {
            let payload: crate::pending_fs::DeleteFilesPayload =
                serde_json::from_str(payload_json).context("decode delete-files intent")?;
            collect_delete_intent(model, id, &payload)?;
            model.intent_candidates.push(IntentRepairCandidate {
                id: id.to_owned(),
                kind: kind.to_owned(),
                payload_json: payload_json.to_owned(),
            });
        }
        crate::media::prune::ORIGINAL_PRUNE_KIND => {
            let payload: crate::media::prune::OriginalPrunePayload =
                serde_json::from_str(payload_json).context("decode original-prune intent")?;
            if payload.post_ids.is_empty() || payload.paths.is_empty() {
                anyhow::bail!("original-prune intent has no targets");
            }
            let protected_posts = payload
                .post_ids
                .iter()
                .map(|post_id| format!("post:{post_id}"))
                .collect::<HashSet<_>>();
            if payload.post_ids.iter().any(|post_id| {
                model
                    .posts
                    .get(post_id)
                    .is_none_or(|post| post.media_state != crate::db::MEDIA_ORIGINAL_PRUNE_PENDING)
            }) {
                anyhow::bail!("original-prune intent has no matching prune-pending post");
            }
            for path in payload.paths {
                let managed =
                    parse_managed_path(&path.path, Some(&path.board_short), &model.boards)
                        .ok_or_else(|| {
                            unsafe_intent_path(
                                "original-prune path is not board-owned managed media",
                            )
                        })?;
                add_original_prune_claim(model, id, managed, &protected_posts);
            }
        }
        crate::pending_fs::UPLOAD_FINALIZE_KIND => {
            let payload: crate::pending_fs::UploadFinalizePayload =
                serde_json::from_str(payload_json).context("decode upload-finalize intent")?;
            collect_upload_intent(model, upload_root, id, &payload)?;
        }
        crate::pending_fs::DELETE_BANNER_ASSETS_KIND => {
            drop(
                serde_json::from_str::<crate::pending_fs::DeleteBannerAssetsPayload>(payload_json)
                    .context("decode banner cleanup intent")?,
            );
            model.findings.push(AuditFinding {
                classification: AuditClassification::LifecycleInProgress,
                category: MediaCategory::Unknown,
                board: None,
                managed_id: "banner-assets".to_owned(),
                owner: Some(format!("intent:{id}")),
            });
        }
        crate::pending_fs::FULL_RESTORE_SWAP_KIND => {
            drop(
                serde_json::from_str::<crate::pending_fs::FullRestoreSwapPayload>(payload_json)
                    .context("decode full-restore intent")?,
            );
            model.global_ambiguity = true;
            model.findings.push(AuditFinding {
                classification: AuditClassification::LifecycleInProgress,
                category: MediaCategory::Unknown,
                board: None,
                managed_id: "full-restore".to_owned(),
                owner: Some(format!("intent:{id}")),
            });
        }
        crate::pending_fs::BOARD_RESTORE_SWAP_KIND => {
            let payload: crate::pending_fs::BoardRestoreSwapPayload =
                serde_json::from_str(payload_json).context("decode board-restore intent")?;
            let board = Path::new(&payload.live)
                .file_name()
                .and_then(OsStr::to_str)
                .filter(|board| model.boards.contains(*board))
                .ok_or_else(|| {
                    unsafe_intent_path("board-restore intent has no trusted board target")
                })?;
            model.scheduled_boards.insert(board.to_owned());
            model.global_ambiguity = true;
            model.findings.push(AuditFinding {
                classification: AuditClassification::LifecycleInProgress,
                category: MediaCategory::Unknown,
                board: Some(board.to_owned()),
                managed_id: "board-restore".to_owned(),
                owner: Some(format!("intent:{id}")),
            });
        }
        _ => anyhow::bail!("unknown filesystem intent kind"),
    }
    Ok(())
}

fn collect_delete_intent(
    model: &mut ReferenceModel,
    id: &str,
    payload: &crate::pending_fs::DeleteFilesPayload,
) -> Result<()> {
    if payload.paths.is_empty() && payload.dirs.is_empty() {
        anyhow::bail!("delete-files intent has no targets");
    }
    for raw_path in &payload.paths {
        let managed = parse_managed_path(raw_path, None, &model.boards)
            .ok_or_else(|| unsafe_intent_path("delete-files intent contains an unsafe path"))?;
        add_scheduled_intent_claim(model, id, managed);
    }
    for board in &payload.dirs {
        if !is_valid_board_short(board) || !model.boards.contains(board) {
            return Err(unsafe_intent_path(
                "delete-files intent contains an unsafe board directory",
            ));
        }
        model.scheduled_boards.insert(board.clone());
    }
    Ok(())
}

fn add_scheduled_intent_claim(model: &mut ReferenceModel, id: &str, path: ManagedPath) {
    let conflict = model.required_claims(&path.relative).next().is_some();
    model.add_record(ReferenceRecord {
        source: ReferenceSource::Intent,
        path: path.relative.clone(),
        board: Some(path.board.clone()),
        category: path.category,
        role: ReferenceRole::ScheduledDeletion,
        owner: format!("intent:{id}"),
        expected_digest: None,
        recoverable_source: None,
    });
    if conflict {
        model.global_ambiguity = true;
        model.findings.push(AuditFinding {
            classification: AuditClassification::IntentConflictsWithActiveReference,
            category: path.category,
            board: Some(path.board),
            managed_id: path.relative,
            owner: Some(format!("intent:{id}")),
        });
    }
}

fn add_original_prune_claim(
    model: &mut ReferenceModel,
    id: &str,
    path: ManagedPath,
    protected_posts: &HashSet<String>,
) {
    let conflict = model.required_claims(&path.relative).any(|claim| {
        !(claim.role == ReferenceRole::Temporary && protected_posts.contains(&claim.owner))
    });
    model.add_record(ReferenceRecord {
        source: ReferenceSource::Intent,
        path: path.relative.clone(),
        board: Some(path.board.clone()),
        category: path.category,
        role: ReferenceRole::ScheduledDeletion,
        owner: format!("intent:{id}"),
        expected_digest: None,
        recoverable_source: None,
    });
    if conflict {
        model.global_ambiguity = true;
        model.findings.push(AuditFinding {
            classification: AuditClassification::IntentConflictsWithActiveReference,
            category: path.category,
            board: Some(path.board),
            managed_id: path.relative,
            owner: Some(format!("intent:{id}")),
        });
    }
}

fn collect_upload_intent(
    model: &mut ReferenceModel,
    upload_root: &Path,
    id: &str,
    payload: &crate::pending_fs::UploadFinalizePayload,
) -> Result<()> {
    let root = upload_root
        .canonicalize()
        .context("canonicalize upload root for finalize intent")?;
    let pending_root = root.join(".pending");
    let stage = Path::new(&payload.stage_dir);
    if !stage.is_absolute()
        || stage
            .components()
            .any(|component| component == Component::ParentDir)
    {
        return Err(unsafe_intent_path(
            "upload-finalize stage is outside its managed root",
        ));
    }
    let normalized_stage = if let Ok(metadata) = std::fs::symlink_metadata(stage) {
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(unsafe_intent_path("upload-finalize stage is unsafe"));
        }
        stage
            .canonicalize()
            .context("canonicalize upload-finalize stage")?
    } else {
        let parent = stage
            .parent()
            .context("upload-finalize stage has no parent")?
            .canonicalize()
            .context("canonicalize missing upload-finalize stage parent")?;
        parent.join(
            stage
                .file_name()
                .context("upload-finalize stage has no file name")?,
        )
    };
    if normalized_stage == pending_root || !normalized_stage.starts_with(&pending_root) {
        return Err(unsafe_intent_path(
            "upload-finalize stage escapes its managed root",
        ));
    }
    let stage_relative = normalized_stage
        .strip_prefix(&root)
        .map_err(|_| unsafe_intent_path("upload-finalize stage is outside upload root"))?
        .to_str()
        .context("upload-finalize stage is non-UTF-8")?;
    if payload.relative_paths.is_empty() {
        anyhow::bail!("upload-finalize intent has no required artifacts");
    }
    let mut seen = HashSet::new();
    for (raw_path, optional) in payload
        .relative_paths
        .iter()
        .map(|path| (path, false))
        .chain(payload.optional_paths.iter().map(|path| (path, true)))
    {
        if !seen.insert(raw_path) {
            anyhow::bail!("upload-finalize intent repeats an artifact");
        }
        let managed = parse_managed_path(raw_path, None, &model.boards)
            .ok_or_else(|| unsafe_intent_path("upload-finalize artifact is not managed media"))?;
        if optional && managed.category != MediaCategory::Thumbnail {
            anyhow::bail!("upload-finalize optional artifact is not a thumbnail");
        }
        let staged_path = format!("{stage_relative}/{}", managed.relative);
        model.add_record(ReferenceRecord {
            source: ReferenceSource::Intent,
            path: managed.relative.clone(),
            board: Some(managed.board.clone()),
            category: managed.category,
            role: ReferenceRole::Temporary,
            owner: format!("intent:{id}"),
            expected_digest: payload.artifact_sha256.get(raw_path).cloned(),
            recoverable_source: Some(staged_path.clone()),
        });
        model.add_record(ReferenceRecord {
            source: ReferenceSource::Intent,
            path: staged_path,
            board: Some(managed.board),
            category: MediaCategory::UploadStage,
            role: ReferenceRole::Temporary,
            owner: format!("intent:{id}"),
            expected_digest: payload.artifact_sha256.get(raw_path).cloned(),
            recoverable_source: Some(managed.relative),
        });
    }
    model.findings.push(AuditFinding {
        classification: AuditClassification::LifecycleInProgress,
        category: MediaCategory::UploadStage,
        board: None,
        managed_id: stage_relative.to_owned(),
        owner: Some(format!("intent:{id}")),
    });
    Ok(())
}

fn audit_references(
    upload_root: &Path,
    model: &mut ReferenceModel,
    report: &mut AuditReport,
    hash_budget: &mut HashBudget,
    example_limit: usize,
) {
    for finding in &model.findings {
        let example = AuditExample {
            classification: finding.classification,
            category: finding.category,
            board: finding.board.clone(),
            managed_id: finding.managed_id.clone(),
            owner: finding.owner.clone(),
            bytes: 0,
            recommended_action: recommended_action(finding.classification),
        };
        report.record(
            finding.classification,
            finding.category,
            0,
            Some(example),
            example_limit,
        );
    }

    let records = model.records.clone();
    for record in &records {
        report.references_examined = report.references_examined.saturating_add(1);
        let inspected = inspected_regular_file(upload_root, &record.path);
        let (classification, bytes) = match inspected {
            Ok(Some((path, identity))) => {
                classify_existing_reference(model, record, &path, &identity, hash_budget)
            }
            Ok(None) => (classify_missing_reference(upload_root, record), 0),
            Err(_) => (AuditClassification::UnsafePath, 0),
        };
        if record.source == ReferenceSource::Hash && record.category == MediaCategory::Original {
            if let Some(candidate) = model
                .hash_candidates
                .iter_mut()
                .find(|candidate| candidate.file_path == record.path)
            {
                candidate.classification = classification;
            }
        }
        report.record(
            classification,
            record.category,
            bytes,
            Some(reference_example(model, record, classification, bytes)),
            example_limit,
        );
    }

    audit_completed_intents(upload_root, model, report, example_limit);
}

fn classify_existing_reference(
    model: &ReferenceModel,
    record: &ReferenceRecord,
    path: &Path,
    identity: &FileIdentity,
    hash_budget: &mut HashBudget,
) -> (AuditClassification, u64) {
    if record.role == ReferenceRole::TerminalMissing {
        return (AuditClassification::IntentInconsistency, identity.size);
    }
    if record.role == ReferenceRole::ScheduledDeletion {
        return (AuditClassification::ScheduledDeletion, identity.size);
    }
    if record.source == ReferenceSource::Hash {
        let other_claim = model.required_claims(&record.path).any(|claim| {
            claim.owner != record.owner
                && (!claim.owner.starts_with("hash:") || claim.role != ReferenceRole::Metadata)
        });
        if let Some(expected) = record.expected_digest.as_deref() {
            match hash_budget.hash_if_bounded(path, identity.size) {
                Ok(Some(actual)) if !actual.eq_ignore_ascii_case(expected) => {
                    return (AuditClassification::DigestConflict, identity.size);
                }
                Err(_) => return (AuditClassification::ScanError, identity.size),
                Ok(Some(_) | None) => {}
            }
        }
        if record.category == MediaCategory::Original && !other_claim {
            return (AuditClassification::StaleHashUnreferenced, identity.size);
        }
    }
    if record.source == ReferenceSource::Post
        && record.category == MediaCategory::Original
        && !model
            .claims
            .get(&record.path)
            .is_some_and(|claims| claims.iter().any(|claim| claim.owner.starts_with("hash:")))
    {
        return (AuditClassification::MissingHashMetadata, identity.size);
    }
    let classification = if record.role == ReferenceRole::Temporary {
        AuditClassification::LifecycleInProgress
    } else {
        AuditClassification::Healthy
    };
    (classification, identity.size)
}

fn classify_missing_reference(upload_root: &Path, record: &ReferenceRecord) -> AuditClassification {
    match record.role {
        ReferenceRole::ScheduledDeletion => AuditClassification::ScheduledDeletion,
        ReferenceRole::TerminalMissing => AuditClassification::IntentionallyPrunedOriginal,
        ReferenceRole::Metadata => AuditClassification::StaleHashMissingFile,
        ReferenceRole::Temporary if record.category == MediaCategory::UploadStage => {
            if record
                .recoverable_source
                .as_deref()
                .is_some_and(|destination| {
                    inspected_regular_file(upload_root, destination)
                        .ok()
                        .flatten()
                        .is_some()
                })
            {
                AuditClassification::MissingStageInstalledDestination
            } else {
                AuditClassification::IntentInconsistency
            }
        }
        ReferenceRole::Temporary | ReferenceRole::Active => match record.source {
            ReferenceSource::Post => match record.category {
                MediaCategory::Original | MediaCategory::TranscodedOutput => {
                    AuditClassification::MissingPrimary
                }
                MediaCategory::Thumbnail => AuditClassification::MissingThumbnail,
                MediaCategory::Waveform => AuditClassification::MissingWaveform,
                MediaCategory::UploadStage
                | MediaCategory::KnownTemporary
                | MediaCategory::Unknown => AuditClassification::UnrecoverableActiveReference,
            },
            ReferenceSource::Job => {
                if record.recoverable_source.as_deref().is_some_and(|source| {
                    inspected_regular_file(upload_root, source)
                        .ok()
                        .flatten()
                        .is_some()
                }) {
                    AuditClassification::RecoverableMissingDerived
                } else if matches!(
                    record.category,
                    MediaCategory::Waveform | MediaCategory::TranscodedOutput
                ) {
                    AuditClassification::MissingDeterministicOutput
                } else {
                    AuditClassification::UnrecoverableActiveReference
                }
            }
            ReferenceSource::Intent => {
                if record.recoverable_source.as_deref().is_some_and(|source| {
                    inspected_regular_file(upload_root, source)
                        .ok()
                        .flatten()
                        .is_some()
                }) {
                    AuditClassification::LifecycleInProgress
                } else {
                    AuditClassification::IntentInconsistency
                }
            }
            ReferenceSource::Hash => AuditClassification::StaleHashMissingFile,
        },
    }
}

fn audit_completed_intents(
    upload_root: &Path,
    model: &ReferenceModel,
    report: &mut AuditReport,
    example_limit: usize,
) {
    for candidate in &model.intent_candidates {
        if candidate.kind != crate::pending_fs::DELETE_FILES_KIND {
            continue;
        }
        let Ok(payload) =
            serde_json::from_str::<crate::pending_fs::DeleteFilesPayload>(&candidate.payload_json)
        else {
            continue;
        };
        let files_absent = payload.paths.iter().all(|path| {
            inspected_regular_file(upload_root, path).is_ok_and(|entry| entry.is_none())
        });
        let dirs_absent = payload
            .dirs
            .iter()
            .all(|board| path_entry_is_absent(&upload_root.join(board)));
        if files_absent && dirs_absent {
            let classification = AuditClassification::CompletedIntent;
            report.record(
                classification,
                MediaCategory::Unknown,
                0,
                Some(AuditExample {
                    classification,
                    category: MediaCategory::Unknown,
                    board: payload.dirs.first().cloned(),
                    managed_id: "completed-delete".to_owned(),
                    owner: Some(format!("intent:{}", candidate.id)),
                    bytes: 0,
                    recommended_action: recommended_action(classification),
                }),
                example_limit,
            );
        }
    }
}

#[derive(Debug, Default)]
struct InventoryPage {
    entries: Vec<InventoryEntry>,
    boards_examined: u64,
    has_more: bool,
    last_key: Option<String>,
    errors: Vec<String>,
}

struct InventoryWalker<'a> {
    root: &'a Path,
    model: &'a ReferenceModel,
    after: Option<&'a str>,
    limit: usize,
    #[cfg(unix)]
    root_device: u64,
    page: InventoryPage,
}

impl<'a> InventoryWalker<'a> {
    fn new(
        root: &'a Path,
        model: &'a ReferenceModel,
        cursor: &'a ReconcileCursor,
        limit: usize,
    ) -> Result<Self> {
        let metadata = std::fs::symlink_metadata(root)
            .with_context(|| format!("inspect managed media root {}", root.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            anyhow::bail!("configured managed media root is not a plain directory");
        }
        #[cfg(unix)]
        let root_device = {
            use std::os::unix::fs::MetadataExt as _;
            metadata.dev()
        };
        Ok(Self {
            root,
            model,
            after: cursor.after_managed_key.as_deref(),
            limit,
            #[cfg(unix)]
            root_device,
            page: InventoryPage::default(),
        })
    }

    fn scan(mut self) -> InventoryPage {
        self.walk_root();
        self.page
    }

    fn walk_root(&mut self) {
        let entries = match sorted_directory_entries(self.root) {
            Ok(entries) => entries,
            Err(error) => {
                self.page
                    .errors
                    .push(format!("managed root enumeration failed: {error}"));
                return;
            }
        };
        for entry in entries {
            if self.page.has_more {
                break;
            }
            let path = entry.path();
            let name = entry.file_name();
            let name_utf8 = name.to_str();
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(_) => {
                    self.push_path(
                        &path,
                        None,
                        MediaCategory::Unknown,
                        InventoryEntryKind::ScanError,
                    );
                    continue;
                }
            };
            if metadata.file_type().is_symlink() {
                self.push_path(
                    &path,
                    None,
                    MediaCategory::Unknown,
                    InventoryEntryKind::Symlink,
                );
                continue;
            }
            if self.crosses_filesystem(&metadata) {
                self.push_path(
                    &path,
                    name_utf8.map(str::to_owned),
                    MediaCategory::Unknown,
                    InventoryEntryKind::UnsafePath,
                );
                continue;
            }
            if metadata.is_dir() {
                match name_utf8 {
                    Some(".pending") => self.walk_pending_root(&path),
                    Some(board) if self.model.boards.contains(board) => {
                        self.page.boards_examined = self.page.boards_examined.saturating_add(1);
                        self.walk_board(&path, board);
                    }
                    _ => self.push_path(
                        &path,
                        name_utf8.map(str::to_owned),
                        MediaCategory::Unknown,
                        InventoryEntryKind::CrossBoard,
                    ),
                }
            } else {
                self.push_path(
                    &path,
                    None,
                    MediaCategory::Unknown,
                    file_entry_kind(&metadata),
                );
            }
        }
    }

    fn walk_board(&mut self, board_path: &Path, board: &str) {
        let entries = match sorted_directory_entries(board_path) {
            Ok(entries) => entries,
            Err(_) => {
                self.push_path(
                    board_path,
                    Some(board.to_owned()),
                    MediaCategory::Unknown,
                    InventoryEntryKind::ScanError,
                );
                return;
            }
        };
        for entry in entries {
            if self.page.has_more {
                break;
            }
            let path = entry.path();
            let name = entry.file_name();
            let name_utf8 = name.to_str();
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(_) => {
                    self.push_path(
                        &path,
                        Some(board.to_owned()),
                        MediaCategory::Unknown,
                        InventoryEntryKind::ScanError,
                    );
                    continue;
                }
            };
            if metadata.file_type().is_symlink() {
                self.push_path(
                    &path,
                    Some(board.to_owned()),
                    MediaCategory::Unknown,
                    InventoryEntryKind::Symlink,
                );
                continue;
            }
            if self.crosses_filesystem(&metadata) {
                self.push_path(
                    &path,
                    Some(board.to_owned()),
                    MediaCategory::Unknown,
                    InventoryEntryKind::UnsafePath,
                );
                continue;
            }
            if metadata.is_dir() {
                match name_utf8 {
                    Some("thumbs") => self.walk_thumbnails(&path, board),
                    Some("_banner" | "_favicon") => {}
                    _ => self.push_path(
                        &path,
                        Some(board.to_owned()),
                        MediaCategory::Unknown,
                        InventoryEntryKind::UnexpectedDirectory,
                    ),
                }
            } else {
                let kind = if name_utf8.is_some_and(is_safe_file_name) {
                    file_entry_kind(&metadata)
                } else {
                    InventoryEntryKind::UnsafePath
                };
                self.push_path(&path, Some(board.to_owned()), MediaCategory::Original, kind);
            }
        }
    }

    fn walk_thumbnails(&mut self, thumbnails: &Path, board: &str) {
        let entries = match sorted_directory_entries(thumbnails) {
            Ok(entries) => entries,
            Err(_) => {
                self.push_path(
                    thumbnails,
                    Some(board.to_owned()),
                    MediaCategory::Thumbnail,
                    InventoryEntryKind::ScanError,
                );
                return;
            }
        };
        for entry in entries {
            if self.page.has_more {
                break;
            }
            let path = entry.path();
            let name = entry.file_name();
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(_) => {
                    self.push_path(
                        &path,
                        Some(board.to_owned()),
                        MediaCategory::Thumbnail,
                        InventoryEntryKind::ScanError,
                    );
                    continue;
                }
            };
            let kind = if metadata.file_type().is_symlink() {
                InventoryEntryKind::Symlink
            } else if self.crosses_filesystem(&metadata) {
                InventoryEntryKind::UnsafePath
            } else if metadata.is_dir() {
                InventoryEntryKind::UnexpectedDirectory
            } else if !name.to_str().is_some_and(is_safe_file_name) {
                InventoryEntryKind::UnsafePath
            } else {
                file_entry_kind(&metadata)
            };
            self.push_path(
                &path,
                Some(board.to_owned()),
                MediaCategory::Thumbnail,
                kind,
            );
        }
    }

    fn walk_pending_root(&mut self, pending_root: &Path) {
        let stages = match sorted_directory_entries(pending_root) {
            Ok(entries) => entries,
            Err(_) => {
                self.push_path(
                    pending_root,
                    None,
                    MediaCategory::UploadStage,
                    InventoryEntryKind::ScanError,
                );
                return;
            }
        };
        for stage in stages {
            if self.page.has_more {
                break;
            }
            let path = stage.path();
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(_) => {
                    self.push_path(
                        &path,
                        None,
                        MediaCategory::UploadStage,
                        InventoryEntryKind::ScanError,
                    );
                    continue;
                }
            };
            if metadata.file_type().is_symlink() {
                self.push_path(
                    &path,
                    None,
                    MediaCategory::UploadStage,
                    InventoryEntryKind::Symlink,
                );
            } else if self.crosses_filesystem(&metadata) {
                self.push_path(
                    &path,
                    None,
                    MediaCategory::UploadStage,
                    InventoryEntryKind::UnsafePath,
                );
            } else if metadata.is_dir() {
                self.walk_pending_stage(&path);
            } else {
                self.push_path(
                    &path,
                    None,
                    MediaCategory::UploadStage,
                    file_entry_kind(&metadata),
                );
            }
        }
    }

    fn walk_pending_stage(&mut self, stage: &Path) {
        let board_entries = match sorted_directory_entries(stage) {
            Ok(entries) => entries,
            Err(_) => {
                self.push_path(
                    stage,
                    None,
                    MediaCategory::UploadStage,
                    InventoryEntryKind::ScanError,
                );
                return;
            }
        };
        for board_entry in board_entries {
            if self.page.has_more {
                break;
            }
            let path = board_entry.path();
            let board = board_entry.file_name();
            let board_utf8 = board.to_str();
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(_) => {
                    self.push_path(
                        &path,
                        board_utf8.map(str::to_owned),
                        MediaCategory::UploadStage,
                        InventoryEntryKind::ScanError,
                    );
                    continue;
                }
            };
            if metadata.file_type().is_symlink() {
                self.push_path(
                    &path,
                    board_utf8.map(str::to_owned),
                    MediaCategory::UploadStage,
                    InventoryEntryKind::Symlink,
                );
            } else if self.crosses_filesystem(&metadata) {
                self.push_path(
                    &path,
                    board_utf8.map(str::to_owned),
                    MediaCategory::UploadStage,
                    InventoryEntryKind::UnsafePath,
                );
            } else if metadata.is_dir()
                && board_utf8.is_some_and(|board| self.model.boards.contains(board))
            {
                self.walk_pending_board(&path, board_utf8.unwrap_or_default());
            } else {
                self.push_path(
                    &path,
                    board_utf8.map(str::to_owned),
                    MediaCategory::UploadStage,
                    InventoryEntryKind::CrossBoard,
                );
            }
        }
    }

    fn walk_pending_board(&mut self, board_path: &Path, board: &str) {
        let entries = match sorted_directory_entries(board_path) {
            Ok(entries) => entries,
            Err(_) => {
                self.push_path(
                    board_path,
                    Some(board.to_owned()),
                    MediaCategory::UploadStage,
                    InventoryEntryKind::ScanError,
                );
                return;
            }
        };
        for entry in entries {
            if self.page.has_more {
                break;
            }
            let path = entry.path();
            let name = entry.file_name();
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(_) => {
                    self.push_path(
                        &path,
                        Some(board.to_owned()),
                        MediaCategory::UploadStage,
                        InventoryEntryKind::ScanError,
                    );
                    continue;
                }
            };
            if metadata.is_dir()
                && !metadata.file_type().is_symlink()
                && name.to_str() == Some("thumbs")
                && !self.crosses_filesystem(&metadata)
            {
                self.walk_pending_thumbnails(&path, board);
                continue;
            }
            let kind = if metadata.file_type().is_symlink() {
                InventoryEntryKind::Symlink
            } else if self.crosses_filesystem(&metadata) {
                InventoryEntryKind::UnsafePath
            } else if metadata.is_dir() {
                InventoryEntryKind::UnexpectedDirectory
            } else if !name.to_str().is_some_and(is_safe_file_name) {
                InventoryEntryKind::UnsafePath
            } else {
                file_entry_kind(&metadata)
            };
            self.push_path(
                &path,
                Some(board.to_owned()),
                MediaCategory::UploadStage,
                kind,
            );
        }
    }

    fn walk_pending_thumbnails(&mut self, thumbnails: &Path, board: &str) {
        let entries = match sorted_directory_entries(thumbnails) {
            Ok(entries) => entries,
            Err(_) => {
                self.push_path(
                    thumbnails,
                    Some(board.to_owned()),
                    MediaCategory::UploadStage,
                    InventoryEntryKind::ScanError,
                );
                return;
            }
        };
        for entry in entries {
            if self.page.has_more {
                break;
            }
            let path = entry.path();
            let name = entry.file_name();
            let metadata = match std::fs::symlink_metadata(&path) {
                Ok(metadata) => metadata,
                Err(_) => {
                    self.push_path(
                        &path,
                        Some(board.to_owned()),
                        MediaCategory::UploadStage,
                        InventoryEntryKind::ScanError,
                    );
                    continue;
                }
            };
            let kind = if metadata.file_type().is_symlink() {
                InventoryEntryKind::Symlink
            } else if self.crosses_filesystem(&metadata) {
                InventoryEntryKind::UnsafePath
            } else if metadata.is_dir() {
                InventoryEntryKind::UnexpectedDirectory
            } else if !name.to_str().is_some_and(is_safe_file_name) {
                InventoryEntryKind::UnsafePath
            } else {
                file_entry_kind(&metadata)
            };
            self.push_path(
                &path,
                Some(board.to_owned()),
                MediaCategory::UploadStage,
                kind,
            );
        }
    }

    fn crosses_filesystem(&self, metadata: &std::fs::Metadata) -> bool {
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            metadata.dev() != self.root_device
        }
        #[cfg(not(unix))]
        {
            let _ = metadata;
            false
        }
    }

    fn push_path(
        &mut self,
        path: &Path,
        board: Option<String>,
        category: MediaCategory,
        entry_kind: InventoryEntryKind,
    ) {
        let relative_path = path.strip_prefix(self.root).ok();
        let sort_key = relative_path.map_or_else(
            || redacted_identifier(&path.to_string_lossy()),
            |relative| hex::encode(relative.as_os_str().as_encoded_bytes()),
        );
        if self.after.is_some_and(|after| sort_key.as_str() <= after) {
            return;
        }
        if self.page.entries.len() >= self.limit {
            self.page.has_more = true;
            return;
        }
        let relative = relative_path.and_then(|relative| {
            relative
                .to_str()
                .map(|value| value.replace(std::path::MAIN_SEPARATOR, "/"))
        });
        self.page.entries.push(InventoryEntry {
            absolute: path.to_path_buf(),
            relative,
            sort_key: sort_key.clone(),
            board,
            category,
            entry_kind,
        });
        self.page.last_key = Some(sort_key);
    }
}

fn sorted_directory_entries(path: &Path) -> Result<Vec<std::fs::DirEntry>> {
    let mut entries = std::fs::read_dir(path)
        .with_context(|| format!("read managed directory {}", path.display()))?
        .collect::<std::io::Result<Vec<_>>>()?;
    entries.sort_unstable_by(|left, right| {
        left.file_name()
            .as_encoded_bytes()
            .cmp(right.file_name().as_encoded_bytes())
    });
    Ok(entries)
}

fn is_safe_file_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.contains('\\')
        && !name.starts_with('.')
}

fn file_entry_kind(metadata: &std::fs::Metadata) -> InventoryEntryKind {
    if !metadata.is_file() {
        return InventoryEntryKind::Special;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        if metadata.nlink() != 1 {
            return InventoryEntryKind::HardLink;
        }
    }
    InventoryEntryKind::File
}

#[expect(
    clippy::too_many_lines,
    reason = "the inventory classification precedence is kept in one auditable fail-closed decision table"
)]
fn audit_inventory(
    upload_root: &Path,
    model: &ReferenceModel,
    page: InventoryPage,
    report: &mut AuditReport,
    hash_budget: &mut HashBudget,
    example_limit: usize,
) -> Vec<OrphanCandidate> {
    report.boards_examined = page.boards_examined;
    for error in page.errors {
        report.error(error, example_limit);
    }
    report.next_cursor = if page.has_more {
        ReconcileCursor {
            after_managed_key: page.last_key,
        }
    } else {
        ReconcileCursor::default()
    };
    let mut candidates = Vec::new();
    for entry in page.entries {
        report.paths_examined = report.paths_examined.saturating_add(1);
        let classification = classification_for_entry_kind(entry.entry_kind);
        if entry.entry_kind != InventoryEntryKind::File {
            let bytes = std::fs::symlink_metadata(&entry.absolute).map_or(0, |meta| meta.len());
            record_inventory_entry(model, report, &entry, classification, bytes, example_limit);
            continue;
        }
        let Some(relative) = entry.relative.as_deref() else {
            record_inventory_entry(
                model,
                report,
                &entry,
                AuditClassification::UnsafePath,
                0,
                example_limit,
            );
            continue;
        };
        let inspected = inspected_regular_file(upload_root, relative);
        let Some((path, identity)) = (match inspected {
            Ok(inspected) => inspected,
            Err(_) => {
                record_inventory_entry(
                    model,
                    report,
                    &entry,
                    AuditClassification::UnsafePath,
                    0,
                    example_limit,
                );
                continue;
            }
        }) else {
            record_inventory_entry(
                model,
                report,
                &entry,
                AuditClassification::ScanError,
                0,
                example_limit,
            );
            continue;
        };
        let category = path_category_from_claims(model, relative, entry.category);
        let required_claims = model.required_claims(relative).collect::<Vec<_>>();
        if let Some(claim) = required_claims.first() {
            let classification = if required_claims
                .iter()
                .any(|claim| claim.role == ReferenceRole::Temporary)
            {
                AuditClassification::LifecycleInProgress
            } else {
                AuditClassification::Healthy
            };
            let mut claimed_entry = entry.clone();
            claimed_entry.category = category;
            if claimed_entry.board.is_none() {
                claimed_entry.board = claim
                    .owner
                    .split_once(':')
                    .and_then(|_| relative.split('/').next())
                    .filter(|board| model.boards.contains(*board))
                    .map(str::to_owned);
            }
            record_inventory_entry(
                model,
                report,
                &claimed_entry,
                classification,
                identity.size,
                example_limit,
            );
            continue;
        }
        if model.scheduled_claim(relative).is_some()
            || entry
                .board
                .as_ref()
                .is_some_and(|board| model.scheduled_boards.contains(board))
        {
            let mut scheduled_entry = entry.clone();
            scheduled_entry.category = category;
            record_inventory_entry(
                model,
                report,
                &scheduled_entry,
                AuditClassification::ScheduledDeletion,
                identity.size,
                example_limit,
            );
            continue;
        }
        let board_conflict = entry.board.as_ref().is_some_and(|board| {
            model.case_conflict_boards.contains(board)
                || !relative_board_matches(relative, board, category)
        });
        if board_conflict {
            let mut unsafe_entry = entry.clone();
            unsafe_entry.category = category;
            record_inventory_entry(
                model,
                report,
                &unsafe_entry,
                AuditClassification::CrossBoardPath,
                identity.size,
                example_limit,
            );
            continue;
        }
        let Some(board) = entry.board.clone() else {
            let mut ambiguous_entry = entry.clone();
            ambiguous_entry.category = category;
            record_inventory_entry(
                model,
                report,
                &ambiguous_entry,
                AuditClassification::AmbiguousOrphan,
                identity.size,
                example_limit,
            );
            continue;
        };
        if category == MediaCategory::UploadStage
            || model.global_ambiguity
            || model.incomplete
            || report.incomplete
        {
            let mut ambiguous_entry = entry.clone();
            ambiguous_entry.category = category;
            record_inventory_entry(
                model,
                report,
                &ambiguous_entry,
                AuditClassification::AmbiguousOrphan,
                identity.size,
                example_limit,
            );
            continue;
        }
        let digest = hash_budget
            .hash_if_bounded(&path, identity.size)
            .ok()
            .flatten();
        candidates.push(OrphanCandidate {
            relative: relative.to_owned(),
            board,
            category,
            identity,
            digest,
        });
    }
    candidates
}

fn relative_board_matches(relative: &str, board: &str, category: MediaCategory) -> bool {
    if category == MediaCategory::UploadStage {
        return relative
            .split('/')
            .nth(2)
            .is_some_and(|component| component == board);
    }
    relative
        .split('/')
        .next()
        .is_some_and(|component| component == board)
}

const fn classification_for_entry_kind(kind: InventoryEntryKind) -> AuditClassification {
    match kind {
        InventoryEntryKind::File => AuditClassification::Healthy,
        InventoryEntryKind::Symlink => AuditClassification::UnsafeSymlink,
        #[cfg(unix)]
        InventoryEntryKind::HardLink => AuditClassification::UnsafeHardLink,
        InventoryEntryKind::UnexpectedDirectory => AuditClassification::UnexpectedDirectory,
        InventoryEntryKind::Special => AuditClassification::UnsafeSpecialEntry,
        InventoryEntryKind::UnsafePath => AuditClassification::UnsafePath,
        InventoryEntryKind::CrossBoard => AuditClassification::CrossBoardPath,
        InventoryEntryKind::ScanError => AuditClassification::ScanError,
    }
}

fn record_inventory_entry(
    model: &ReferenceModel,
    report: &mut AuditReport,
    entry: &InventoryEntry,
    classification: AuditClassification,
    bytes: u64,
    example_limit: usize,
) {
    let managed_id = entry
        .relative
        .clone()
        .unwrap_or_else(|| format!("redacted-entry:{}", entry.sort_key));
    let board = entry
        .board
        .as_ref()
        .filter(|board| model.boards.contains(*board))
        .cloned();
    report.record(
        classification,
        entry.category,
        bytes,
        Some(AuditExample {
            classification,
            category: entry.category,
            board,
            managed_id,
            owner: None,
            bytes,
            recommended_action: recommended_action(classification),
        }),
        example_limit,
    );
}

fn finalize_orphan_classifications(
    candidates: &[OrphanCandidate],
    stable: bool,
    report: &mut AuditReport,
    example_limit: usize,
) {
    let classification = if stable {
        AuditClassification::SafeOrphanCandidate
    } else {
        AuditClassification::AmbiguousOrphan
    };
    for candidate in candidates {
        report.record(
            classification,
            candidate.category,
            candidate.identity.size,
            Some(AuditExample {
                classification,
                category: candidate.category,
                board: Some(candidate.board.clone()),
                managed_id: candidate.relative.clone(),
                owner: None,
                bytes: candidate.identity.size,
                recommended_action: recommended_action(classification),
            }),
            example_limit,
        );
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RepairAttempt {
    Applied,
    Conflict,
}

const fn reserve_repair_attempt(attempted: &mut usize, limit: usize) -> bool {
    if *attempted >= limit {
        return false;
    }
    *attempted = attempted.saturating_add(1);
    true
}

fn apply_completed_intent_repairs(
    conn: &rusqlite::Connection,
    upload_root: &Path,
    candidates: &[IntentRepairCandidate],
    attempted: &mut usize,
    limit: usize,
    report: &mut AuditReport,
) {
    for candidate in candidates {
        if !reserve_repair_attempt(attempted, limit) {
            return;
        }
        let repair = repair_completed_intent(conn, upload_root, candidate);
        match repair {
            Ok(RepairAttempt::Applied) => {
                report.repairs.completed_intents_removed =
                    report.repairs.completed_intents_removed.saturating_add(1);
            }
            Ok(RepairAttempt::Conflict) => record_repair_conflict(report),
            Err(error) => {
                report.repairs.failures = report.repairs.failures.saturating_add(1);
                tracing::warn!(
                    target: "media_reconcile",
                    scan_id = %report.scan_id,
                    intent_id = %candidate.id,
                    error = %error,
                    "completed-intent repair failed"
                );
            }
        }
    }
}

fn apply_obsolete_job_repairs(
    conn: &rusqlite::Connection,
    upload_root: &Path,
    candidates: &[JobRepairCandidate],
    attempted: &mut usize,
    limit: usize,
    report: &mut AuditReport,
) {
    for candidate in candidates {
        if !reserve_repair_attempt(attempted, limit) {
            return;
        }
        let repair = repair_obsolete_job(conn, upload_root, candidate);
        match repair {
            Ok(RepairAttempt::Applied) => {
                report.repairs.obsolete_jobs_removed =
                    report.repairs.obsolete_jobs_removed.saturating_add(1);
            }
            Ok(RepairAttempt::Conflict) => record_repair_conflict(report),
            Err(error) => {
                report.repairs.failures = report.repairs.failures.saturating_add(1);
                tracing::warn!(
                    target: "media_reconcile",
                    scan_id = %report.scan_id,
                    job_id = candidate.id,
                    error = %error,
                    "obsolete-job repair failed"
                );
            }
        }
    }
}

fn apply_stale_hash_repairs(
    conn: &rusqlite::Connection,
    upload_root: &Path,
    candidates: &[HashRepairCandidate],
    attempted: &mut usize,
    limit: usize,
    report: &mut AuditReport,
) {
    for candidate in candidates {
        if !matches!(
            candidate.classification,
            AuditClassification::StaleHashMissingFile | AuditClassification::StaleHashUnreferenced
        ) {
            continue;
        }
        if !reserve_repair_attempt(attempted, limit) {
            return;
        }
        let repair = repair_stale_hash(conn, upload_root, candidate);
        match repair {
            Ok(RepairAttempt::Applied) => {
                report.repairs.stale_hash_rows_removed =
                    report.repairs.stale_hash_rows_removed.saturating_add(1);
            }
            Ok(RepairAttempt::Conflict) => record_repair_conflict(report),
            Err(error) => {
                report.repairs.failures = report.repairs.failures.saturating_add(1);
                tracing::warn!(
                    target: "media_reconcile",
                    scan_id = %report.scan_id,
                    hash = %candidate.digest.get(..12).unwrap_or(&candidate.digest),
                    error = %error,
                    "stale-hash repair failed"
                );
            }
        }
    }
}

fn apply_orphan_repairs(
    conn: &rusqlite::Connection,
    upload_root: &Path,
    candidates: &[OrphanCandidate],
    attempted: &mut usize,
    limit: usize,
    report: &mut AuditReport,
) {
    for candidate in candidates {
        if candidate.digest.is_none() {
            continue;
        }
        if !reserve_repair_attempt(attempted, limit) {
            return;
        }
        let repair = repair_orphan(conn, upload_root, candidate);
        match repair {
            Ok(RepairAttempt::Applied) => {
                report.repairs.files_scheduled = report.repairs.files_scheduled.saturating_add(1);
                report.repairs.bytes_scheduled = report
                    .repairs
                    .bytes_scheduled
                    .saturating_add(candidate.identity.size);
            }
            Ok(RepairAttempt::Conflict) => record_repair_conflict(report),
            Err(error) => {
                report.repairs.failures = report.repairs.failures.saturating_add(1);
                tracing::warn!(
                    target: "media_reconcile",
                    scan_id = %report.scan_id,
                    board = %candidate.board,
                    path_fingerprint = %redacted_identifier(&candidate.relative),
                    error = %error,
                    "safe-orphan repair failed"
                );
            }
        }
    }
}

fn apply_repairs(
    conn: &rusqlite::Connection,
    upload_root: &Path,
    model: &ReferenceModel,
    orphan_candidates: &[OrphanCandidate],
    limit: usize,
    report: &mut AuditReport,
) {
    let mut attempted = 0_usize;
    apply_completed_intent_repairs(
        conn,
        upload_root,
        &model.intent_candidates,
        &mut attempted,
        limit,
        report,
    );
    if attempted >= limit {
        return;
    }
    apply_obsolete_job_repairs(
        conn,
        upload_root,
        &model.job_candidates,
        &mut attempted,
        limit,
        report,
    );
    if attempted >= limit {
        return;
    }
    apply_stale_hash_repairs(
        conn,
        upload_root,
        &model.hash_candidates,
        &mut attempted,
        limit,
        report,
    );
    if attempted >= limit {
        return;
    }
    apply_orphan_repairs(
        conn,
        upload_root,
        orphan_candidates,
        &mut attempted,
        limit,
        report,
    );
}

const fn record_repair_conflict(report: &mut AuditReport) {
    report.repairs.revalidation_conflicts = report.repairs.revalidation_conflicts.saturating_add(1);
}

fn immediate<T>(conn: &rusqlite::Connection, operation: impl FnOnce() -> Result<T>) -> Result<T> {
    conn.execute_batch("BEGIN IMMEDIATE")
        .context("begin media reconciliation repair transaction")?;
    match operation() {
        Ok(value) => {
            conn.execute_batch("COMMIT")
                .context("commit media reconciliation repair transaction")?;
            Ok(value)
        }
        Err(error) => {
            drop(conn.execute_batch("ROLLBACK"));
            Err(error)
        }
    }
}

fn repair_orphan(
    conn: &rusqlite::Connection,
    upload_root: &Path,
    candidate: &OrphanCandidate,
) -> Result<RepairAttempt> {
    immediate(conn, || {
        if path_has_authoritative_claim(conn, upload_root, &candidate.relative, None)? {
            return Ok(RepairAttempt::Conflict);
        }
        let Some((path, current_identity)) =
            inspected_regular_file(upload_root, &candidate.relative)?
        else {
            return Ok(RepairAttempt::Conflict);
        };
        if current_identity != candidate.identity {
            return Ok(RepairAttempt::Conflict);
        }
        let Some(expected_digest) = candidate.digest.as_deref() else {
            return Ok(RepairAttempt::Conflict);
        };
        if sha256_regular_file(&path)? != expected_digest {
            return Ok(RepairAttempt::Conflict);
        }
        let board_exists = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM boards WHERE short_name = ?1)",
            [&candidate.board],
            |row| row.get::<_, bool>(0),
        )?;
        if !board_exists {
            return Ok(RepairAttempt::Conflict);
        }
        let Some(operation) =
            crate::db::build_delete_files_pending_op(std::slice::from_ref(&candidate.relative))?
        else {
            return Ok(RepairAttempt::Conflict);
        };
        crate::db::insert_pending_fs_op(conn, &operation)?;
        let acquired_reference = direct_post_reference_exists(conn, &candidate.relative)?
            || conn.query_row(
                "SELECT EXISTS(
                     SELECT 1 FROM file_hashes
                     WHERE file_path = ?1 OR thumb_path = ?1
                 )",
                [&candidate.relative],
                |row| row.get::<_, bool>(0),
            )?;
        let current = inspected_regular_file(upload_root, &candidate.relative)?;
        let identity_changed =
            current.as_ref().map(|(_, identity)| identity) != Some(&candidate.identity);
        let digest_changed = match current.as_ref() {
            Some((path, _)) => sha256_regular_file(path)? != expected_digest,
            None => true,
        };
        if acquired_reference || identity_changed || digest_changed {
            crate::db::delete_pending_fs_op(conn, &operation.id)?;
            return Ok(RepairAttempt::Conflict);
        }
        Ok(RepairAttempt::Applied)
    })
}

fn repair_stale_hash(
    conn: &rusqlite::Connection,
    upload_root: &Path,
    candidate: &HashRepairCandidate,
) -> Result<RepairAttempt> {
    immediate(conn, || {
        let current = conn
            .query_row(
                "SELECT file_path, thumb_path, mime_type
                 FROM file_hashes WHERE sha256 = ?1",
                [&candidate.digest],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        if current.as_ref()
            != Some(&(
                candidate.file_path.clone(),
                candidate.thumb_path.clone(),
                candidate.mime_type.clone(),
            ))
        {
            return Ok(RepairAttempt::Conflict);
        }
        if direct_post_reference_exists(conn, &candidate.file_path)?
            || (!candidate.thumb_path.is_empty()
                && direct_post_reference_exists(conn, &candidate.thumb_path)?)
            || path_has_lifecycle_claim(conn, upload_root, &candidate.file_path)?
            || (!candidate.thumb_path.is_empty()
                && path_has_lifecycle_claim(conn, upload_root, &candidate.thumb_path)?)
        {
            return Ok(RepairAttempt::Conflict);
        }
        let removed = conn.execute(
            "DELETE FROM file_hashes
             WHERE sha256 = ?1 AND file_path = ?2 AND thumb_path = ?3 AND mime_type = ?4",
            params![
                candidate.digest,
                candidate.file_path,
                candidate.thumb_path,
                candidate.mime_type
            ],
        )?;
        Ok(if removed == 1 {
            RepairAttempt::Applied
        } else {
            RepairAttempt::Conflict
        })
    })
}

fn repair_obsolete_job(
    conn: &rusqlite::Connection,
    upload_root: &Path,
    candidate: &JobRepairCandidate,
) -> Result<RepairAttempt> {
    immediate(conn, || {
        let current = conn
            .query_row(
                "SELECT job_type, payload, status FROM background_jobs WHERE id = ?1",
                [candidate.id],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((job_type, payload, status)) = current else {
            return Ok(RepairAttempt::Conflict);
        };
        if job_type != candidate.job_type
            || payload != candidate.payload
            || !matches!(status.as_str(), "done" | "failed")
        {
            return Ok(RepairAttempt::Conflict);
        }
        let ParsedJob::Media {
            post_id,
            board,
            source,
            output,
            ..
        } = parse_job(&job_type, &payload)?
        else {
            return Ok(RepairAttempt::Conflict);
        };
        let compatible = conn
            .query_row(
                "SELECT file_path FROM posts WHERE id = ?1
                   AND board_id = (SELECT id FROM boards WHERE short_name = ?2)",
                params![post_id, board],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
            .is_some_and(|path| path == source || path == output);
        if compatible
            || inspected_regular_file(upload_root, &source)?.is_some()
            || inspected_regular_file(upload_root, &output)?.is_some()
        {
            return Ok(RepairAttempt::Conflict);
        }
        let removed = conn.execute(
            "DELETE FROM background_jobs
             WHERE id = ?1 AND job_type = ?2 AND payload = ?3
               AND status IN ('done', 'failed')",
            params![candidate.id, candidate.job_type, candidate.payload],
        )?;
        Ok(if removed == 1 {
            RepairAttempt::Applied
        } else {
            RepairAttempt::Conflict
        })
    })
}

fn repair_completed_intent(
    conn: &rusqlite::Connection,
    upload_root: &Path,
    candidate: &IntentRepairCandidate,
) -> Result<RepairAttempt> {
    if candidate.kind != crate::pending_fs::DELETE_FILES_KIND {
        return Ok(RepairAttempt::Conflict);
    }
    immediate(conn, || {
        let current = conn
            .query_row(
                "SELECT kind, payload_json FROM pending_fs_ops WHERE id = ?1",
                [&candidate.id],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;
        if current.as_ref() != Some(&(candidate.kind.clone(), candidate.payload_json.clone())) {
            return Ok(RepairAttempt::Conflict);
        }
        let payload: crate::pending_fs::DeleteFilesPayload =
            serde_json::from_str(&candidate.payload_json)?;
        for path in &payload.paths {
            match inspected_regular_file(upload_root, path) {
                Ok(None) => {}
                Ok(Some(_)) | Err(_) => return Ok(RepairAttempt::Conflict),
            }
        }
        if payload
            .dirs
            .iter()
            .any(|board| !path_entry_is_absent(&upload_root.join(board)))
        {
            return Ok(RepairAttempt::Conflict);
        }
        let removed = conn.execute(
            "DELETE FROM pending_fs_ops WHERE id = ?1 AND kind = ?2 AND payload_json = ?3",
            params![candidate.id, candidate.kind, candidate.payload_json],
        )?;
        Ok(if removed == 1 {
            RepairAttempt::Applied
        } else {
            RepairAttempt::Conflict
        })
    })
}

fn direct_post_reference_exists(conn: &rusqlite::Connection, path: &str) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM posts
             WHERE file_path = ?1 OR thumb_path = ?1 OR audio_file_path = ?1
         )",
        [path],
        |row| row.get(0),
    )
    .context("check direct post media reference")
}

fn path_entry_is_absent(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_err_and(|error| error.kind() == std::io::ErrorKind::NotFound)
}

fn path_has_authoritative_claim(
    conn: &rusqlite::Connection,
    upload_root: &Path,
    path: &str,
    excluded_hash: Option<&str>,
) -> Result<bool> {
    if direct_post_reference_exists(conn, path)? {
        return Ok(true);
    }
    let hash_exists = conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM file_hashes
             WHERE (file_path = ?1 OR thumb_path = ?1)
               AND (?2 IS NULL OR sha256 != ?2)
         )",
        params![path, excluded_hash],
        |row| row.get::<_, bool>(0),
    )?;
    Ok(hash_exists || path_has_lifecycle_claim(conn, upload_root, path)?)
}

fn path_has_lifecycle_claim(
    conn: &rusqlite::Connection,
    upload_root: &Path,
    path: &str,
) -> Result<bool> {
    let mut job_statement =
        conn.prepare("SELECT job_type, payload FROM background_jobs ORDER BY id ASC")?;
    let jobs = job_statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (job_type, payload) in jobs {
        match parse_job(&job_type, &payload) {
            Ok(ParsedJob::Media { source, output, .. }) if source == path || output == path => {
                return Ok(true);
            }
            Ok(_) => {}
            Err(_) => return Ok(true),
        }
    }
    let mut intent_statement = conn
        .prepare("SELECT kind, payload_json FROM pending_fs_ops ORDER BY created_at ASC, id ASC")?;
    let intents = intent_statement
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (kind, payload) in intents {
        match intent_claims_path(upload_root, &kind, &payload, path) {
            Ok(false) => {}
            Ok(true) | Err(_) => return Ok(true),
        }
    }
    Ok(false)
}

fn intent_claims_path(
    upload_root: &Path,
    kind: &str,
    payload_json: &str,
    candidate: &str,
) -> Result<bool> {
    match kind {
        crate::pending_fs::DELETE_FILES_KIND => {
            let payload: crate::pending_fs::DeleteFilesPayload =
                serde_json::from_str(payload_json)?;
            Ok(payload.paths.iter().any(|path| path == candidate)
                || payload.dirs.iter().any(|board| {
                    candidate
                        .split('/')
                        .next()
                        .is_some_and(|owner| owner == board)
                }))
        }
        crate::media::prune::ORIGINAL_PRUNE_KIND => {
            let payload: crate::media::prune::OriginalPrunePayload =
                serde_json::from_str(payload_json)?;
            Ok(payload.paths.iter().any(|path| path.path == candidate))
        }
        crate::pending_fs::UPLOAD_FINALIZE_KIND => {
            let payload: crate::pending_fs::UploadFinalizePayload =
                serde_json::from_str(payload_json)?;
            let target = payload
                .relative_paths
                .iter()
                .chain(payload.optional_paths.iter())
                .any(|path| path == candidate);
            let staged = Path::new(&payload.stage_dir)
                .strip_prefix(upload_root)
                .ok()
                .and_then(Path::to_str)
                .is_some_and(|stage| {
                    payload
                        .relative_paths
                        .iter()
                        .chain(payload.optional_paths.iter())
                        .any(|path| format!("{stage}/{path}") == candidate)
                });
            Ok(target || staged)
        }
        crate::pending_fs::DELETE_BANNER_ASSETS_KIND => {
            drop(serde_json::from_str::<
                crate::pending_fs::DeleteBannerAssetsPayload,
            >(payload_json)?);
            Ok(false)
        }
        crate::pending_fs::FULL_RESTORE_SWAP_KIND | crate::pending_fs::BOARD_RESTORE_SWAP_KIND => {
            Ok(true)
        }
        _ => anyhow::bail!("unknown filesystem intent kind"),
    }
}

/// Audit one bounded managed-media page and optionally schedule proven repairs.
///
/// The database reference set is read inside a short, consistent transaction.
/// Filesystem enumeration happens after that transaction closes. Repair mode
/// starts a separate `BEGIN IMMEDIATE` transaction per candidate and repeats
/// both reference and file-identity checks before mutation.
///
/// # Errors
/// Returns an error when the configured root is unsafe, a database connection
/// cannot be acquired, or the authoritative reference snapshot cannot be read.
pub fn reconcile_managed_media(
    pool: &crate::db::DbPool,
    upload_dir: &str,
    mode: ReconcileMode,
    cursor: &ReconcileCursor,
    limits: ReconcileLimits,
) -> Result<AuditReport> {
    let limits = limits.bounded();
    let started_at = chrono::Utc::now().timestamp();
    let scan_id = uuid::Uuid::new_v4().simple().to_string();
    tracing::info!(
        target: "media_reconcile",
        scan_id = %scan_id,
        mode = ?mode,
        files_limit = limits.files_per_pass,
        references_limit = limits.database_rows_per_pass,
        "managed-media audit started"
    );
    let mut report = AuditReport::new(scan_id, started_at);
    let upload_root = Path::new(upload_dir)
        .canonicalize()
        .context("canonicalize configured managed-media root")?;
    crate::utils::fs_security::assert_dir_no_symlink(&upload_root)
        .context("configured managed-media root failed safety validation")?;
    let conn = pool
        .get()
        .context("get database connection for managed-media audit")?;
    let mut model = collect_reference_model(&conn, &upload_root, limits.database_rows_per_pass)?;
    report.references_examined = u64::try_from(model.database_rows_scanned).unwrap_or(u64::MAX);
    if model.incomplete {
        report.error(
            "authoritative database reference snapshot exceeded its configured row limit",
            limits.examples_per_pass,
        );
    }
    let mut hash_budget = HashBudget::new(limits.hash_bytes_per_pass);
    audit_references(
        &upload_root,
        &mut model,
        &mut report,
        &mut hash_budget,
        limits.examples_per_pass,
    );
    let page = match InventoryWalker::new(&upload_root, &model, cursor, limits.files_per_pass) {
        Ok(walker) => walker.scan(),
        Err(error) => {
            report.error(
                format!("managed filesystem inventory could not start: {error}"),
                limits.examples_per_pass,
            );
            InventoryPage::default()
        }
    };
    let orphan_candidates = audit_inventory(
        &upload_root,
        &model,
        page,
        &mut report,
        &mut hash_budget,
        limits.examples_per_pass,
    );
    let final_data_version = conn
        .query_row("PRAGMA data_version", [], |row| row.get::<_, i64>(0))
        .context("read final database generation for managed-media audit")?;
    report.transactionally_stable = final_data_version == model.data_version;
    if !report.transactionally_stable {
        report.incomplete = true;
        report.error(
            "database generation changed during filesystem enumeration",
            limits.examples_per_pass,
        );
    }
    let repair_safe = report.transactionally_stable
        && !report.incomplete
        && !model.incomplete
        && !model.global_ambiguity;
    finalize_orphan_classifications(
        &orphan_candidates,
        repair_safe,
        &mut report,
        limits.examples_per_pass,
    );
    if mode == ReconcileMode::Repair && repair_safe {
        apply_repairs(
            &conn,
            &upload_root,
            &model,
            &orphan_candidates,
            limits.repairs_per_pass,
            &mut report,
        );
    }
    report.completed_at = chrono::Utc::now().timestamp();
    update_metrics(&report);
    log_completion(&report, mode);
    Ok(report)
}

fn update_metrics(report: &AuditReport) {
    FILES_SCANNED_TOTAL.fetch_add(report.paths_examined, Ordering::Relaxed);
    REFERENCES_SCANNED_TOTAL.fetch_add(report.references_examined, Ordering::Relaxed);
    let missing = report
        .counts
        .iter()
        .filter(|(classification, _)| classification.is_missing())
        .fold(0_u64, |sum, (_, count)| sum.saturating_add(*count));
    MISSING_REFERENCES_TOTAL.fetch_add(missing, Ordering::Relaxed);
    let orphan_bytes = report
        .bytes_by_classification
        .get(&AuditClassification::SafeOrphanCandidate)
        .copied()
        .unwrap_or_default();
    SAFE_ORPHAN_BYTES_TOTAL.fetch_add(orphan_bytes, Ordering::Relaxed);
    let ambiguous = report
        .counts
        .get(&AuditClassification::AmbiguousOrphan)
        .copied()
        .unwrap_or_default();
    AMBIGUOUS_FILES_TOTAL.fetch_add(ambiguous, Ordering::Relaxed);
    REPAIRS_TOTAL.fetch_add(report.repairs.completed(), Ordering::Relaxed);
    REPAIR_CONFLICTS_TOTAL.fetch_add(report.repairs.revalidation_conflicts, Ordering::Relaxed);
    if report.incomplete {
        INCOMPLETE_SCANS_TOTAL.fetch_add(1, Ordering::Relaxed);
    }
}

fn log_completion(report: &AuditReport, mode: ReconcileMode) {
    let safe_orphans = report
        .counts
        .get(&AuditClassification::SafeOrphanCandidate)
        .copied()
        .unwrap_or_default();
    let ambiguous = report
        .counts
        .get(&AuditClassification::AmbiguousOrphan)
        .copied()
        .unwrap_or_default();
    let missing = report
        .counts
        .iter()
        .filter(|(classification, _)| classification.is_missing())
        .fold(0_u64, |sum, (_, count)| sum.saturating_add(*count));
    let unsafe_entries = [
        AuditClassification::UnsafeSymlink,
        AuditClassification::UnsafeHardLink,
        AuditClassification::UnexpectedDirectory,
        AuditClassification::UnsafeSpecialEntry,
        AuditClassification::UnsafePath,
        AuditClassification::CrossBoardPath,
    ]
    .iter()
    .fold(0_u64, |sum, classification| {
        sum.saturating_add(
            report
                .counts
                .get(classification)
                .copied()
                .unwrap_or_default(),
        )
    });
    tracing::info!(
        target: "media_reconcile",
        scan_id = %report.scan_id,
        mode = ?mode,
        boards_examined = report.boards_examined,
        files_examined = report.paths_examined,
        references_examined = report.references_examined,
        safe_orphans,
        ambiguous,
        missing,
        unsafe_entries,
        safe_orphan_bytes = report
            .bytes_by_classification
            .get(&AuditClassification::SafeOrphanCandidate)
            .copied()
            .unwrap_or_default(),
        files_scheduled = report.repairs.files_scheduled,
        stale_hash_rows_removed = report.repairs.stale_hash_rows_removed,
        obsolete_jobs_removed = report.repairs.obsolete_jobs_removed,
        completed_intents_removed = report.repairs.completed_intents_removed,
        repair_conflicts = report.repairs.revalidation_conflicts,
        incomplete = report.incomplete,
        transactionally_stable = report.transactionally_stable,
        duration_seconds = report.completed_at.saturating_sub(report.started_at),
        "managed-media audit completed"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::ensure;

    struct Fixture {
        root: tempfile::TempDir,
        pool: crate::db::DbPool,
        board_id: i64,
        thread_id: i64,
    }

    impl Fixture {
        fn new() -> Result<Self> {
            let root = tempfile::tempdir()?;
            std::fs::create_dir_all(root.path().join("b/thumbs"))?;
            let pool = crate::db::init_test_pool()?;
            let conn = pool.get()?;
            conn.execute(
                "INSERT INTO boards (short_name, name, description)
                 VALUES ('b', 'Board', '')",
                [],
            )?;
            let board_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO threads (board_id, subject) VALUES (?1, 'thread')",
                [board_id],
            )?;
            let thread_id = conn.last_insert_rowid();
            drop(conn);
            Ok(Self {
                root,
                pool,
                board_id,
                thread_id,
            })
        }

        fn upload_dir(&self) -> Result<&str> {
            self.root.path().to_str().context("UTF-8 fixture root")
        }

        fn write(&self, relative: &str, contents: &[u8]) -> Result<()> {
            let path = self.root.path().join(relative);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(path, contents)?;
            Ok(())
        }

        fn post(
            &self,
            file: Option<&str>,
            thumb: Option<&str>,
            mime: Option<&str>,
            state: &str,
        ) -> Result<i64> {
            let conn = self.pool.get()?;
            conn.execute(
                "INSERT INTO posts
                 (thread_id, board_id, name, body, body_html, deletion_token,
                  file_path, thumb_path, mime_type, media_processing_state)
                 VALUES (?1, ?2, 'anon', '', '', 'token', ?3, ?4, ?5, ?6)",
                params![self.thread_id, self.board_id, file, thumb, mime, state],
            )?;
            Ok(conn.last_insert_rowid())
        }

        fn hash(&self, digest: &str, file: &str, thumb: &str, mime: &str) -> Result<()> {
            self.pool.get()?.execute(
                "INSERT INTO file_hashes (sha256, file_path, thumb_path, mime_type)
                 VALUES (?1, ?2, ?3, ?4)",
                params![digest, file, thumb, mime],
            )?;
            Ok(())
        }

        fn job(&self, job_type: &str, payload: &serde_json::Value, status: &str) -> Result<()> {
            self.pool.get()?.execute(
                "INSERT INTO background_jobs (job_type, payload, status) VALUES (?1, ?2, ?3)",
                params![job_type, payload.to_string(), status],
            )?;
            Ok(())
        }

        fn intent(&self, id: &str, kind: &str, payload_json: &str) -> Result<()> {
            self.pool.get()?.execute(
                "INSERT INTO pending_fs_ops (id, kind, payload_json) VALUES (?1, ?2, ?3)",
                params![id, kind, payload_json],
            )?;
            Ok(())
        }

        fn audit(&self, mode: ReconcileMode, limits: ReconcileLimits) -> Result<AuditReport> {
            reconcile_managed_media(
                &self.pool,
                self.upload_dir()?,
                mode,
                &ReconcileCursor::default(),
                limits,
            )
        }

        fn pending_ops(&self) -> Result<Vec<crate::db::PendingFsOpRow>> {
            let conn = self.pool.get()?;
            crate::db::list_pending_fs_ops(&conn)
        }
    }

    fn limits() -> ReconcileLimits {
        ReconcileLimits {
            files_per_pass: 128,
            database_rows_per_pass: 1_024,
            hash_bytes_per_pass: 1024 * 1024,
            repairs_per_pass: 128,
            examples_per_pass: 128,
        }
    }

    fn count(report: &AuditReport, classification: AuditClassification) -> u64 {
        report
            .counts
            .get(&classification)
            .copied()
            .unwrap_or_default()
    }

    fn digest(contents: &[u8]) -> String {
        hex::encode(Sha256::digest(contents))
    }

    fn candidate(fixture: &Fixture, relative: &str) -> Result<OrphanCandidate> {
        let path = fixture.root.path().join(relative);
        let metadata = std::fs::symlink_metadata(&path)?;
        Ok(OrphanCandidate {
            relative: relative.to_owned(),
            board: "b".to_owned(),
            category: if relative.contains("/thumbs/") {
                MediaCategory::Thumbnail
            } else {
                MediaCategory::Original
            },
            identity: FileIdentity::from_metadata(&metadata),
            digest: Some(sha256_regular_file(&path)?),
        })
    }

    #[test]
    fn healthy_shared_pruned_and_job_media_are_preserved() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.write("b/shared.webp", b"shared")?;
        fixture.write("b/thumbs/shared.webp", b"thumbnail")?;
        fixture.hash(
            &digest(b"shared"),
            "b/shared.webp",
            "b/thumbs/shared.webp",
            "image/webp",
        )?;
        for _ in 0..2 {
            fixture.post(
                Some("b/shared.webp"),
                Some("b/thumbs/shared.webp"),
                Some("image/webp"),
                "",
            )?;
        }

        fixture.write("b/thumbs/pruned.webp", b"retained")?;
        fixture.post(
            Some("b/pruned.webp"),
            Some("b/thumbs/pruned.webp"),
            Some("image/webp"),
            crate::db::MEDIA_ORIGINAL_PRUNED,
        )?;

        fixture.write("b/audio.mp3", b"audio")?;
        fixture.write("b/thumbs/audio.png", b"waveform")?;
        fixture.hash(
            &digest(b"audio"),
            "b/audio.mp3",
            "b/thumbs/audio.png",
            "audio/mpeg",
        )?;
        let audio_post = fixture.post(
            Some("b/audio.mp3"),
            Some("b/thumbs/audio.png"),
            Some("audio/mpeg"),
            crate::db::MEDIA_PROCESSING_PENDING,
        )?;
        let job = serde_json::json!({
            "t": "AudioWaveform",
            "d": {"post_id": audio_post, "file_path": "b/audio.mp3", "board_short": "b"}
        });
        fixture.job("audio_waveform", &job, "pending")?;
        fixture.write("b/video.mp4", b"video")?;
        let video_post = fixture.post(
            Some("b/video.mp4"),
            None,
            Some("video/mp4"),
            crate::db::MEDIA_PROCESSING_PENDING,
        )?;
        let video_job = serde_json::json!({
            "t": "VideoTranscode",
            "d": {"post_id": video_post, "file_path": "b/video.mp4", "board_short": "b"}
        });
        fixture.job("video_transcode", &video_job, "running")?;
        fixture.write("b/prune-pending.webp", b"prune")?;
        let prune_post = fixture.post(
            Some("b/prune-pending.webp"),
            None,
            Some("image/webp"),
            crate::db::MEDIA_ORIGINAL_PRUNE_PENDING,
        )?;
        let prune_payload = crate::media::prune::OriginalPrunePayload {
            post_ids: vec![prune_post],
            paths: vec![crate::media::prune::CandidatePath {
                path: "b/prune-pending.webp".to_owned(),
                board_short: "b".to_owned(),
                size: 5,
            }],
        };
        fixture.intent(
            "prune",
            crate::media::prune::ORIGINAL_PRUNE_KIND,
            &serde_json::to_string(&prune_payload)?,
        )?;

        fixture.write("b/deleting.webp", b"delete")?;
        let delete_payload = crate::pending_fs::DeleteFilesPayload {
            paths: vec!["b/deleting.webp".to_owned()],
            dirs: Vec::new(),
        };
        fixture.intent(
            "delete",
            crate::pending_fs::DELETE_FILES_KIND,
            &serde_json::to_string(&delete_payload)?,
        )?;
        let report = fixture.audit(ReconcileMode::Audit, limits())?;
        ensure!(report.transactionally_stable);
        ensure!(!report.incomplete);
        ensure!(count(&report, AuditClassification::Healthy) > 0);
        ensure!(count(&report, AuditClassification::LifecycleInProgress) > 0);
        ensure!(count(&report, AuditClassification::ScheduledDeletion) > 0);
        ensure!(count(&report, AuditClassification::IntentionallyPrunedOriginal) == 1);
        ensure!(count(&report, AuditClassification::SafeOrphanCandidate) == 0);
        ensure!(report.repairs.completed() == 0);
        ensure!(fixture.root.path().join("b/shared.webp").exists());
        ensure!(fixture.root.path().join("b/thumbs/pruned.webp").exists());
        Ok(())
    }

    #[test]
    fn safe_orphan_repair_is_durable_interruptible_and_idempotent() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.write("b/orphan.webp", b"orphan")?;

        let audit = fixture.audit(ReconcileMode::Audit, limits())?;
        ensure!(count(&audit, AuditClassification::SafeOrphanCandidate) == 1);
        ensure!(fixture.root.path().join("b/orphan.webp").exists());
        ensure!(fixture.pending_ops()?.is_empty());

        let repair = fixture.audit(ReconcileMode::Repair, limits())?;
        ensure!(repair.repairs.files_scheduled == 1);
        ensure!(fixture.root.path().join("b/orphan.webp").exists());
        ensure!(fixture.pending_ops()?.len() == 1);

        crate::pending_fs::reconcile_pending_fs_ops(&fixture.pool, fixture.upload_dir()?)?;
        ensure!(!fixture.root.path().join("b/orphan.webp").exists());
        ensure!(fixture.pending_ops()?.is_empty());

        let fixed_point = fixture.audit(ReconcileMode::Repair, limits())?;
        ensure!(fixed_point.repairs.completed() == 0);
        Ok(())
    }

    #[test]
    fn shared_media_becomes_orphan_only_after_every_claim_is_removed() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.write("b/shared.webp", b"shared")?;
        let shared_digest = digest(b"shared");
        fixture.hash(&shared_digest, "b/shared.webp", "", "image/webp")?;
        let first_post = fixture.post(Some("b/shared.webp"), None, Some("image/webp"), "")?;
        let second_post = fixture.post(Some("b/shared.webp"), None, Some("image/webp"), "")?;

        ensure!(
            count(
                &fixture.audit(ReconcileMode::Audit, limits())?,
                AuditClassification::SafeOrphanCandidate
            ) == 0
        );
        fixture
            .pool
            .get()?
            .execute("DELETE FROM posts WHERE id = ?1", [first_post])?;
        ensure!(
            count(
                &fixture.audit(ReconcileMode::Audit, limits())?,
                AuditClassification::SafeOrphanCandidate
            ) == 0
        );
        fixture
            .pool
            .get()?
            .execute("DELETE FROM posts WHERE id = ?1", [second_post])?;
        ensure!(
            count(
                &fixture.audit(ReconcileMode::Audit, limits())?,
                AuditClassification::SafeOrphanCandidate
            ) == 0
        );
        fixture.pool.get()?.execute(
            "DELETE FROM file_hashes WHERE sha256 = ?1",
            [&shared_digest],
        )?;

        let repair = fixture.audit(ReconcileMode::Repair, limits())?;
        ensure!(repair.repairs.files_scheduled == 1);
        crate::pending_fs::reconcile_pending_fs_ops(&fixture.pool, fixture.upload_dir()?)?;
        ensure!(!fixture.root.path().join("b/shared.webp").exists());
        ensure!(
            fixture
                .audit(ReconcileMode::Repair, limits())?
                .repairs
                .completed()
                == 0
        );
        Ok(())
    }

    #[test]
    fn interrupted_after_filesystem_deletion_converges_on_replay() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.write("b/interrupted.webp", b"orphan")?;
        let repair = fixture.audit(ReconcileMode::Repair, limits())?;
        ensure!(repair.repairs.files_scheduled == 1);
        ensure!(fixture.pending_ops()?.len() == 1);

        std::fs::remove_file(fixture.root.path().join("b/interrupted.webp"))?;
        ensure!(fixture.pending_ops()?.len() == 1);
        crate::pending_fs::reconcile_pending_fs_ops(&fixture.pool, fixture.upload_dir()?)?;
        ensure!(fixture.pending_ops()?.is_empty());
        let fixed_point = fixture.audit(ReconcileMode::Repair, limits())?;
        ensure!(fixed_point.repairs.completed() == 0);
        Ok(())
    }

    #[test]
    fn file_replaced_after_audit_is_not_scheduled() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.write("b/replaced.webp", b"first")?;
        let audited = candidate(&fixture, "b/replaced.webp")?;
        std::fs::remove_file(fixture.root.path().join("b/replaced.webp"))?;
        fixture.write("b/replaced.webp", b"replacement")?;

        let conn = fixture.pool.get()?;
        let attempt = repair_orphan(&conn, fixture.root.path(), &audited)?;
        ensure!(attempt == RepairAttempt::Conflict);
        ensure!(fixture.pending_ops()?.is_empty());
        ensure!(std::fs::read(fixture.root.path().join("b/replaced.webp"))? == b"replacement");
        Ok(())
    }

    #[test]
    fn new_reference_after_classification_cancels_cleanup_without_leaving_intent() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.write("b/race.webp", b"race")?;
        let audited = candidate(&fixture, "b/race.webp")?;
        fixture.post(Some("b/race.webp"), None, Some("image/webp"), "")?;

        let conn = fixture.pool.get()?;
        let attempt = repair_orphan(&conn, fixture.root.path(), &audited)?;
        ensure!(attempt == RepairAttempt::Conflict);
        ensure!(fixture.root.path().join("b/race.webp").exists());
        ensure!(fixture.pending_ops()?.is_empty());
        Ok(())
    }

    #[test]
    fn trigger_created_reference_at_intent_insert_is_caught_before_commit() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.write("b/trigger-race.webp", b"race")?;
        let audited = candidate(&fixture, "b/trigger-race.webp")?;
        fixture.pool.get()?.execute_batch(&format!(
            "CREATE TRIGGER reconcile_race
             AFTER INSERT ON pending_fs_ops
             BEGIN
                 INSERT INTO posts
                 (thread_id, board_id, name, body, body_html, deletion_token, file_path)
                 VALUES ({}, {}, 'anon', '', '', 'token', 'b/trigger-race.webp');
             END;",
            fixture.thread_id, fixture.board_id
        ))?;

        let conn = fixture.pool.get()?;
        let attempt = repair_orphan(&conn, fixture.root.path(), &audited)?;
        ensure!(attempt == RepairAttempt::Conflict);
        ensure!(fixture.pending_ops()?.is_empty());
        let conn = fixture.pool.get()?;
        ensure!(direct_post_reference_exists(&conn, "b/trigger-race.webp")?);
        ensure!(fixture.root.path().join("b/trigger-race.webp").exists());
        Ok(())
    }

    #[test]
    fn missing_media_and_pruned_thumbnail_receive_conservative_classifications() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.post(
            Some("b/missing.webp"),
            Some("b/thumbs/missing.webp"),
            Some("image/webp"),
            "",
        )?;
        fixture.write("b/thumbs/pruned.webp", b"retained")?;
        fixture.post(
            Some("b/pruned.webp"),
            Some("b/thumbs/pruned.webp"),
            Some("image/webp"),
            crate::db::MEDIA_ORIGINAL_PRUNED,
        )?;
        fixture.post(
            Some("b/audio.mp3"),
            Some("b/thumbs/audio.png"),
            Some("audio/mpeg"),
            "",
        )?;
        let video_post = fixture.post(
            Some("b/missing-video.mp4"),
            None,
            Some("video/mp4"),
            crate::db::MEDIA_PROCESSING_PENDING,
        )?;
        let video_job = serde_json::json!({
            "t": "VideoTranscode",
            "d": {
                "post_id": video_post,
                "file_path": "b/missing-video.mp4",
                "board_short": "b"
            }
        });
        fixture.pool.get()?.execute(
            "INSERT INTO background_jobs (job_type, payload, status)
             VALUES ('video_transcode', ?1, 'pending')",
            [video_job.to_string()],
        )?;

        let report = fixture.audit(ReconcileMode::Repair, limits())?;
        ensure!(count(&report, AuditClassification::MissingPrimary) >= 2);
        ensure!(count(&report, AuditClassification::MissingThumbnail) == 1);
        ensure!(count(&report, AuditClassification::MissingWaveform) == 1);
        ensure!(count(&report, AuditClassification::MissingDeterministicOutput) == 1);
        ensure!(count(&report, AuditClassification::IntentionallyPrunedOriginal) == 1);
        ensure!(report.repairs.files_scheduled == 0);
        ensure!(fixture.root.path().join("b/thumbs/pruned.webp").exists());
        Ok(())
    }

    #[test]
    fn stale_hash_repairs_are_transactional_and_shared_hashes_remain() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.hash(&digest(b"missing"), "b/missing.webp", "", "image/webp")?;
        fixture.write("b/shared.webp", b"shared")?;
        fixture.hash(&digest(b"shared"), "b/shared.webp", "", "image/webp")?;
        fixture.post(Some("b/shared.webp"), None, Some("image/webp"), "")?;

        let report = fixture.audit(ReconcileMode::Repair, limits())?;
        ensure!(report.repairs.stale_hash_rows_removed == 1);
        let conn = fixture.pool.get()?;
        let missing: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM file_hashes WHERE file_path = 'b/missing.webp')",
            [],
            |row| row.get(0),
        )?;
        let shared: bool = conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM file_hashes WHERE file_path = 'b/shared.webp')",
            [],
            |row| row.get(0),
        )?;
        ensure!(!missing);
        ensure!(shared);
        ensure!(fixture.root.path().join("b/shared.webp").exists());
        Ok(())
    }

    #[test]
    fn conflicting_and_cross_board_hash_metadata_remains_quarantined() -> Result<()> {
        let fixture = Fixture::new()?;
        std::fs::create_dir_all(fixture.root.path().join("c/thumbs"))?;
        fixture.pool.get()?.execute(
            "INSERT INTO boards (short_name, name, description) VALUES ('c', 'Other', '')",
            [],
        )?;
        fixture.write("b/conflict.webp", b"conflict")?;
        fixture.hash(&digest(b"conflict"), "b/conflict.webp", "", "image/webp")?;
        fixture.hash(&digest(b"different"), "b/conflict.webp", "", "image/webp")?;
        fixture.write("b/cross.webp", b"cross")?;
        fixture.hash(
            &digest(b"cross"),
            "b/cross.webp",
            "c/thumbs/cross.webp",
            "image/webp",
        )?;

        let report = fixture.audit(ReconcileMode::Repair, limits())?;
        ensure!(count(&report, AuditClassification::ConflictingHashMetadata) == 1);
        ensure!(count(&report, AuditClassification::CrossBoardMetadata) == 1);
        ensure!(report.repairs.completed() == 0);
        let rows: i64 =
            fixture
                .pool
                .get()?
                .query_row("SELECT COUNT(*) FROM file_hashes", [], |row| row.get(0))?;
        ensure!(rows == 3);
        ensure!(fixture.root.path().join("b/conflict.webp").exists());
        ensure!(fixture.root.path().join("b/cross.webp").exists());
        Ok(())
    }

    #[test]
    fn conflicting_hash_examples_are_ordered_by_managed_path() -> Result<()> {
        let fixture = Fixture::new()?;
        for (path, first, second) in [
            (
                "b/z-conflict.webp",
                b"z-first".as_slice(),
                b"z-second".as_slice(),
            ),
            (
                "b/a-conflict.webp",
                b"a-first".as_slice(),
                b"a-second".as_slice(),
            ),
        ] {
            fixture.write(path, first)?;
            fixture.hash(&digest(first), path, "", "image/webp")?;
            fixture.hash(&digest(second), path, "", "image/webp")?;
        }

        let first = fixture.audit(ReconcileMode::Audit, limits())?;
        let second = fixture.audit(ReconcileMode::Audit, limits())?;
        let conflicting_paths = |report: &AuditReport| {
            report
                .examples
                .iter()
                .filter(|example| {
                    example.classification == AuditClassification::ConflictingHashMetadata
                })
                .map(|example| example.managed_id.clone())
                .collect::<Vec<_>>()
        };

        ensure!(conflicting_paths(&first) == ["b/a-conflict.webp", "b/z-conflict.webp"]);
        ensure!(conflicting_paths(&first) == conflicting_paths(&second));
        Ok(())
    }

    #[test]
    fn malformed_intent_makes_apparent_orphan_ambiguous() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.write("b/orphan.webp", b"orphan")?;
        fixture.pool.get()?.execute(
            "INSERT INTO pending_fs_ops (id, kind, payload_json)
             VALUES ('bad', 'delete_files', '{not-json')",
            [],
        )?;

        let report = fixture.audit(ReconcileMode::Repair, limits())?;
        ensure!(count(&report, AuditClassification::MalformedIntent) == 1);
        ensure!(count(&report, AuditClassification::AmbiguousOrphan) == 1);
        ensure!(report.repairs.completed() == 0);
        ensure!(fixture.root.path().join("b/orphan.webp").exists());
        ensure!(fixture.pending_ops()?.len() == 1);
        Ok(())
    }

    #[test]
    fn unsafe_external_intent_is_distinct_and_preserves_unrelated_media() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.write("b/orphan.webp", b"orphan")?;
        let payload = crate::pending_fs::DeleteFilesPayload {
            paths: vec!["/tmp/outside-rustchan".to_owned()],
            dirs: Vec::new(),
        };
        fixture.pool.get()?.execute(
            "INSERT INTO pending_fs_ops (id, kind, payload_json)
             VALUES ('external', 'delete_files', ?1)",
            [serde_json::to_string(&payload)?],
        )?;

        let report = fixture.audit(ReconcileMode::Repair, limits())?;
        ensure!(count(&report, AuditClassification::UnsafeExternalIntent) == 1);
        ensure!(count(&report, AuditClassification::MalformedIntent) == 0);
        ensure!(count(&report, AuditClassification::AmbiguousOrphan) == 1);
        ensure!(report.repairs.completed() == 0);
        ensure!(fixture.root.path().join("b/orphan.webp").exists());
        ensure!(fixture.pending_ops()?.len() == 1);
        Ok(())
    }

    #[test]
    fn malformed_job_does_not_hide_valid_or_superseded_media_jobs() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.write("b/video.mp4", b"source")?;
        fixture.write("b/video.webm", b"output")?;
        let post_id = fixture.post(Some("b/video.webm"), None, Some("video/webm"), "")?;
        let job = serde_json::json!({
            "t": "VideoTranscode",
            "d": {"post_id": post_id, "file_path": "b/video.mp4", "board_short": "b"}
        })
        .to_string();
        let conn = fixture.pool.get()?;
        conn.execute(
            "INSERT INTO background_jobs (job_type, payload, status)
             VALUES ('video_transcode', ?1, 'done')",
            [&job],
        )?;
        conn.execute(
            "INSERT INTO background_jobs (job_type, payload, status)
             VALUES ('video_transcode', ?1, 'pending')",
            [&job],
        )?;
        conn.execute(
            "INSERT INTO background_jobs (job_type, payload, status)
             VALUES ('video_transcode', '{bad-json', 'failed')",
            [],
        )?;
        drop(conn);

        let report = fixture.audit(ReconcileMode::Repair, limits())?;
        ensure!(count(&report, AuditClassification::MalformedJob) == 1);
        ensure!(count(&report, AuditClassification::ObsoleteJob) == 1);
        ensure!(count(&report, AuditClassification::Healthy) > 0);
        ensure!(report.repairs.completed() == 0);
        let jobs: i64 =
            fixture
                .pool
                .get()?
                .query_row("SELECT COUNT(*) FROM background_jobs", [], |row| row.get(0))?;
        ensure!(jobs == 3);
        ensure!(fixture.root.path().join("b/video.mp4").exists());
        ensure!(fixture.root.path().join("b/video.webm").exists());
        Ok(())
    }

    #[test]
    fn incomplete_reference_snapshot_preserves_every_apparent_orphan() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.write("b/orphan.webp", b"orphan")?;
        fixture.post(None, None, None, "")?;
        let limited = ReconcileLimits {
            database_rows_per_pass: 1,
            ..limits()
        };

        let report = fixture.audit(ReconcileMode::Repair, limited)?;
        ensure!(report.incomplete);
        ensure!(count(&report, AuditClassification::SafeOrphanCandidate) == 0);
        ensure!(count(&report, AuditClassification::AmbiguousOrphan) == 1);
        ensure!(report.repairs.completed() == 0);
        ensure!(fixture.root.path().join("b/orphan.webp").exists());
        Ok(())
    }

    #[test]
    fn active_finalize_stage_and_missing_destination_are_lifecycle_work() -> Result<()> {
        let fixture = Fixture::new()?;
        let stage = fixture.root.path().join(".pending/upload-test");
        fixture.write(".pending/upload-test/b/upload.webp", b"staged")?;
        let staged_digest = digest(b"staged");
        let payload = crate::pending_fs::UploadFinalizePayload {
            stage_dir: stage.to_string_lossy().into_owned(),
            relative_paths: vec!["b/upload.webp".to_owned()],
            optional_paths: Vec::new(),
            artifact_sha256: BTreeMap::from([("b/upload.webp".to_owned(), staged_digest)]),
            primary_hash: None,
            primary_file_path: None,
            primary_thumb_path: None,
            primary_mime_type: None,
        };
        fixture.pool.get()?.execute(
            "INSERT INTO pending_fs_ops (id, kind, payload_json)
             VALUES ('finalize', 'upload_finalize', ?1)",
            [serde_json::to_string(&payload)?],
        )?;

        fixture.write("b/installed.webp", b"installed")?;
        let installed_payload = crate::pending_fs::UploadFinalizePayload {
            stage_dir: fixture
                .root
                .path()
                .join(".pending/upload-installed")
                .to_string_lossy()
                .into_owned(),
            relative_paths: vec!["b/installed.webp".to_owned()],
            optional_paths: Vec::new(),
            artifact_sha256: BTreeMap::from([(
                "b/installed.webp".to_owned(),
                digest(b"installed"),
            )]),
            primary_hash: None,
            primary_file_path: None,
            primary_thumb_path: None,
            primary_mime_type: None,
        };
        fixture.pool.get()?.execute(
            "INSERT INTO pending_fs_ops (id, kind, payload_json)
             VALUES ('installed', 'upload_finalize', ?1)",
            [serde_json::to_string(&installed_payload)?],
        )?;

        let lost_payload = crate::pending_fs::UploadFinalizePayload {
            stage_dir: fixture
                .root
                .path()
                .join(".pending/upload-lost")
                .to_string_lossy()
                .into_owned(),
            relative_paths: vec!["b/lost.webp".to_owned()],
            optional_paths: Vec::new(),
            artifact_sha256: BTreeMap::from([("b/lost.webp".to_owned(), digest(b"lost"))]),
            primary_hash: None,
            primary_file_path: None,
            primary_thumb_path: None,
            primary_mime_type: None,
        };
        fixture.pool.get()?.execute(
            "INSERT INTO pending_fs_ops (id, kind, payload_json)
             VALUES ('lost', 'upload_finalize', ?1)",
            [serde_json::to_string(&lost_payload)?],
        )?;

        let report = fixture.audit(ReconcileMode::Repair, limits())?;
        ensure!(count(&report, AuditClassification::LifecycleInProgress) >= 2);
        ensure!(
            count(
                &report,
                AuditClassification::MissingStageInstalledDestination
            ) == 1
        );
        ensure!(count(&report, AuditClassification::IntentInconsistency) >= 2);
        ensure!(count(&report, AuditClassification::MalformedIntent) == 0);
        ensure!(report.repairs.files_scheduled == 0);
        ensure!(fixture
            .root
            .path()
            .join(".pending/upload-test/b/upload.webp")
            .exists());
        Ok(())
    }

    #[test]
    fn cleanup_intent_that_conflicts_with_post_is_report_only() -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.write("b/active.webp", b"active")?;
        fixture.post(Some("b/active.webp"), None, Some("image/webp"), "")?;
        let payload = serde_json::to_string(&crate::pending_fs::DeleteFilesPayload {
            paths: vec!["b/active.webp".to_owned()],
            dirs: Vec::new(),
        })?;
        fixture.pool.get()?.execute(
            "INSERT INTO pending_fs_ops (id, kind, payload_json)
             VALUES ('conflict', 'delete_files', ?1)",
            [payload],
        )?;

        let report = fixture.audit(ReconcileMode::Repair, limits())?;
        ensure!(
            count(
                &report,
                AuditClassification::IntentConflictsWithActiveReference
            ) == 1
        );
        ensure!(report.repairs.completed() == 0);
        ensure!(fixture.root.path().join("b/active.webp").exists());
        ensure!(fixture.pending_ops()?.len() == 1);
        Ok(())
    }

    #[test]
    fn missing_waveform_with_valid_source_is_recoverable_but_not_attached_by_inference(
    ) -> Result<()> {
        let fixture = Fixture::new()?;
        fixture.write("b/audio.mp3", b"audio")?;
        let post_id = fixture.post(
            Some("b/audio.mp3"),
            Some("b/thumbs/audio.png"),
            Some("audio/mpeg"),
            crate::db::MEDIA_PROCESSING_PENDING,
        )?;
        let job = serde_json::json!({
            "t": "AudioWaveform",
            "d": {"post_id": post_id, "file_path": "b/audio.mp3", "board_short": "b"}
        });
        fixture.pool.get()?.execute(
            "INSERT INTO background_jobs (job_type, payload, status)
             VALUES ('audio_waveform', ?1, 'pending')",
            [job.to_string()],
        )?;

        let report = fixture.audit(ReconcileMode::Repair, limits())?;
        ensure!(count(&report, AuditClassification::RecoverableMissingDerived) == 1);
        ensure!(count(&report, AuditClassification::MissingWaveform) == 1);
        ensure!(report.repairs.completed() == 0);
        let thumb: Option<String> = fixture.pool.get()?.query_row(
            "SELECT thumb_path FROM posts WHERE id = ?1",
            [post_id],
            |row| row.get(0),
        )?;
        ensure!(thumb.as_deref() == Some("b/thumbs/audio.png"));
        Ok(())
    }

    #[test]
    fn completed_delete_intent_and_obsolete_terminal_job_repair_safely() -> Result<()> {
        let fixture = Fixture::new()?;
        let payload = serde_json::to_string(&crate::pending_fs::DeleteFilesPayload {
            paths: vec!["b/already-gone.webp".to_owned()],
            dirs: Vec::new(),
        })?;
        fixture.pool.get()?.execute(
            "INSERT INTO pending_fs_ops (id, kind, payload_json)
             VALUES ('complete', 'delete_files', ?1)",
            [payload],
        )?;
        let job = serde_json::json!({
            "t": "VideoTranscode",
            "d": {"post_id": 9999, "file_path": "b/gone.mp4", "board_short": "b"}
        });
        fixture.pool.get()?.execute(
            "INSERT INTO background_jobs (job_type, payload, status)
             VALUES ('video_transcode', ?1, 'done')",
            [job.to_string()],
        )?;

        let report = fixture.audit(ReconcileMode::Repair, limits())?;
        ensure!(count(&report, AuditClassification::CompletedIntent) == 1);
        ensure!(count(&report, AuditClassification::ObsoleteJob) == 1);
        ensure!(report.repairs.completed_intents_removed == 1);
        ensure!(report.repairs.obsolete_jobs_removed == 1);
        ensure!(fixture.pending_ops()?.is_empty());
        let jobs: i64 =
            fixture
                .pool
                .get()?
                .query_row("SELECT COUNT(*) FROM background_jobs", [], |row| row.get(0))?;
        ensure!(jobs == 0);
        Ok(())
    }

    #[test]
    fn repair_budget_is_shared_in_category_order() -> Result<()> {
        let fixture = Fixture::new()?;
        let payload = serde_json::to_string(&crate::pending_fs::DeleteFilesPayload {
            paths: vec!["b/already-gone.webp".to_owned()],
            dirs: Vec::new(),
        })?;
        fixture.intent("complete", crate::pending_fs::DELETE_FILES_KIND, &payload)?;
        let job = serde_json::json!({
            "t": "VideoTranscode",
            "d": {"post_id": 9999, "file_path": "b/gone.mp4", "board_short": "b"}
        });
        fixture.job("video_transcode", &job, "done")?;
        fixture.hash(&digest(b"missing"), "b/missing.webp", "", "image/webp")?;
        fixture.write("b/orphan.webp", b"orphan")?;

        let first = fixture.audit(
            ReconcileMode::Repair,
            ReconcileLimits {
                repairs_per_pass: 2,
                ..limits()
            },
        )?;
        ensure!(first.repairs.completed_intents_removed == 1);
        ensure!(first.repairs.obsolete_jobs_removed == 1);
        ensure!(first.repairs.stale_hash_rows_removed == 0);
        ensure!(first.repairs.files_scheduled == 0);

        let second = fixture.audit(
            ReconcileMode::Repair,
            ReconcileLimits {
                repairs_per_pass: 1,
                ..limits()
            },
        )?;
        ensure!(second.repairs.stale_hash_rows_removed == 1);
        ensure!(second.repairs.files_scheduled == 0);

        let third = fixture.audit(
            ReconcileMode::Repair,
            ReconcileLimits {
                repairs_per_pass: 1,
                ..limits()
            },
        )?;
        ensure!(third.repairs.files_scheduled == 1);
        ensure!(fixture.pending_ops()?.len() == 1);
        Ok(())
    }

    #[test]
    fn inventory_batches_resume_and_reports_are_deterministic() -> Result<()> {
        let fixture = Fixture::new()?;
        for name in ["a.webp", "b.webp", "c.webp", "d.webp", "e.webp"] {
            fixture.write(&format!("b/{name}"), name.as_bytes())?;
        }
        let page_limits = ReconcileLimits {
            files_per_pass: 2,
            ..limits()
        };
        let first = reconcile_managed_media(
            &fixture.pool,
            fixture.upload_dir()?,
            ReconcileMode::Audit,
            &ReconcileCursor::default(),
            page_limits,
        )?;
        ensure!(first.paths_examined == 2);
        ensure!(first.next_cursor.after_managed_key.is_some());
        let second = reconcile_managed_media(
            &fixture.pool,
            fixture.upload_dir()?,
            ReconcileMode::Audit,
            &first.next_cursor,
            page_limits,
        )?;
        ensure!(second.paths_examined == 2);
        ensure!(second.next_cursor.after_managed_key.is_some());
        let third = reconcile_managed_media(
            &fixture.pool,
            fixture.upload_dir()?,
            ReconcileMode::Audit,
            &second.next_cursor,
            page_limits,
        )?;
        ensure!(third.paths_examined == 1);
        ensure!(third.next_cursor.after_managed_key.is_none());
        ensure!(
            count(&first, AuditClassification::SafeOrphanCandidate)
                + count(&second, AuditClassification::SafeOrphanCandidate)
                + count(&third, AuditClassification::SafeOrphanCandidate)
                == 5
        );

        let stable_one = fixture.audit(ReconcileMode::Audit, limits())?;
        let stable_two = fixture.audit(ReconcileMode::Audit, limits())?;
        ensure!(stable_one.counts == stable_two.counts);
        ensure!(stable_one.categories == stable_two.categories);
        ensure!(stable_one.bytes_by_classification == stable_two.bytes_by_classification);
        ensure!(stable_one.examples == stable_two.examples);
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn unsafe_symlink_hardlink_directory_and_socket_are_report_only() -> Result<()> {
        use std::os::unix::fs as unix_fs;
        use std::os::unix::net::UnixListener;

        let fixture = Fixture::new()?;
        let outside = tempfile::NamedTempFile::new()?;
        unix_fs::symlink(outside.path(), fixture.root.path().join("b/link.webp"))?;
        fixture.write("b/hard.webp", b"hard")?;
        std::fs::hard_link(
            fixture.root.path().join("b/hard.webp"),
            fixture.root.path().join("b/hard-copy.webp"),
        )?;
        std::fs::create_dir_all(fixture.root.path().join("b/unexpected"))?;
        let socket_path = fixture.root.path().join("b/media.sock");
        let socket = UnixListener::bind(&socket_path)?;

        let report = fixture.audit(ReconcileMode::Repair, limits())?;
        ensure!(count(&report, AuditClassification::UnsafeSymlink) == 1);
        ensure!(count(&report, AuditClassification::UnsafeHardLink) == 2);
        ensure!(count(&report, AuditClassification::UnexpectedDirectory) == 1);
        ensure!(count(&report, AuditClassification::UnsafeSpecialEntry) == 1);
        ensure!(report.repairs.completed() == 0);
        ensure!(fixture
            .root
            .path()
            .join("b/link.webp")
            .symlink_metadata()
            .is_ok());
        ensure!(fixture.root.path().join("b/hard.webp").exists());
        ensure!(socket_path.exists());
        drop(socket);
        Ok(())
    }
}
