//! Bounded Ogg validation for the Speex compatibility path.

use anyhow::{ensure, Context as _, Result};
use std::fs::File;
use std::io::{Read as _, Seek as _};
use std::path::Path;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// Match the maximum supported audio upload size without retaining the clip.
const MAX_CONTAINER_BYTES: u64 = 512 * 1024 * 1024;

/// Validate every page before classifying an otherwise uncovered audio stream.
pub(super) fn inspect(
    path: &Path,
    started: Instant,
    timeout: Duration,
    cancel: Option<&CancellationToken>,
) -> Result<bool> {
    let check = || -> Result<()> {
        ensure!(
            !cancel.is_some_and(CancellationToken::is_cancelled),
            "Speex inspection cancelled"
        );
        ensure!(started.elapsed() < timeout, "Speex inspection timed out");
        Ok(())
    };
    check()?;
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "Speex source is not a regular file");
    if metadata.len() < 27 {
        return Ok(false);
    }
    let mut prefix = [0; 4];
    file.read_exact(&mut prefix)?;
    if &prefix != b"OggS" {
        return Ok(false);
    }
    file.rewind()?;
    let mut remaining = metadata.len();
    let first = Page::read(&mut file, &mut remaining)?;
    if first.body.get(..8) != Some(b"Speex   ") {
        return Ok(false);
    }
    ensure!(
        metadata.len() <= MAX_CONTAINER_BYTES,
        "Speex input exceeds source budget"
    );
    let extra_headers = validate_identification(&first)?;
    let serial = first
        .header
        .get(14..18)
        .context("missing Ogg serial")?
        .to_vec();
    let mut sequence = 0_u32;
    let mut packets = 0_u64;
    let mut continued = false;
    let mut page = first;
    loop {
        check()?;
        page.validate(&serial, sequence, continued)?;
        packets += u64::try_from(page.lacing.iter().filter(|size| **size < 255).count())?;
        continued = page.lacing.last() == Some(&255);
        let flags = *page.header.get(5).context("missing Ogg flags")?;
        if remaining == 0 {
            ensure!(flags & 4 != 0 && !continued, "incomplete Speex container");
            ensure!(
                packets > 2 + u64::from(extra_headers),
                "Speex has no audio packets"
            );
            break;
        }
        ensure!(
            flags & 4 == 0,
            "chained Speex containers require separate validation"
        );
        sequence = sequence.checked_add(1).context("Ogg sequence overflow")?;
        page = Page::read(&mut file, &mut remaining)?;
    }
    check()?;
    Ok(true)
}

/// One Ogg page, with at most 255 lacing bytes and 65,025 payload bytes.
struct Page {
    /// Fixed Ogg framing header.
    header: [u8; 27],
    /// Packet fragment lengths.
    lacing: Vec<u8>,
    /// Bounded page payload.
    body: Vec<u8>,
}

impl Page {
    /// Read only lengths supported by the bytes remaining in the regular file.
    fn read(file: &mut File, remaining: &mut u64) -> Result<Self> {
        ensure!(*remaining >= 27, "truncated Ogg page");
        let mut header = [0; 27];
        file.read_exact(&mut header)?;
        let segments = usize::from(*header.get(26).context("missing Ogg lacing count")?);
        let mut lacing = vec![0; segments];
        file.read_exact(&mut lacing)?;
        let size = lacing.iter().map(|size| usize::from(*size)).sum::<usize>();
        let total = 27 + u64::try_from(segments)? + u64::try_from(size)?;
        ensure!(total <= *remaining, "truncated Ogg payload");
        let mut body = vec![0; size];
        file.read_exact(&mut body)?;
        *remaining -= total;
        Ok(Self {
            header,
            lacing,
            body,
        })
    }

    /// Verify CRC, serial, sequence, continuation and stream-boundary flags.
    fn validate(&self, serial: &[u8], sequence: u32, continued: bool) -> Result<()> {
        let flags = *self.header.get(5).context("missing Ogg flags")?;
        ensure!(
            self.header.get(..4) == Some(b"OggS")
                && self.header.get(4) == Some(&0)
                && flags & !7 == 0
                && (flags & 1 != 0) == continued
                && (flags & 2 != 0) == (sequence == 0)
                && self.header.get(14..18) == Some(serial)
                && self.header.get(18..22) == Some(sequence.to_le_bytes().as_slice()),
            "invalid Ogg Speex page sequence"
        );
        let expected = u32::from_le_bytes(
            self.header
                .get(22..26)
                .context("missing Ogg checksum")?
                .try_into()?,
        );
        let mut header = self.header;
        header
            .get_mut(22..26)
            .context("missing Ogg checksum")?
            .fill(0);
        let actual =
            header
                .iter()
                .chain(&self.lacing)
                .chain(&self.body)
                .fold(0_u32, |mut crc, byte| {
                    crc ^= u32::from(*byte) << 24;
                    for _ in 0..8 {
                        crc = (crc << 1)
                            ^ if crc & 0x8000_0000 != 0 {
                                0x04c1_1db7
                            } else {
                                0
                            };
                    }
                    crc
                });
        ensure!(actual == expected, "invalid Ogg Speex page checksum");
        Ok(())
    }
}

/// Validate the standard Speex identification fields before requesting fallback.
fn validate_identification(page: &Page) -> Result<u32> {
    ensure!(
        page.lacing == [80] && page.body.len() == 80,
        "invalid Speex identification size"
    );
    let field = |offset: usize| -> Result<i32> {
        Ok(i32::from_le_bytes(
            page.body
                .get(offset..offset + 4)
                .context("truncated Speex identification")?
                .try_into()?,
        ))
    };
    let mode = field(40)?;
    ensure!((0..=2).contains(&mode), "invalid Speex mode");
    let shift = u32::try_from(mode)?;
    ensure!(
        field(28)? == 1
            && field(32)? == 80
            && field(36)? == 8000_i32.checked_shl(shift).context("Speex rate overflow")?
            && (1..=4).contains(&field(44)?)
            && (1..=2).contains(&field(48)?)
            && field(56)? == 160_i32.checked_shl(shift).context("Speex frame overflow")?
            && (0..=1).contains(&field(60)?)
            && (1..=64).contains(&field(64)?)
            && (0..=8).contains(&field(68)?)
            && field(72)? == 0
            && field(76)? == 0,
        "invalid Speex identification metadata"
    );
    Ok(u32::try_from(field(68)?)?)
}
