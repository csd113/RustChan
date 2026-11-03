//! Public values describing bounded managed-media reconciliation.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Default maximum examples retained by one reconciliation pass.
pub(super) const DEFAULT_EXAMPLE_LIMIT: usize = 32;

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
    pub(super) const fn is_missing(self) -> bool {
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
    pub(super) fn new(scan_id: String, started_at: i64) -> Self {
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

    pub(super) fn record(
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

    pub(super) fn error(&mut self, message: impl Into<String>, example_limit: usize) {
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
