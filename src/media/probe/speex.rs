//! Bounded Ogg validation for Speex and discrete Opus logical streams.

use anyhow::{ensure, Context as _, Result};
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{Read as _, Seek as _};
use std::path::Path;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

/// Match the maximum supported audio upload size without retaining the clip.
const MAX_CONTAINER_BYTES: u64 = 512 * 1024 * 1024;
/// Bound metadata retained for simultaneous or chained logical streams.
const MAX_LOGICAL_STREAMS: usize = 64;

/// Validation state for one independently numbered Ogg logical stream.
#[derive(Default)]
struct LogicalStream {
    /// Next page number required for this serial.
    sequence: u32,
    /// Completed headers and audio packets seen so far.
    packets: u64,
    /// Whether the last page left an unfinished packet.
    continued: bool,
    /// Validated optional header count from Speex identification.
    extra_headers: u32,
    /// Whether an authenticated, complete EOS page was observed.
    ended: bool,
}

impl LogicalStream {
    /// Finished chains and newly opened BOS headers permit another grouped BOS page.
    const fn permits_bos(&self) -> bool {
        self.ended || (self.sequence == 1 && self.packets == 1)
    }
}

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
    let mut streams = BTreeMap::<[u8; 4], LogicalStream>::new();
    let mut page = first;
    loop {
        check()?;
        let serial: [u8; 4] = page
            .header
            .get(14..18)
            .context("missing Ogg serial")?
            .try_into()?;
        if !streams.contains_key(&serial) {
            ensure!(
                streams.values().all(|logical_stream| logical_stream.ended)
                    || streams.values().all(LogicalStream::permits_bos),
                "Speex BOS pages are not grouped at a chain boundary"
            );
            ensure!(
                streams.len() < MAX_LOGICAL_STREAMS,
                "Speex logical stream budget exceeded"
            );
            ensure!(
                page.body.get(..8) == Some(b"Speex   "),
                "mixed-codec Speex container is not validated"
            );
            let extra_headers = validate_identification(&page)?;
            let _previous_entry = streams.insert(
                serial,
                LogicalStream {
                    extra_headers,
                    ..LogicalStream::default()
                },
            );
        }
        let stream = streams
            .get_mut(&serial)
            .context("missing Speex stream state")?;
        ensure!(!stream.ended, "Ogg pages follow logical stream end");
        page.validate(&serial, stream.sequence, stream.continued)?;
        stream.packets = stream
            .packets
            .checked_add(u64::try_from(
                page.lacing.iter().filter(|size| **size < 255).count(),
            )?)
            .context("Speex packet count overflow")?;
        stream.continued = page.lacing.last() == Some(&255);
        let flags = *page.header.get(5).context("missing Ogg flags")?;
        if flags & 4 != 0 {
            ensure!(!stream.continued, "incomplete Speex packet");
            ensure!(
                stream.packets > 2 + u64::from(stream.extra_headers),
                "Speex has no audio packets"
            );
            stream.ended = true;
        }
        if remaining == 0 {
            ensure!(
                streams.values().all(|logical_stream| logical_stream.ended),
                "incomplete Speex container"
            );
            break;
        }
        stream.sequence = stream
            .sequence
            .checked_add(1)
            .context("Ogg sequence overflow")?;
        page = Page::read(&mut file, &mut remaining)?;
    }
    check()?;
    Ok(true)
}

/// Validate an otherwise unrecognized discrete Opus stream before declaring its kind.
pub(super) fn discrete_opus_header(
    path: &Path,
    started: Instant,
    timeout: Duration,
    cancel: Option<&CancellationToken>,
) -> Result<Option<Vec<u8>>> {
    let check = || -> Result<()> {
        ensure!(
            !cancel.is_some_and(CancellationToken::is_cancelled),
            "Opus inspection cancelled"
        );
        ensure!(started.elapsed() < timeout, "Opus inspection timed out");
        Ok(())
    };
    check()?;
    let mut file = File::open(path)?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "Opus source is not a regular file");
    if metadata.len() < 27 {
        return Ok(None);
    }
    let mut prefix = [0; 4];
    file.read_exact(&mut prefix)?;
    if &prefix != b"OggS" {
        return Ok(None);
    }
    file.rewind()?;
    let mut remaining = metadata.len();
    let first = Page::read(&mut file, &mut remaining)?;
    if first.body.get(..8) != Some(b"OpusHead") || first.body.get(18) != Some(&255) {
        return Ok(None);
    }
    ensure!(
        metadata.len() <= MAX_CONTAINER_BYTES,
        "Opus input exceeds source budget"
    );
    ensure!(
        first.lacing.iter().filter(|size| **size < 255).count() == 1
            && first.lacing.last() != Some(&255),
        "invalid Opus identification framing"
    );
    let header = first.body.clone();
    let serial = first
        .header
        .get(14..18)
        .context("missing Opus serial")?
        .to_vec();
    let mut page = first;
    let mut sequence = 0_u32;
    let mut continued = false;
    let mut packets = 0_u64;
    loop {
        check()?;
        page.validate(&serial, sequence, continued)?;
        packets = packets
            .checked_add(u64::try_from(
                page.lacing.iter().filter(|size| **size < 255).count(),
            )?)
            .context("Opus packet count overflow")?;
        continued = page.lacing.last() == Some(&255);
        let flags = *page.header.get(5).context("missing Opus flags")?;
        if remaining == 0 {
            ensure!(
                flags & 4 != 0 && !continued && packets > 2,
                "incomplete Opus stream"
            );
            break;
        }
        ensure!(
            flags & 4 == 0,
            "chained discrete Opus requires separate decoder states"
        );
        sequence = sequence.checked_add(1).context("Opus sequence overflow")?;
        page = Page::read(&mut file, &mut remaining)?;
    }
    check()?;
    Ok(Some(header))
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
        let payload_size = u64::try_from(size)?;
        let total = u64::try_from(segments)?
            .checked_add(27)
            .and_then(|header_size| header_size.checked_add(payload_size))
            .context("Ogg page size overflow")?;
        ensure!(total <= *remaining, "truncated Ogg payload");
        let mut body = vec![0; size];
        file.read_exact(&mut body)?;
        *remaining = remaining
            .checked_sub(total)
            .context("Ogg page exceeds remaining file")?;
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
            "invalid Ogg page sequence"
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
                    crc ^= u32::from(*byte) << 24_i32;
                    for _ in 0_i32..8_i32 {
                        crc = (crc << 1_i32)
                            ^ if crc & 0x8000_0000 != 0 {
                                0x04c1_1db7
                            } else {
                                0
                            };
                    }
                    crc
                });
        ensure!(actual == expected, "invalid Ogg page checksum");
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
                .get(
                    offset
                        ..offset
                            .checked_add(4)
                            .context("Speex field offset overflow")?,
                )
                .context("truncated Speex identification")?
                .try_into()?,
        ))
    };
    let mode = field(40)?;
    ensure!((0_i32..=2_i32).contains(&mode), "invalid Speex mode");
    let shift = u32::try_from(mode)?;
    ensure!(
        field(28)? == 1_i32
            && field(32)? == 80_i32
            && field(36)? == 8000_i32.checked_shl(shift).context("Speex rate overflow")?
            && (1_i32..=4_i32).contains(&field(44)?)
            && (1_i32..=2_i32).contains(&field(48)?)
            && field(56)? == 160_i32.checked_shl(shift).context("Speex frame overflow")?
            && (0_i32..=1_i32).contains(&field(60)?)
            && (1_i32..=64_i32).contains(&field(64)?)
            && (0_i32..=8_i32).contains(&field(68)?)
            && field(72)? == 0_i32
            && field(76)? == 0_i32,
        "invalid Speex identification metadata"
    );
    Ok(u32::try_from(field(68)?)?)
}
