//! Bounded MP4 metadata supplements: edit timing and video codec declarations.

use anyhow::{ensure, Context as _, Result};
use std::fs::File;
use std::io::{Read as _, Seek as _, SeekFrom};
use std::path::Path;

/// Maximum framing boxes visited, regardless of their declared payload size.
const MAX_BOXES: usize = 4096;
/// Maximum retained presentation segments.
const MAX_EDITS: usize = 64;

/// One explicitly declared segment of the movie's audio presentation.
enum Edit {
    /// Empty edits introduce real presentation silence.
    Silence(u64),
    /// Unit-rate, forward-moving range of decoded media frames.
    Audio {
        /// First raw decoded frame included in this edit.
        start: u64,
        /// Exclusive end of this edit's raw decoded range.
        end: u64,
    },
}

/// Streaming selection without retaining PCM or allocating silence buffers.
pub(super) struct Presentation {
    /// Bounded edit plan, expressed in decoded sample frames.
    edits: Vec<Edit>,
    /// Next edit to emit.
    next: usize,
    /// Current raw decoded frame position.
    position: u64,
}

impl Presentation {
    /// Emit explicit empty edits with deadline checks throughout long silence.
    fn silence(
        &mut self,
        visit: &mut impl FnMut(f32) -> Result<()>,
        check: &mut impl FnMut() -> Result<()>,
    ) -> Result<u64> {
        let mut emitted = 0_u64;
        while let Some(Edit::Silence(count)) = self.edits.get(self.next) {
            for frame in 0..*count {
                if frame % 1024 == 0 {
                    check()?;
                }
                visit(0.0)?;
            }
            emitted = emitted
                .checked_add(*count)
                .context("MP4 presentation length overflow")?;
            self.next = self
                .next
                .checked_add(1)
                .context("MP4 edit cursor overflow")?;
        }
        Ok(emitted)
    }

    /// Select one raw frame and advance every completed edit exactly once.
    pub(super) fn frame(
        &mut self,
        amplitude: f32,
        visit: &mut impl FnMut(f32) -> Result<()>,
        mut check: impl FnMut() -> Result<()>,
    ) -> Result<u64> {
        let mut emitted = self.silence(visit, &mut check)?;
        if let Some(Edit::Audio { start, end }) = self.edits.get(self.next) {
            if self.position >= *start && self.position < *end {
                visit(amplitude)?;
                emitted = emitted
                    .checked_add(1)
                    .context("MP4 presentation length overflow")?;
            }
            if self
                .position
                .checked_add(1)
                .context("MP4 frame position overflow")?
                >= *end
            {
                self.next = self
                    .next
                    .checked_add(1)
                    .context("MP4 edit cursor overflow")?;
            }
        }
        self.position = self
            .position
            .checked_add(1)
            .context("MP4 frame position overflow")?;
        Ok(emitted)
    }

    /// Reject a range past decoded media rather than inventing missing samples.
    pub(super) fn finish(
        &mut self,
        visit: &mut impl FnMut(f32) -> Result<()>,
        mut check: impl FnMut() -> Result<()>,
    ) -> Result<u64> {
        let emitted = self.silence(visit, &mut check)?;
        ensure!(
            self.next == self.edits.len(),
            "MP4 edit extends beyond decoded audio"
        );
        Ok(emitted)
    }
}

/// Checked framing locations; encoded sample payloads are never retained.
#[derive(Clone, Copy)]
struct Atom {
    /// Four-byte container box type.
    kind: [u8; 4],
    /// First byte after the normal or extended framing header.
    body: u64,
    /// Exclusive boundary, checked against its enclosing box.
    end: u64,
}

/// Deadline-aware metadata walker with a fixed allocation ceiling.
struct Reader<F> {
    /// Validated regular upload file.
    file: File,
    /// Total framing boxes inspected across every nesting level.
    boxes: usize,
    /// Job deadline and cancellation check.
    check: F,
}

impl<F: FnMut() -> Result<()>> Reader<F> {
    /// Read one fixed-size field after checking its container boundary.
    fn field<const N: usize>(&mut self, atom: Atom, offset: u64) -> Result<[u8; N]> {
        (self.check)()?;
        let start = atom
            .body
            .checked_add(offset)
            .context("MP4 field offset overflow")?;
        ensure!(
            start
                .checked_add(u64::try_from(N)?)
                .is_some_and(|end| end <= atom.end),
            "truncated MP4 timing field"
        );
        let _position = self.file.seek(SeekFrom::Start(start))?;
        let mut bytes = [0; N];
        self.file.read_exact(&mut bytes)?;
        Ok(bytes)
    }

    /// Walk only direct child headers, skipping bounded payload extents.
    fn children(&mut self, start: u64, end: u64) -> Result<Vec<Atom>> {
        let mut cursor = start;
        let mut atoms = Vec::new();
        while cursor < end {
            (self.check)()?;
            ensure!(
                self.boxes < MAX_BOXES
                    && end
                        .checked_sub(cursor)
                        .is_some_and(|remaining| remaining >= 8),
                "MP4 timing box budget or extent invalid"
            );
            self.boxes = self
                .boxes
                .checked_add(1)
                .context("MP4 box count overflow")?;
            let outer = Atom {
                kind: [0; 4],
                body: cursor,
                end,
            };
            let size = u32::from_be_bytes(self.field::<4>(outer, 0)?);
            let kind = self.field::<4>(outer, 4)?;
            let (size, header) = match size {
                0 => (
                    end.checked_sub(cursor)
                        .context("invalid MP4 box boundary")?,
                    8,
                ),
                1 => (u64::from_be_bytes(self.field::<8>(outer, 8)?), 16),
                size => (u64::from(size), 8),
            };
            let next = cursor
                .checked_add(size)
                .context("MP4 box extent overflow")?;
            ensure!(
                size >= header && next <= end,
                "invalid MP4 timing box extent"
            );
            atoms.push(Atom {
                kind,
                body: cursor
                    .checked_add(header)
                    .context("MP4 box header overflow")?,
                end: next,
            });
            cursor = next;
        }
        Ok(atoms)
    }

    /// Decode one bounded edit, validating media time before rate compatibility.
    fn edit_entry(&mut self, elst: Atom, offset: u64, version: u8) -> Result<(u64, i64, bool)> {
        let (duration, start, rate_offset) = if version == 0 {
            (
                u64::from(u32::from_be_bytes(self.field::<4>(elst, offset)?)),
                i64::from(i32::from_be_bytes(
                    self.field::<4>(
                        elst,
                        offset
                            .checked_add(4)
                            .context("MP4 edit field offset overflow")?,
                    )?,
                )),
                offset
                    .checked_add(8)
                    .context("MP4 edit field offset overflow")?,
            )
        } else {
            (
                u64::from_be_bytes(self.field::<8>(elst, offset)?),
                i64::from_be_bytes(
                    self.field::<8>(
                        elst,
                        offset
                            .checked_add(8)
                            .context("MP4 edit field offset overflow")?,
                    )?,
                ),
                offset
                    .checked_add(16)
                    .context("MP4 edit field offset overflow")?,
            )
        };
        ensure!(start >= -1, "invalid MP4 edit media time");
        let non_unit_rate = self.field::<4>(elst, rate_offset)? != [0, 1, 0, 0];
        Ok((duration, start, non_unit_rate))
    }

    /// Validate full-box version and select its 32/64-bit timestamp fields.
    fn timestamp_offset(&mut self, atom: Atom) -> Result<u64> {
        match self.field::<1>(atom, 0)? {
            [0] => Ok(12),
            [1] => Ok(20),
            _ => anyhow::bail!("invalid MP4 timing version"),
        }
    }

    /// Decode a nonzero movie or media timescale.
    fn timescale(&mut self, atom: Atom) -> Result<u32> {
        let offset = self.timestamp_offset(atom)?;
        let scale = u32::from_be_bytes(self.field::<4>(atom, offset)?);
        ensure!(scale > 0, "zero MP4 timescale");
        Ok(scale)
    }
}

/// Require exactly one structural timing box rather than choosing duplicates.
fn one(atoms: &[Atom], kind: [u8; 4]) -> Result<Atom> {
    let mut found = atoms.iter().filter(|atom| atom.kind == kind);
    let atom = *found.next().context("missing MP4 timing box")?;
    ensure!(found.next().is_none(), "duplicate MP4 timing box");
    Ok(atom)
}

/// Recover a declared AV1/VP8/VP9 sample-entry codec for one validated MP4 track.
/// Symphonia 0.6 recognizes these entries but leaves their codec ID unset.
/// Walk structural boxes by track ID; never scan media payloads for codec tags.
pub(in crate::media) fn declared_video_codec(
    path: &Path,
    track_id: u32,
    check: impl FnMut() -> Result<()>,
) -> Result<Option<&'static str>> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "MP4 codec source is not a regular file");
    let mut reader = Reader {
        file,
        boxes: 0,
        check,
    };
    let root = reader.children(0, metadata.len())?;
    let moov = one(&root, *b"moov")?;
    let movie = reader.children(moov.body, moov.end)?;
    let mut chosen = None;
    for track in movie.iter().filter(|atom| atom.kind == *b"trak") {
        let children = reader.children(track.body, track.end)?;
        let tkhd = one(&children, *b"tkhd")?;
        let offset = reader.timestamp_offset(tkhd)?;
        if u32::from_be_bytes(reader.field::<4>(tkhd, offset)?) == track_id {
            ensure!(chosen.is_none(), "duplicate MP4 video track identifier");
            chosen = Some(children);
        }
    }
    let mut children = chosen.context("MP4 video codec track missing")?;
    for kind in [*b"mdia", *b"minf", *b"stbl"] {
        let parent = one(&children, kind)?;
        children = reader.children(parent.body, parent.end)?;
    }
    let stsd = one(&children, *b"stsd")?;
    ensure!(
        reader.field::<1>(stsd, 0)? == [0],
        "invalid MP4 sample description version"
    );
    ensure!(
        u32::from_be_bytes(reader.field::<4>(stsd, 4)?) == 1,
        "ambiguous MP4 video sample descriptions"
    );
    let entries = reader.children(
        stsd.body
            .checked_add(8)
            .context("MP4 stsd offset overflow")?,
        stsd.end,
    )?;
    ensure!(
        entries.len() == 1,
        "invalid MP4 video sample description extent"
    );
    let entry = entries.first().context("MP4 video sample entry missing")?;
    let codec = match &entry.kind {
        b"av01" => "av1",
        b"vp08" => "vp8",
        b"vp09" => "vp9",
        _ => return Ok(None),
    };
    ensure!(
        entry
            .end
            .checked_sub(entry.body)
            .is_some_and(|size| size >= 78),
        "truncated MP4 visual sample entry"
    );
    Ok(Some(codec))
}

/// Convert rational timestamps to the nearest whole decoded sample frame.
fn frames(units: u64, scale: u32, rate: u32) -> Result<u64> {
    ensure!(scale > 0 && rate > 0, "invalid MP4 timing scale");
    Ok(u64::try_from(
        u128::from(units)
            .checked_mul(u128::from(rate))
            .and_then(|numerator| numerator.checked_add(u128::from(scale) / 2))
            .and_then(|rounded| rounded.checked_div(u128::from(scale)))
            .context("MP4 sample frame conversion overflow")?,
    )?)
}

/// Compute the checked byte offset of an edit-table entry after its header.
fn edit_offset(index: u64, size: u64) -> Result<u64> {
    index
        .checked_mul(size)
        .and_then(|bytes| bytes.checked_add(8))
        .context("MP4 edit table offset overflow")
}

/// Valid but unimplemented temporal operations retain an explicit compatibility path.
fn unsupported() -> anyhow::Error {
    symphonia::core::errors::Error::Unsupported(
        "audio edit list requires the compatibility decoder",
    )
    .into()
}

/// Read unit-rate ordered edits for the track selected by the content-based demuxer.
pub(super) fn read(
    path: &Path,
    track_id: u32,
    rate: u32,
    check: impl FnMut() -> Result<()>,
) -> Result<Option<Presentation>> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    ensure!(
        metadata.is_file(),
        "MP4 timing source is not a regular file"
    );
    let mut reader = Reader {
        file,
        boxes: 0,
        check,
    };
    let root = reader.children(0, metadata.len())?;
    let moov = one(&root, *b"moov")?;
    let movie = reader.children(moov.body, moov.end)?;
    let movie_scale = reader.timescale(one(&movie, *b"mvhd")?)?;
    let mut chosen = None;
    for track in movie.iter().filter(|atom| atom.kind == *b"trak") {
        let children = reader.children(track.body, track.end)?;
        let header = one(&children, *b"tkhd")?;
        let offset = reader.timestamp_offset(header)?;
        if u32::from_be_bytes(reader.field::<4>(header, offset)?) == track_id {
            ensure!(chosen.is_none(), "duplicate MP4 track identifier");
            chosen = Some(children);
        }
    }
    let track = chosen.context("MP4 audio timing track missing")?;
    let Some(edit_container) = track.iter().find(|atom| atom.kind == *b"edts") else {
        return Ok(None);
    };
    ensure!(
        track.iter().filter(|atom| atom.kind == *b"edts").count() == 1,
        "duplicate MP4 edit container"
    );
    let edit_atoms = reader.children(edit_container.body, edit_container.end)?;
    let elst = one(&edit_atoms, *b"elst")?;
    let media_container = one(&track, *b"mdia")?;
    let media = reader.children(media_container.body, media_container.end)?;
    let media_scale = reader.timescale(one(&media, *b"mdhd")?)?;
    let [version] = reader.field::<1>(elst, 0)?;
    ensure!(version <= 1, "invalid MP4 edit version");
    let count = usize::try_from(u32::from_be_bytes(reader.field::<4>(elst, 4)?))?;
    ensure!(count <= MAX_EDITS, "MP4 edit budget exceeded");
    let size = if version == 0 { 12 } else { 20 };
    ensure!(
        elst.end.checked_sub(elst.body) == Some(edit_offset(u64::try_from(count)?, size)?),
        "invalid MP4 edit table size"
    );
    let mut edits = Vec::with_capacity(count);
    let mut movie_end = 0_u64;
    let mut previous_end = 0_u64;
    let mut compatibility = false;
    for index in 0..count {
        let offset = edit_offset(u64::try_from(index)?, size)?;
        let (duration, start, non_unit_rate) = reader.edit_entry(elst, offset, version)?;
        compatibility |= non_unit_rate;
        let old_end = frames(movie_end, movie_scale, rate)?;
        movie_end = movie_end
            .checked_add(duration)
            .context("MP4 movie duration overflow")?;
        let duration_frames = frames(movie_end, movie_scale, rate)?
            .checked_sub(old_end)
            .context("MP4 edit frame range is reversed")?;
        if duration_frames == 0 {
            continue;
        }
        if start == -1 {
            edits.push(Edit::Silence(duration_frames));
        } else {
            let start = frames(u64::try_from(start)?, media_scale, rate)?;
            let end = start
                .checked_add(duration_frames)
                .context("MP4 media duration overflow")?;
            compatibility |= start < previous_end;
            previous_end = end;
            edits.push(Edit::Audio { start, end });
        }
    }
    if compatibility {
        return Err(unsupported());
    }
    Ok((!edits.is_empty()).then_some(Presentation {
        edits,
        next: 0,
        position: 0,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_codec_declarations_obey_track_identity_and_box_boundaries() -> Result<()> {
        let original = include_bytes!("../../tests/fixtures/media/av1.mp4");
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("misleading.webm");
        std::fs::write(&path, original)?;
        ensure!(
            declared_video_codec(&path, 1, || Ok(()))? == Some("av1"),
            "AV1 declaration lost"
        );
        ensure!(
            declared_video_codec(&path, 999, || Ok(())).is_err(),
            "wrong track matched"
        );
        ensure!(
            declared_video_codec(&path, 1, || anyhow::bail!("cancelled")).is_err(),
            "cancelled inspection continued"
        );

        let stsd = original
            .windows(4)
            .position(|bytes| bytes == b"stsd")
            .context("missing sample descriptions")?;
        let entry = stsd
            .checked_add(16)
            .context("sample entry offset overflow")?;
        ensure!(
            original.get(entry..entry + 4) == Some(b"av01"),
            "fixture layout changed"
        );
        let mut unrelated_tag = original.to_vec();
        unrelated_tag
            .get_mut(entry..entry + 4)
            .context("sample entry bounds")?
            .copy_from_slice(b"zzzz");
        // An AV1-looking tag inside an unrelated free-box payload is not metadata.
        unrelated_tag.extend_from_slice(b"\0\0\0\x0cfreeav01");
        std::fs::write(&path, unrelated_tag)?;
        ensure!(
            declared_video_codec(&path, 1, || Ok(()))?.is_none(),
            "payload tag became a codec"
        );

        let mut excessive_entries = original.to_vec();
        excessive_entries
            .get_mut(stsd + 8..stsd + 12)
            .context("stsd count bounds")?
            .copy_from_slice(&u32::MAX.to_be_bytes());
        std::fs::write(&path, excessive_entries)?;
        ensure!(
            declared_video_codec(&path, 1, || Ok(())).is_err(),
            "unbounded sample descriptions accepted"
        );
        std::fs::write(
            &path,
            original
                .get(..entry + 3)
                .context("truncated fixture bounds")?,
        )?;
        ensure!(
            declared_video_codec(&path, 1, || Ok(())).is_err(),
            "truncated movie accepted"
        );
        Ok(())
    }

    /// Empty edits, gaps, and multiple forward ranges preserve presentation order.
    #[test]
    fn silence_and_forward_ranges_are_streamed_without_padding() -> Result<()> {
        let mut plan = Presentation {
            edits: vec![
                Edit::Silence(2),
                Edit::Audio { start: 1, end: 3 },
                Edit::Audio { start: 5, end: 6 },
                Edit::Silence(1),
            ],
            next: 0,
            position: 0,
        };
        let mut samples = Vec::new();
        let mut visit = |sample| {
            samples.push(sample);
            Ok(())
        };
        let mut total = 0;
        for value in 0_u16..7 {
            total += plan.frame(f32::from(value), &mut visit, || Ok(()))?;
        }
        total += plan.finish(&mut visit, || Ok(()))?;
        ensure!(
            total == 6 && samples == [0.0, 0.0, 1.0, 2.0, 5.0, 0.0],
            "MP4 timeline selection changed"
        );
        let mut missing = Presentation {
            edits: vec![Edit::Audio { start: 0, end: 2 }],
            next: 0,
            position: 0,
        };
        let _decoded_frames = missing.frame(0.5, &mut |_| Ok(()), || Ok(()))?;
        ensure!(
            missing.finish(&mut |_| Ok(()), || Ok(())).is_err(),
            "missing media was invented"
        );
        let mut long = Presentation {
            edits: vec![Edit::Silence(u64::MAX)],
            next: 0,
            position: 0,
        };
        ensure!(
            long.frame(0.0, &mut |_| Ok(()), || anyhow::bail!("cancelled"))
                .is_err(),
            "unbounded silence ignored cancellation"
        );
        Ok(())
    }

    /// A genuine AAC movie protects framing, versions and bounded edit allocations.
    #[test]
    fn malformed_movie_timing_is_rejected_before_allocation() -> Result<()> {
        let source = include_bytes!("../../tests/fixtures/media/tone.m4a");
        let elst = source
            .windows(4)
            .position(|bytes| bytes == b"elst")
            .context("fixture edit list missing")?;
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("misnamed.bin");
        for (offset, bytes) in [
            (elst + 8, u32::MAX.to_be_bytes().to_vec()),
            (elst + 4, vec![2]),
            (elst - 4, u32::MAX.to_be_bytes().to_vec()),
            (elst + 16, (-2_i32).to_be_bytes().to_vec()),
        ] {
            let mut invalid = source.to_vec();
            invalid
                .get_mut(offset..offset + bytes.len())
                .context("fixture offset missing")?
                .copy_from_slice(&bytes);
            std::fs::write(&path, invalid)?;
            let error = read(&path, 1, 48_000, || Ok(()))
                .err()
                .context("invalid edit metadata accepted")?;
            ensure!(
                !super::super::is_unsupported(&error),
                "malformed edits selected compatibility"
            );
        }
        let mut dwell = source.to_vec();
        dwell
            .get_mut(elst + 20..elst + 24)
            .context("fixture rate missing")?
            .fill(0);
        std::fs::write(&path, dwell)?;
        let error = read(&path, 1, 48_000, || Ok(()))
            .err()
            .context("unimplemented dwell was ignored")?;
        ensure!(
            super::super::is_unsupported(&error),
            "valid dwell lost its explicit compatibility path"
        );
        ensure!(
            read(&path, 1, 48_000, || anyhow::bail!("deadline")).is_err(),
            "timing parser ignored deadline"
        );
        Ok(())
    }
    /// An unsupported first edit must not hide malformed metadata in later entries.
    #[test]
    fn malformed_edit_after_valid_dwell_cannot_select_compatibility() -> Result<()> {
        let source = include_bytes!("../../tests/fixtures/media/tone.m4a");
        let elst = source
            .windows(4)
            .position(|bytes| bytes == b"elst")
            .context("fixture edit list missing")?;
        ensure!(
            source.get(elst + 8..elst + 12) == Some(&1_u32.to_be_bytes()),
            "fixture must have one version-zero edit"
        );
        let mut second = source
            .get(elst + 12..elst + 24)
            .context("fixture entry missing")?
            .to_vec();
        second
            .get_mut(4..8)
            .context("media time missing")?
            .copy_from_slice(&(-2_i32).to_be_bytes());
        let mut invalid = source.to_vec();
        invalid
            .get_mut(elst + 8..elst + 12)
            .context("count missing")?
            .copy_from_slice(&2_u32.to_be_bytes());
        invalid
            .get_mut(elst + 20..elst + 24)
            .context("rate missing")?
            .fill(0);
        for kind in [b"moov", b"trak", b"edts", b"elst"] {
            let kind_offset = source
                .windows(4)
                .position(|bytes| bytes == kind)
                .context("fixture ancestor missing")?;
            let field = invalid
                .get_mut(kind_offset - 4..kind_offset)
                .context("fixture size missing")?;
            let size = u32::from_be_bytes(field.try_into()?)
                .checked_add(12)
                .context("fixture size overflow")?;
            field.copy_from_slice(&size.to_be_bytes());
        }
        drop(invalid.splice(elst + 24..elst + 24, second));
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("dwell-then-invalid.m4a");
        std::fs::write(&path, invalid)?;
        let error = read(&path, 1, 48_000, || Ok(()))
            .err()
            .context("malformed later edit accepted")?;
        ensure!(
            !super::super::is_unsupported(&error),
            "unsupported first edit hid malformed later media time"
        );
        Ok(())
    }
}
