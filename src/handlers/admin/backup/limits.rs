//! Bounded streaming used before writing untrusted archive payloads.

/// Copies limited.
pub(super) fn copy_limited<R: std::io::Read, W: std::io::Write>(
    reader: &mut R,
    writer: &mut W,
    max_bytes: u64,
) -> std::io::Result<u64> {
    let mut buf = vec![0u8; 65_536];
    let mut total = 0u64;
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        let n_u64 = u64::try_from(n).map_err(|error| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!("Read size does not fit the byte counter: {error}"),
            )
        })?;
        total = total.checked_add(n_u64).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "Decompressed byte count overflowed",
            )
        })?;
        if total > max_bytes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "Decompressed entry exceeds {} MiB limit — possible zip bomb",
                    max_bytes / 1024 / 1024
                ),
            ));
        }
        if let Some(slice) = buf.get(..n) {
            writer.write_all(slice)?;
        }
    }
    Ok(total)
}

/// Copies limited with total budget.
pub(super) fn copy_limited_with_total_budget<R: std::io::Read, W: std::io::Write>(
    reader: &mut R,
    writer: &mut W,
    max_bytes: u64,
    total_written: &mut u64,
    total_budget: u64,
    label: &str,
) -> std::io::Result<u64> {
    let remaining = total_budget.checked_sub(*total_written).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("{label} exceeds the extracted restore budget"),
        )
    })?;
    // Enforce the aggregate budget BEFORE writing, rather than after an entire
    // oversized entry has already consumed disk space.
    let copied = copy_limited(reader, writer, max_bytes.min(remaining))?;
    *total_written = total_written.checked_add(copied).ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "Restore byte count overflow",
        )
    })?;
    Ok(copied)
}
