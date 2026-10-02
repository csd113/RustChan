//! Container/track inspection without spawning a tool or decoding video frames.

use anyhow::{ensure, Context as _, Result};
use std::fs::File;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use symphonia::core::codecs::{audio, video, CodecParameters};
use symphonia::core::common::Limit;
use symphonia::core::formats::{probe::Hint, FormatOptions, FormatReader, TrackType};
use symphonia::core::io::{MediaSourceStream, MediaSourceStreamOptions};
use symphonia::core::meta::MetadataOptions;
use tokio_util::sync::CancellationToken;

mod speex;

/// Maximum actual header bytes read, including seeks and repeated scans.
const MAX_HEADER_READ_BYTES: u64 = 64 * 1024 * 1024;
/// Bound encoded packet buffering before the decoder can allocate PCM.
const MAX_PACKET_READ_BYTES: u64 = 16 * 1024 * 1024;

/// Shared read window reset at each streaming audio packet boundary.
#[derive(Debug)]
pub(crate) struct ContainerReadBudget {
    /// Bytes delivered in the current parsing operation.
    bytes: AtomicU64,
    /// Current header or packet ceiling.
    limit: AtomicU64,
}

impl ContainerReadBudget {
    /// Start a bounded header scan.
    const fn new() -> Self {
        Self {
            bytes: AtomicU64::new(0),
            limit: AtomicU64::new(MAX_HEADER_READ_BYTES),
        }
    }

    /// Start a fresh encoded-packet read budget on the single blocking reader.
    pub(crate) fn start_packet(&self) {
        self.bytes.store(0, Ordering::Relaxed);
        self.limit.store(MAX_PACKET_READ_BYTES, Ordering::Relaxed);
    }
}

/// Broad media stream category declared by a container.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum StreamKind {
    /// Audio tracks without any video track.
    AudioOnly,
    /// At least one video track (with or without audio).
    Video,
}

/// Stream kinds and primary codec declarations, without pixel/PCM decoding.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaInfo {
    /// Whether the container is audio-only or includes video.
    pub kind: StreamKind,
    /// Primary video codec, using the existing `FFprobe` naming convention.
    pub video_codec: Option<&'static str>,
    /// Primary audio codec, using the existing `FFprobe` naming convention.
    pub audio_codec: Option<&'static str>,
}

/// Containers accepted by the `WebM` conversion policy, distinct from codecs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum VideoContainer {
    /// EBML with the `WebM` document type.
    Webm,
    /// EBML with the Matroska document type.
    Matroska,
    /// ISO base media container (including MP4).
    Mp4,
    /// Another container recognized by the Rust reader.
    Other,
}

/// Video metadata used for conversion decisions and output validation.
#[derive(Debug)]
pub(crate) struct VideoInfo {
    /// Container established from signatures and the actual format reader.
    pub container: VideoContainer,
    /// Video codecs in stream order; never inferred from filename or MIME.
    pub codecs: Vec<&'static str>,
    /// Audio codecs in stream order.
    pub audio_codecs: Vec<&'static str>,
    /// First video stream's coded dimensions.
    pub dimensions: Option<(u16, u16)>,
    /// Duration from the container timebase, when declared.
    pub duration: Option<symphonia::core::units::Time>,
}

impl VideoInfo {
    /// Preserve compatible VP8/VP9 `WebM`; AV1 follows the established VP9 policy.
    pub(crate) fn needs_webm_conversion(&self) -> bool {
        self.container != VideoContainer::Webm
            || self.codecs.is_empty()
            || !self
                .codecs
                .iter()
                .all(|codec| matches!(*codec, "vp8" | "vp9"))
            || !self
                .audio_codecs
                .iter()
                .all(|codec| matches!(*codec, "opus" | "vorbis"))
    }

    /// Enforce the generated VP9/Opus `WebM` contract before promotion.
    pub(crate) fn validate_webm_output(&self) -> Result<()> {
        ensure!(
            self.container == VideoContainer::Webm,
            "output container is not WebM"
        );
        ensure!(
            self.codecs.as_slice() == ["vp9"],
            "output must contain one VP9 video stream"
        );
        ensure!(
            self.audio_codecs.is_empty() || self.audio_codecs.as_slice() == ["opus"],
            "output audio must be Opus"
        );
        let (width, height) = self.dimensions.context("output has no video dimensions")?;
        ensure!(width > 0 && height > 0, "output has zero video dimensions");
        ensure!(
            u64::from(width) * u64::from(height) <= super::MAX_UNTRUSTED_IMAGE_PIXELS,
            "output video dimensions exceed the media pixel budget"
        );
        if let Some(duration) = self.duration {
            // Time::is_positive in Symphonia 0.6 checks whole seconds only.
            ensure!(
                duration > symphonia::core::units::Time::ZERO,
                "output duration is not positive"
            );
        }
        Ok(())
    }
}

/// Inspect video tracks with the existing bounded Rust container reader.
/// The header determines `WebM` versus Matroska; the parser determines codecs.
pub(crate) fn inspect_video(path: &Path) -> Result<VideoInfo> {
    use std::io::Read as _;
    use symphonia::core::formats::well_known::{FORMAT_ID_ISOMP4, FORMAT_ID_MKV};
    let format = open_format(path)?;
    let mut header = Vec::with_capacity(512);
    File::open(path)?.take(512).read_to_end(&mut header)?;
    let container = match format.format_info().format {
        FORMAT_ID_MKV => match crate::utils::files::video_container_mime(&header)? {
            "video/webm" => VideoContainer::Webm,
            "video/x-matroska" => VideoContainer::Matroska,
            _ => anyhow::bail!("Matroska parser and header disagree"),
        },
        FORMAT_ID_ISOMP4 => VideoContainer::Mp4,
        _ => VideoContainer::Other,
    };
    let mut codecs = Vec::new();
    let mut audio_codecs = Vec::new();
    let mut dimensions = None;
    for track in format.tracks() {
        match &track.codec_params {
            Some(CodecParameters::Video(params)) => {
                if codecs.is_empty() {
                    dimensions = params.width.zip(params.height);
                }
                codecs.push(video_track_codec(
                    path,
                    track.id,
                    params.codec,
                    container == VideoContainer::Mp4,
                )?);
            }
            Some(CodecParameters::Audio(params)) => {
                audio_codecs.push(audio_codec_name(params.codec));
            }
            _ => {}
        }
    }
    ensure!(!codecs.is_empty(), "container has no video stream");
    let media = format.media_info();
    let duration = media
        .time_base
        .zip(media.duration)
        .map(|(base, duration)| {
            base.calc_duration(duration)
                .context("video duration overflow")
        })
        .transpose()?;
    Ok(VideoInfo {
        container,
        codecs,
        audio_codecs,
        dimensions,
        duration,
    })
}

/// Scan generated packets to EOF with bounded reads; reject header-only output.
fn validate_video_packets(path: &Path) -> Result<()> {
    let (mut format, window) = open_bounded_format(
        path,
        Instant::now(),
        Duration::from_secs(crate::config::ffmpeg_timeout_secs()),
        None,
    )?;
    let track_id = format
        .first_track(TrackType::Video)
        .context("missing video stream")?
        .id;
    let mut has_video = false;
    loop {
        window.start_packet();
        let Some(packet) = format.next_packet()? else {
            break;
        };
        if packet.track_id == track_id {
            ensure!(!packet.data.is_empty(), "empty video packet");
            has_video = true;
        }
    }
    ensure!(has_video, "output has no video packets");
    Ok(())
}

/// Validate generated VP9/Opus `WebM` metadata and its bounded packet stream.
///
/// # Errors
/// Returns an error for the wrong container, codec, geometry, duration or packets.
pub fn validate_webm_output_file(path: &Path) -> Result<()> {
    inspect_video(path)?.validate_webm_output()?;
    validate_video_packets(path)
}

/// Bound parser work even while it is scanning headers before returning packets.
struct ContainerSource {
    /// The validated regular file.
    file: File,
    /// Stable length used by parsers and the repeated-read budget.
    length: u64,
    /// Start of this container reader's lifetime.
    started: Instant,
    /// Existing configured media-processing deadline.
    timeout: Duration,
    /// Bytes read, including parser seeks and repeated header scans.
    read_bytes: u64,
    /// Bound progressive allocations inside a parser's exact-size reads.
    window: Arc<ContainerReadBudget>,
    /// Optional audio-worker shutdown propagation during header/packet reads.
    cancel: Option<CancellationToken>,
}

impl ContainerSource {
    /// Check the deadline and permit at most two full scans plus buffer lookahead.
    fn check(&self) -> std::io::Result<()> {
        if self
            .cancel
            .as_ref()
            .is_some_and(CancellationToken::is_cancelled)
        {
            return Err(std::io::Error::other(
                "container reading cancelled during shutdown",
            ));
        }
        if self.started.elapsed() >= self.timeout {
            return Err(std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "container inspection timed out",
            ));
        }
        if self.read_bytes > self.length.saturating_mul(2).saturating_add(65_536) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "container parser exceeded its read budget",
            ));
        }
        Ok(())
    }
}

impl std::io::Read for ContainerSource {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        self.check()?;
        if buffer.is_empty() {
            return Ok(0);
        }
        let remaining = self
            .window
            .limit
            .load(Ordering::Relaxed)
            .saturating_sub(self.window.bytes.load(Ordering::Relaxed));
        if remaining == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "container operation exceeded its allocation/read budget",
            ));
        }
        let allowed = buffer
            .len()
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        let count = self.file.read(
            buffer
                .get_mut(..allowed)
                .ok_or_else(|| std::io::Error::other("invalid container read buffer"))?,
        )?;
        self.window
            .bytes
            .fetch_add(u64::try_from(count).unwrap_or(u64::MAX), Ordering::Relaxed);
        self.read_bytes = self
            .read_bytes
            .saturating_add(u64::try_from(count).unwrap_or(u64::MAX));
        self.check()?;
        Ok(count)
    }
}

impl std::io::Seek for ContainerSource {
    fn seek(&mut self, position: std::io::SeekFrom) -> std::io::Result<u64> {
        self.check()?;
        self.file.seek(position)
    }
}

impl symphonia::core::io::MediaSource for ContainerSource {
    fn is_seekable(&self) -> bool {
        true
    }
    fn byte_len(&self) -> Option<u64> {
        Some(self.length)
    }
}

/// Supply bounded Opus header/packet reads to the standalone Ogg decoder.
pub(crate) fn open_discrete_opus_source(
    path: &Path,
    started: Instant,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<(impl std::io::Read + use<>, Arc<ContainerReadBudget>)> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    ensure!(metadata.is_file(), "Opus source is not a regular file");
    let window = Arc::new(ContainerReadBudget::new());
    // Bound OpusTags strings/counts before packet-mode's separate ceiling.
    window.limit.store(1024 * 1024, Ordering::Relaxed);
    Ok((
        ContainerSource {
            length: metadata.len(),
            file,
            started,
            timeout,
            read_bytes: 0,
            window: Arc::clone(&window),
            cancel: Some(cancel.clone()),
        },
        window,
    ))
}

/// Inspect every page of a discrete Opus stream that Symphonia cannot recognize.
pub(crate) fn discrete_opus_header(
    path: &Path,
    started: Instant,
    timeout: Duration,
    cancel: Option<&CancellationToken>,
) -> Result<Option<Vec<u8>>> {
    speex::discrete_opus_header(path, started, timeout, cancel)
}

/// Open a format reader using content signatures, never a client extension.
pub(crate) fn open_format(path: &Path) -> Result<Box<dyn FormatReader>> {
    open_bounded_format(
        path,
        Instant::now(),
        Duration::from_secs(crate::config::ffmpeg_timeout_secs()),
        None,
    )
    .map(|(reader, _)| reader)
}

/// Open audio with the job's existing deadline, cancellation and packet budget.
pub(crate) fn open_audio_format(
    path: &Path,
    started: Instant,
    timeout: Duration,
    cancel: &CancellationToken,
) -> Result<(Box<dyn FormatReader>, Arc<ContainerReadBudget>)> {
    open_bounded_format(path, started, timeout, Some(cancel.clone()))
}

/// Construct a content-based reader with bounded source operations.
fn open_bounded_format(
    path: &Path,
    started: Instant,
    timeout: Duration,
    cancel: Option<CancellationToken>,
) -> Result<(Box<dyn FormatReader>, Arc<ContainerReadBudget>)> {
    let source = File::open(path).context("open media container")?;
    ensure!(
        source.metadata()?.is_file(),
        "media source is not a regular file"
    );
    let window = Arc::new(ContainerReadBudget::new());
    let source = ContainerSource {
        length: source.metadata()?.len(),
        file: source,
        started,
        timeout,
        read_bytes: 0,
        window: Arc::clone(&window),
        cancel,
    };
    let stream = MediaSourceStream::new(Box::new(source), MediaSourceStreamOptions::default());
    let reader = symphonia::default::get_probe()
        .probe(
            &Hint::new(),
            stream,
            FormatOptions::default(),
            MetadataOptions::default()
                .limit_tag_bytes(Limit::Maximum(1024 * 1024))
                .limit_visual_bytes(Limit::Maximum(1024 * 1024)),
        )
        .context("inspect media container")?;
    Ok((reader, window))
}

/// Inspect audio/video tracks and primary codec metadata.
///
/// # Errors
/// Returns an error for unreadable, malformed or trackless input.
pub fn inspect(path: &Path) -> Result<MediaInfo> {
    if is_speex(
        path,
        Instant::now(),
        Duration::from_secs(crate::config::ffmpeg_timeout_secs()),
        None,
    )? {
        return Ok(MediaInfo {
            kind: StreamKind::AudioOnly,
            video_codec: None,
            audio_codec: Some("speex"),
        });
    }
    if let Some(header) = discrete_opus_header(
        path,
        Instant::now(),
        Duration::from_secs(crate::config::ffmpeg_timeout_secs()),
        None,
    )? {
        // The same bounded Rust reader validates OpusHead/OpusTags without PCM decoding.
        let cancel = CancellationToken::new();
        let (source, _) = open_discrete_opus_source(
            path,
            Instant::now(),
            Duration::from_secs(crate::config::ffmpeg_timeout_secs()),
            &cancel,
        )?;
        let reader = opus_pure::OggOpusReader::new(source)?;
        super::audio::validate_discrete_opus_header(&header, reader.head())?;
        return Ok(MediaInfo {
            kind: StreamKind::AudioOnly,
            video_codec: None,
            audio_codec: Some("opus"),
        });
    }
    let format = open_format(path)?;
    let mut video_codec = None;
    let mut audio_codec = None;
    for track in format.tracks() {
        match &track.codec_params {
            Some(CodecParameters::Video(params)) => {
                if video_codec.is_none() {
                    video_codec = Some(video_track_codec(
                        path,
                        track.id,
                        params.codec,
                        format.format_info().format
                            == symphonia::core::formats::well_known::FORMAT_ID_ISOMP4,
                    )?);
                }
            }
            Some(CodecParameters::Audio(params)) => {
                audio_codec.get_or_insert_with(|| audio_codec_name(params.codec));
            }
            _ => {}
        }
    }
    ensure!(
        video_codec.is_some() || audio_codec.is_some(),
        "container has no audio or video streams"
    );
    let kind = if format.first_track(TrackType::Video).is_some() {
        StreamKind::Video
    } else {
        StreamKind::AudioOnly
    };
    Ok(MediaInfo {
        kind,
        video_codec,
        audio_codec,
    })
}

/// Validate every logical Speex stream before selecting its unavailable decoder.
pub(crate) fn is_speex(
    path: &Path,
    started: Instant,
    timeout: Duration,
    cancel: Option<&CancellationToken>,
) -> Result<bool> {
    speex::inspect(path, started, timeout, cancel)
}

/// Inspect the broad stream category.
///
/// # Errors
/// Returns an error for unreadable, malformed or trackless input.
pub fn probe_stream_kind(path: &Path) -> Result<StreamKind> {
    inspect(path).map(|info| info.kind)
}

/// Inspect the first video codec declaration without decoding a video frame.
///
/// # Errors
/// Returns an error if container inspection fails or no video track exists.
pub fn probe_video_codec(path: &str) -> Result<String> {
    inspect(Path::new(path))?
        .video_codec
        .context("container has no video codec")
        .map(str::to_owned)
}

/// Inspect the first audio codec declaration.
///
/// # Errors
/// Returns an error if container inspection fails or no audio track exists.
pub fn probe_audio_codec(path: &Path) -> Result<String> {
    inspect(path)?
        .audio_codec
        .context("container has no audio codec")
        .map(str::to_owned)
}

/// Supplement only missing MP4 codec declarations using the bounded box reader.
fn video_track_codec(
    path: &Path,
    track_id: u32,
    codec: video::VideoCodecId,
    is_mp4: bool,
) -> Result<&'static str> {
    let name = video_codec_name(codec);
    if name != "unknown" || !is_mp4 {
        return Ok(name);
    }
    let started = Instant::now();
    let timeout = Duration::from_secs(crate::config::ffmpeg_timeout_secs());
    super::audio::mp4::declared_video_codec(path, track_id, || {
        ensure!(
            started.elapsed() < timeout,
            "MP4 video codec inspection timed out"
        );
        Ok(())
    })
    .map(|codec| codec.unwrap_or("unknown"))
}

/// Normalize common video codec declarations used by the upload/transcode paths.
const fn video_codec_name(codec: video::VideoCodecId) -> &'static str {
    // Normalize opaque sample-entry FourCC IDs as well as well-known IDs.
    // These fixed ASCII literals satisfy FourCc::new's documented invariant.
    const MP4_AV1: video::VideoCodecId =
        video::VideoCodecId::new(symphonia::core::common::FourCc::new(*b"av01"));
    const MP4_VP8: video::VideoCodecId =
        video::VideoCodecId::new(symphonia::core::common::FourCc::new(*b"vp08"));
    const MP4_VP9: video::VideoCodecId =
        video::VideoCodecId::new(symphonia::core::common::FourCc::new(*b"vp09"));
    use video::well_known::{
        CODEC_ID_AV1, CODEC_ID_H264, CODEC_ID_HEVC, CODEC_ID_MJPEG, CODEC_ID_MPEG4, CODEC_ID_VP8,
        CODEC_ID_VP9,
    };
    match codec {
        CODEC_ID_H264 => "h264",
        CODEC_ID_MPEG4 => "mpeg4",
        CODEC_ID_MJPEG => "mjpeg",
        CODEC_ID_HEVC => "hevc",
        CODEC_ID_VP8 | MP4_VP8 => "vp8",
        CODEC_ID_VP9 | MP4_VP9 => "vp9",
        CODEC_ID_AV1 | MP4_AV1 => "av1",
        _ => "unknown",
    }
}

/// Normalize supported audio codecs while retaining unknown container tracks.
pub(crate) const fn audio_codec_name(codec: audio::AudioCodecId) -> &'static str {
    use audio::well_known::{
        CODEC_ID_AAC, CODEC_ID_AC3, CODEC_ID_ALAC, CODEC_ID_EAC3, CODEC_ID_FLAC, CODEC_ID_MP1,
        CODEC_ID_MP2, CODEC_ID_MP3, CODEC_ID_OPUS, CODEC_ID_PCM_F32LE, CODEC_ID_PCM_F64LE,
        CODEC_ID_PCM_S16LE, CODEC_ID_PCM_S24LE, CODEC_ID_PCM_S32LE, CODEC_ID_PCM_U8,
        CODEC_ID_SPEEX, CODEC_ID_VORBIS,
    };
    match codec {
        CODEC_ID_MP1 => "mp1",
        CODEC_ID_MP2 => "mp2",
        CODEC_ID_MP3 => "mp3",
        CODEC_ID_FLAC => "flac",
        CODEC_ID_VORBIS => "vorbis",
        CODEC_ID_OPUS => "opus",
        CODEC_ID_AAC => "aac",
        CODEC_ID_ALAC => "alac",
        CODEC_ID_AC3 => "ac3",
        CODEC_ID_EAC3 => "eac3",
        CODEC_ID_SPEEX => "speex",
        CODEC_ID_PCM_S16LE => "pcm_s16le",
        CODEC_ID_PCM_S24LE => "pcm_s24le",
        CODEC_ID_PCM_S32LE => "pcm_s32le",
        CODEC_ID_PCM_U8 => "pcm_u8",
        CODEC_ID_PCM_F32LE => "pcm_f32le",
        CODEC_ID_PCM_F64LE => "pcm_f64le",
        _ => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;
    use symphonia::core::io::ReadBytes as _;

    #[test]
    fn av1_vp8_vp9_detection_and_container_policy_use_stream_metadata() -> Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media");
        let dir = tempfile::tempdir()?;
        let disguised = dir.path().join("misleading.jpg");
        for (name, container, codec, convert) in [
            ("av1.webm", VideoContainer::Webm, "av1", true),
            ("av1.mkv", VideoContainer::Matroska, "av1", true),
            ("av1.mp4", VideoContainer::Mp4, "av1", true),
            ("vp8.webm", VideoContainer::Webm, "vp8", false),
            ("video.webm", VideoContainer::Webm, "vp9", false),
            ("video.mkv", VideoContainer::Matroska, "h264", true),
            ("video.mp4", VideoContainer::Mp4, "h264", true),
        ] {
            std::fs::copy(root.join(name), &disguised)?;
            let info = inspect_video(&disguised).with_context(|| format!("inspect {name}"))?;
            ensure!(
                info.container == container,
                "wrong container for {name}: {info:?}"
            );
            ensure!(info.codecs == [codec], "wrong codec for {name}: {info:?}");
            ensure!(
                info.needs_webm_conversion() == convert,
                "wrong policy for {name}"
            );
            ensure!(
                inspect(&disguised)?.video_codec == Some(codec),
                "legacy probe disagrees"
            );
        }
        Ok(())
    }

    #[test]
    fn webm_policy_checks_all_video_and_audio_tracks() {
        let mut info = VideoInfo {
            container: VideoContainer::Webm,
            codecs: vec!["vp9"],
            audio_codecs: vec!["opus", "vorbis"],
            dimensions: Some((64, 64)),
            duration: None,
        };
        assert!(!info.needs_webm_conversion());
        for codec in ["av1", "h264", "unknown"] {
            info.codecs = vec!["vp9", codec];
            assert!(info.needs_webm_conversion());
        }
        info.codecs = vec!["vp9"];
        info.audio_codecs.push("aac");
        assert!(info.needs_webm_conversion());
        info.audio_codecs.clear();
        info.container = VideoContainer::Matroska;
        assert!(info.needs_webm_conversion());
    }

    #[test]
    fn generated_webm_validation_rejects_wrong_container_codec_geometry_and_duration() -> Result<()>
    {
        let mut info = VideoInfo {
            container: VideoContainer::Webm,
            codecs: vec!["vp9"],
            audio_codecs: vec!["opus"],
            dimensions: Some((64, 64)),
            duration: Some(symphonia::core::units::Time::from_millis(400)),
        };
        info.validate_webm_output()?;
        for container in [
            VideoContainer::Matroska,
            VideoContainer::Mp4,
            VideoContainer::Other,
        ] {
            info.container = container;
            ensure!(
                info.validate_webm_output().is_err(),
                "wrong container accepted"
            );
        }
        info.container = VideoContainer::Webm;
        for codecs in [vec![], vec!["vp8"], vec!["av1"], vec!["vp9", "vp9"]] {
            info.codecs = codecs;
            ensure!(
                info.validate_webm_output().is_err(),
                "wrong video streams accepted"
            );
        }
        info.codecs = vec!["vp9"];
        info.audio_codecs = vec!["aac"];
        ensure!(info.validate_webm_output().is_err(), "wrong audio accepted");
        info.audio_codecs.clear();
        for dimensions in [
            None,
            Some((0, 64)),
            Some((64, 0)),
            Some((u16::MAX, u16::MAX)),
        ] {
            info.dimensions = dimensions;
            ensure!(
                info.validate_webm_output().is_err(),
                "invalid geometry accepted"
            );
        }
        info.dimensions = Some((64, 64));
        for duration in [
            symphonia::core::units::Time::ZERO,
            symphonia::core::units::Time::from_millis(-1),
        ] {
            info.duration = Some(duration);
            ensure!(
                info.validate_webm_output().is_err(),
                "invalid duration accepted"
            );
        }
        info.duration = None;
        info.validate_webm_output()?;
        Ok(())
    }

    #[test]
    fn generated_webm_file_validation_ignores_extension_and_rejects_invalid_files() -> Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media");
        let dir = tempfile::tempdir()?;
        let output = dir.path().join("generated.webm");
        for name in [
            "av1.webm",
            "av1.mkv",
            "av1.mp4",
            "vp8.webm",
            "video.mkv",
            "video.mp4",
            "audio.webm",
        ] {
            std::fs::copy(root.join(name), &output)?;
            ensure!(
                validate_webm_output_file(&output).is_err(),
                "invalid output {name} accepted"
            );
        }
        for bytes in [b"".as_slice(), b"partial WebM", b"\x1a\x45\xdf\xa3\x01"] {
            std::fs::write(&output, bytes)?;
            ensure!(
                validate_webm_output_file(&output).is_err(),
                "partial output accepted"
            );
        }
        std::fs::copy(root.join("video.webm"), &output)?;
        validate_webm_output_file(&output)?;
        Ok(())
    }

    #[test]
    fn parser_exact_reads_obey_packet_budget_cancellation_and_deadline() -> Result<()> {
        let file = tempfile::tempfile()?;
        file.set_len(MAX_PACKET_READ_BYTES * 4)?;
        let cancel = CancellationToken::new();
        let window = Arc::new(ContainerReadBudget::new());
        window.start_packet();
        let source = ContainerSource {
            file,
            length: MAX_PACKET_READ_BYTES * 4,
            started: Instant::now(),
            timeout: Duration::from_secs(60),
            read_bytes: 0,
            window: Arc::clone(&window),
            cancel: Some(cancel.clone()),
        };
        let mut stream =
            MediaSourceStream::new(Box::new(source), MediaSourceStreamOptions::default());
        let error = stream
            .read_boxed_slice_exact(usize::try_from(MAX_PACKET_READ_BYTES * 2)?)
            .err()
            .context("oversized parser allocation succeeded")?;
        ensure!(
            error.kind() == std::io::ErrorKind::InvalidData,
            "wrong budget error: {error}"
        );
        ensure!(
            window.bytes.load(Ordering::Relaxed) == MAX_PACKET_READ_BYTES,
            "read limit overshot"
        );
        let mut source = ContainerSource {
            file: tempfile::tempfile()?,
            length: 0,
            started: Instant::now(),
            timeout: Duration::from_secs(60),
            read_bytes: 0,
            window,
            cancel: Some(cancel.clone()),
        };
        cancel.cancel();
        let mut byte = [0];
        ensure!(
            source.read(&mut byte).is_err(),
            "cancelled parser read continued"
        );
        source.cancel = None;
        source.timeout = Duration::ZERO;
        let error = source
            .read(&mut byte)
            .err()
            .context("expired parser read continued")?;
        ensure!(
            error.kind() == std::io::ErrorKind::TimedOut,
            "wrong deadline error"
        );
        Ok(())
    }

    #[test]
    fn speex_corruption_and_truncation_do_not_select_fallback() -> Result<()> {
        let original = include_bytes!("../../tests/fixtures/media/tone.spx");
        let dir = tempfile::tempdir()?;
        let path = dir.path().join("bad.spx");
        let mut bad_crc = original.to_vec();
        *bad_crc.last_mut().context("Speex fixture empty")? ^= 1;
        for bytes in [
            bad_crc.as_slice(),
            original
                .get(..original.len() - 1)
                .context("missing Speex tail")?,
            original.get(..108).context("missing Speex header")?,
        ] {
            std::fs::write(&path, bytes)?;
            let error = inspect(&path).err().context("invalid Speex accepted")?;
            ensure!(
                !super::super::audio::is_unsupported(&error),
                "invalid Speex selected fallback"
            );
        }
        Ok(())
    }

    #[test]
    fn every_speex_logical_stream_must_end_and_pass_crc() -> Result<()> {
        let original = include_bytes!("../../tests/fixtures/media/speex-chained.spx");
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("bad.spx");
        let mut corrupted = original.to_vec();
        *corrupted.last_mut().context("empty chained fixture")? ^= 1;
        for bytes in [
            corrupted.as_slice(),
            original
                .get(..original.len() - 1)
                .context("empty fixture")?,
        ] {
            std::fs::write(&path, bytes)?;
            let error = inspect(&path)
                .err()
                .context("invalid second stream was accepted")?;
            ensure!(
                !super::super::audio::is_unsupported(&error),
                "invalid second stream selected fallback"
            );
        }
        let cancel = CancellationToken::new();
        cancel.cancel();
        ensure!(
            is_speex(&path, Instant::now(), Duration::from_secs(1), Some(&cancel)).is_err(),
            "cancelled validation continued"
        );
        ensure!(
            is_speex(&path, Instant::now(), Duration::ZERO, None).is_err(),
            "expired validation continued"
        );
        Ok(())
    }

    #[test]
    fn discrete_opus_crc_and_truncation_fail_closed() -> Result<()> {
        let original = include_bytes!("../../tests/fixtures/media/opus-surround-discrete.opus");
        let directory = tempfile::tempdir()?;
        let path = directory.path().join("misleading.jpg");
        let mut corrupt = original.to_vec();
        *corrupt.last_mut().context("empty Opus fixture")? ^= 1;
        for bytes in [
            corrupt.as_slice(),
            original
                .get(..original.len() - 1)
                .context("empty Opus fixture")?,
        ] {
            std::fs::write(&path, bytes)?;
            let error = inspect(&path)
                .err()
                .context("invalid discrete Opus accepted")?;
            ensure!(
                !super::super::audio::is_unsupported(&error),
                "invalid Opus selected compatibility"
            );
        }
        Ok(())
    }

    #[test]
    fn fixture_metadata_preserves_verified_stream_types_and_codecs() -> Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media");
        for (file, kind, codec) in [
            ("video.mp4", StreamKind::Video, "h264"),
            ("video.webm", StreamKind::Video, "vp9"),
            ("video.mkv", StreamKind::Video, "h264"),
            ("tone.m4a", StreamKind::AudioOnly, "aac"),
            ("tone-alac.m4a", StreamKind::AudioOnly, "alac"),
            ("he-aac.m4a", StreamKind::AudioOnly, "aac"),
            ("ac3.m4a", StreamKind::AudioOnly, "ac3"),
            ("surround.opus", StreamKind::AudioOnly, "opus"),
            ("opus-surround-discrete.opus", StreamKind::AudioOnly, "opus"),
            ("tone.spx", StreamKind::AudioOnly, "speex"),
            ("speex-chained.spx", StreamKind::AudioOnly, "speex"),
            ("speex-multiplexed.spx", StreamKind::AudioOnly, "speex"),
            (
                "speex-chained-multiplexed.spx",
                StreamKind::AudioOnly,
                "speex",
            ),
            ("audio.mkv", StreamKind::AudioOnly, "flac"),
            ("multiple-streams.mp4", StreamKind::Video, "h264"),
            ("audio.webm", StreamKind::AudioOnly, "opus"),
            ("tone.mp3", StreamKind::AudioOnly, "mp3"),
            ("tone.flac", StreamKind::AudioOnly, "flac"),
            ("tone.wav", StreamKind::AudioOnly, "pcm_s16le"),
            ("tone.ogg", StreamKind::AudioOnly, "vorbis"),
            ("tone.opus", StreamKind::AudioOnly, "opus"),
            ("tone.aac", StreamKind::AudioOnly, "aac"),
        ] {
            let path = root.join(file);
            let info = inspect(&path).with_context(|| format!("inspect {file}"))?;
            ensure!(info.kind == kind, "wrong stream type for {file}: {info:?}");
            let actual = if kind == StreamKind::Video {
                info.video_codec
            } else {
                info.audio_codec
            };
            ensure!(actual == Some(codec), "wrong codec for {file}: {info:?}");
        }
        Ok(())
    }

    #[test]
    fn content_signatures_override_extensions_and_malformed_containers_fail() -> Result<()> {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/media");
        let dir = tempfile::tempdir()?;
        let misleading = dir.path().join("image.jpg");
        std::fs::copy(root.join("audio.webm"), &misleading)?;
        ensure!(
            inspect(&misleading)?.kind == StreamKind::AudioOnly,
            "extension overrode content"
        );
        for content in [
            b"\x1a\x45\xdf\xa3\x01".as_slice(),
            b"\0\0\0\x18ftypisom",
            b"RIFF\xff\xff\xff\xffWAVE",
        ] {
            std::fs::write(&misleading, content)?;
            ensure!(
                inspect(&misleading).is_err(),
                "malformed container accepted"
            );
        }
        Ok(())
    }
}
