//! Incremental PCM decoding and deterministic, bounded-memory waveform images.

use anyhow::{ensure, Context as _, Result};
use image::{Rgba, RgbaImage};
#[path = "audio_mp4.rs"]
mod mp4;
#[path = "audio_opus.rs"]
mod opus;
use std::path::Path;
use std::time::{Duration, Instant};
use symphonia::core::codecs::audio::well_known::CODEC_ID_OPUS;
use symphonia::core::codecs::audio::{AudioCodecParameters, AudioDecoder, AudioDecoderOptions};
use symphonia::core::formats::TrackType;
use symphonia::core::io::{BitReaderLtr, FiniteBitStream as _, ReadBitsLtr as _};
use symphonia::core::packet::Packet;
use tokio_util::sync::CancellationToken;

/// Distinguish unavailable codecs from malformed input and resource failures.
pub(crate) fn is_unsupported(error: &anyhow::Error) -> bool {
    // Symphonia also calls missing MP4 atoms and excessive MKV geometry
    // Unsupported. Only audited codec limitations may select compatibility.
    // Opus reconstruction and packet errors fail closed.
    error.chain().any(|cause| {
        matches!(
            cause.downcast_ref::<symphonia::core::errors::Error>(),
            Some(symphonia::core::errors::Error::Unsupported(
                "Opus mapping family requires the compatibility decoder"
                    | "AAC profile needs the compatibility decoder"
                    | "audio codec needs the compatibility decoder"
                    | "Speex requires the compatibility decoder"
                    | "aac: aac too complex"
                    | "aac: spectral band replication is unsupported"
                    | "audio edit list requires the compatibility decoder"
            ))
        )
    })
}

/// Maximum interleaved samples retained for a single decoded audio packet.
const MAX_PACKET_SAMPLES: usize = 4 * 1024 * 1024;

/// Audio codecs share one packet-to-interleaved-PCM boundary.
enum PacketDecoder {
    /// Mature general-purpose pure-Rust audio decoders.
    Symphonia(Box<dyn AudioDecoder>),
    /// Bounded mono/stereo/multistream Opus with explicit channel mappings.
    Opus(Box<opus::Decoder>),
}

impl PacketDecoder {
    /// Select a decoder from validated container metadata.
    fn new(params: &AudioCodecParameters, channels: usize) -> Result<Self> {
        if params.codec != CODEC_ID_OPUS {
            if let Some(frames) = params.max_frames_per_packet {
                ensure!(
                    u128::from(frames) * u128::try_from(channels)?
                        <= u128::try_from(MAX_PACKET_SAMPLES)?,
                    "audio packet exceeds sample budget"
                );
            }
            return Ok(Self::Symphonia(
                symphonia::default::get_codecs()
                    .make_audio_decoder(params, &AudioDecoderOptions::default())?,
            ));
        }
        let header = params
            .extra_data
            .as_deref()
            .context("Opus header missing")?;
        Ok(Self::Opus(Box::new(opus::Decoder::new(header, channels)?)))
    }

    /// Decode one packet into a reused bounded float buffer.
    fn decode(
        &mut self,
        packet: &Packet,
        channels: usize,
        samples: &mut Vec<f32>,
        budget: &AudioBudget<'_>,
    ) -> Result<()> {
        match self {
            Self::Symphonia(decoder) => {
                let pcm = decoder.decode(packet).context("decode audio packet")?;
                let count = pcm.samples_interleaved();
                ensure!(
                    count <= MAX_PACKET_SAMPLES,
                    "decoded audio packet exceeds sample budget"
                );
                ensure!(
                    pcm.spec().channels().count() == channels,
                    "audio channel count changed"
                );
                samples.resize(count, 0.0);
                pcm.copy_to_slice_interleaved(samples.as_mut_slice());
            }
            Self::Opus(decoder) => {
                decoder.decode(&packet.data, samples, || budget.check())?;
            }
        }
        ensure!(
            samples.iter().all(|sample| sample.is_finite()),
            "non-finite PCM samples"
        );
        Ok(())
    }
}

/// Validate a discrete Opus identification header without decoding PCM.
pub(crate) fn validate_discrete_opus_header(
    header: &[u8],
    head: &opus_pure::OpusHead,
) -> Result<()> {
    let channels = usize::from(head.channel_count);
    opus::Decoder::validate(header, channels)?;
    ensure!(
        header.get(18) == Some(&head.mapping_family)
            && header.get(19) == Some(&head.stream_count)
            && header.get(20) == Some(&head.coupled_count)
            && header.get(21..21 + channels) == Some(head.channel_mapping.as_slice())
            && header.get(10..12) == Some(head.pre_skip.to_le_bytes().as_slice())
            && header.get(16..18) == Some(head.output_gain_q8.to_le_bytes().as_slice()),
        "Opus headers changed during validation"
    );
    Ok(())
}

/// Stream discrete Ogg layouts through the same bounded, explicitly mapped decoder.
fn visit_discrete_opus(
    input: &Path,
    header: &[u8],
    budget: &AudioBudget<'_>,
    mut visit: impl FnMut(f32) -> Result<()>,
) -> Result<u64> {
    use opus_pure::{OggOpusReader, OggPacket, Trim};
    let (source, reads) = super::probe::open_discrete_opus_source(
        input,
        budget.started,
        budget.timeout,
        budget.cancel,
    )?;
    let mut reader = OggOpusReader::new(source)?;
    validate_discrete_opus_header(header, reader.head())?;
    let channels = usize::from(reader.head().channel_count);
    let mut decoder = opus::Decoder::new(header, channels)?;
    let mut trim = Trim::new(reader.head(), 48_000, channels)?;
    let gain = 10.0_f32.powf(f32::from(reader.head().output_gain_q8) / (256.0 * 20.0));
    let mut packet = OggPacket::default();
    let mut samples = Vec::new();
    let mut frames = 0_u64;
    loop {
        budget.check()?;
        reads.start_packet();
        if !reader.read_packet_into(&mut packet)? {
            break;
        }
        decoder.decode(&packet.data, &mut samples, || budget.check())?;
        ensure!(
            samples.iter().all(|sample| sample.is_finite()),
            "non-finite Opus PCM"
        );
        for frame in trim.keep(&packet, &samples).chunks_exact(channels) {
            let amplitude = frame_amplitude(frame, gain)?;
            visit(amplitude)?;
            frames = frames
                .checked_add(1)
                .context("Opus sample count overflow")?;
        }
    }
    budget.check()?;
    ensure!(frames > 0, "Opus contains no decodable samples");
    Ok(frames)
}

/// Opus presentation metadata is independent of the PCM decoder state.
struct OpusPresentation {
    /// Exact remaining delay from the identification header, in 48 kHz samples.
    delay: usize,
    /// Linear output gain from the signed Q8 decibel header field.
    gain: f32,
    /// Container timestamp units for packet trimming.
    time_base: symphonia::core::units::TimeBase,
}

impl OpusPresentation {
    /// Read metadata from the already validated stream parameters.
    fn new(
        params: &AudioCodecParameters,
        time_base: Option<symphonia::core::units::TimeBase>,
    ) -> Result<Self> {
        let header = params
            .extra_data
            .as_deref()
            .context("Opus header missing")?;
        let delay: [u8; 2] = header
            .get(10..12)
            .context("Opus delay missing")?
            .try_into()?;
        let gain: [u8; 2] = header
            .get(16..18)
            .context("Opus gain missing")?
            .try_into()?;
        Ok(Self {
            delay: usize::from(u16::from_le_bytes(delay)),
            // OpusHead gain is Q8 dB (RFC 7845 section 5.1).
            gain: 10.0_f32.powf(f32::from(i16::from_le_bytes(gain)) / (256.0 * 20.0)),
            time_base: time_base.context("Opus time base missing")?,
        })
    }

    /// Return playable frames, removing exact header delay and container padding.
    fn samples<'a>(
        &mut self,
        packet: &Packet,
        samples: &'a [f32],
        channels: usize,
    ) -> Result<&'a [f32]> {
        let to_frames = |duration: symphonia::core::units::Duration| -> Result<usize> {
            let numerator =
                u128::from(duration.get()) * u128::from(self.time_base.numer.get()) * 48_000;
            let denominator = u128::from(self.time_base.denom.get());
            Ok(usize::try_from(
                (numerator + denominator / 2) / denominator,
            )?)
        };
        // Ogg supplies exact trimming; Matroska can round delay to milliseconds.
        let start = to_frames(packet.trim_start)?
            .max(self.delay)
            .min(samples.len() / channels);
        self.delay = self.delay.saturating_sub(start);
        let end = to_frames(packet.trim_end)?;
        let first = start.checked_mul(channels).context("Opus trim overflow")?;
        let last = samples
            .len()
            .checked_sub(end.checked_mul(channels).context("Opus trim overflow")?)
            .context("Opus padding exceeds packet")?;
        samples.get(first..last).context("Opus trim exceeds packet")
    }
}

/// Deadline/cancellation policy shared by both streaming waveform passes.
struct AudioBudget<'a> {
    /// Time at the start of the job.
    started: Instant,
    /// Existing operator media-processing timeout.
    timeout: Duration,
    /// Worker shutdown cancellation token.
    cancel: &'a CancellationToken,
}

/// Detect backward-compatible HE-AAC signaling after an AAC-LC configuration.
///
/// Symphonia 0.6.1 skips this sync extension when no explicit SBR prefix was
/// present, decoding only the low-rate core while reporting AAC-LC. Inspect the
/// bounded configuration, including extension frequency, before choosing it.
fn aac_needs_compatibility(extra: &[u8]) -> Result<bool> {
    ensure!(extra.len() <= 64, "AAC configuration exceeds safety budget");
    let mut bits = BitReaderLtr::new(extra);
    let profile = bits.read_bits_leq32(5)?;
    ensure!(matches!(profile, 1..=5 | 29), "unvalidated AAC profile");
    read_aac_frequency(&mut bits)?;
    let channels = bits.read_bits_leq32(4)?;
    ensure!(channels <= 7, "invalid AAC channel configuration");
    let explicit_sbr = matches!(profile, 5 | 29);
    if explicit_sbr {
        read_aac_frequency(&mut bits)?;
        ensure!(bits.read_bits_leq32(5)? == 2, "invalid HE-AAC core profile");
    }
    let _short_frame = bits.read_bool()?;
    if bits.read_bool()? {
        bits.ignore_bits(14)?;
    }
    if bits.read_bool()? {
        let _extension_flag3 = bits.read_bool()?;
    }
    if explicit_sbr || profile != 2 {
        return Ok(true);
    }
    // PCE channel layouts are handled by the decoder's explicit limitation.
    if channels == 0 || bits.bits_left() < 16 {
        return Ok(false);
    }
    if bits.read_bits_leq32(11)? != 0x2b7 {
        return Ok(false);
    }
    ensure!(
        bits.read_bits_leq32(5)? == 5,
        "invalid AAC SBR extension type"
    );
    let sbr = bits.read_bool()?;
    if sbr {
        read_aac_frequency(&mut bits)?;
        if bits.bits_left() >= 12 && bits.read_bits_leq32(11)? == 0x548 {
            let _parametric_stereo = bits.read_bool()?;
        }
    }
    Ok(sbr)
}

/// Validate a sampling-frequency index or a nonzero explicit 24-bit frequency.
fn read_aac_frequency(bits: &mut BitReaderLtr<'_>) -> Result<()> {
    let index = bits.read_bits_leq32(4)?;
    if index == 15 {
        ensure!(
            bits.read_bits_leq32(24)? > 0,
            "invalid AAC sampling frequency"
        );
    } else {
        ensure!(index <= 12, "invalid AAC sampling frequency index");
    }
    Ok(())
}

impl AudioBudget<'_> {
    /// Check the deadline before decoding another packet or publishing pixels.
    fn check(&self) -> Result<()> {
        ensure!(
            !self.cancel.is_cancelled(),
            "audio processing cancelled during shutdown"
        );
        ensure!(
            self.started.elapsed() < self.timeout,
            "audio processing timed out"
        );
        Ok(())
    }
}

/// Reject audited unavailable codecs while distinguishing malformed configurations.
fn validate_audio_codec(params: &AudioCodecParameters) -> Result<bool> {
    if params.codec == symphonia::core::codecs::audio::well_known::CODEC_ID_AAC
        && params.extra_data.as_deref().map_or_else(
            || Ok(params.profile.is_some_and(|profile| {
                profile != symphonia::core::codecs::audio::well_known::profiles::CODEC_PROFILE_AAC_LC
            })),
            aac_needs_compatibility,
        )?
    {
        return Err(symphonia::core::errors::Error::Unsupported(
            "AAC profile needs the compatibility decoder",
        )
        .into());
    }
    let is_opus = params.codec == CODEC_ID_OPUS;
    if !is_opus
        && symphonia::default::get_codecs()
            .get_audio_decoder(params.codec)
            .is_none()
    {
        return Err(symphonia::core::errors::Error::Unsupported(
            "audio codec needs the compatibility decoder",
        )
        .into());
    }
    Ok(is_opus)
}

/// Mean absolute channel amplitude avoids cancellation of out-of-phase stereo.
fn frame_amplitude(frame: &[f32], gain: f32) -> Result<f32> {
    let channels = u16::try_from(frame.len())?;
    ensure!(channels > 0, "empty audio channel frame");
    Ok(frame
        .iter()
        .map(|sample| (sample * gain).abs().min(1.0))
        .sum::<f32>()
        / f32::from(channels))
}

/// Walk decoded channel frames without retaining the clip in memory.
fn visit_samples(
    input: &Path,
    budget: &AudioBudget<'_>,
    mut visit: impl FnMut(f32) -> Result<()>,
) -> Result<u64> {
    budget.check()?;
    if super::probe::is_speex(input, budget.started, budget.timeout, Some(budget.cancel))? {
        return Err(symphonia::core::errors::Error::Unsupported(
            "Speex requires the compatibility decoder",
        )
        .into());
    }
    if let Some(header) = super::probe::discrete_opus_header(
        input,
        budget.started,
        budget.timeout,
        Some(budget.cancel),
    )? {
        return visit_discrete_opus(input, &header, budget, visit);
    }
    let (mut format, read_budget) =
        super::probe::open_audio_format(input, budget.started, budget.timeout, budget.cancel)?;
    let track = format
        .first_track(TrackType::Audio)
        .context("media has no audio track")?;
    let track_id = track.id;
    let params = track
        .codec_params
        .as_ref()
        .and_then(symphonia::core::codecs::CodecParameters::audio)
        .context("audio codec parameters missing")?;
    let mut movie_presentation = if format.format_info().short_name == "isomp4" {
        mp4::read(
            input,
            track_id,
            params
                .sample_rate
                .context("MP4 audio sample rate missing")?,
            || budget.check(),
        )?
    } else {
        None
    };
    let is_opus = validate_audio_codec(params)?;
    let mut opus = if is_opus {
        Some(OpusPresentation::new(params, track.time_base)?)
    } else {
        None
    };
    let gain = opus.as_ref().map_or(1.0, |presentation| presentation.gain);
    let channels = params
        .channels
        .as_ref()
        .context("audio channels missing")?
        .count();
    ensure!(
        (1..=255).contains(&channels),
        "audio channels exceed safety limit"
    );
    let mut decoder = PacketDecoder::new(params, channels)?;
    if is_opus && movie_presentation.is_some() {
        return Err(symphonia::core::errors::Error::Unsupported(
            "audio edit list requires the compatibility decoder",
        )
        .into());
    }
    let mut samples = Vec::new();
    let mut frames = 0_u64;
    loop {
        budget.check()?;
        read_budget.start_packet();
        let Some(packet) = format.next_packet().context("read audio packet")? else {
            break;
        };
        if packet.track_id != track_id {
            continue;
        }
        decoder.decode(&packet, channels, &mut samples, budget)?;
        let presentation = if let Some(opus) = &mut opus {
            opus.samples(&packet, &samples, channels)?
        } else {
            samples.as_slice()
        };
        // Mean absolute channel amplitude avoids cancelling out-of-phase stereo.
        for frame in presentation.chunks_exact(channels) {
            let amplitude = frame_amplitude(frame, gain)?;
            let emitted = if let Some(presentation) = &mut movie_presentation {
                presentation.frame(amplitude, &mut visit, || budget.check())?
            } else {
                visit(amplitude)?;
                1
            };
            frames = frames
                .checked_add(emitted)
                .context("audio sample count overflow")?;
        }
    }
    if let Some(presentation) = &mut movie_presentation {
        frames = frames
            .checked_add(presentation.finish(&mut visit, || budget.check())?)
            .context("audio sample count overflow")?;
    }
    ensure!(frames > 0, "audio contains no decodable samples");
    budget.check()?;
    Ok(frames)
}

/// Render an audio waveform with exact bucket boundaries and bounded memory.
///
/// A first streaming pass measures decoded duration; a second fills width-sized
/// peak buckets. This also handles containers without reliable duration metadata.
///
/// # Errors
/// Returns an error for malformed/unsupported audio, resource limits, cancellation,
/// or output I/O failures. Call on a blocking worker, not an async executor.
pub fn render_waveform(
    input: &Path,
    output: &Path,
    width: u32,
    height: u32,
    cancel: &CancellationToken,
) -> Result<()> {
    super::images::validate_dimensions(width, height)?;
    let budget = AudioBudget {
        started: Instant::now(),
        timeout: Duration::from_secs(crate::config::ffmpeg_timeout_secs()),
        cancel,
    };
    let total = visit_samples(input, &budget, |_| Ok(()))?;
    let mut peaks = vec![0.0_f32; usize::try_from(width)?];
    let mut position = 0_u64;
    let actual = visit_samples(input, &budget, |amplitude| {
        let bucket = u128::from(position) * u128::from(width) / u128::from(total);
        let index = usize::try_from(bucket.min(u128::from(width - 1)))?;
        let peak = peaks
            .get_mut(index)
            .context("waveform bucket out of range")?;
        *peak = peak.max(amplitude);
        position = position
            .checked_add(1)
            .context("waveform position overflow")?;
        Ok(())
    })?;
    ensure!(actual == total, "audio changed between waveform passes");
    let pixels = render_peaks(&peaks, width, height)?;
    budget.check()?;
    pixels
        .save_with_format(output, image::ImageFormat::Png)
        .context("write waveform PNG")
}

/// Draw a symmetric gray amplitude envelope on a transparent canvas.
#[expect(
    clippy::cast_possible_truncation,
    clippy::cast_sign_loss,
    clippy::as_conversions,
    reason = "clamped normalized amplitudes map to a bounded pixel radius"
)]
fn render_peaks(peaks: &[f32], width: u32, height: u32) -> Result<RgbaImage> {
    let mut image = RgbaImage::new(width, height);
    let middle = height / 2;
    let max_radius = middle.saturating_sub(1);
    let radius_scale = f32::from(u16::try_from(max_radius).context("waveform is too tall")?);
    for (column, peak) in peaks.iter().enumerate() {
        let radius = (peak.clamp(0.0, 1.0) * radius_scale).round() as u32;
        let x = u32::try_from(column)?;
        for y in middle.saturating_sub(radius)..=middle.saturating_add(radius).min(height - 1) {
            image.put_pixel(x, y, Rgba([0x88, 0x88, 0x88, 255]));
        }
    }
    Ok(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compatibility_errors_exclude_malformed_packets_io_and_resource_failures() {
        assert!(is_unsupported(
            &symphonia::core::errors::Error::Unsupported(
                "audio codec needs the compatibility decoder"
            )
            .into()
        ));
        for error in [
            anyhow::Error::from(opus_pure::Error::Internal("transform failure")),
            anyhow::Error::from(opus_pure::Error::InvalidPacket("invalid framing")),
            anyhow::Error::from(opus_pure::Error::InvalidArgument("output buffer budget")),
            anyhow::Error::from(symphonia::core::errors::Error::DecodeError(
                "invalid header",
            )),
            anyhow::Error::from(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "deadline",
            )),
            anyhow::anyhow!("audio packet exceeds sample budget"),
        ] {
            assert!(!is_unsupported(&error));
        }
        for message in [
            "core (probe): no suitable format reader found",
            "isomp4: missing ftyp atom",
            "isomp4: missing moov atom",
            "mkv: missing segment element",
            "mkv: video width too large",
            "mkv: timestamp scale too large (report this)",
            "ogg: page is not marked as first",
            "aac: sample rate is required",
            "aac: coupling channel element",
            "aac: program config",
            "aac: predictor data",
            "aac: gain control data",
            "adts: only 1 aac frame per adts packet is supported",
        ] {
            assert!(
                !is_unsupported(&symphonia::core::errors::Error::Unsupported(message).into()),
                "invalid/resource error selected fallback: {message}"
            );
        }
    }

    #[test]
    fn all_supported_audio_fixtures_produce_deterministic_visible_waveforms() -> Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media");
        let dir = tempfile::tempdir()?;
        let first = dir.path().join("first.png");
        let second = dir.path().join("second.png");
        for file in [
            "tone.wav",
            "tone.mp3",
            "tone.flac",
            "tone.ogg",
            "tone.opus",
            "speech-mode.opus",
            "surround.opus",
            "tone.aac",
            "tone.m4a",
            "tone-alac.m4a",
            "audio.webm",
            "audio.mkv",
        ] {
            render_waveform(
                &root.join(file),
                &first,
                250,
                125,
                &CancellationToken::new(),
            )
            .with_context(|| format!("waveform for {file}"))?;
            render_waveform(
                &root.join(file),
                &second,
                250,
                125,
                &CancellationToken::new(),
            )?;
            ensure!(
                std::fs::read(&first)? == std::fs::read(&second)?,
                "{file} waveform is not deterministic"
            );
            let pixels = image::open(&first)?.into_rgba8();
            ensure!(
                pixels.dimensions() == (250, 125),
                "waveform dimensions changed"
            );
            ensure!(
                pixels
                    .pixels()
                    .filter(|pixel| pixel.0.get(3) == Some(&255))
                    .count()
                    > 250,
                "{file} waveform looks silent"
            );
        }
        Ok(())
    }

    /// Build valid PCM without any executable or codec fixture generator.
    fn write_pcm(path: &Path, channels: u16, frames: u32, silence: bool) -> Result<()> {
        use std::io::Write as _;
        let data_len = frames
            .checked_mul(u32::from(channels) * 2)
            .context("PCM length")?;
        let mut file = std::fs::File::create(path)?;
        file.write_all(b"RIFF")?;
        file.write_all(&(data_len + 36).to_le_bytes())?;
        file.write_all(b"WAVEfmt ")?;
        file.write_all(&16_u32.to_le_bytes())?;
        file.write_all(&1_u16.to_le_bytes())?;
        file.write_all(&channels.to_le_bytes())?;
        file.write_all(&8000_u32.to_le_bytes())?;
        file.write_all(&(8000 * u32::from(channels) * 2).to_le_bytes())?;
        file.write_all(&(channels * 2).to_le_bytes())?;
        file.write_all(&16_u16.to_le_bytes())?;
        file.write_all(b"data")?;
        file.write_all(&data_len.to_le_bytes())?;
        let mut samples = Vec::with_capacity(usize::try_from(data_len)?);
        for frame in 0..frames {
            for channel in 0..channels {
                let sample = if silence {
                    0_i16
                } else if (frame + u32::from(channel)) % 2 == 0 {
                    16_000_i16
                } else {
                    -16_000_i16
                };
                samples.extend_from_slice(&sample.to_le_bytes());
            }
        }
        file.write_all(&samples)?;
        Ok(())
    }

    #[test]
    fn actual_silence_single_frame_surround_and_long_pcm_are_streamed() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let input = dir.path().join("sound.wav");
        let output = dir.path().join("wave.png");
        for (channels, frames, silence) in [
            (1, 1, false),
            (6, 800, false),
            (2, 480, true),
            (1, 960_000, true),
        ] {
            write_pcm(&input, channels, frames, silence)?;
            render_waveform(&input, &output, 250, 125, &CancellationToken::new())?;
            let pixels = image::open(&output)?.into_rgba8();
            let ink = pixels
                .pixels()
                .filter(|pixel| pixel.0.get(3) == Some(&255))
                .count();
            ensure!(
                pixels.dimensions() == (250, 125),
                "PCM waveform dimensions changed"
            );
            if silence {
                ensure!(ink == 250, "silence should be a flat line");
            } else {
                ensure!(ink > 250, "opposite-phase/single-frame audio disappeared");
            }
        }
        Ok(())
    }

    #[test]
    fn opus_encoder_delay_and_end_padding_do_not_extend_the_waveform() -> Result<()> {
        let input = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media/tone.opus");
        let cancel = CancellationToken::new();
        let budget = AudioBudget {
            started: Instant::now(),
            timeout: Duration::from_secs(30),
            cancel: &cancel,
        };
        ensure!(
            visit_samples(&input, &budget, |_| Ok(()))? == 5760,
            "120ms Opus clip includes encoder delay/padding"
        );
        Ok(())
    }

    #[test]
    fn silence_malformed_and_cancellation_are_safe() -> Result<()> {
        let image = render_peaks(&[0.0, 0.0, 0.0], 3, 2)?;
        ensure!(
            image
                .pixels()
                .filter(|pixel| pixel.0.get(3) == Some(&255))
                .count()
                == 3,
            "silence should be a flat center line"
        );
        let dir = tempfile::tempdir()?;
        let source = dir.path().join("bad.wav");
        let output = dir.path().join("wave.png");
        std::fs::write(&source, b"RIFF\xff\xff\xff\xffWAVE")?;
        ensure!(
            render_waveform(&source, &output, 250, 125, &CancellationToken::new()).is_err(),
            "malformed audio decoded"
        );
        ensure!(!output.exists(), "malformed audio published output");
        let cancel = CancellationToken::new();
        cancel.cancel();
        ensure!(
            render_waveform(&source, &output, 250, 125, &cancel).is_err(),
            "cancelled decoding continued"
        );
        Ok(())
    }

    #[test]
    fn malformed_container_errors_cannot_select_compatibility() -> Result<()> {
        let dir = tempfile::tempdir()?;
        let source = dir.path().join("bad.m4a");
        let output = dir.path().join("wave.png");
        for bytes in [
            b"not an audio container".as_slice(),
            b"\0\0\0\x18ftypisom\0\0\x02\0isomiso2",
            b"OggS\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0\0",
            b"\x1a\x45\xdf\xa3\x01",
        ] {
            std::fs::write(&source, bytes)?;
            let error = render_waveform(&source, &output, 250, 125, &CancellationToken::new())
                .err()
                .context("malformed audio was decoded")?;
            ensure!(
                !is_unsupported(&error),
                "malformed input selected fallback: {error:#}"
            );
            ensure!(!output.exists(), "malformed input published a waveform");
        }
        Ok(())
    }

    /// libopus reference samples verify exact duration, mapping, padding and gain.
    #[test]
    fn mapped_opus_matches_independent_pcm_without_external_tools() -> Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media");
        let cancel = CancellationToken::new();
        for name in ["opus-surround-distinct", "opus-surround-padded-gain"] {
            let expected = std::fs::read(root.join(format!("{name}-native.f32")))?;
            ensure!(
                expected.len().is_multiple_of(24),
                "invalid six-channel reference"
            );
            let expected = expected
                .as_chunks::<24>()
                .0
                .iter()
                .map(|frame| {
                    frame
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|sample| f32::from_le_bytes(*sample).abs().min(1.0))
                        .sum::<f32>()
                        / 6.0
                })
                .collect::<Vec<_>>();
            let mut actual = Vec::new();
            let budget = AudioBudget {
                started: Instant::now(),
                timeout: Duration::from_secs(10),
                cancel: &cancel,
            };
            let frames = visit_samples(&root.join(format!("{name}.opus")), &budget, |amplitude| {
                actual.push(amplitude);
                Ok(())
            })
            .with_context(|| format!("decode {name}"))?;
            ensure!(
                frames == 11_520 && actual.len() == expected.len(),
                "Opus presentation duration changed for {name}"
            );
            ensure!(
                actual
                    .iter()
                    .zip(&expected)
                    .all(|(a, b)| (a - b).abs() <= 0.000_04),
                "Opus amplitude differs from libopus for {name}"
            );
        }
        Ok(())
    }

    /// The native reference applies the movie edit list, including encoder delay.
    #[test]
    fn aac_lc_movie_edits_preserve_presentation_duration_and_waveform() -> Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media");
        let bytes = std::fs::read(root.join("aac-lc-native.f32"))?;
        let expected = bytes
            .as_chunks::<8>()
            .0
            .iter()
            .map(|frame| {
                frame
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|sample| f32::from_le_bytes(*sample).abs())
                    .sum::<f32>()
                    / 2.0
            })
            .collect::<Vec<_>>();
        let cancel = CancellationToken::new();
        let budget = AudioBudget {
            started: Instant::now(),
            timeout: Duration::from_secs(10),
            cancel: &cancel,
        };
        let mut actual = Vec::new();
        let frames = visit_samples(&root.join("tone.m4a"), &budget, |amplitude| {
            actual.push(amplitude);
            Ok(())
        })?;
        ensure!(
            frames == 5_760 && actual.len() == expected.len(),
            "AAC encoder padding changed movie presentation"
        );
        ensure!(
            actual
                .iter()
                .zip(&expected)
                .all(|(a, b)| (a - b).abs() <= 0.002),
            "AAC movie waveform differs from independent decoder"
        );
        Ok(())
    }

    #[test]
    fn implicit_he_aac_is_unsupported_and_truncated_extensions_are_invalid() -> Result<()> {
        ensure!(
            aac_needs_compatibility(&[0x13, 0x10, 0x56, 0xe5, 0x98])?,
            "implicit SBR was ignored"
        );
        ensure!(
            !aac_needs_compatibility(&[0x11, 0x90])?,
            "AAC-LC selected compatibility"
        );
        let error = aac_needs_compatibility(&[0x13, 0x10, 0x56, 0xe5])
            .err()
            .context("truncated SBR configuration was accepted")?;
        ensure!(!is_unsupported(&error), "truncated SBR selected fallback");
        for bytes in [&[0x28][..], &[0x00, 0x90], &[0x17, 0x10]] {
            let error = aac_needs_compatibility(bytes)
                .err()
                .context("invalid AAC config accepted")?;
            ensure!(
                !is_unsupported(&error),
                "invalid AAC config selected fallback"
            );
        }
        Ok(())
    }
    #[test]
    fn in_band_sbr_never_publishes_core_only_waveforms() -> Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media");
        let input = root.join("he-aac-inband.aac");
        let dir = tempfile::tempdir()?;
        let output = dir.path().join("wave.png");
        let cancel = CancellationToken::new();
        let error = render_waveform(&input, &output, 100, 50, &cancel)
            .err()
            .context("in-band HE-AAC was decoded as core-only AAC-LC")?;
        ensure!(
            is_unsupported(&error),
            "SBR did not select explicit compatibility: {error:#}"
        );
        ensure!(!output.exists(), "incomplete SBR waveform was published");
        let (mut format, _) = super::super::probe::open_audio_format(
            &input,
            Instant::now(),
            Duration::from_secs(5),
            &cancel,
        )?;
        let track = format
            .first_track(TrackType::Audio)
            .context("no AAC track")?;
        let params = track
            .codec_params
            .as_ref()
            .and_then(symphonia::core::codecs::CodecParameters::audio)
            .context("no AAC parameters")?
            .clone();
        let channels = params.channels.as_ref().context("no AAC channels")?.count();
        let packet = format.next_packet()?.context("no AAC packet")?;
        let budget = AudioBudget {
            started: Instant::now(),
            timeout: Duration::from_secs(5),
            cancel: &cancel,
        };
        for removed in 1..=3 {
            let mut truncated = packet.clone();
            truncated.data = packet
                .data
                .get(
                    ..packet
                        .data
                        .len()
                        .checked_sub(removed)
                        .context("AAC packet too short")?,
                )
                .context("AAC fixture truncation outside packet")?
                .to_vec()
                .into_boxed_slice();
            let error = PacketDecoder::new(&params, channels)?
                .decode(&truncated, channels, &mut Vec::new(), &budget)
                .err()
                .context("truncated AAC raw-data block was accepted")?;
            ensure!(
                !is_unsupported(&error),
                "truncated SBR payload selected compatibility: {error:#}"
            );
        }
        Ok(())
    }
}
