// src/utils/files/disk_space.rs

use anyhow::Result;
use std::path::Path;

#[cfg(unix)]
fn widen_to_u64<T>(value: T) -> u64
where
    T: Into<u64>,
{
    value.into()
}

#[cfg(unix)]
pub(super) fn check_disk_space(dir: &Path, needed_bytes: usize) -> Result<()> {
    // SAFETY:
    // - `path_cstr` is a valid NUL-terminated string for the duration of the call.
    // - `stat` points to valid writable memory and lives until `statvfs` returns.
    // - `libc::statvfs` does not retain either pointer after returning.
    unsafe {
        let dir_bytes = dir.to_string_lossy();
        if let Ok(path_cstr) = std::ffi::CString::new(dir_bytes.as_bytes()) {
            let mut stat: libc::statvfs = std::mem::zeroed();
            if libc::statvfs(path_cstr.as_ptr(), &raw mut stat) == 0 {
                let free_bytes =
                    widen_to_u64(stat.f_bavail).saturating_mul(widen_to_u64(stat.f_frsize));
                let needed = (needed_bytes as u64).saturating_mul(2);
                if free_bytes < needed {
                    return Err(anyhow::anyhow!(
                        "Insufficient disk space: need ~{} MiB free, only ~{} MiB available.",
                        needed / (1024 * 1024),
                        free_bytes / (1024 * 1024)
                    ));
                }
            }
        }
    }
    Ok(())
}

#[cfg(not(unix))]
pub(super) fn check_disk_space(_dir: &Path, _needed_bytes: usize) -> Result<()> {
    Ok(())
}
