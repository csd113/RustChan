//! Bounded, lexical filesystem enumeration without repair or reference decisions.

use super::{redacted_identifier, MediaCategory, ReconcileCursor};
use anyhow::{Context as _, Result};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub(super) struct InventoryEntry {
    pub(super) absolute: PathBuf,
    pub(super) relative: Option<String>,
    pub(super) sort_key: String,
    pub(super) board: Option<String>,
    pub(super) category: MediaCategory,
    pub(super) entry_kind: InventoryEntryKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum InventoryEntryKind {
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

#[derive(Debug, Default)]
pub(super) struct InventoryPage {
    pub(super) entries: Vec<InventoryEntry>,
    pub(super) boards_examined: u64,
    pub(super) has_more: bool,
    pub(super) last_key: Option<String>,
    pub(super) errors: Vec<String>,
}

pub(super) struct InventoryWalker<'a> {
    root: &'a Path,
    boards: &'a BTreeSet<String>,
    after: Option<&'a str>,
    limit: usize,
    #[cfg(unix)]
    root_device: u64,
    page: InventoryPage,
}

impl<'a> InventoryWalker<'a> {
    pub(super) fn new(
        root: &'a Path,
        boards: &'a BTreeSet<String>,
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
            boards,
            after: cursor.after_managed_key.as_deref(),
            limit,
            #[cfg(unix)]
            root_device,
            page: InventoryPage::default(),
        })
    }

    pub(super) fn scan(mut self) -> InventoryPage {
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
                    Some(board) if self.boards.contains(board) => {
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
                && board_utf8.is_some_and(|board_name| self.boards.contains(board_name))
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

    #[cfg(unix)]
    fn crosses_filesystem(&self, metadata: &std::fs::Metadata) -> bool {
        use std::os::unix::fs::MetadataExt as _;
        metadata.dev() != self.root_device
    }

    #[cfg(not(unix))]
    #[expect(
        clippy::unused_self,
        reason = "shared inventory callers use the filesystem identity stored by the Unix implementation"
    )]
    const fn crosses_filesystem(&self, _metadata: &std::fs::Metadata) -> bool {
        false
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
