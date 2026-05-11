use crate::error::{AppError, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

pub(crate) const BACKUP_V4_FORMAT: &str = "rustchan-backup-v4";
pub(crate) const BACKUP_V4_ARCHIVE_CONTAINER: &str = "zip";
pub(crate) const PARTS_DIR_NAME: &str = "parts";
pub(crate) const MANIFEST_FILE_NAME: &str = "manifest.json";
pub(crate) const BACKUP_METADATA_FILE_NAME: &str = "backup.json";
pub(crate) const CHECKSUMS_FILE_NAME: &str = "checksums.sha256";
pub(crate) const README_FILE_NAME: &str = "README.txt";

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BackupScope {
    FullSite,
    Board,
    SelectedBoards,
    PreMaintenance,
}

impl BackupScope {
    #[must_use]
    pub(crate) const fn slug(self) -> &'static str {
        match self {
            Self::FullSite => "full-site",
            Self::Board => "board",
            Self::SelectedBoards => "selected-boards",
            Self::PreMaintenance => "pre-maintenance",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BackupStorageMode {
    SingleZip,
    SplitZip,
    Directory,
    LegacyZip,
}

impl BackupStorageMode {
    #[must_use]
    pub(crate) const fn display_name(self) -> &'static str {
        match self {
            Self::SingleZip => "Single ZIP",
            Self::SplitZip => "Split ZIP",
            Self::Directory => "Directory",
            Self::LegacyZip => "Legacy ZIP",
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum BackupFileKind {
    Db,
    Settings,
    BoardJson,
    ThreadExport,
    PostExport,
    FileInventoryExport,
    OriginalMedia,
    Thumbnail,
    Audio,
    Banner,
    Favicon,
    TorKey,
    Maintenance,
    PendingFsOps,
    Log,
}

// The serialized manifest intentionally exposes independent inclusion bits for admin readability and compatibility.
#[expect(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub(crate) struct BackupIncludeFlags {
    pub database: bool,
    pub settings: bool,
    pub uploads: bool,
    pub thumbnails: bool,
    pub tor_keys: bool,
    pub board_exports: bool,
    pub file_inventory: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct DbSnapshotInfo {
    pub path: String,
    pub size: u64,
    pub sha256: String,
    pub integrity_check: Option<String>,
    pub foreign_key_check: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct BackupFileEntry {
    pub logical_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_logical_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub board: Option<String>,
    pub kind: BackupFileKind,
    pub size: u64,
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zip_part: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zip_entry_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compression_method: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct BackupPartInfo {
    pub filename: String,
    pub part_index: u32,
    pub total_parts: u32,
    pub backup_id: String,
    pub size: u64,
    pub sha256: String,
    pub target_part_size: u64,
    pub oversized: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub(crate) struct MaintenanceMetadata {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job_id: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_at: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_class: Option<String>,
    pub includes_uploads: bool,
    pub includes_file_inventory: bool,
    pub includes_tor_keys: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_integrity_check: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pre_foreign_key_check: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct BackupManifest {
    pub format: String,
    pub archive_container: String,
    pub backup_id: String,
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
    pub rustchan_version: String,
    pub scope: BackupScope,
    pub storage_mode: BackupStorageMode,
    #[serde(default)]
    pub included_boards: Vec<crate::models::BackupBoardSummary>,
    pub includes: BackupIncludeFlags,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub db_snapshot: Option<DbSnapshotInfo>,
    #[serde(default)]
    pub files: Vec<BackupFileEntry>,
    #[serde(default)]
    pub parts: Vec<BackupPartInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub maintenance: Option<MaintenanceMetadata>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub(crate) struct BackupMetadata {
    pub format: String,
    pub backup_id: String,
    pub scope: BackupScope,
    pub storage_mode: BackupStorageMode,
    pub created_at: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<i64>,
    pub total_size_bytes: u64,
    pub verified: bool,
    pub part_count: u32,
    pub includes_tor_keys: bool,
    #[serde(default)]
    pub included_boards: Vec<crate::models::BackupBoardSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct DirectoryStats {
    pub files: u64,
    pub bytes: u64,
}

impl DirectoryStats {
    #[must_use]
    pub(crate) const fn zero() -> Self {
        Self { files: 0, bytes: 0 }
    }

    pub(crate) const fn saturating_add_assign(&mut self, other: &Self) {
        self.files = self.files.saturating_add(other.files);
        self.bytes = self.bytes.saturating_add(other.bytes);
    }
}

#[derive(Debug, Clone)]
pub(crate) struct SavedBackupLayout {
    pub root_dir: PathBuf,
    pub backup_ref: String,
    pub manifest_path: PathBuf,
    pub metadata_path: PathBuf,
}

#[derive(Debug, Clone)]
pub(crate) struct VerifiedSavedV4File {
    pub logical_path: String,
    pub board: Option<String>,
    pub kind: BackupFileKind,
    pub size: u64,
    pub sha256: String,
    pub source: VerifiedSavedV4FileSource,
}

#[derive(Debug, Clone)]
pub(crate) enum VerifiedSavedV4FileSource {
    RootFile(PathBuf),
    ZipEntry {
        part_path: PathBuf,
        entry_path: String,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct VerifiedSavedV4DbSnapshot {
    pub file: VerifiedSavedV4File,
}

#[derive(Debug, Clone)]
pub(crate) struct VerifiedSavedV4BoardLayout {
    pub board_json: VerifiedSavedV4File,
    pub upload_files: Vec<VerifiedSavedV4File>,
}

#[derive(Debug, Clone)]
pub(crate) struct VerifiedSavedV4Root {
    pub metadata: BackupMetadata,
    pub manifest: BackupManifest,
    pub completed_at: i64,
    pub db_snapshot: Option<VerifiedSavedV4DbSnapshot>,
    pub site_favicon_files: Vec<VerifiedSavedV4File>,
    pub site_banner_files: Vec<VerifiedSavedV4File>,
    pub tor_key_files: Vec<VerifiedSavedV4File>,
    pub boards: HashMap<String, VerifiedSavedV4BoardLayout>,
}

#[must_use]
pub(crate) fn backups_root_dir() -> PathBuf {
    crate::config::backups_dir()
}

#[must_use]
pub(crate) fn build_backup_id(_scope: BackupScope, scope_label: &str) -> String {
    let timestamp = chrono::Local::now().format("%Y-%m-%d_%H%M").to_string();
    let short = uuid::Uuid::new_v4().simple().to_string();
    let short = short.get(..6).unwrap_or(&short);
    let scope_label = scope_label.replace([' ', '/'], "-");
    format!("{timestamp}_{scope_label}_{short}").replace("__", "_")
}

pub(crate) fn create_backup_root(backup_id: &str) -> Result<PathBuf> {
    let root = backups_root_dir().join(backup_id);
    std::fs::create_dir_all(&root).map_err(|error| {
        AppError::Internal(anyhow::anyhow!(
            "Create backup directory {}: {error}",
            root.display()
        ))
    })?;
    Ok(root)
}

pub(crate) fn detect_saved_backup_layout(root: &Path) -> Option<SavedBackupLayout> {
    if !root.is_dir() {
        return None;
    }
    let manifest_path = root.join(MANIFEST_FILE_NAME);
    let metadata_path = root.join(BACKUP_METADATA_FILE_NAME);
    if !manifest_path.is_file() || !metadata_path.is_file() {
        return None;
    }
    Some(SavedBackupLayout {
        root_dir: root.to_path_buf(),
        backup_ref: root.file_name()?.to_str()?.to_owned(),
        manifest_path,
        metadata_path,
    })
}

pub(crate) fn iter_saved_backup_layouts() -> Vec<SavedBackupLayout> {
    let mut layouts = Vec::new();
    let Ok(entries) = std::fs::read_dir(backups_root_dir()) else {
        return layouts;
    };
    for entry in entries.flatten() {
        if let Some(layout) = detect_saved_backup_layout(&entry.path()) {
            layouts.push(layout);
        }
    }
    layouts.sort_by(|left, right| right.backup_ref.cmp(&left.backup_ref));
    layouts
}

pub(crate) fn load_manifest(path: &Path) -> Result<BackupManifest> {
    let bytes = std::fs::read(path).map_err(|error| {
        AppError::Internal(anyhow::anyhow!("Read manifest {}: {error}", path.display()))
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        AppError::BadRequest(format!(
            "Invalid Backup v4 manifest {}: {error}",
            path.display()
        ))
    })
}

pub(crate) fn load_metadata(path: &Path) -> Result<BackupMetadata> {
    let bytes = std::fs::read(path).map_err(|error| {
        AppError::Internal(anyhow::anyhow!(
            "Read backup metadata {}: {error}",
            path.display()
        ))
    })?;
    serde_json::from_slice(&bytes).map_err(|error| {
        AppError::BadRequest(format!(
            "Invalid Backup v4 metadata {}: {error}",
            path.display()
        ))
    })
}

pub(crate) fn write_json_pretty<T: Serialize>(path: &Path, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec_pretty(value).map_err(|error| {
        AppError::Internal(anyhow::anyhow!("Serialize {}: {error}", path.display()))
    })?;
    std::fs::write(path, bytes).map_err(|error| {
        AppError::Internal(anyhow::anyhow!("Write {}: {error}", path.display()))
    })?;
    Ok(())
}

pub(crate) fn write_text(path: &Path, text: &str) -> Result<()> {
    std::fs::write(path, text.as_bytes()).map_err(|error| {
        AppError::Internal(anyhow::anyhow!("Write {}: {error}", path.display()))
    })?;
    Ok(())
}

pub(crate) fn sha256_hex_for_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}

pub(crate) fn sha256_hex_for_file(path: &Path) -> Result<String> {
    let mut file = std::fs::File::open(path).map_err(|error| {
        AppError::Internal(anyhow::anyhow!(
            "Open {} for hashing: {error}",
            path.display()
        ))
    })?;
    sha256_hex_for_reader(&mut file)
}

pub(crate) fn sha256_hex_for_reader<R: Read>(reader: &mut R) -> Result<String> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024].into_boxed_slice();
    loop {
        let read = reader.read(&mut buffer).map_err(|error| {
            AppError::Internal(anyhow::anyhow!("Read stream for hashing: {error}"))
        })?;
        if read == 0 {
            break;
        }
        let chunk = buffer.get(..read).ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!(
                "Invalid read size while hashing backup stream"
            ))
        })?;
        hasher.update(chunk);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub(crate) fn copy_file_and_hash<W: Write>(
    source_path: &Path,
    writer: &mut W,
) -> Result<(u64, String)> {
    let mut source = std::fs::File::open(source_path).map_err(|error| {
        AppError::Internal(anyhow::anyhow!("Open {}: {error}", source_path.display()))
    })?;
    copy_reader_and_hash(&mut source, writer)
}

pub(crate) fn copy_reader_and_hash<R: Read, W: Write>(
    reader: &mut R,
    writer: &mut W,
) -> Result<(u64, String)> {
    let mut hasher = Sha256::new();
    let mut buffer = vec![0u8; 64 * 1024].into_boxed_slice();
    let mut written = 0u64;
    loop {
        let read = reader.read(&mut buffer).map_err(|error| {
            AppError::Internal(anyhow::anyhow!("Read backup payload stream: {error}"))
        })?;
        if read == 0 {
            break;
        }
        let chunk = buffer.get(..read).ok_or_else(|| {
            AppError::Internal(anyhow::anyhow!(
                "Invalid read size while copying backup payload stream"
            ))
        })?;
        writer.write_all(chunk).map_err(|error| {
            AppError::Internal(anyhow::anyhow!("Write backup payload stream: {error}"))
        })?;
        written = written.saturating_add(read as u64);
        hasher.update(chunk);
    }
    Ok((written, hex::encode(hasher.finalize())))
}

pub(crate) fn scan_dir_stats(dir: &Path) -> DirectoryStats {
    if crate::utils::fs_security::assert_dir_no_symlink(dir).is_err() {
        return DirectoryStats::zero();
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return DirectoryStats::zero();
    };
    let mut stats = DirectoryStats::zero();
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = std::fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.file_type().is_dir() {
            stats.saturating_add_assign(&scan_dir_stats(&path));
            continue;
        }
        if metadata.file_type().is_file()
            && crate::utils::fs_security::assert_regular_file_no_symlink(&path).is_ok()
        {
            stats.files = stats.files.saturating_add(1);
            stats.bytes = stats.bytes.saturating_add(metadata.len());
        }
    }
    stats
}

pub(crate) fn validate_backup_id_matches_parts(manifest: &BackupManifest) -> Result<()> {
    for part in &manifest.parts {
        if part.backup_id != manifest.backup_id {
            return Err(AppError::BadRequest(format!(
                "Backup v4 part {} belongs to backup_id {}, expected {}.",
                part.filename, part.backup_id, manifest.backup_id
            )));
        }
    }
    Ok(())
}

pub(crate) fn build_readme(
    manifest: &BackupManifest,
    metadata: &BackupMetadata,
    has_tor_keys: bool,
) -> String {
    let mut out = String::new();
    let _ = writeln!(out, "RustChan Backup v4");
    let _ = writeln!(out);
    let _ = writeln!(out, "Backup ID: {}", manifest.backup_id);
    let _ = writeln!(out, "Scope: {}", manifest.scope.slug());
    let _ = writeln!(out, "Mode: {}", metadata.storage_mode.display_name());
    let _ = writeln!(out, "Archive container: {}", manifest.archive_container);
    let _ = writeln!(
        out,
        "Tor keys included: {}",
        if has_tor_keys { "yes" } else { "no" }
    );
    let _ = writeln!(out);
    let _ = writeln!(out, "Important files:");
    let _ = writeln!(out, "- {BACKUP_METADATA_FILE_NAME}");
    let _ = writeln!(out, "- {MANIFEST_FILE_NAME}");
    let _ = writeln!(out, "- {CHECKSUMS_FILE_NAME}");
    if metadata.storage_mode == BackupStorageMode::SplitZip {
        let _ = writeln!(out, "- {PARTS_DIR_NAME}/");
    }
    if has_tor_keys {
        let _ = writeln!(out, "- tor-keys/");
    }
    let _ = writeln!(out);
    let _ = writeln!(out, "Keep all files in this backup folder together.");
    let _ = writeln!(out, "Do not mix ZIP parts from different backups.");
    let _ = writeln!(
        out,
        "Use RustChan restore for safe programmatic restore. Standard ZIP tools are useful only for manual inspection and emergency extraction."
    );
    out
}

pub(crate) fn write_root_checksums(root_dir: &Path, extra_paths: &[&Path]) -> Result<()> {
    let mut lines = String::new();
    for name in [
        README_FILE_NAME,
        BACKUP_METADATA_FILE_NAME,
        MANIFEST_FILE_NAME,
        CHECKSUMS_FILE_NAME,
    ] {
        let path = root_dir.join(name);
        if !path.is_file() {
            continue;
        }
        let sha256 = sha256_hex_for_file(&path)?;
        let _ = writeln!(lines, "{sha256}  {name}");
    }
    for path in extra_paths {
        if !path.is_file() {
            continue;
        }
        let sha256 = sha256_hex_for_file(path)?;
        let rel = path.strip_prefix(root_dir).unwrap_or(path);
        let _ = writeln!(lines, "{}  {}", sha256, rel.to_string_lossy());
    }
    write_text(&root_dir.join(CHECKSUMS_FILE_NAME), &lines)
}

pub(crate) fn sanitize_logical_path(path: &str) -> Result<()> {
    const SLASH_LIKE_SEPARATORS: [char; 5] =
        ['\u{2044}', '\u{2215}', '\u{29f8}', '\u{29f9}', '\u{ff0f}'];

    if path.is_empty() || path.contains('\\') || path.starts_with('/') || path.contains('\0') {
        return Err(AppError::BadRequest(format!(
            "Backup v4 contains suspicious logical path '{path}'"
        )));
    }
    let lower = path.to_ascii_lowercase();
    if lower.contains("%2e")
        || lower.contains("%2f")
        || lower.contains("%5c")
        || path.chars().any(|ch| SLASH_LIKE_SEPARATORS.contains(&ch))
    {
        return Err(AppError::BadRequest(format!(
            "Backup v4 contains suspicious logical path '{path}'"
        )));
    }
    let normalized = path.trim_end_matches('/');
    if normalized.is_empty() {
        return Err(AppError::BadRequest(format!(
            "Backup v4 contains suspicious logical path '{path}'"
        )));
    }
    for part in normalized.split('/') {
        if part.is_empty() || part == "." || part == ".." || part.contains(':') {
            return Err(AppError::BadRequest(format!(
                "Backup v4 contains suspicious logical path '{path}'"
            )));
        }
    }
    Ok(())
}

pub(crate) fn runtime_upload_path_to_logical(
    board_short: &str,
    runtime_relative_path: &str,
) -> Result<(String, BackupFileKind)> {
    super::common::validate_restored_media_path_for_board(
        runtime_relative_path,
        board_short,
        "Backup v4 runtime media path",
    )?;
    let prefix = format!("{board_short}/");
    let suffix = runtime_relative_path.strip_prefix(&prefix).ok_or_else(|| {
        AppError::BadRequest(format!(
            "Backup v4 runtime media path '{runtime_relative_path}' is outside /{board_short}/."
        ))
    })?;

    if let Some(favicon_rel) = suffix.strip_prefix("_favicon/") {
        sanitize_logical_path(favicon_rel)?;
        return Ok((
            format!("boards/{board_short}/favicon/{favicon_rel}"),
            BackupFileKind::Favicon,
        ));
    }
    if let Some(banner_rel) = suffix.strip_prefix("_banner/") {
        sanitize_logical_path(banner_rel)?;
        return Ok((
            format!("boards/{board_short}/banner/{banner_rel}"),
            BackupFileKind::Banner,
        ));
    }
    if let Some(thumb_rel) = suffix.strip_prefix("thumbs/") {
        sanitize_logical_path(thumb_rel)?;
        return Ok((
            format!("boards/{board_short}/media/thumbs/{thumb_rel}"),
            BackupFileKind::Thumbnail,
        ));
    }
    sanitize_logical_path(suffix)?;
    Ok((
        format!("boards/{board_short}/media/src/{suffix}"),
        BackupFileKind::OriginalMedia,
    ))
}

pub(crate) fn logical_upload_path_to_runtime(
    logical_path: &str,
) -> Result<(String, BackupFileKind)> {
    sanitize_logical_path(logical_path)?;
    let parts = logical_path.split('/').collect::<Vec<_>>();
    if parts.len() < 4 || parts.first() != Some(&"boards") {
        return Err(AppError::BadRequest(format!(
            "Backup v4 logical media path '{logical_path}' is invalid."
        )));
    }
    let Some(board_short) = parts.get(1).copied() else {
        return Err(AppError::BadRequest(format!(
            "Backup v4 logical media path '{logical_path}' is invalid."
        )));
    };
    super::common::validate_board_short_name(board_short)?;

    match (parts.get(2).copied(), parts.get(3).copied()) {
        (Some("media"), Some("src")) => {
            let suffix = parts
                .get(4..)
                .map_or_else(String::new, |rest| rest.join("/"));
            sanitize_logical_path(&suffix)?;
            Ok((
                format!("{board_short}/{suffix}"),
                BackupFileKind::OriginalMedia,
            ))
        }
        (Some("media"), Some("thumbs")) => {
            let suffix = parts
                .get(4..)
                .map_or_else(String::new, |rest| rest.join("/"));
            sanitize_logical_path(&suffix)?;
            Ok((
                format!("{board_short}/thumbs/{suffix}"),
                BackupFileKind::Thumbnail,
            ))
        }
        (Some("favicon"), Some(file_name)) => {
            sanitize_logical_path(file_name)?;
            Ok((
                format!("{board_short}/_favicon/{file_name}"),
                BackupFileKind::Favicon,
            ))
        }
        (Some("banner"), Some(file_name)) => {
            sanitize_logical_path(file_name)?;
            Ok((
                format!("{board_short}/_banner/{file_name}"),
                BackupFileKind::Banner,
            ))
        }
        _ => Err(AppError::BadRequest(format!(
            "Backup v4 logical media path '{logical_path}' is invalid."
        ))),
    }
}

const fn scope_label(scope: BackupScope) -> &'static str {
    match scope {
        BackupScope::FullSite => "full_site",
        BackupScope::Board => "board",
        BackupScope::SelectedBoards => "selected_boards",
        BackupScope::PreMaintenance => "pre_maintenance",
    }
}

fn expected_scope_label(expected_scopes: &[BackupScope]) -> String {
    expected_scopes
        .iter()
        .map(|scope| scope_label(*scope))
        .collect::<Vec<_>>()
        .join(" or ")
}

fn resolve_saved_v4_file(root_dir: &Path, declared_path: &str, context: &str) -> Result<PathBuf> {
    sanitize_logical_path(declared_path)?;
    let candidate = root_dir.join(declared_path);
    if !candidate.exists() {
        return Err(AppError::BadRequest(format!(
            "{context} is missing declared file '{declared_path}'."
        )));
    }
    let resolved =
        crate::utils::fs_security::canonical_child_of(root_dir, &candidate).map_err(|error| {
            AppError::BadRequest(format!(
                "{context} path '{declared_path}' is unsafe: {error}"
            ))
        })?;
    crate::utils::fs_security::assert_regular_file_no_symlink(&resolved).map_err(|error| {
        AppError::BadRequest(format!(
            "{context} path '{declared_path}' is unsafe: {error}"
        ))
    })?;
    Ok(resolved)
}

fn validate_declared_file_metadata(
    logical_path: &str,
    kind: BackupFileKind,
    board: Option<&str>,
    runtime_logical_path: Option<&str>,
    size: u64,
    sha256: &str,
    source: VerifiedSavedV4FileSource,
) -> Result<VerifiedSavedV4File> {
    if let Some(board_short) = board {
        super::common::validate_board_short_name(board_short)?;
    }

    match kind {
        BackupFileKind::OriginalMedia
        | BackupFileKind::Thumbnail
        | BackupFileKind::Banner
        | BackupFileKind::Favicon
            if board.is_some() =>
        {
            let (runtime_path, runtime_kind) = logical_upload_path_to_runtime(logical_path)?;
            let board_short = board.ok_or_else(|| {
                AppError::BadRequest(format!(
                    "Backup v4 file '{logical_path}' is missing its board owner."
                ))
            })?;
            super::common::validate_restored_media_path_for_board(
                &runtime_path,
                board_short,
                "Backup v4 board media path",
            )?;
            if runtime_kind != kind {
                return Err(AppError::BadRequest(format!(
                    "Backup v4 file '{logical_path}' kind does not match its board media path."
                )));
            }
            if let Some(runtime_logical_path) = runtime_logical_path {
                if runtime_logical_path != runtime_path {
                    return Err(AppError::BadRequest(format!(
                        "Backup v4 file '{logical_path}' has mismatched runtime path metadata."
                    )));
                }
            }
        }
        BackupFileKind::Favicon if board.is_none() => {
            if !logical_path.starts_with("site-assets/favicon/") {
                return Err(AppError::BadRequest(format!(
                    "Backup v4 site favicon path '{logical_path}' is invalid."
                )));
            }
        }
        BackupFileKind::Banner if board.is_none() => {
            if !logical_path.starts_with("site-assets/banner/") {
                return Err(AppError::BadRequest(format!(
                    "Backup v4 site banner path '{logical_path}' is invalid."
                )));
            }
        }
        BackupFileKind::TorKey => {
            if kind != BackupFileKind::TorKey {
                return Err(AppError::BadRequest(format!(
                    "Backup v4 Tor key path '{logical_path}' has the wrong kind."
                )));
            }
            if board.is_some() {
                return Err(AppError::BadRequest(format!(
                    "Backup v4 Tor key path '{logical_path}' cannot belong to a board."
                )));
            }
            if !logical_path.starts_with("tor-keys/") {
                return Err(AppError::BadRequest(format!(
                    "Backup v4 Tor key path '{logical_path}' escapes the tor-keys/ scope."
                )));
            }
        }
        BackupFileKind::BoardJson => {
            let board_short = board.ok_or_else(|| {
                AppError::BadRequest(format!(
                    "Backup v4 board export '{logical_path}' is missing a board owner."
                ))
            })?;
            if logical_path != format!("boards/{board_short}/board.json") {
                return Err(AppError::BadRequest(format!(
                    "Backup v4 board manifest path '{logical_path}' is invalid."
                )));
            }
        }
        BackupFileKind::ThreadExport => {
            let board_short = board.ok_or_else(|| {
                AppError::BadRequest(format!(
                    "Backup v4 thread export '{logical_path}' is missing a board owner."
                ))
            })?;
            if logical_path != format!("boards/{board_short}/threads.jsonl") {
                return Err(AppError::BadRequest(format!(
                    "Backup v4 thread export path '{logical_path}' is invalid."
                )));
            }
        }
        BackupFileKind::PostExport => {
            let board_short = board.ok_or_else(|| {
                AppError::BadRequest(format!(
                    "Backup v4 post export '{logical_path}' is missing a board owner."
                ))
            })?;
            if logical_path != format!("boards/{board_short}/posts.jsonl") {
                return Err(AppError::BadRequest(format!(
                    "Backup v4 post export path '{logical_path}' is invalid."
                )));
            }
        }
        BackupFileKind::FileInventoryExport => {
            let board_short = board.ok_or_else(|| {
                AppError::BadRequest(format!(
                    "Backup v4 file inventory '{logical_path}' is missing a board owner."
                ))
            })?;
            if logical_path != format!("boards/{board_short}/files.jsonl") {
                return Err(AppError::BadRequest(format!(
                    "Backup v4 file inventory path '{logical_path}' is invalid."
                )));
            }
        }
        BackupFileKind::Settings => {
            if !logical_path.starts_with("config/") {
                return Err(AppError::BadRequest(format!(
                    "Backup v4 settings path '{logical_path}' is invalid."
                )));
            }
        }
        BackupFileKind::Maintenance | BackupFileKind::PendingFsOps => {
            if !logical_path.starts_with("maintenance/") {
                return Err(AppError::BadRequest(format!(
                    "Backup v4 maintenance path '{logical_path}' is invalid."
                )));
            }
        }
        BackupFileKind::Db => {
            if !logical_path.starts_with("db/") {
                return Err(AppError::BadRequest(format!(
                    "Backup v4 DB path '{logical_path}' is invalid."
                )));
            }
        }
        BackupFileKind::OriginalMedia
        | BackupFileKind::Thumbnail
        | BackupFileKind::Audio
        | BackupFileKind::Log
        | BackupFileKind::Banner
        | BackupFileKind::Favicon => {
            if board.is_none() {
                return Err(AppError::BadRequest(format!(
                    "Backup v4 file '{logical_path}' is missing required board metadata."
                )));
            }
        }
    }

    Ok(VerifiedSavedV4File {
        logical_path: logical_path.to_owned(),
        board: board.map(ToOwned::to_owned),
        kind,
        size,
        sha256: sha256.to_owned(),
        source,
    })
}

fn verify_root_declared_file(
    root_dir: &Path,
    logical_path: &str,
    kind: BackupFileKind,
    board: Option<&str>,
    runtime_logical_path: Option<&str>,
    expected_size: u64,
    expected_sha256: &str,
) -> Result<VerifiedSavedV4File> {
    let resolved_path = resolve_saved_v4_file(root_dir, logical_path, "Backup v4 file")?;
    let metadata = std::fs::metadata(&resolved_path).map_err(|error| {
        AppError::Internal(anyhow::anyhow!(
            "Inspect verified Backup v4 file {}: {error}",
            resolved_path.display()
        ))
    })?;
    if metadata.len() != expected_size {
        return Err(AppError::BadRequest(format!(
            "Backup v4 file '{logical_path}' size mismatch: manifest={expected_size}, actual={}.",
            metadata.len()
        )));
    }
    let actual_sha256 = sha256_hex_for_file(&resolved_path)?;
    if actual_sha256 != expected_sha256 {
        return Err(AppError::BadRequest(format!(
            "Backup v4 file '{logical_path}' checksum mismatch."
        )));
    }
    validate_declared_file_metadata(
        logical_path,
        kind,
        board,
        runtime_logical_path,
        expected_size,
        expected_sha256,
        VerifiedSavedV4FileSource::RootFile(resolved_path),
    )
}

pub(crate) fn copy_verified_file_to_writer<W: Write>(
    file: &VerifiedSavedV4File,
    writer: &mut W,
) -> Result<()> {
    match &file.source {
        VerifiedSavedV4FileSource::RootFile(path) => {
            let mut source = std::fs::File::open(path).map_err(|error| {
                AppError::Internal(anyhow::anyhow!("Open {}: {error}", path.display()))
            })?;
            std::io::copy(&mut source, writer).map_err(|error| {
                AppError::Internal(anyhow::anyhow!("Copy verified file: {error}"))
            })?;
        }
        VerifiedSavedV4FileSource::ZipEntry {
            part_path,
            entry_path,
        } => {
            let part_file = std::fs::File::open(part_path).map_err(|error| {
                AppError::Internal(anyhow::anyhow!("Open {}: {error}", part_path.display()))
            })?;
            let mut archive =
                zip::ZipArchive::new(std::io::BufReader::new(part_file)).map_err(|error| {
                    AppError::BadRequest(format!("Invalid split ZIP part: {error}"))
                })?;
            let mut entry = archive.by_name(entry_path).map_err(|error| {
                AppError::BadRequest(format!(
                    "Verified Backup v4 ZIP entry '{entry_path}' is no longer readable: {error}"
                ))
            })?;
            std::io::copy(&mut entry, writer).map_err(|error| {
                AppError::Internal(anyhow::anyhow!(
                    "Copy verified Backup v4 ZIP entry '{entry_path}': {error}"
                ))
            })?;
        }
    }
    Ok(())
}

pub(crate) fn read_verified_file(file: &VerifiedSavedV4File) -> Result<Vec<u8>> {
    let capacity = usize::try_from(file.size).unwrap_or(usize::MAX);
    let mut bytes = Vec::with_capacity(capacity.min(1024 * 1024));
    copy_verified_file_to_writer(file, &mut bytes)?;
    Ok(bytes)
}

const fn verified_file_is_root_stored(file: &VerifiedSavedV4File) -> bool {
    matches!(file.source, VerifiedSavedV4FileSource::RootFile(_))
}

fn validate_board_json_identity(
    board_json_file: &VerifiedSavedV4File,
    expected_short_name: &str,
    expected_name: &str,
) -> Result<()> {
    let bytes = read_verified_file(board_json_file)?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
        AppError::BadRequest(format!(
            "Invalid board.json for /{expected_short_name}/ in Backup v4: {error}"
        ))
    })?;
    let board = value
        .get("board")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            AppError::BadRequest(format!(
            "Invalid board.json for /{expected_short_name}/ in Backup v4: missing board object."
        ))
        })?;
    let actual_short_name = board
        .get("short_name")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "Invalid board.json for /{expected_short_name}/ in Backup v4: missing board.short_name."
            ))
        })?;
    super::common::validate_board_short_name(actual_short_name)?;
    if actual_short_name != expected_short_name {
        return Err(AppError::BadRequest(format!(
            "Backup v4 board.json identity mismatch: selected /{expected_short_name}/ contains /{actual_short_name}/."
        )));
    }
    if let Some(actual_name) = board.get("name").and_then(serde_json::Value::as_str) {
        if actual_name != expected_name {
            return Err(AppError::BadRequest(format!(
                "Backup v4 board.json identity mismatch for /{expected_short_name}/: board name does not match manifest metadata."
            )));
        }
    }
    Ok(())
}

fn collect_unexpected_files(
    root_dir: &Path,
    current: &Path,
    allowed_files: &HashSet<String>,
    unexpected: &mut Vec<String>,
) -> Result<()> {
    let entries = std::fs::read_dir(current).map_err(|error| {
        AppError::Internal(anyhow::anyhow!("Read {}: {error}", current.display()))
    })?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            AppError::Internal(anyhow::anyhow!(
                "Read directory entry {}: {error}",
                current.display()
            ))
        })?;
        let path = entry.path();
        let metadata = std::fs::symlink_metadata(&path).map_err(|error| {
            AppError::Internal(anyhow::anyhow!("Inspect {}: {error}", path.display()))
        })?;
        if metadata.file_type().is_symlink() {
            return Err(AppError::BadRequest(format!(
                "Saved Backup v4 directory {} contains a symlink at {}.",
                root_dir.display(),
                path.display()
            )));
        }
        if metadata.file_type().is_dir() {
            collect_unexpected_files(root_dir, &path, allowed_files, unexpected)?;
            continue;
        }
        if !metadata.file_type().is_file() {
            return Err(AppError::BadRequest(format!(
                "Saved Backup v4 directory {} contains a non-regular file at {}.",
                root_dir.display(),
                path.display()
            )));
        }
        let relative = path.strip_prefix(root_dir).map_err(|error| {
            AppError::Internal(anyhow::anyhow!(
                "Resolve {} relative to {}: {error}",
                path.display(),
                root_dir.display()
            ))
        })?;
        let relative = relative.to_string_lossy().replace('\\', "/");
        if !allowed_files.contains(&relative) {
            unexpected.push(relative);
        }
    }
    Ok(())
}

fn verify_split_part_path(root_dir: &Path, filename: &str) -> Result<PathBuf> {
    sanitize_logical_path(filename)?;
    let parts = filename.split('/').collect::<Vec<_>>();
    if parts.len() != 2 || parts.first() != Some(&PARTS_DIR_NAME) {
        return Err(AppError::BadRequest(format!(
            "Backup v4 split part path '{filename}' must be an immediate file under parts/."
        )));
    }
    parse_split_part_index(filename)?;
    resolve_saved_v4_file(root_dir, filename, "Backup v4 split ZIP part")
}

fn parse_split_part_index(filename: &str) -> Result<u32> {
    let Some(name) = filename.strip_prefix("parts/") else {
        return Err(AppError::BadRequest(format!(
            "Backup v4 split part path '{filename}' must stay under parts/."
        )));
    };
    let Some(digits) = name
        .strip_prefix("part-")
        .and_then(|rest| rest.strip_suffix(".zip"))
    else {
        return Err(AppError::BadRequest(format!(
            "Backup v4 split part path '{filename}' must match parts/part-0001.zip."
        )));
    };
    if digits.len() < 4 || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(AppError::BadRequest(format!(
            "Backup v4 split part path '{filename}' must match parts/part-0001.zip."
        )));
    }
    let index = digits.parse::<u32>().map_err(|_error| {
        AppError::BadRequest(format!(
            "Backup v4 split part path '{filename}' has an invalid part index."
        ))
    })?;
    if index == 0 {
        return Err(AppError::BadRequest(format!(
            "Backup v4 split part path '{filename}' uses invalid part index 0."
        )));
    }
    let expected = format!("parts/part-{index:04}.zip");
    if filename != expected {
        return Err(AppError::BadRequest(format!(
            "Backup v4 split part path '{filename}' does not match declared index {index}."
        )));
    }
    Ok(index)
}

fn verify_zip_entry_name(entry_name: &str) -> Result<()> {
    sanitize_logical_path(entry_name)?;
    if entry_name.starts_with("parts/")
        || entry_name == MANIFEST_FILE_NAME
        || entry_name == BACKUP_METADATA_FILE_NAME
        || entry_name == CHECKSUMS_FILE_NAME
        || entry_name == README_FILE_NAME
    {
        return Err(AppError::BadRequest(format!(
            "Backup v4 split ZIP contains reserved entry '{entry_name}'."
        )));
    }
    Ok(())
}

fn verify_split_zip_files(
    root_dir: &Path,
    manifest: &BackupManifest,
    layout_ref: &str,
) -> Result<(HashMap<String, VerifiedSavedV4File>, HashSet<String>)> {
    let mut allowed_files = HashSet::new();
    let mut part_indexes = HashSet::new();
    let mut part_filenames = HashSet::new();
    let mut part_paths = HashMap::new();
    let expected_total = u32::try_from(manifest.parts.len()).unwrap_or(u32::MAX);
    if manifest.parts.is_empty() {
        return Err(AppError::BadRequest(format!(
            "Saved backup {layout_ref} is split ZIP but has no parts."
        )));
    }
    for part in &manifest.parts {
        if !part_filenames.insert(part.filename.clone()) {
            return Err(AppError::BadRequest(format!(
                "Saved backup {layout_ref} contains duplicate split ZIP part filename '{}'.",
                part.filename
            )));
        }
        if !part_indexes.insert(part.part_index) {
            return Err(AppError::BadRequest(format!(
                "Saved backup {layout_ref} contains duplicate split ZIP part index {}.",
                part.part_index
            )));
        }
        if part.part_index == 0 || part.part_index > expected_total {
            return Err(AppError::BadRequest(format!(
                "Saved backup {layout_ref} has split ZIP part index {} outside 1..={expected_total}.",
                part.part_index
            )));
        }
        if part.total_parts != expected_total {
            return Err(AppError::BadRequest(format!(
                "Saved backup {layout_ref} has inconsistent split ZIP total_parts metadata."
            )));
        }
        let filename_index = parse_split_part_index(&part.filename)?;
        if filename_index != part.part_index {
            return Err(AppError::BadRequest(format!(
                "Saved backup {layout_ref} split ZIP filename '{}' does not match part index {}.",
                part.filename, part.part_index
            )));
        }
        let part_path = verify_split_part_path(root_dir, &part.filename)?;
        let metadata = std::fs::metadata(&part_path).map_err(|error| {
            AppError::Internal(anyhow::anyhow!(
                "Inspect split ZIP part {}: {error}",
                part_path.display()
            ))
        })?;
        if metadata.len() != part.size {
            return Err(AppError::BadRequest(format!(
                "Backup v4 split part '{}' size mismatch.",
                part.filename
            )));
        }
        let actual_sha256 = sha256_hex_for_file(&part_path)?;
        if actual_sha256 != part.sha256 {
            return Err(AppError::BadRequest(format!(
                "Backup v4 split part '{}' checksum mismatch.",
                part.filename
            )));
        }
        allowed_files.insert(part.filename.clone());
        part_paths.insert(part.filename.clone(), part_path);
    }
    for expected_index in 1..=expected_total {
        if !part_indexes.contains(&expected_index) {
            return Err(AppError::BadRequest(format!(
                "Saved backup {layout_ref} is missing split ZIP part index {expected_index}."
            )));
        }
    }

    let mut declared_by_part: HashMap<String, HashMap<String, &BackupFileEntry>> = HashMap::new();
    let mut root_entries = Vec::new();
    let mut declared_logical_paths = HashSet::new();
    for entry in &manifest.files {
        if !declared_logical_paths.insert(entry.logical_path.clone()) {
            return Err(AppError::BadRequest(format!(
                "Saved backup {layout_ref} contains duplicate logical path '{}'.",
                entry.logical_path
            )));
        }
        match entry.zip_part.as_deref() {
            Some(part_filename) => {
                let entry_path = entry
                    .zip_entry_path
                    .as_deref()
                    .unwrap_or(&entry.logical_path);
                verify_zip_entry_name(entry_path)?;
                if !part_paths.contains_key(part_filename) {
                    return Err(AppError::BadRequest(format!(
                        "Backup v4 file '{}' references unknown split ZIP part '{}'.",
                        entry.logical_path, part_filename
                    )));
                }
                let part_entries = declared_by_part
                    .entry(part_filename.to_owned())
                    .or_default();
                if part_entries.insert(entry_path.to_owned(), entry).is_some() {
                    return Err(AppError::BadRequest(format!(
                        "Saved backup {layout_ref} contains duplicate ZIP entry path '{entry_path}'."
                    )));
                }
            }
            None => root_entries.push(entry),
        }
    }

    let mut verified_files = HashMap::new();
    for entry in root_entries {
        let verified = verify_root_declared_file(
            root_dir,
            &entry.logical_path,
            entry.kind,
            entry.board.as_deref(),
            entry.runtime_logical_path.as_deref(),
            entry.size,
            &entry.sha256,
        )?;
        verified_files.insert(entry.logical_path.clone(), verified);
    }

    let mut sorted_part_paths: Vec<_> = part_paths.iter().collect();
    sorted_part_paths.sort_by_key(|(part_filename, _)| *part_filename);
    for (part_filename, part_path) in sorted_part_paths {
        let Some(expected_entries) = declared_by_part.get(part_filename) else {
            return Err(AppError::BadRequest(format!(
                "Backup v4 split part '{part_filename}' has no declared file entries."
            )));
        };
        let part_file = std::fs::File::open(part_path).map_err(|error| {
            AppError::Internal(anyhow::anyhow!("Open {}: {error}", part_path.display()))
        })?;
        let mut archive = zip::ZipArchive::new(std::io::BufReader::new(part_file))
            .map_err(|error| AppError::BadRequest(format!("Invalid split ZIP part: {error}")))?;
        let mut seen_entries = HashSet::new();
        for index in 0..archive.len() {
            let mut zip_entry = archive.by_index(index).map_err(|error| {
                AppError::Internal(anyhow::anyhow!("Read split ZIP entry {index}: {error}"))
            })?;
            let entry_name = zip_entry.name().to_owned();
            verify_zip_entry_name(&entry_name)?;
            if !seen_entries.insert(entry_name.clone()) {
                return Err(AppError::BadRequest(format!(
                    "Backup v4 split part '{part_filename}' contains duplicate ZIP entry '{entry_name}'."
                )));
            }
            let Some(manifest_entry) = expected_entries.get(&entry_name) else {
                return Err(AppError::BadRequest(format!(
                    "Backup v4 split part '{part_filename}' contains undeclared ZIP entry '{entry_name}'."
                )));
            };
            let actual_size = zip_entry.size();
            if actual_size != manifest_entry.size {
                return Err(AppError::BadRequest(format!(
                    "Backup v4 ZIP entry '{}' size mismatch.",
                    manifest_entry.logical_path
                )));
            }
            let actual_sha256 = sha256_hex_for_reader(&mut zip_entry)?;
            if actual_sha256 != manifest_entry.sha256 {
                return Err(AppError::BadRequest(format!(
                    "Backup v4 ZIP entry '{}' checksum mismatch.",
                    manifest_entry.logical_path
                )));
            }
            let verified = validate_declared_file_metadata(
                &manifest_entry.logical_path,
                manifest_entry.kind,
                manifest_entry.board.as_deref(),
                manifest_entry.runtime_logical_path.as_deref(),
                manifest_entry.size,
                &manifest_entry.sha256,
                VerifiedSavedV4FileSource::ZipEntry {
                    part_path: part_path.clone(),
                    entry_path: entry_name,
                },
            )?;
            verified_files.insert(manifest_entry.logical_path.clone(), verified);
        }
        let mut expected_entry_names: Vec<_> = expected_entries.keys().collect();
        expected_entry_names.sort();
        for entry_name in expected_entry_names {
            if !seen_entries.contains(entry_name) {
                return Err(AppError::BadRequest(format!(
                    "Backup v4 split part '{part_filename}' is missing declared ZIP entry '{entry_name}'."
                )));
            }
        }
    }

    Ok((verified_files, allowed_files))
}

pub(crate) fn verify_saved_v4_root(
    root_dir: &Path,
    expected_scopes: &[BackupScope],
) -> Result<VerifiedSavedV4Root> {
    crate::utils::fs_security::assert_dir_no_symlink(root_dir).map_err(|error| {
        AppError::BadRequest(format!(
            "Saved Backup v4 root {} is unsafe: {error}",
            root_dir.display()
        ))
    })?;

    let layout = detect_saved_backup_layout(root_dir).ok_or_else(|| {
        AppError::BadRequest("Saved Backup v4 folder metadata is missing.".into())
    })?;
    let metadata = load_metadata(&layout.metadata_path)?;
    let manifest = load_manifest(&layout.manifest_path)?;

    if metadata.format != BACKUP_V4_FORMAT || manifest.format != BACKUP_V4_FORMAT {
        return Err(AppError::BadRequest(
            "Saved backup does not use RustChan Backup v4 metadata.".into(),
        ));
    }
    if metadata.backup_id != manifest.backup_id {
        return Err(AppError::BadRequest(format!(
            "Saved backup {} has mismatched backup_id metadata.",
            layout.backup_ref
        )));
    }
    if layout.backup_ref != metadata.backup_id {
        return Err(AppError::BadRequest(format!(
            "Saved backup root '{}' does not match backup_id '{}'.",
            layout.backup_ref, metadata.backup_id
        )));
    }
    if metadata.scope != manifest.scope {
        return Err(AppError::BadRequest(format!(
            "Saved backup {} has mismatched scope metadata.",
            layout.backup_ref
        )));
    }
    if !expected_scopes.contains(&manifest.scope) {
        return Err(AppError::BadRequest(format!(
            "Saved backup {} has scope {}, expected {}.",
            layout.backup_ref,
            scope_label(manifest.scope),
            expected_scope_label(expected_scopes)
        )));
    }
    if metadata.storage_mode != manifest.storage_mode {
        return Err(AppError::BadRequest(format!(
            "Saved backup {} has mismatched storage_mode metadata.",
            layout.backup_ref
        )));
    }
    if !matches!(
        manifest.storage_mode,
        BackupStorageMode::Directory | BackupStorageMode::SplitZip
    ) {
        return Err(AppError::BadRequest(format!(
            "Saved backup {} uses unsupported saved-v4 storage mode '{}'.",
            layout.backup_ref,
            manifest.storage_mode.display_name()
        )));
    }
    if manifest.archive_container != BACKUP_V4_ARCHIVE_CONTAINER {
        return Err(AppError::BadRequest(format!(
            "Saved backup {} uses unsupported archive container '{}'.",
            layout.backup_ref, manifest.archive_container
        )));
    }
    if !metadata.verified {
        return Err(AppError::BadRequest(format!(
            "Saved backup {} is not marked complete.",
            layout.backup_ref
        )));
    }
    let completed_at = metadata
        .completed_at
        .zip(manifest.completed_at)
        .ok_or_else(|| {
            AppError::BadRequest(format!(
                "Saved backup {} is missing completed_at metadata.",
                layout.backup_ref
            ))
        })?;
    if completed_at.0 != completed_at.1 {
        return Err(AppError::BadRequest(format!(
            "Saved backup {} has mismatched completed_at metadata.",
            layout.backup_ref
        )));
    }
    if completed_at.0 < metadata.created_at || completed_at.1 < manifest.created_at {
        return Err(AppError::BadRequest(format!(
            "Saved backup {} has invalid completion timestamps.",
            layout.backup_ref
        )));
    }
    if metadata.part_count != u32::try_from(manifest.parts.len()).unwrap_or(u32::MAX) {
        return Err(AppError::BadRequest(format!(
            "Saved backup {} has mismatched part count metadata.",
            layout.backup_ref
        )));
    }
    if metadata.includes_tor_keys != manifest.includes.tor_keys {
        return Err(AppError::BadRequest(format!(
            "Saved backup {} has mismatched Tor key metadata.",
            layout.backup_ref
        )));
    }
    if metadata.included_boards != manifest.included_boards {
        return Err(AppError::BadRequest(format!(
            "Saved backup {} has mismatched included board metadata.",
            layout.backup_ref
        )));
    }
    validate_backup_id_matches_parts(&manifest)?;
    if manifest.storage_mode == BackupStorageMode::Directory && !manifest.parts.is_empty() {
        return Err(AppError::BadRequest(format!(
            "Saved backup {} is directory mode but contains split ZIP metadata.",
            layout.backup_ref
        )));
    }
    if manifest.storage_mode == BackupStorageMode::SplitZip && manifest.parts.is_empty() {
        return Err(AppError::BadRequest(format!(
            "Saved backup {} is split ZIP mode but contains no parts.",
            layout.backup_ref
        )));
    }

    let (verified_files, split_part_allowed_files) =
        if manifest.storage_mode == BackupStorageMode::SplitZip {
            verify_split_zip_files(root_dir, &manifest, &layout.backup_ref)?
        } else {
            let mut declared_logical_paths = HashSet::new();
            let mut verified_files = HashMap::new();
            for entry in &manifest.files {
                if !declared_logical_paths.insert(entry.logical_path.clone()) {
                    return Err(AppError::BadRequest(format!(
                        "Saved backup {} contains duplicate logical path '{}'.",
                        layout.backup_ref, entry.logical_path
                    )));
                }
                if entry.zip_part.is_some() || entry.zip_entry_path.is_some() {
                    return Err(AppError::BadRequest(format!(
                        "Saved backup {} directory-mode file '{}' contains split ZIP metadata.",
                        layout.backup_ref, entry.logical_path
                    )));
                }
                let verified = verify_root_declared_file(
                    root_dir,
                    &entry.logical_path,
                    entry.kind,
                    entry.board.as_deref(),
                    entry.runtime_logical_path.as_deref(),
                    entry.size,
                    &entry.sha256,
                )?;
                verified_files.insert(entry.logical_path.clone(), verified);
            }
            (verified_files, HashSet::new())
        };

    if verified_files.len() != manifest.files.len() {
        return Err(AppError::BadRequest(format!(
            "Saved backup {} verified file inventory is incomplete.",
            layout.backup_ref
        )));
    }

    if manifest.storage_mode == BackupStorageMode::SplitZip {
        for entry in &manifest.files {
            if !verified_files.contains_key(&entry.logical_path) {
                return Err(AppError::BadRequest(format!(
                    "Saved backup {} did not verify declared file '{}'.",
                    layout.backup_ref, entry.logical_path
                )));
            }
        }
    }

    let db_snapshot = match &manifest.db_snapshot {
        Some(snapshot) => {
            let db_file = if let Some(existing) = verified_files.get(&snapshot.path) {
                if existing.kind != BackupFileKind::Db {
                    return Err(AppError::BadRequest(format!(
                        "Backup v4 DB snapshot path '{}' does not point to a DB entry.",
                        snapshot.path
                    )));
                }
                if existing.size != snapshot.size || existing.sha256 != snapshot.sha256 {
                    return Err(AppError::BadRequest(format!(
                        "Backup v4 DB snapshot path '{}' disagrees with the manifest file entry.",
                        snapshot.path
                    )));
                }
                existing.clone()
            } else {
                verify_root_declared_file(
                    root_dir,
                    &snapshot.path,
                    BackupFileKind::Db,
                    None,
                    None,
                    snapshot.size,
                    &snapshot.sha256,
                )?
            };
            Some(VerifiedSavedV4DbSnapshot { file: db_file })
        }
        None => None,
    };

    match manifest.scope {
        BackupScope::FullSite | BackupScope::PreMaintenance => {
            if !manifest.includes.database || db_snapshot.is_none() {
                return Err(AppError::BadRequest(format!(
                    "Saved backup {} is missing its required DB snapshot.",
                    layout.backup_ref
                )));
            }
        }
        BackupScope::Board | BackupScope::SelectedBoards => {
            if manifest.includes.database || db_snapshot.is_some() {
                return Err(AppError::BadRequest(format!(
                    "Saved backup {} board-scoped backup must not include a DB snapshot.",
                    layout.backup_ref
                )));
            }
        }
    }

    let mut allowed_files = HashSet::from([
        README_FILE_NAME.to_owned(),
        BACKUP_METADATA_FILE_NAME.to_owned(),
        MANIFEST_FILE_NAME.to_owned(),
        CHECKSUMS_FILE_NAME.to_owned(),
    ]);
    allowed_files.extend(split_part_allowed_files);
    if let Some(snapshot) = &db_snapshot {
        if verified_file_is_root_stored(&snapshot.file) {
            allowed_files.insert(snapshot.file.logical_path.clone());
        }
    }

    let included_boards = manifest
        .included_boards
        .iter()
        .map(|board| (board.short_name.clone(), board.clone()))
        .collect::<HashMap<_, _>>();
    let mut board_json = HashMap::new();
    let mut threads_jsonl = HashMap::new();
    let mut posts_jsonl = HashMap::new();
    let mut files_jsonl = HashMap::new();
    let mut board_uploads: HashMap<String, Vec<VerifiedSavedV4File>> = HashMap::new();
    let mut site_favicon_files = Vec::new();
    let mut site_banner_files = Vec::new();
    let mut tor_key_files = Vec::new();

    let mut sorted_verified_files: Vec<_> = verified_files.values().collect();
    sorted_verified_files.sort_by(|left, right| left.logical_path.cmp(&right.logical_path));
    for verified in sorted_verified_files {
        if verified_file_is_root_stored(verified) {
            allowed_files.insert(verified.logical_path.clone());
        }
        if let Some(board_short) = verified.board.as_deref() {
            if !included_boards.contains_key(board_short) {
                return Err(AppError::BadRequest(format!(
                    "Backup v4 file '{}' references unknown board /{board_short}/.",
                    verified.logical_path
                )));
            }
        }

        match verified.kind {
            BackupFileKind::Db
            | BackupFileKind::Settings
            | BackupFileKind::Maintenance
            | BackupFileKind::PendingFsOps => {}
            BackupFileKind::Favicon if verified.board.is_none() => {
                site_favicon_files.push(verified.clone());
            }
            BackupFileKind::Banner if verified.board.is_none() => {
                site_banner_files.push(verified.clone());
            }
            BackupFileKind::TorKey => tor_key_files.push(verified.clone()),
            BackupFileKind::BoardJson => {
                let board_short = verified.board.clone().ok_or_else(|| {
                    AppError::BadRequest(format!(
                        "Backup v4 file '{}' is missing board metadata.",
                        verified.logical_path
                    ))
                })?;
                if board_json.insert(board_short, verified.clone()).is_some() {
                    return Err(AppError::BadRequest(format!(
                        "Saved backup {} contains duplicate board.json exports.",
                        layout.backup_ref
                    )));
                }
            }
            BackupFileKind::ThreadExport => {
                let board_short = verified.board.clone().ok_or_else(|| {
                    AppError::BadRequest(format!(
                        "Backup v4 file '{}' is missing board metadata.",
                        verified.logical_path
                    ))
                })?;
                if threads_jsonl
                    .insert(board_short, verified.clone())
                    .is_some()
                {
                    return Err(AppError::BadRequest(format!(
                        "Saved backup {} contains duplicate threads.jsonl exports.",
                        layout.backup_ref
                    )));
                }
            }
            BackupFileKind::PostExport => {
                let board_short = verified.board.clone().ok_or_else(|| {
                    AppError::BadRequest(format!(
                        "Backup v4 file '{}' is missing board metadata.",
                        verified.logical_path
                    ))
                })?;
                if posts_jsonl.insert(board_short, verified.clone()).is_some() {
                    return Err(AppError::BadRequest(format!(
                        "Saved backup {} contains duplicate posts.jsonl exports.",
                        layout.backup_ref
                    )));
                }
            }
            BackupFileKind::FileInventoryExport => {
                let board_short = verified.board.clone().ok_or_else(|| {
                    AppError::BadRequest(format!(
                        "Backup v4 file '{}' is missing board metadata.",
                        verified.logical_path
                    ))
                })?;
                if files_jsonl.insert(board_short, verified.clone()).is_some() {
                    return Err(AppError::BadRequest(format!(
                        "Saved backup {} contains duplicate files.jsonl exports.",
                        layout.backup_ref
                    )));
                }
            }
            BackupFileKind::OriginalMedia
            | BackupFileKind::Thumbnail
            | BackupFileKind::Banner
            | BackupFileKind::Favicon => {
                let board_short = verified.board.clone().ok_or_else(|| {
                    AppError::BadRequest(format!(
                        "Backup v4 board asset '{}' is missing board metadata.",
                        verified.logical_path
                    ))
                })?;
                board_uploads
                    .entry(board_short)
                    .or_default()
                    .push(verified.clone());
            }
            BackupFileKind::Audio | BackupFileKind::Log => {
                return Err(AppError::BadRequest(format!(
                    "Backup v4 file '{}' uses unsupported kind {:?} in saved-v4 restore.",
                    verified.logical_path, verified.kind
                )));
            }
        }
    }

    let mut boards = HashMap::new();
    let mut sorted_included_boards: Vec<_> = included_boards.iter().collect();
    sorted_included_boards.sort_by_key(|(board_short, _)| *board_short);
    for (board_short, board_summary) in sorted_included_boards {
        let board_json_file = board_json.remove(board_short).ok_or_else(|| {
            AppError::BadRequest(format!(
                "Saved backup {} is missing board.json for /{board_short}/.",
                layout.backup_ref
            ))
        })?;
        let threads_jsonl_file = threads_jsonl.remove(board_short).ok_or_else(|| {
            AppError::BadRequest(format!(
                "Saved backup {} is missing threads.jsonl for /{board_short}/.",
                layout.backup_ref
            ))
        })?;
        let posts_jsonl_file = posts_jsonl.remove(board_short).ok_or_else(|| {
            AppError::BadRequest(format!(
                "Saved backup {} is missing posts.jsonl for /{board_short}/.",
                layout.backup_ref
            ))
        })?;
        let files_jsonl_file = files_jsonl.remove(board_short).ok_or_else(|| {
            AppError::BadRequest(format!(
                "Saved backup {} is missing files.jsonl for /{board_short}/.",
                layout.backup_ref
            ))
        })?;
        validate_board_json_identity(&board_json_file, board_short, &board_summary.name)?;
        boards.insert(
            board_short.clone(),
            VerifiedSavedV4BoardLayout {
                board_json: board_json_file,
                upload_files: board_uploads.remove(board_short).unwrap_or_default(),
            },
        );
        let _ = (
            threads_jsonl_file,
            posts_jsonl_file,
            files_jsonl_file,
            board_summary,
        );
    }

    if !board_json.is_empty()
        || !threads_jsonl.is_empty()
        || !posts_jsonl.is_empty()
        || !files_jsonl.is_empty()
        || !board_uploads.is_empty()
    {
        return Err(AppError::BadRequest(format!(
            "Saved backup {} contains board-scoped files for boards not declared in included_boards.",
            layout.backup_ref
        )));
    }

    if manifest.scope == BackupScope::Board && boards.len() != 1 {
        return Err(AppError::BadRequest(format!(
            "Saved backup {} board backup must contain exactly one board.",
            layout.backup_ref
        )));
    }
    if manifest.scope == BackupScope::SelectedBoards && boards.is_empty() {
        return Err(AppError::BadRequest(format!(
            "Saved backup {} selected-boards backup must contain at least one board.",
            layout.backup_ref
        )));
    }
    if manifest.scope == BackupScope::PreMaintenance
        && (!boards.is_empty() || manifest.includes.board_exports)
    {
        return Err(AppError::BadRequest(format!(
            "Saved backup {} pre-maintenance backup must not contain board exports.",
            layout.backup_ref
        )));
    }

    let mut unexpected_files = Vec::new();
    collect_unexpected_files(root_dir, root_dir, &allowed_files, &mut unexpected_files)?;
    if !unexpected_files.is_empty() {
        unexpected_files.sort();
        return Err(AppError::BadRequest(format!(
            "Saved backup {} contains unexpected files: {}.",
            layout.backup_ref,
            unexpected_files.join(", ")
        )));
    }

    Ok(VerifiedSavedV4Root {
        metadata,
        manifest,
        completed_at: completed_at.0,
        db_snapshot,
        site_favicon_files,
        site_banner_files,
        tor_key_files,
        boards,
    })
}

#[cfg(test)]
pub(crate) fn write_saved_v4_fixture_for_test(
    root_dir: &Path,
    scope: BackupScope,
    files: Vec<(BackupFileEntry, Vec<u8>)>,
    db_snapshot: Option<Vec<u8>>,
    completed_at: i64,
) -> (BackupMetadata, BackupManifest) {
    fn write_file(path: &Path, contents: &[u8]) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).expect("create parent");
        }
        std::fs::write(path, contents).expect("write file");
    }

    std::fs::create_dir_all(root_dir).expect("root");
    let backup_id = root_dir
        .file_name()
        .and_then(|name| name.to_str())
        .expect("backup id")
        .to_owned();
    let created_at = completed_at - 60;
    let mut manifest_files = Vec::new();
    if let Some(db_bytes) = db_snapshot.as_ref() {
        let db_path = "db/rustchan.sqlite3";
        write_file(&root_dir.join(db_path), db_bytes);
        manifest_files.push(test_file_entry_for_test(
            db_path,
            None,
            BackupFileKind::Db,
            db_bytes,
        ));
    }
    for (entry, bytes) in files {
        write_file(&root_dir.join(&entry.logical_path), &bytes);
        manifest_files.push(entry);
    }
    let included_boards = match scope {
        BackupScope::Board | BackupScope::SelectedBoards | BackupScope::FullSite => {
            vec![crate::models::BackupBoardSummary {
                short_name: "tech".to_owned(),
                name: "Technology".to_owned(),
            }]
        }
        BackupScope::PreMaintenance => Vec::new(),
    };
    let manifest = BackupManifest {
        format: BACKUP_V4_FORMAT.to_owned(),
        archive_container: BACKUP_V4_ARCHIVE_CONTAINER.to_owned(),
        backup_id: backup_id.clone(),
        created_at,
        completed_at: Some(completed_at),
        rustchan_version: "test".to_owned(),
        scope,
        storage_mode: BackupStorageMode::Directory,
        included_boards: included_boards.clone(),
        includes: BackupIncludeFlags {
            database: db_snapshot.is_some(),
            settings: false,
            uploads: true,
            thumbnails: true,
            tor_keys: false,
            board_exports: scope != BackupScope::PreMaintenance,
            file_inventory: scope != BackupScope::PreMaintenance,
        },
        db_snapshot: db_snapshot.as_ref().map(|bytes| DbSnapshotInfo {
            path: "db/rustchan.sqlite3".to_owned(),
            size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
            sha256: sha256_hex_for_bytes(bytes),
            integrity_check: None,
            foreign_key_check: None,
        }),
        files: manifest_files.clone(),
        parts: Vec::new(),
        maintenance: None,
    };
    let metadata = BackupMetadata {
        format: BACKUP_V4_FORMAT.to_owned(),
        backup_id,
        scope,
        storage_mode: BackupStorageMode::Directory,
        created_at,
        completed_at: Some(completed_at),
        total_size_bytes: 0,
        verified: true,
        part_count: 0,
        includes_tor_keys: false,
        included_boards,
        manifest_path: Some(root_dir.join(MANIFEST_FILE_NAME).display().to_string()),
    };
    write_json_pretty(&root_dir.join(MANIFEST_FILE_NAME), &manifest).expect("manifest");
    write_json_pretty(&root_dir.join(BACKUP_METADATA_FILE_NAME), &metadata).expect("metadata");
    write_text(&root_dir.join(README_FILE_NAME), "test").expect("readme");
    write_text(&root_dir.join(CHECKSUMS_FILE_NAME), "").expect("checksums");
    (metadata, manifest)
}

#[cfg(test)]
pub(crate) fn test_file_entry_for_test(
    logical_path: &str,
    board: Option<&str>,
    kind: BackupFileKind,
    bytes: &[u8],
) -> BackupFileEntry {
    BackupFileEntry {
        logical_path: logical_path.to_owned(),
        runtime_logical_path: None,
        board: board.map(ToOwned::to_owned),
        kind,
        size: u64::try_from(bytes.len()).unwrap_or(u64::MAX),
        sha256: sha256_hex_for_bytes(bytes),
        zip_part: None,
        zip_entry_path: None,
        compression_method: None,
    }
}

#[cfg(test)]
pub(crate) fn board_fixture_files_for_test() -> Vec<(BackupFileEntry, Vec<u8>)> {
    let board_json = br#"{"version":1,"board":{"id":1,"short_name":"tech","name":"Technology","description":"","nsfw":false,"max_threads":100,"max_archived_threads":150,"bump_limit":300,"allow_images":true,"allow_video":true,"allow_audio":false,"allow_any_files":false,"allow_tripcodes":true,"edit_window_secs":300,"allow_editing":false,"allow_self_delete":false,"allow_archive":true,"allow_video_embeds":false,"allow_captcha":false,"show_poster_ids":false,"collapse_greentext":false,"post_cooldown_secs":0,"banner_mode":"inherit","access_mode":"public","access_password_hash":"","created_at":1},"threads":[],"posts":[],"polls":[],"poll_options":[],"poll_votes":[],"file_hashes":[],"banners":[]}"#;
    vec![
        (
            test_file_entry_for_test(
                "boards/tech/board.json",
                Some("tech"),
                BackupFileKind::BoardJson,
                board_json,
            ),
            board_json.to_vec(),
        ),
        (
            test_file_entry_for_test(
                "boards/tech/threads.jsonl",
                Some("tech"),
                BackupFileKind::ThreadExport,
                b"{}\n",
            ),
            b"{}\n".to_vec(),
        ),
        (
            test_file_entry_for_test(
                "boards/tech/posts.jsonl",
                Some("tech"),
                BackupFileKind::PostExport,
                b"{}\n",
            ),
            b"{}\n".to_vec(),
        ),
        (
            test_file_entry_for_test(
                "boards/tech/files.jsonl",
                Some("tech"),
                BackupFileKind::FileInventoryExport,
                b"{}\n",
            ),
            b"{}\n".to_vec(),
        ),
        (
            test_file_entry_for_test(
                "boards/tech/media/src/example.txt",
                Some("tech"),
                BackupFileKind::OriginalMedia,
                b"media",
            ),
            b"media".to_vec(),
        ),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn saved_full_fixture(label: &str) -> (tempfile::TempDir, PathBuf, BackupManifest) {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join(label);
        let (_metadata, manifest) = write_saved_v4_fixture_for_test(
            &root,
            BackupScope::FullSite,
            board_fixture_files_for_test(),
            Some(b"sqlite".to_vec()),
            1_715_000_000_i64,
        );
        (dir, root, manifest)
    }

    fn rewrite_manifest(root: &Path, manifest: &BackupManifest) {
        write_json_pretty(&root.join(MANIFEST_FILE_NAME), manifest).expect("rewrite manifest");
    }

    fn rewrite_metadata_part_count(root: &Path, part_count: u32) {
        let mut metadata = load_metadata(&root.join(BACKUP_METADATA_FILE_NAME)).expect("metadata");
        metadata.part_count = part_count;
        write_json_pretty(&root.join(BACKUP_METADATA_FILE_NAME), &metadata)
            .expect("rewrite metadata");
    }

    fn media_entry_mut(manifest: &mut BackupManifest) -> &mut BackupFileEntry {
        manifest
            .files
            .iter_mut()
            .find(|entry| entry.logical_path == "boards/tech/media/src/example.txt")
            .expect("media entry")
    }

    fn convert_fixture_to_split(root: &Path, target_part_size: u64) -> BackupManifest {
        let mut manifest = load_manifest(&root.join(MANIFEST_FILE_NAME)).expect("manifest");
        let mut entries = manifest
            .files
            .iter()
            .enumerate()
            .collect::<Vec<(usize, &BackupFileEntry)>>();
        entries.sort_by(|left, right| left.1.logical_path.cmp(&right.1.logical_path));
        let parts_dir = root.join(PARTS_DIR_NAME);
        std::fs::create_dir_all(&parts_dir).expect("parts dir");
        let mut part_infos = Vec::new();
        let mut current_files = Vec::new();
        let mut current_bytes = 0u64;
        let mut groups: Vec<(Vec<usize>, bool)> = Vec::new();
        for (index, entry) in entries {
            if current_files.is_empty() && entry.size > target_part_size {
                groups.push((vec![index], true));
                continue;
            }
            if !current_files.is_empty()
                && current_bytes.saturating_add(entry.size) > target_part_size
            {
                groups.push((std::mem::take(&mut current_files), false));
                current_bytes = 0;
            }
            current_bytes = current_bytes.saturating_add(entry.size);
            current_files.push(index);
        }
        if !current_files.is_empty() {
            groups.push((current_files, false));
        }

        let total_parts = u32::try_from(groups.len()).expect("part count");
        for (offset, (file_indexes, oversized)) in groups.iter().enumerate() {
            let part_index = u32::try_from(offset + 1).expect("part index");
            let part_filename = format!("parts/part-{part_index:04}.zip");
            let part_path = root.join(&part_filename);
            let file = std::fs::File::create(&part_path).expect("part file");
            let mut zip = zip::ZipWriter::new(file);
            for file_index in file_indexes {
                let entry = manifest.files.get(*file_index).expect("entry");
                let source_path = root.join(&entry.logical_path);
                zip.start_file(
                    &entry.logical_path,
                    zip::write::SimpleFileOptions::default(),
                )
                .expect("start entry");
                let mut source = std::fs::File::open(&source_path).expect("source");
                std::io::copy(&mut source, &mut zip).expect("copy entry");
            }
            zip.finish().expect("finish part");
            for file_index in file_indexes {
                let entry = manifest.files.get_mut(*file_index).expect("entry");
                entry.zip_part = Some(part_filename.clone());
                entry.zip_entry_path = Some(entry.logical_path.clone());
                entry.compression_method = Some("stored".to_owned());
                std::fs::remove_file(root.join(&entry.logical_path)).expect("remove root file");
            }
            let metadata = std::fs::metadata(&part_path).expect("part metadata");
            part_infos.push(BackupPartInfo {
                filename: part_filename,
                part_index,
                total_parts,
                backup_id: manifest.backup_id.clone(),
                size: metadata.len(),
                sha256: sha256_hex_for_file(&part_path).expect("part sha"),
                target_part_size,
                oversized: *oversized,
            });
        }
        manifest.storage_mode = BackupStorageMode::SplitZip;
        manifest.parts = part_infos;
        write_json_pretty(&root.join(MANIFEST_FILE_NAME), &manifest).expect("manifest");
        let mut metadata = load_metadata(&root.join(BACKUP_METADATA_FILE_NAME)).expect("metadata");
        metadata.storage_mode = BackupStorageMode::SplitZip;
        metadata.part_count = total_parts;
        write_json_pretty(&root.join(BACKUP_METADATA_FILE_NAME), &metadata).expect("metadata");
        manifest
    }

    fn rewrite_split_entry(
        root: &Path,
        manifest: &mut BackupManifest,
        logical_path: &str,
        replacement: &[u8],
    ) {
        let entry = manifest
            .files
            .iter_mut()
            .find(|entry| entry.logical_path == logical_path)
            .expect("entry");
        let part_filename = entry.zip_part.clone().expect("zip part");
        let entry_path = entry.zip_entry_path.clone().expect("zip entry");
        entry.size = u64::try_from(replacement.len()).expect("replacement len");
        entry.sha256 = sha256_hex_for_bytes(replacement);

        let part_path = root.join(&part_filename);
        let source = std::fs::File::open(&part_path).expect("open part");
        let mut archive = zip::ZipArchive::new(source).expect("zip archive");
        let mut zip_entries = Vec::new();
        for index in 0..archive.len() {
            let mut zip_entry = archive.by_index(index).expect("zip entry");
            let name = zip_entry.name().to_owned();
            let mut bytes = Vec::new();
            zip_entry.read_to_end(&mut bytes).expect("read zip entry");
            if name == entry_path {
                bytes = replacement.to_vec();
            }
            zip_entries.push((name, bytes));
        }
        drop(archive);

        let output = std::fs::File::create(&part_path).expect("rewrite part");
        let mut zip = zip::ZipWriter::new(output);
        for (name, bytes) in zip_entries {
            zip.start_file(name, zip::write::SimpleFileOptions::default())
                .expect("start entry");
            zip.write_all(&bytes).expect("write entry");
        }
        zip.finish().expect("finish part");

        let part = manifest
            .parts
            .iter_mut()
            .find(|part| part.filename == part_filename)
            .expect("part");
        part.size = std::fs::metadata(&part_path).expect("part metadata").len();
        part.sha256 = sha256_hex_for_file(&part_path).expect("part sha");
        rewrite_manifest(root, manifest);
    }

    #[test]
    fn verify_saved_v4_root_accepts_valid_full_backup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("2026-05-06_full-site_abcd12");
        write_saved_v4_fixture_for_test(
            &root,
            BackupScope::FullSite,
            board_fixture_files_for_test(),
            Some(b"sqlite".to_vec()),
            1_715_000_000_i64,
        );

        let verified = verify_saved_v4_root(&root, &[BackupScope::FullSite])
            .expect("verify saved full backup");
        assert_eq!(verified.completed_at, 1_715_000_000_i64);
        assert!(verified.db_snapshot.is_some());
        assert!(verified.boards.contains_key("tech"));
    }

    #[test]
    fn sanitize_logical_path_rejects_encoded_and_unicode_separator_tricks() {
        assert!(sanitize_logical_path("../db.sqlite").is_err());
        assert!(sanitize_logical_path("..%2fdb.sqlite").is_err());
        assert!(sanitize_logical_path("boards\u{2215}tech/file").is_err());
    }

    #[test]
    fn verify_saved_v4_root_rejects_size_mismatch() {
        let (_dir, root, mut manifest) = saved_full_fixture("2026-05-06_full-site_size");
        media_entry_mut(&mut manifest).size = 999;
        rewrite_manifest(&root, &manifest);

        let error = verify_saved_v4_root(&root, &[BackupScope::FullSite])
            .expect_err("size mismatch should fail");
        assert!(error.to_string().contains("size mismatch"));
    }

    #[test]
    fn verify_saved_v4_root_rejects_absolute_path() {
        let (_dir, root, mut manifest) = saved_full_fixture("2026-05-06_full-site_absolute");
        media_entry_mut(&mut manifest).logical_path = "/tmp/escape.txt".to_owned();
        rewrite_manifest(&root, &manifest);

        let error = verify_saved_v4_root(&root, &[BackupScope::FullSite])
            .expect_err("absolute path should fail");
        assert!(error.to_string().contains("suspicious logical path"));
    }

    #[test]
    fn verify_saved_v4_root_rejects_backslash_path() {
        let (_dir, root, mut manifest) = saved_full_fixture("2026-05-06_full-site_backslash");
        media_entry_mut(&mut manifest).logical_path =
            "boards\\tech\\media\\src\\example.txt".to_owned();
        rewrite_manifest(&root, &manifest);

        let error = verify_saved_v4_root(&root, &[BackupScope::FullSite])
            .expect_err("backslash path should fail");
        assert!(error.to_string().contains("suspicious logical path"));
    }

    #[test]
    fn verify_saved_v4_root_rejects_windows_drive_path() {
        let (_dir, root, mut manifest) = saved_full_fixture("2026-05-06_full-site_drive");
        media_entry_mut(&mut manifest).logical_path = "C:/backup/example.txt".to_owned();
        rewrite_manifest(&root, &manifest);

        let error = verify_saved_v4_root(&root, &[BackupScope::FullSite])
            .expect_err("Windows drive path should fail");
        assert!(error.to_string().contains("suspicious logical path"));
    }

    #[test]
    fn verify_saved_v4_root_rejects_non_regular_declared_file() {
        let (_dir, root, _manifest) = saved_full_fixture("2026-05-06_full-site_nonregular");
        let media_path = root.join("boards/tech/media/src/example.txt");
        std::fs::remove_file(&media_path).expect("remove media file");
        std::fs::create_dir(&media_path).expect("replace media file with directory");

        let error = verify_saved_v4_root(&root, &[BackupScope::FullSite])
            .expect_err("non-regular file should fail");
        assert!(error.to_string().contains("unsafe"));
    }

    #[test]
    fn verify_saved_v4_root_rejects_tor_key_outside_tor_keys_scope() {
        let (_dir, root, mut manifest) = saved_full_fixture("2026-05-06_full-site_tor-scope");
        let tor_bytes = b"secret";
        let tor_path = root.join("config/hs_ed25519_secret_key");
        std::fs::create_dir_all(tor_path.parent().expect("tor parent")).expect("create config dir");
        std::fs::write(&tor_path, tor_bytes).expect("write tor key outside scope");
        manifest.includes.tor_keys = true;
        manifest.files.push(test_file_entry_for_test(
            "config/hs_ed25519_secret_key",
            None,
            BackupFileKind::TorKey,
            tor_bytes,
        ));
        rewrite_manifest(&root, &manifest);

        let mut metadata = load_metadata(&root.join(BACKUP_METADATA_FILE_NAME)).expect("metadata");
        metadata.includes_tor_keys = true;
        write_json_pretty(&root.join(BACKUP_METADATA_FILE_NAME), &metadata)
            .expect("rewrite metadata");

        let error = verify_saved_v4_root(&root, &[BackupScope::FullSite])
            .expect_err("tor key outside tor-keys scope should fail");
        assert!(error.to_string().contains("escapes the tor-keys/ scope"));
    }

    #[test]
    fn verify_saved_v4_root_rejects_checksum_mismatch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("2026-05-06_full-site_checksum");
        write_saved_v4_fixture_for_test(
            &root,
            BackupScope::FullSite,
            board_fixture_files_for_test(),
            Some(b"sqlite".to_vec()),
            1_715_000_100_i64,
        );
        std::fs::write(root.join("boards/tech/media/src/example.txt"), b"other")
            .expect("tamper file");

        let error = verify_saved_v4_root(&root, &[BackupScope::FullSite])
            .expect_err("checksum mismatch should fail");
        let message = error.to_string();
        assert!(message.contains("checksum mismatch") || message.contains("size mismatch"));
    }

    #[test]
    fn verify_saved_v4_root_rejects_missing_declared_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("2026-05-06_full-site_missing");
        write_saved_v4_fixture_for_test(
            &root,
            BackupScope::FullSite,
            board_fixture_files_for_test(),
            Some(b"sqlite".to_vec()),
            1_715_000_200_i64,
        );
        std::fs::remove_file(root.join("boards/tech/media/src/example.txt")).expect("remove file");

        let error = verify_saved_v4_root(&root, &[BackupScope::FullSite])
            .expect_err("missing declared file should fail");
        assert!(error.to_string().contains("missing declared file"));
    }

    #[test]
    fn verify_saved_v4_root_accepts_valid_split_zip_backup() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("2026-05-06_full-site_split");
        write_saved_v4_fixture_for_test(
            &root,
            BackupScope::FullSite,
            board_fixture_files_for_test(),
            Some(b"sqlite".to_vec()),
            1_715_000_250_i64,
        );
        convert_fixture_to_split(&root, 16);

        let verified =
            verify_saved_v4_root(&root, &[BackupScope::FullSite]).expect("verify split backup");
        assert_eq!(verified.metadata.storage_mode, BackupStorageMode::SplitZip);
        assert!(!verified.manifest.parts.is_empty());
        assert!(verified.boards.contains_key("tech"));
    }

    #[test]
    fn verify_saved_v4_root_rejects_split_board_json_identity_mismatch() {
        let (_dir, root, _manifest) = saved_full_fixture("2026-05-06_full-site_board-json");
        let mut manifest = convert_fixture_to_split(&root, 16);
        let mismatched_board_json = br#"{"version":1,"board":{"id":1,"short_name":"b","name":"Random","description":"","nsfw":false,"max_threads":100,"max_archived_threads":150,"bump_limit":300,"allow_images":true,"allow_video":true,"allow_audio":false,"allow_any_files":false,"allow_tripcodes":true,"edit_window_secs":300,"allow_editing":false,"allow_self_delete":false,"allow_archive":true,"allow_video_embeds":false,"allow_captcha":false,"show_poster_ids":false,"collapse_greentext":false,"post_cooldown_secs":0,"banner_mode":"inherit","access_mode":"public","access_password_hash":"","created_at":1},"threads":[],"posts":[],"polls":[],"poll_options":[],"poll_votes":[],"file_hashes":[],"banners":[]}"#;
        rewrite_split_entry(
            &root,
            &mut manifest,
            "boards/tech/board.json",
            mismatched_board_json,
        );

        let error = verify_saved_v4_root(&root, &[BackupScope::FullSite])
            .expect_err("mismatched board.json should fail");
        assert!(error.to_string().contains("board.json identity mismatch"));
    }

    #[test]
    fn verify_saved_v4_root_rejects_missing_split_part() {
        let (_dir, root, _manifest) = saved_full_fixture("2026-05-06_full-site_missing-part");
        let manifest = convert_fixture_to_split(&root, 16);
        let part = root.join(&manifest.parts.first().expect("split part").filename);
        std::fs::remove_file(part).expect("remove part");

        let error = verify_saved_v4_root(&root, &[BackupScope::FullSite])
            .expect_err("missing part should fail");
        assert!(error.to_string().contains("missing declared file"));
    }

    #[test]
    fn verify_saved_v4_root_rejects_corrupt_split_part_hash() {
        let (_dir, root, _manifest) = saved_full_fixture("2026-05-06_full-site_corrupt-part");
        let manifest = convert_fixture_to_split(&root, 16);
        let part = root.join(&manifest.parts.first().expect("split part").filename);
        std::fs::write(part, b"not a zip").expect("corrupt part");

        let error = verify_saved_v4_root(&root, &[BackupScope::FullSite])
            .expect_err("corrupt part hash should fail");
        let message = error.to_string();
        assert!(message.contains("checksum mismatch") || message.contains("size mismatch"));
    }

    #[test]
    fn verify_saved_v4_root_rejects_zip_backed_file_duplicate_at_root() {
        let (_dir, root, _manifest) = saved_full_fixture("2026-05-06_full-site_duplicate-root");
        convert_fixture_to_split(&root, 16);
        let duplicate = root.join("boards/tech/media/src/example.txt");
        std::fs::create_dir_all(duplicate.parent().expect("duplicate parent"))
            .expect("create duplicate parent");
        std::fs::write(duplicate, b"media").expect("write duplicate root payload");

        let error = verify_saved_v4_root(&root, &[BackupScope::FullSite])
            .expect_err("duplicate root payload should fail");
        assert!(error.to_string().contains("unexpected files"));
    }

    #[test]
    fn verify_saved_v4_root_rejects_nested_split_part_path() {
        let (_dir, root, _manifest) = saved_full_fixture("2026-05-06_full-site_nested-part");
        let mut manifest = convert_fixture_to_split(&root, 16);
        manifest.parts.first_mut().expect("part").filename =
            "parts/nested/part-0001.zip".to_owned();
        rewrite_manifest(&root, &manifest);

        let error = verify_saved_v4_root(&root, &[BackupScope::FullSite])
            .expect_err("nested split part path should fail");
        assert!(error.to_string().contains("part-0001.zip"));
    }

    #[test]
    fn verify_saved_v4_root_rejects_bad_split_part_name() {
        let (_dir, root, _manifest) = saved_full_fixture("2026-05-06_full-site_bad-part-name");
        let mut manifest = convert_fixture_to_split(&root, 16);
        manifest.parts.first_mut().expect("part").filename = "parts/foo.zip".to_owned();
        rewrite_manifest(&root, &manifest);

        let error = verify_saved_v4_root(&root, &[BackupScope::FullSite])
            .expect_err("bad split part name should fail");
        assert!(error.to_string().contains("part-0001.zip"));
    }

    #[test]
    fn verify_saved_v4_root_rejects_duplicate_split_part_filename() {
        let (_dir, root, _manifest) =
            saved_full_fixture("2026-05-06_full-site_duplicate-part-file");
        let mut manifest = convert_fixture_to_split(&root, 16);
        let duplicate_filename = manifest.parts.first().expect("first part").filename.clone();
        manifest.parts.get_mut(1).expect("second part").filename = duplicate_filename;
        rewrite_manifest(&root, &manifest);

        let error = verify_saved_v4_root(&root, &[BackupScope::FullSite])
            .expect_err("duplicate split part filename should fail");
        assert!(error
            .to_string()
            .contains("duplicate split ZIP part filename"));
    }

    #[test]
    fn verify_saved_v4_root_rejects_duplicate_split_part_index() {
        let (_dir, root, _manifest) =
            saved_full_fixture("2026-05-06_full-site_duplicate-part-index");
        let mut manifest = convert_fixture_to_split(&root, 16);
        let duplicate_index = manifest.parts.first().expect("first part").part_index;
        manifest.parts.get_mut(1).expect("second part").part_index = duplicate_index;
        rewrite_manifest(&root, &manifest);

        let error = verify_saved_v4_root(&root, &[BackupScope::FullSite])
            .expect_err("duplicate split part index should fail");
        assert!(error.to_string().contains("duplicate split ZIP part index"));
    }

    #[test]
    fn verify_saved_v4_root_rejects_split_part_total_mismatch() {
        let (_dir, root, _manifest) = saved_full_fixture("2026-05-06_full-site_part-total");
        let mut manifest = convert_fixture_to_split(&root, 16);
        let wrong_total = manifest
            .parts
            .first()
            .expect("part")
            .total_parts
            .saturating_add(1);
        manifest.parts.first_mut().expect("part").total_parts = wrong_total;
        rewrite_manifest(&root, &manifest);

        let error = verify_saved_v4_root(&root, &[BackupScope::FullSite])
            .expect_err("split part total mismatch should fail");
        assert!(error.to_string().contains("total_parts"));
    }

    #[test]
    fn verify_saved_v4_root_rejects_non_contiguous_split_part_index() {
        let (_dir, root, _manifest) = saved_full_fixture("2026-05-06_full-site_part-gap");
        let mut manifest = convert_fixture_to_split(&root, 16);
        manifest.parts.remove(0);
        let new_total = u32::try_from(manifest.parts.len()).expect("part count");
        for part in &mut manifest.parts {
            part.total_parts = new_total;
        }
        rewrite_manifest(&root, &manifest);
        rewrite_metadata_part_count(&root, new_total);

        let error = verify_saved_v4_root(&root, &[BackupScope::FullSite])
            .expect_err("non-contiguous split part index should fail");
        let message = error.to_string();
        assert!(message.contains("outside 1..=") || message.contains("missing split ZIP part"));
    }

    #[test]
    fn verify_saved_v4_root_rejects_split_part_filename_index_mismatch() {
        let (_dir, root, _manifest) = saved_full_fixture("2026-05-06_full-site_part-name-index");
        let mut manifest = convert_fixture_to_split(&root, 16);
        let original_filename = manifest.parts.first().expect("part").filename.clone();
        let original = root.join(original_filename);
        let replacement_filename = "parts/part-9999.zip".to_owned();
        manifest.parts.first_mut().expect("part").filename = replacement_filename.clone();
        std::fs::copy(original, root.join(replacement_filename)).expect("copy part");
        rewrite_manifest(&root, &manifest);

        let error = verify_saved_v4_root(&root, &[BackupScope::FullSite])
            .expect_err("split part filename/index mismatch should fail");
        assert!(error.to_string().contains("filename"));
    }

    #[test]
    fn verify_saved_v4_root_rejects_zero_split_part_index() {
        let (_dir, root, _manifest) = saved_full_fixture("2026-05-06_full-site_part-zero");
        let mut manifest = convert_fixture_to_split(&root, 16);
        manifest.parts.first_mut().expect("part").part_index = 0;
        rewrite_manifest(&root, &manifest);

        let error = verify_saved_v4_root(&root, &[BackupScope::FullSite])
            .expect_err("zero split part index should fail");
        assert!(error.to_string().contains("outside 1..="));
    }

    #[test]
    fn verify_saved_v4_root_rejects_undeclared_split_zip_entry() {
        let (_dir, root, _manifest) = saved_full_fixture("2026-05-06_full-site_extra-entry");
        let mut manifest = convert_fixture_to_split(&root, 16);
        let part_filename = manifest.parts.first().expect("split part").filename.clone();
        let part_path = root.join(&part_filename);
        let file = std::fs::File::create(&part_path).expect("rewrite part");
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("undeclared.txt", zip::write::SimpleFileOptions::default())
            .expect("start extra");
        zip.write_all(b"extra").expect("write extra");
        zip.finish().expect("finish extra");
        let first_part = manifest.parts.first_mut().expect("split part");
        first_part.size = std::fs::metadata(&part_path).expect("metadata").len();
        first_part.sha256 = sha256_hex_for_file(&part_path).expect("sha");
        write_json_pretty(&root.join(MANIFEST_FILE_NAME), &manifest).expect("manifest");

        let error = verify_saved_v4_root(&root, &[BackupScope::FullSite])
            .expect_err("undeclared entry should fail");
        assert!(error.to_string().contains("undeclared ZIP entry"));
    }

    #[test]
    fn verify_saved_v4_root_rejects_unsafe_split_zip_entry_path() {
        let (_dir, root, _manifest) = saved_full_fixture("2026-05-06_full-site_unsafe-entry");
        let mut manifest = convert_fixture_to_split(&root, 16);
        let part_path = root.join(&manifest.parts.first().expect("split part").filename);
        let file = std::fs::File::create(&part_path).expect("rewrite part");
        let mut zip = zip::ZipWriter::new(file);
        zip.start_file("../escape.txt", zip::write::SimpleFileOptions::default())
            .expect("start unsafe");
        zip.write_all(b"escape").expect("write unsafe");
        zip.finish().expect("finish unsafe");
        let first_part = manifest.parts.first_mut().expect("split part");
        first_part.size = std::fs::metadata(&part_path).expect("metadata").len();
        first_part.sha256 = sha256_hex_for_file(&part_path).expect("sha");
        write_json_pretty(&root.join(MANIFEST_FILE_NAME), &manifest).expect("manifest");

        let error = verify_saved_v4_root(&root, &[BackupScope::FullSite])
            .expect_err("unsafe entry should fail");
        assert!(error.to_string().contains("suspicious logical path"));
    }

    #[test]
    fn verify_saved_v4_root_rejects_duplicate_logical_paths() {
        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("2026-05-06_full-site_duplicate");
        let (_metadata, mut manifest) = write_saved_v4_fixture_for_test(
            &root,
            BackupScope::FullSite,
            board_fixture_files_for_test(),
            Some(b"sqlite".to_vec()),
            1_715_000_300_i64,
        );
        manifest.files.push(
            manifest
                .files
                .iter()
                .find(|entry| entry.logical_path == "boards/tech/media/src/example.txt")
                .cloned()
                .expect("existing media entry"),
        );
        write_json_pretty(&root.join(MANIFEST_FILE_NAME), &manifest).expect("rewrite manifest");

        let error = verify_saved_v4_root(&root, &[BackupScope::FullSite])
            .expect_err("duplicate logical paths should fail");
        assert!(error.to_string().contains("duplicate logical path"));
    }

    #[cfg(unix)]
    #[test]
    fn verify_saved_v4_root_rejects_symlink_components() {
        use std::os::unix::fs as unix_fs;

        let dir = tempfile::tempdir().expect("tempdir");
        let root = dir.path().join("2026-05-06_full-site_symlink");
        let (_metadata, mut manifest) = write_saved_v4_fixture_for_test(
            &root,
            BackupScope::FullSite,
            board_fixture_files_for_test(),
            Some(b"sqlite".to_vec()),
            1_715_000_400_i64,
        );
        let outside_dir = dir.path().join("outside");
        std::fs::create_dir_all(&outside_dir).expect("outside dir");
        std::fs::write(outside_dir.join("escape.txt"), b"secret").expect("outside file");
        std::fs::create_dir_all(root.join("boards/tech/media/src")).expect("media dir");
        unix_fs::symlink(&outside_dir, root.join("boards/tech/media/src/link")).expect("symlink");
        if let Some(entry) = manifest
            .files
            .iter_mut()
            .find(|entry| entry.logical_path == "boards/tech/media/src/example.txt")
        {
            entry.logical_path = "boards/tech/media/src/link/escape.txt".to_owned();
            entry.sha256 = sha256_hex_for_bytes(b"secret");
            entry.size = 6;
        }
        write_json_pretty(&root.join(MANIFEST_FILE_NAME), &manifest).expect("rewrite manifest");

        let error = verify_saved_v4_root(&root, &[BackupScope::FullSite])
            .expect_err("symlink component should fail");
        assert!(error.to_string().contains("unsafe"));
    }
}
