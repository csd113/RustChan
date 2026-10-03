//! Bounded Opus multistream decoding with RFC 6716 Appendix B framing.
//!
//! Codec reconstruction comes from `opus-pure`. This adapter preserves the
//! container's explicit mapping and padding, including discrete-channel layouts.

use anyhow::{ensure, Context as _, Result};
use opus_pure::{OpusDecoder, MAX_PACKET_SAMPLES};

/// One decoder per coded mono/stereo stream, with explicit output-channel mapping.
#[derive(Debug)]
pub(super) struct Decoder {
    /// Independent state for each coded mono or coupled stereo stream.
    streams: Vec<OpusDecoder>,
    /// Number of initial streams that each reconstruct two channels.
    coupled: usize,
    /// Output-channel indices into coded PCM; 255 represents silence.
    mapping: Vec<u8>,
    /// Reused single-stream interleaved reconstruction buffer.
    pcm: Vec<f32>,
    /// Reused ordinary packet after removing self-delimiting framing.
    packet: Vec<u8>,
}

impl Decoder {
    /// Validate the identification header before allocating decoder state.
    pub(super) fn new(header: &[u8], channels: usize) -> Result<Self> {
        let (streams, coupled, mapping) = Self::layout(header, channels)?;
        let streams = (0..streams)
            .map(|index| OpusDecoder::new(48_000, if index < coupled { 2 } else { 1 }))
            .collect::<std::result::Result<Vec<_>, _>>()?;
        Ok(Self {
            streams,
            coupled,
            mapping,
            pcm: Vec::new(),
            packet: Vec::new(),
        })
    }

    /// Validate metadata without allocating codec reconstruction state during probing.
    pub(super) fn validate(header: &[u8], channels: usize) -> Result<()> {
        let _validated_layout = Self::layout(header, channels)?;
        Ok(())
    }

    /// Decode the bounded header layout, including arbitrary and silent channel mappings.
    fn layout(header: &[u8], channels: usize) -> Result<(usize, usize, Vec<u8>)> {
        ensure!((1..=255).contains(&channels), "invalid Opus channel count");
        ensure!(header.get(..8) == Some(b"OpusHead"), "invalid Opus header");
        ensure!(
            header.get(8).is_some_and(|version| version >> 4_i32 == 0),
            "unsupported Opus header version"
        );
        ensure!(
            header.get(9).copied().map(usize::from) == Some(channels),
            "Opus channels disagree with container"
        );
        let family = *header.get(18).context("truncated Opus mapping")?;
        let (streams, coupled, mapping) = if family == 0 {
            ensure!(
                (1..=2).contains(&channels),
                "invalid single-stream Opus channels"
            );
            (
                1,
                channels
                    .checked_sub(1)
                    .context("invalid Opus single-stream channels")?,
                if channels == 1 { vec![0] } else { vec![0, 1] },
            )
        } else {
            let streams = usize::from(*header.get(19).context("truncated Opus stream count")?);
            let coupled = usize::from(*header.get(20).context("truncated Opus coupled count")?);
            let mapping = header
                .get(
                    21..21_usize
                        .checked_add(channels)
                        .context("Opus channel map size overflow")?,
                )
                .context("truncated Opus channel map")?;
            let coded_channels = streams
                .checked_add(coupled)
                .context("Opus coded channel count overflow")?;
            ensure!(
                streams > 0 && coupled <= streams && coded_channels <= 255,
                "invalid Opus stream mapping"
            );
            ensure!(
                mapping
                    .iter()
                    .all(|slot| *slot == 255 || usize::from(*slot) < coded_channels),
                "invalid Opus channel mapping"
            );
            if !matches!(family, 1 | 2 | 255) {
                return Err(symphonia::core::errors::Error::Unsupported(
                    "Opus mapping family requires the compatibility decoder",
                )
                .into());
            }
            ensure!(
                family != 1 || channels <= 8,
                "invalid surround Opus channel count"
            );
            (streams, coupled, mapping.to_vec())
        };
        Ok((streams, coupled, mapping))
    }

    /// Decode every stream and reject differing packet durations before publication.
    pub(super) fn decode(
        &mut self,
        data: &[u8],
        output: &mut Vec<f32>,
        mut check: impl FnMut() -> Result<()>,
    ) -> Result<usize> {
        ensure!(!data.is_empty(), "empty Opus packet");
        let channels = self.mapping.len();
        output.clear();
        output.resize(
            MAX_PACKET_SAMPLES
                .checked_mul(channels)
                .context("Opus buffer overflow")?,
            0.0,
        );
        let mut remaining = data;
        let mut frames = None;
        let count = self.streams.len();
        for (index, decoder) in self.streams.iter_mut().enumerate() {
            check()?;
            let stream_channels = if index < self.coupled { 2 } else { 1 };
            self.pcm.resize(
                MAX_PACKET_SAMPLES
                    .checked_mul(stream_channels)
                    .context("Opus PCM buffer overflow")?,
                0.0,
            );
            let encoded = if index.checked_add(1) == Some(count) {
                remaining
            } else {
                let consumed = normalize_self_delimited(remaining, &mut self.packet)?;
                remaining = remaining
                    .get(consumed..)
                    .context("invalid Opus packet extent")?;
                &self.packet
            };
            let decoded = decoder.decode(encoded, MAX_PACKET_SAMPLES, &mut self.pcm)?;
            ensure!(
                decoded > 0 && decoded <= MAX_PACKET_SAMPLES,
                "invalid decoded Opus duration"
            );
            if let Some(expected) = frames {
                ensure!(decoded == expected, "Opus streams have different durations");
            }
            frames = Some(decoded);
            let base = if index < self.coupled {
                index
                    .checked_mul(2)
                    .context("Opus coupled channel offset overflow")?
            } else {
                index
                    .checked_add(self.coupled)
                    .context("Opus channel offset overflow")?
            };
            let end = base
                .checked_add(stream_channels)
                .context("Opus stream channel range overflow")?;
            for (channel, mapped) in self.mapping.iter().copied().enumerate() {
                let mapped = usize::from(mapped);
                if mapped >= base && mapped < end {
                    let stream_channel = mapped
                        .checked_sub(base)
                        .context("invalid Opus channel offset")?;
                    for (target, source) in output
                        .chunks_exact_mut(channels)
                        .take(decoded)
                        .zip(self.pcm.chunks_exact(stream_channels))
                    {
                        *target
                            .get_mut(channel)
                            .context("Opus output channel is missing")? = *source
                            .get(stream_channel)
                            .context("Opus coded channel is missing")?;
                    }
                }
            }
        }
        let frames = frames.context("Opus packet has no streams")?;
        output.truncate(
            frames
                .checked_mul(channels)
                .context("Opus output buffer overflow")?,
        );
        Ok(frames)
    }
}

/// Remove Appendix B's extra frame-length field, retaining ordinary framing/padding.
fn normalize_self_delimited(data: &[u8], output: &mut Vec<u8>) -> Result<usize> {
    let toc = *data.first().context("empty self-delimited Opus packet")?;
    let mut cursor = 1;
    let mut padding = 0_usize;
    let mut payload = 0_usize;
    let (count, cbr) = match toc & 3 {
        0 => (1, false),
        1 => (2, true),
        2 => {
            payload = read_size(data, &mut cursor)?;
            (2, false)
        }
        _ => {
            let flags = read_byte(data, &mut cursor)?;
            let count = usize::from(flags & 63);
            ensure!((1..=48).contains(&count), "invalid Opus frame count");
            if flags & 64 != 0 {
                loop {
                    let byte = read_byte(data, &mut cursor)?;
                    padding = padding
                        .checked_add(if byte == 255 { 254 } else { usize::from(byte) })
                        .context("Opus padding overflow")?;
                    ensure!(padding <= data.len(), "truncated Opus padding");
                    if byte != 255 {
                        break;
                    }
                }
            }
            let cbr = flags & 128 == 0;
            if !cbr {
                for _ in 1..count {
                    payload = payload
                        .checked_add(read_size(data, &mut cursor)?)
                        .context("Opus payload overflow")?;
                }
            }
            (count, cbr)
        }
    };
    let samples_per_frame = if toc & 128 != 0 {
        120 << ((toc >> 3_i32) & 3)
    } else if toc & 0x60 == 0x60 {
        if toc & 8 != 0 {
            960
        } else {
            480
        }
    } else {
        *[480, 960, 1920, 2880]
            .get(usize::from((toc >> 3_i32) & 3))
            .context("invalid Opus TOC")?
    };
    ensure!(
        count
            .checked_mul(samples_per_frame)
            .is_some_and(|samples| samples <= MAX_PACKET_SAMPLES),
        "Opus packet exceeds duration limit"
    );
    let extra_start = cursor;
    let last = read_size(data, &mut cursor)?;
    payload = if cbr {
        last.checked_mul(count)
    } else {
        payload.checked_add(last)
    }
    .context("Opus payload overflow")?;
    let end = cursor
        .checked_add(payload)
        .and_then(|size| size.checked_add(padding))
        .context("Opus packet size overflow")?;
    ensure!(end <= data.len(), "truncated self-delimited Opus packet");
    output.clear();
    output.extend_from_slice(
        data.get(..extra_start)
            .context("invalid Opus header extent")?,
    );
    output.extend_from_slice(
        data.get(cursor..end)
            .context("invalid Opus payload extent")?,
    );
    Ok(end)
}

/// Read one bounded length byte.
fn read_byte(data: &[u8], cursor: &mut usize) -> Result<u8> {
    let value = *data.get(*cursor).context("truncated Opus packet header")?;
    *cursor = cursor
        .checked_add(1)
        .context("Opus packet cursor overflow")?;
    Ok(value)
}

/// RFC 6716 Section 3.2.1 one- or two-byte frame size, always at most 1275.
fn read_size(data: &[u8], cursor: &mut usize) -> Result<usize> {
    let first = read_byte(data, cursor)?;
    Ok(if first < 252 {
        usize::from(first)
    } else {
        usize::from(read_byte(data, cursor)?)
            .checked_mul(4)
            .and_then(|high| high.checked_add(usize::from(first)))
            .context("Opus frame size overflow")?
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Retain code 0/1/2/3 framing, VBR/CBR, multi-byte lengths and trailing padding.
    #[test]
    fn self_delimited_packet_lengths_and_padding_are_exact() -> Result<()> {
        for (source, expected, consumed) in [
            (vec![0, 3, 1, 2, 3, 99], vec![0, 1, 2, 3], 5),
            (vec![1, 2, 1, 2, 3, 4, 99], vec![1, 1, 2, 3, 4], 6),
            (vec![2, 1, 2, 1, 2, 3, 99], vec![2, 1, 1, 2, 3], 6),
            (vec![3, 65, 2, 1, 7, 0, 0, 99], vec![3, 65, 2, 7, 0, 0], 7),
            (vec![3, 130, 1, 2, 7, 8, 9, 99], vec![3, 130, 1, 7, 8, 9], 7),
        ] {
            let mut actual = Vec::new();
            ensure!(
                normalize_self_delimited(&source, &mut actual)? == consumed,
                "packet boundary changed"
            );
            ensure!(actual == expected, "ordinary framing changed");
        }
        let mut source = vec![0, 252, 0];
        source.extend_from_slice(&[7; 252]);
        source.push(99);
        let mut output = Vec::new();
        ensure!(
            normalize_self_delimited(&source, &mut output)? == 255,
            "two-byte frame size changed"
        );
        ensure!(output.len() == 253, "length field was retained");
        Ok(())
    }

    /// Compare each mapped channel against a separate libopus reference decoder.
    #[test]
    fn channel_mapping_padding_and_gain_match_independent_pcm() -> Result<()> {
        use opus_pure::{OggOpusReader, Trim};
        use std::fs::File;
        use std::path::Path;

        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media");
        for name in [
            "opus-surround-distinct",
            "opus-surround-padded-gain",
            "opus-surround-discrete",
        ] {
            let mut reader = OggOpusReader::new(File::open(root.join(format!("{name}.opus")))?)?;
            let head = reader.head().clone();
            let encoded = std::fs::read(root.join(format!("{name}.opus")))?;
            let offset = 27 + usize::from(*encoded.get(26).context("missing fixture lacing")?);
            let length = usize::from(*encoded.get(27).context("missing fixture header size")?);
            let header = encoded
                .get(offset..offset + length)
                .context("truncated fixture header")?;
            let mut decoder = Decoder::new(header, 6)?;
            let mut trim = Trim::new(&head, 48_000, 6)?;
            let gain = 10.0_f32.powf(f32::from(head.output_gain_q8) / (256.0 * 20.0));
            let reference = std::fs::read(root.join(format!("{name}-native.f32")))?;
            let reference = reference
                .as_chunks::<4>()
                .0
                .iter()
                .map(|bytes| f32::from_le_bytes(*bytes))
                .collect::<Vec<_>>();
            let mut output = Vec::new();
            let mut actual = Vec::new();
            for packet in reader.packets() {
                let packet = packet?;
                let _decoded_frames = decoder.decode(&packet.data, &mut output, || Ok(()))?;
                actual.extend(
                    trim.keep(&packet, &output)
                        .iter()
                        .map(|sample| sample * gain),
                );
            }
            ensure!(
                actual.len() == 69_120 && reference.len() == actual.len(),
                "duration changed for {name}"
            );
            // FFmpeg's libopus wrapper reorders all six-channel output to SMPTE,
            // including family 255. The adapter preserves the explicit wire map.
            let order = [0, 2, 1, 5, 3, 4];
            for (a, b) in actual
                .as_chunks::<6>()
                .0
                .iter()
                .zip(reference.as_chunks::<6>().0.iter())
            {
                for (channel, index) in order.iter().copied().enumerate() {
                    ensure!(
                        (a.get(index).context("missing mapped channel")?
                            - b.get(channel).context("missing reference channel")?)
                        .abs()
                            <= 0.000_04,
                        "channel {channel} changed for {name}"
                    );
                }
            }
        }
        Ok(())
    }

    /// Missing lengths, impossible padding, frame counts and durations fail closed.
    #[test]
    fn malformed_multistream_framing_is_rejected() {
        for source in [
            vec![],
            vec![0],
            vec![0, 252],
            vec![0, 4, 1],
            vec![3, 0],
            vec![3, 65, 255],
            vec![3, 63],
            vec![27, 3, 0],
        ] {
            assert!(normalize_self_delimited(&source, &mut Vec::new()).is_err());
        }
    }
}
