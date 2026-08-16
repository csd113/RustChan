// src/utils/files/disk_space.rs

use anyhow::Result;
use std::path::Path;

#[cfg(unix)]
/// Converts an available-block count and fragment size into bytes.
const fn available_bytes_from_blocks(available_blocks: u64, fragment_size: u64) -> u64 {
    available_blocks.saturating_mul(fragment_size)
}

#[cfg(unix)]
/// Ensures the destination has at least twice the requested free space.
pub(super) fn check_disk_space(dir: &Path, needed_bytes: usize) -> Result<()> {
    let Ok(stats) = rustix::fs::statvfs(dir) else {
        return Ok(());
    };
    let free_bytes = available_bytes_from_blocks(stats.f_bavail, stats.f_frsize);

    let needed = u64::try_from(needed_bytes)
        .unwrap_or(u64::MAX)
        .saturating_mul(2);
    if free_bytes < needed {
        return Err(anyhow::anyhow!(
            "Insufficient disk space: need ~{} MiB free, only ~{} MiB available.",
            needed / (1024 * 1024),
            free_bytes / (1024 * 1024)
        ));
    }
    Ok(())
}

#[cfg(not(unix))]
/// Skips the Unix-specific free-space preflight on other platforms.
pub(super) fn check_disk_space(_dir: &Path, _needed_bytes: usize) -> Result<()> {
    Ok(())
}

#[cfg(all(test, unix))]
mod tests {
    //! Tests for filesystem-capacity conversion and query failure behavior.

    use super::{available_bytes_from_blocks, check_disk_space};

    /// Converts available filesystem blocks using the reported fragment size.
    #[test]
    fn converts_available_blocks_to_bytes() {
        assert_eq!(
            available_bytes_from_blocks(750, 1024),
            750 * 1024,
            "filesystem block conversion returned the wrong available-byte count"
        );
    }

    /// Saturates instead of wrapping an unrepresentable filesystem capacity.
    #[test]
    fn saturates_unrepresentable_available_capacity() {
        assert_eq!(
            available_bytes_from_blocks(u64::MAX, 2),
            u64::MAX,
            "filesystem capacity conversion must not wrap"
        );
    }

    /// Preserves fail-open behavior when filesystem metadata cannot be queried.
    #[test]
    fn query_failure_skips_the_preflight() -> anyhow::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let missing_dir = temp_dir.path().join("missing");

        check_disk_space(&missing_dir, usize::MAX)
    }
}
