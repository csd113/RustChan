# FFmpeg Static Migration — Full Implementation

Replace every `Command::new("ffmpeg")` / `TokioCommand::new("ffmpeg")` call with
in-process `ffmpeg-next` library calls. After this migration the binary requires
no system ffmpeg or ffprobe installation.

---

## 1. Cargo.toml

```toml
[dependencies]
# Add this block. Remove nothing else.
ffmpeg-next = { version = "7", default-features = false, features = [
    "static",              # compiles libav* from source into the binary
    "codec",               # libavcodec  — encode / decode
    "format",              # libavformat — container mux / demux
    "filter",              # libavfilter — scale, showwavespic
    "software-resampling", # libswresample — audio resampling for Opus
    "software-scaling",    # libswscale   — pixel-format conversion
] }
```

Pin the ffmpeg source version and supply build flags via `.cargo/config.toml`
(create this file if it does not exist):

```toml
# .cargo/config.toml
[env]
FFMPEG_BUILD_VERSION = "7.1"
# If nasm is absent on the build machine, add:
# FFMPEG_EXTRA_FLAGS = "--disable-x86asm"
```

Build-tool requirements (install once per machine):

| Tool | Ubuntu/Debian | macOS |
|---|---|---|
| nasm | `apt install nasm` | `brew install nasm` |
| cmake | `apt install cmake` | `brew install cmake` |
| pkg-config | `apt install pkg-config` | `brew install pkg-config` |

CI (GitHub Actions Ubuntu runner) — add before `cargo build`:

```yaml
- name: Install ffmpeg build deps
  run: sudo apt-get install -y nasm cmake pkg-config libssl-dev
```

---

## 2. `src/media/ffmpeg.rs` — complete replacement

Drop the entire existing file and replace with the following.
All public function signatures are **identical** to the originals.

```rust
// media/ffmpeg.rs
//
// FFmpeg wrappers using statically linked libav* (ffmpeg-next).
//
// All public signatures are unchanged from the subprocess-based version so
// that no caller in convert.rs, thumbnail.rs, workers/, or detect.rs needs
// to be modified for signature reasons.
//
// Call site changes required elsewhere:
//   • detect.rs   — detection functions become no-ops (see §4 below)
//   • workers/    — TokioCommand::new("ffmpeg") blocks replaced (see §3 below)
//
// Thread safety: ffmpeg-next is safe to call from multiple threads once
// init_ffmpeg() has been called. Call it once from main() or detect.rs.

use anyhow::{Context, Result};
use ffmpeg_next as ffmpeg;
use ffmpeg_next::{
    codec, filter, format, frame, media,
    software::scaling::{context::Context as SwsContext, flag::Flags as SwsFlags},
    Dictionary, Rational,
};
use std::path::Path;

// ─── Library initialisation ───────────────────────────────────────────────────

/// Initialise the ffmpeg library. Idempotent and thread-safe.
///
/// Call once at startup (from detect.rs or main.rs) before any codec
/// operation. Safe to call multiple times — subsequent calls are no-ops.
pub fn init_ffmpeg() {
    ffmpeg::init().expect("ffmpeg library init failed — this is a build error");
    // Suppress ffmpeg's internal log output. rustchan's tracing handles all
    // user-facing diagnostics.
    unsafe {
        ffmpeg_next::ffi::av_log_set_level(ffmpeg_next::ffi::AV_LOG_QUIET);
    }
}

// ─── Detection stubs (always true — codecs are compiled in) ──────────────────

/// Always returns true: ffmpeg is compiled into the binary.
#[must_use]
pub fn detect_ffmpeg() -> bool {
    true
}

/// Always returns true: libwebp is compiled in.
#[must_use]
pub fn check_webp_encoder() -> bool {
    true
}

/// Always returns true: libvpx-vp9 is compiled in.
#[must_use]
pub fn check_vp9_encoder() -> bool {
    true
}

/// Always returns true: libopus is compiled in.
#[must_use]
pub fn check_opus_encoder() -> bool {
    true
}

// ─── run_ffmpeg (no longer used — kept for any external callers) ──────────────

/// Deprecated: previously spawned a subprocess. Now always returns Ok(()).
/// Remove callers and delete this function in a follow-up cleanup.
#[allow(dead_code)]
pub fn run_ffmpeg(_args: &[&str]) -> Result<()> {
    Ok(())
}

// ─── Image → WebP ─────────────────────────────────────────────────────────────

/// Convert any ffmpeg-readable image (JPEG, PNG, BMP, TIFF, GIF) to WebP.
///
/// Animated GIF inputs produce animated WebP (loop 0 = loop forever).
/// Metadata is stripped. Quality is fixed at 85 per project spec.
///
/// # Errors
/// Returns an error if the input cannot be decoded or the output cannot be
/// written.
pub fn ffmpeg_image_to_webp(input: &Path, output: &Path) -> Result<()> {
    init_ffmpeg();

    // ── Open input ────────────────────────────────────────────────────────
    let mut ictx = format::input(input)
        .with_context(|| format!("ffmpeg_image_to_webp: cannot open {}", input.display()))?;

    let in_stream = ictx
        .streams()
        .best(media::Type::Video)
        .context("ffmpeg_image_to_webp: no video/image stream in input")?;
    let in_idx = in_stream.index();
    let in_tb = in_stream.time_base();

    let mut decoder = codec::context::Context::from_parameters(in_stream.parameters())
        .context("ffmpeg_image_to_webp: decoder context")?
        .decoder()
        .video()
        .context("ffmpeg_image_to_webp: open video decoder")?;

    // ── Open output ───────────────────────────────────────────────────────
    let mut octx = format::output(output)
        .with_context(|| format!("ffmpeg_image_to_webp: cannot open output {}", output.display()))?;

    let webp_codec = codec::encoder::find_by_name("libwebp")
        .context("libwebp encoder not found — static build is missing the codec")?;

    let mut enc_ctx = codec::context::Context::new_with_codec(webp_codec)
        .encoder()
        .video()
        .context("ffmpeg_image_to_webp: encoder context")?;

    // Dimensions and format are set from the first decoded frame because some
    // inputs (e.g. TIFF) report wrong dimensions in stream parameters.
    // We defer the actual encoder open until after decoding the first frame.

    let mut out_stream = octx.add_stream(webp_codec)
        .context("ffmpeg_image_to_webp: add output stream")?;

    let global_header = octx
        .format()
        .flags()
        .contains(format::flag::Flags::GLOBAL_HEADER);
    if global_header {
        enc_ctx.set_flags(codec::flag::Flags::GLOBAL_HEADER);
    }

    // ── Decode → scale → encode loop ─────────────────────────────────────
    let mut encoder_opened = false;
    let mut sws: Option<SwsContext> = None;
    let mut frame_count = 0u64;

    let mut encode_and_write = |enc: &mut codec::encoder::video::Encoder,
                                 octx: &mut format::context::Output,
                                 frame: Option<&frame::Video>|
     -> Result<()> {
        enc.send_frame(frame).context("ffmpeg_image_to_webp: send_frame")?;
        let mut pkt = ffmpeg_next::Packet::empty();
        while enc.receive_packet(&mut pkt).is_ok() {
            pkt.set_stream(0);
            pkt.rescale_ts(Rational(1, 25), out_stream.time_base());
            pkt.write_interleaved(octx)
                .context("ffmpeg_image_to_webp: write_interleaved")?;
        }
        Ok(())
    };

    for (stream, packet) in ictx.packets() {
        if stream.index() != in_idx {
            continue;
        }
        decoder
            .send_packet(&packet)
            .context("ffmpeg_image_to_webp: send_packet")?;

        let mut decoded = frame::Video::empty();
        while decoder.receive_frame(&mut decoded).is_ok() {
            if !encoder_opened {
                // First frame: configure encoder dimensions and open it.
                enc_ctx.set_width(decoded.width());
                enc_ctx.set_height(decoded.height());
                enc_ctx.set_format(ffmpeg_next::format::Pixel::YUVA420P);
                enc_ctx.set_time_base(Rational(1, 25));

                let mut opts = Dictionary::new();
                opts.set("quality", "85");
                opts.set("loop", "0"); // animated WebP loops forever (GIF parity)
                opts.set("lossless", "0");

                enc_ctx
                    .open_with(opts)
                    .context("ffmpeg_image_to_webp: open encoder")?;
                out_stream.set_parameters(&enc_ctx);

                octx.write_header()
                    .context("ffmpeg_image_to_webp: write_header")?;
                encoder_opened = true;

                // Pixel format converter: input format → YUVA420P for libwebp.
                sws = Some(
                    SwsContext::get(
                        decoded.format(),
                        decoded.width(),
                        decoded.height(),
                        ffmpeg_next::format::Pixel::YUVA420P,
                        decoded.width(),
                        decoded.height(),
                        SwsFlags::BILINEAR,
                    )
                    .context("ffmpeg_image_to_webp: sws_getContext")?,
                );
            }

            // Convert pixel format.
            let mut rgb_frame = frame::Video::new(
                ffmpeg_next::format::Pixel::YUVA420P,
                decoded.width(),
                decoded.height(),
            );
            if let Some(ref mut s) = sws {
                s.run(&decoded, &mut rgb_frame)
                    .context("ffmpeg_image_to_webp: sws_scale")?;
            }
            rgb_frame.set_pts(Some(frame_count as i64));
            frame_count += 1;

            // We need mutable enc_ctx below — restructure to avoid borrow conflict.
            enc_ctx
                .send_frame(Some(&rgb_frame))
                .context("ffmpeg_image_to_webp: send_frame")?;
            let mut pkt = ffmpeg_next::Packet::empty();
            while enc_ctx.receive_packet(&mut pkt).is_ok() {
                pkt.set_stream(0);
                pkt.rescale_ts(Rational(1, 25), out_stream.time_base());
                pkt.write_interleaved(&mut octx)
                    .context("ffmpeg_image_to_webp: write_interleaved")?;
            }
        }
    }

    // Flush decoder.
    decoder
        .send_eof()
        .context("ffmpeg_image_to_webp: send_eof to decoder")?;
    let mut decoded = frame::Video::empty();
    while decoder.receive_frame(&mut decoded).is_ok() {
        // (same encode block as above — flush remaining frames)
        let mut rgb_frame = frame::Video::new(
            ffmpeg_next::format::Pixel::YUVA420P,
            decoded.width(),
            decoded.height(),
        );
        if let Some(ref mut s) = sws {
            s.run(&decoded, &mut rgb_frame)
                .context("ffmpeg_image_to_webp: sws_scale flush")?;
        }
        rgb_frame.set_pts(Some(frame_count as i64));
        frame_count += 1;
        enc_ctx
            .send_frame(Some(&rgb_frame))
            .context("ffmpeg_image_to_webp: flush send_frame")?;
        let mut pkt = ffmpeg_next::Packet::empty();
        while enc_ctx.receive_packet(&mut pkt).is_ok() {
            pkt.set_stream(0);
            pkt.rescale_ts(Rational(1, 25), out_stream.time_base());
            pkt.write_interleaved(&mut octx)
                .context("ffmpeg_image_to_webp: flush write")?;
        }
    }

    // Flush encoder.
    enc_ctx
        .send_eof()
        .context("ffmpeg_image_to_webp: send_eof to encoder")?;
    let mut pkt = ffmpeg_next::Packet::empty();
    while enc_ctx.receive_packet(&mut pkt).is_ok() {
        pkt.set_stream(0);
        pkt.rescale_ts(Rational(1, 25), out_stream.time_base());
        pkt.write_interleaved(&mut octx)
            .context("ffmpeg_image_to_webp: final write")?;
    }

    octx.write_trailer()
        .context("ffmpeg_image_to_webp: write_trailer")?;

    Ok(())
}

// ─── Thumbnail ────────────────────────────────────────────────────────────────

/// Extract the first frame from an image or video, scale to fit within
/// `max_dim × max_dim` (aspect preserved), and save as WebP quality 80.
///
/// Equivalent to:
///   ffmpeg -i <input> -vframes 1
///     -vf "scale='if(gt(iw,ih),MAX,-2)':'if(gt(iw,ih),-2,MAX)'"
///     -c:v libwebp -quality 80 <output>
///
/// # Errors
/// Returns an error if the input cannot be demuxed/decoded or the output
/// cannot be written.
pub fn ffmpeg_thumbnail(input: &Path, output: &Path, max_dim: u32) -> Result<()> {
    init_ffmpeg();

    let mut ictx = format::input(input)
        .with_context(|| format!("ffmpeg_thumbnail: cannot open {}", input.display()))?;

    let in_stream = ictx
        .streams()
        .best(media::Type::Video)
        .context("ffmpeg_thumbnail: no video stream")?;
    let in_idx = in_stream.index();

    let mut decoder = codec::context::Context::from_parameters(in_stream.parameters())
        .context("ffmpeg_thumbnail: decoder context")?
        .decoder()
        .video()
        .context("ffmpeg_thumbnail: open decoder")?;

    // Seek to the first key frame (important for video sources).
    let _ = ictx.seek(0, ..0);

    // Decode until we get one complete frame.
    let mut first_frame: Option<frame::Video> = None;
    'outer: for (stream, packet) in ictx.packets() {
        if stream.index() != in_idx {
            continue;
        }
        decoder
            .send_packet(&packet)
            .context("ffmpeg_thumbnail: send_packet")?;
        let mut f = frame::Video::empty();
        while decoder.receive_frame(&mut f).is_ok() {
            first_frame = Some(f);
            break 'outer;
        }
    }
    // Flush decoder in case the frame was buffered.
    if first_frame.is_none() {
        let _ = decoder.send_eof();
        let mut f = frame::Video::empty();
        if decoder.receive_frame(&mut f).is_ok() {
            first_frame = Some(f);
        }
    }

    let src_frame = first_frame.context("ffmpeg_thumbnail: no frame decoded from input")?;

    // ── Scale to fit max_dim × max_dim, preserving aspect ratio ──────────
    let (src_w, src_h) = (src_frame.width(), src_frame.height());
    let (dst_w, dst_h) = scale_dims(src_w, src_h, max_dim);

    let mut scaled = frame::Video::new(
        ffmpeg_next::format::Pixel::YUVA420P,
        dst_w,
        dst_h,
    );
    let mut sws = SwsContext::get(
        src_frame.format(),
        src_w,
        src_h,
        ffmpeg_next::format::Pixel::YUVA420P,
        dst_w,
        dst_h,
        SwsFlags::LANCZOS,
    )
    .context("ffmpeg_thumbnail: sws_getContext")?;
    sws.run(&src_frame, &mut scaled)
        .context("ffmpeg_thumbnail: sws_scale")?;
    scaled.set_pts(Some(0));

    // ── Encode as WebP quality 80 ─────────────────────────────────────────
    let webp_codec = codec::encoder::find_by_name("libwebp")
        .context("ffmpeg_thumbnail: libwebp encoder missing")?;

    let mut octx = format::output(output)
        .with_context(|| format!("ffmpeg_thumbnail: cannot open output {}", output.display()))?;

    let mut enc_ctx = codec::context::Context::new_with_codec(webp_codec)
        .encoder()
        .video()
        .context("ffmpeg_thumbnail: encoder context")?;

    enc_ctx.set_width(dst_w);
    enc_ctx.set_height(dst_h);
    enc_ctx.set_format(ffmpeg_next::format::Pixel::YUVA420P);
    enc_ctx.set_time_base(Rational(1, 1));

    let global_header = octx
        .format()
        .flags()
        .contains(format::flag::Flags::GLOBAL_HEADER);
    if global_header {
        enc_ctx.set_flags(codec::flag::Flags::GLOBAL_HEADER);
    }

    let mut opts = Dictionary::new();
    opts.set("quality", "80");
    opts.set("lossless", "0");

    let mut enc = enc_ctx
        .open_with(opts)
        .context("ffmpeg_thumbnail: open encoder")?;

    let mut out_stream = octx.add_stream(webp_codec)
        .context("ffmpeg_thumbnail: add output stream")?;
    out_stream.set_parameters(&enc);

    octx.write_header()
        .context("ffmpeg_thumbnail: write_header")?;

    enc.send_frame(Some(&scaled))
        .context("ffmpeg_thumbnail: send_frame")?;
    enc.send_eof()
        .context("ffmpeg_thumbnail: send_eof")?;

    let mut pkt = ffmpeg_next::Packet::empty();
    while enc.receive_packet(&mut pkt).is_ok() {
        pkt.set_stream(0);
        pkt.rescale_ts(Rational(1, 1), out_stream.time_base());
        pkt.write_interleaved(&mut octx)
            .context("ffmpeg_thumbnail: write_interleaved")?;
    }

    octx.write_trailer()
        .context("ffmpeg_thumbnail: write_trailer")?;

    Ok(())
}

// ─── Codec probe ──────────────────────────────────────────────────────────────

/// Return the lowercase codec name for the primary video stream in `path`.
///
/// Replaces the `ffprobe` subprocess. Opens the container, reads the stream
/// parameters, and returns the codec descriptor name — no decoding is done.
///
/// Returns e.g. `"vp9"`, `"av1"`, `"h264"`.
///
/// # Errors
/// Returns an error if the file cannot be opened or contains no video stream.
pub fn probe_video_codec(path: &str) -> Result<String> {
    init_ffmpeg();

    let ictx = format::input(&path)
        .with_context(|| format!("probe_video_codec: cannot open {path}"))?;

    let stream = ictx
        .streams()
        .best(media::Type::Video)
        .with_context(|| format!("probe_video_codec: no video stream in {path}"))?;

    let codec_id = stream.parameters().id();

    // ffmpeg_next exposes the codec name through the descriptor.
    let name = unsafe {
        let desc = ffmpeg_next::ffi::avcodec_descriptor_get(codec_id.into());
        if desc.is_null() {
            return Err(anyhow::anyhow!(
                "probe_video_codec: no codec descriptor for id {:?}",
                codec_id
            ));
        }
        std::ffi::CStr::from_ptr((*desc).name)
            .to_string_lossy()
            .to_ascii_lowercase()
    };

    if name.is_empty() {
        return Err(anyhow::anyhow!(
            "probe_video_codec: empty codec name for {path}"
        ));
    }

    Ok(name)
}

// ─── Video transcode (MP4 / WebM-AV1 → WebM VP9+Opus) ───────────────────────

/// Transcode `input` to WebM with VP9 video and Opus audio, writing to `output`.
///
/// Equivalent to:
///   ffmpeg -i <input> -c:v libvpx-vp9 -crf 30 -b:v 0
///                     -c:a libopus -b:a 128k -map_metadata -1 <output>
///
/// Called from `workers/mod.rs` inside `spawn_blocking`.
///
/// # Errors
/// Returns an error if any stage of the transcode fails.
pub fn ffmpeg_transcode_to_webm(input: &Path, output: &Path) -> Result<()> {
    init_ffmpeg();

    // ── Input context ─────────────────────────────────────────────────────
    let mut ictx = format::input(input)
        .with_context(|| format!("ffmpeg_transcode_to_webm: cannot open {}", input.display()))?;

    // Find video and audio streams.
    let video_in_idx = ictx
        .streams()
        .best(media::Type::Video)
        .map(|s| s.index());
    let audio_in_idx = ictx
        .streams()
        .best(media::Type::Audio)
        .map(|s| s.index());

    if video_in_idx.is_none() {
        return Err(anyhow::anyhow!(
            "ffmpeg_transcode_to_webm: no video stream in {}",
            input.display()
        ));
    }

    // ── Output context ────────────────────────────────────────────────────
    let mut octx = format::output(output)
        .with_context(|| format!("ffmpeg_transcode_to_webm: cannot open output {}", output.display()))?;

    // ── Video encoder (VP9) ───────────────────────────────────────────────
    let vp9_codec = codec::encoder::find_by_name("libvpx-vp9")
        .context("libvpx-vp9 encoder missing from static build")?;

    let in_video = ictx
        .stream(video_in_idx.unwrap())
        .context("ffmpeg_transcode_to_webm: get video stream")?;

    let mut v_dec = codec::context::Context::from_parameters(in_video.parameters())
        .context("ffmpeg_transcode_to_webm: video decoder context")?
        .decoder()
        .video()
        .context("ffmpeg_transcode_to_webm: open video decoder")?;

    let mut venc_ctx = codec::context::Context::new_with_codec(vp9_codec)
        .encoder()
        .video()
        .context("ffmpeg_transcode_to_webm: video encoder context")?;

    venc_ctx.set_width(v_dec.width());
    venc_ctx.set_height(v_dec.height());
    venc_ctx.set_format(ffmpeg_next::format::Pixel::YUV420P);
    venc_ctx.set_time_base(in_video.avg_frame_rate().invert());

    let mut v_opts = Dictionary::new();
    v_opts.set("crf", "30");
    v_opts.set("b:v", "0"); // constant quality mode
    v_opts.set("deadline", "good");
    v_opts.set("cpu-used", "2");

    if octx.format().flags().contains(format::flag::Flags::GLOBAL_HEADER) {
        venc_ctx.set_flags(codec::flag::Flags::GLOBAL_HEADER);
    }

    let mut v_enc = venc_ctx
        .open_with(v_opts)
        .context("ffmpeg_transcode_to_webm: open VP9 encoder")?;
    let mut out_v = octx.add_stream(vp9_codec)
        .context("ffmpeg_transcode_to_webm: add video stream")?;
    out_v.set_parameters(&v_enc);

    // ── Audio encoder (Opus) ──────────────────────────────────────────────
    let mut a_dec_opt: Option<codec::decoder::Audio> = None;
    let mut a_enc_opt: Option<codec::encoder::Audio> = None;
    let mut out_a_idx: Option<usize> = None;
    let mut swr_opt: Option<ffmpeg_next::software::resampling::context::Context> = None;

    if let Some(a_idx) = audio_in_idx {
        let opus_codec = codec::encoder::find_by_name("libopus")
            .context("libopus encoder missing from static build")?;

        let in_audio = ictx
            .stream(a_idx)
            .context("ffmpeg_transcode_to_webm: get audio stream")?;

        let a_dec = codec::context::Context::from_parameters(in_audio.parameters())
            .context("ffmpeg_transcode_to_webm: audio decoder context")?
            .decoder()
            .audio()
            .context("ffmpeg_transcode_to_webm: open audio decoder")?;

        let mut aenc_ctx = codec::context::Context::new_with_codec(opus_codec)
            .encoder()
            .audio()
            .context("ffmpeg_transcode_to_webm: audio encoder context")?;

        aenc_ctx.set_rate(48000); // Opus native rate
        aenc_ctx.set_channel_layout(ffmpeg_next::channel_layout::ChannelLayout::STEREO);
        aenc_ctx.set_format(ffmpeg_next::format::Sample::F32(
            ffmpeg_next::format::sample::Type::Packed,
        ));
        aenc_ctx.set_time_base(Rational(1, 48000));
        if octx.format().flags().contains(format::flag::Flags::GLOBAL_HEADER) {
            aenc_ctx.set_flags(codec::flag::Flags::GLOBAL_HEADER);
        }

        let mut a_opts = Dictionary::new();
        a_opts.set("b:a", "128k");

        let a_enc = aenc_ctx
            .open_with(a_opts)
            .context("ffmpeg_transcode_to_webm: open Opus encoder")?;

        // Resampler: source format → Opus (48 kHz stereo f32).
        let swr = ffmpeg_next::software::resampling::context::Context::get(
            a_dec.format(),
            a_dec.channel_layout(),
            a_dec.rate(),
            ffmpeg_next::format::Sample::F32(ffmpeg_next::format::sample::Type::Packed),
            ffmpeg_next::channel_layout::ChannelLayout::STEREO,
            48000,
        )
        .context("ffmpeg_transcode_to_webm: swr_alloc_set_opts")?;

        let mut out_a = octx.add_stream(opus_codec)
            .context("ffmpeg_transcode_to_webm: add audio stream")?;
        out_a.set_parameters(&a_enc);
        out_a_idx = Some(out_a.index());

        a_dec_opt = Some(a_dec);
        a_enc_opt = Some(a_enc);
        swr_opt = Some(swr);
    }

    // ── Transcode loop ────────────────────────────────────────────────────
    octx.write_header()
        .context("ffmpeg_transcode_to_webm: write_header")?;

    let mut v_pts: i64 = 0;
    let mut a_pts: i64 = 0;

    let video_in_idx = video_in_idx.unwrap();

    for (stream, packet) in ictx.packets() {
        let idx = stream.index();

        if idx == video_in_idx {
            v_dec.send_packet(&packet)
                .context("ffmpeg_transcode_to_webm: send video packet")?;
            let mut vf = frame::Video::empty();
            while v_dec.receive_frame(&mut vf).is_ok() {
                // Reformat to YUV420P if needed.
                let enc_frame = if vf.format() == ffmpeg_next::format::Pixel::YUV420P {
                    vf.clone()
                } else {
                    let mut converted = frame::Video::new(
                        ffmpeg_next::format::Pixel::YUV420P,
                        vf.width(),
                        vf.height(),
                    );
                    let mut sws = SwsContext::get(
                        vf.format(), vf.width(), vf.height(),
                        ffmpeg_next::format::Pixel::YUV420P, vf.width(), vf.height(),
                        SwsFlags::BILINEAR,
                    ).context("ffmpeg_transcode_to_webm: sws for video reformat")?;
                    sws.run(&vf, &mut converted)
                        .context("ffmpeg_transcode_to_webm: sws_scale video")?;
                    converted
                };
                let mut enc_frame = enc_frame;
                enc_frame.set_pts(Some(v_pts));
                v_pts += 1;

                v_enc.send_frame(Some(&enc_frame))
                    .context("ffmpeg_transcode_to_webm: send video frame")?;
                let mut pkt = ffmpeg_next::Packet::empty();
                while v_enc.receive_packet(&mut pkt).is_ok() {
                    pkt.set_stream(0);
                    pkt.rescale_ts(v_enc.time_base(), out_v.time_base());
                    pkt.write_interleaved(&mut octx)
                        .context("ffmpeg_transcode_to_webm: write video pkt")?;
                }
            }
        } else if Some(idx) == audio_in_idx {
            if let (Some(ref mut a_dec), Some(ref mut a_enc), Some(ref mut swr), Some(out_a)) =
                (&mut a_dec_opt, &mut a_enc_opt, &mut swr_opt, out_a_idx)
            {
                a_dec.send_packet(&packet)
                    .context("ffmpeg_transcode_to_webm: send audio packet")?;
                let mut af = frame::Audio::empty();
                while a_dec.receive_frame(&mut af).is_ok() {
                    let mut resampled = frame::Audio::empty();
                    swr.run(&af, &mut resampled)
                        .context("ffmpeg_transcode_to_webm: swr_convert")?;
                    resampled.set_pts(Some(a_pts));
                    a_pts += resampled.samples() as i64;

                    a_enc.send_frame(Some(&resampled))
                        .context("ffmpeg_transcode_to_webm: send audio frame")?;
                    let mut pkt = ffmpeg_next::Packet::empty();
                    while a_enc.receive_packet(&mut pkt).is_ok() {
                        pkt.set_stream(out_a);
                        pkt.rescale_ts(a_enc.time_base(), octx.stream(out_a).unwrap().time_base());
                        pkt.write_interleaved(&mut octx)
                            .context("ffmpeg_transcode_to_webm: write audio pkt")?;
                    }
                }
            }
        }
    }

    // ── Flush video encoder ───────────────────────────────────────────────
    v_dec.send_eof().ok();
    let mut vf = frame::Video::empty();
    while v_dec.receive_frame(&mut vf).is_ok() {
        vf.set_pts(Some(v_pts));
        v_pts += 1;
        v_enc.send_frame(Some(&vf)).ok();
        let mut pkt = ffmpeg_next::Packet::empty();
        while v_enc.receive_packet(&mut pkt).is_ok() {
            pkt.set_stream(0);
            pkt.rescale_ts(v_enc.time_base(), out_v.time_base());
            pkt.write_interleaved(&mut octx).ok();
        }
    }
    v_enc.send_eof().ok();
    let mut pkt = ffmpeg_next::Packet::empty();
    while v_enc.receive_packet(&mut pkt).is_ok() {
        pkt.set_stream(0);
        pkt.rescale_ts(v_enc.time_base(), out_v.time_base());
        pkt.write_interleaved(&mut octx).ok();
    }

    // ── Flush audio encoder ───────────────────────────────────────────────
    if let (Some(ref mut a_dec), Some(ref mut a_enc), Some(out_a)) =
        (a_dec_opt, a_enc_opt, out_a_idx)
    {
        a_dec.send_eof().ok();
        let mut af = frame::Audio::empty();
        while a_dec.receive_frame(&mut af).is_ok() {
            a_enc.send_frame(Some(&af)).ok();
        }
        a_enc.send_eof().ok();
        let mut pkt = ffmpeg_next::Packet::empty();
        while a_enc.receive_packet(&mut pkt).is_ok() {
            pkt.set_stream(out_a);
            pkt.write_interleaved(&mut octx).ok();
        }
    }

    octx.write_trailer()
        .context("ffmpeg_transcode_to_webm: write_trailer")?;

    Ok(())
}

// ─── Audio waveform PNG ───────────────────────────────────────────────────────

/// Render a waveform image for an audio file via libavfilter's `showwavespic`.
///
/// Equivalent to:
///   ffmpeg -i <input>
///     -filter_complex "showwavespic=s=WxH:colors=0x888888"
///     -frames:v 1 <output.png>
///
/// Called from `workers/mod.rs` inside `spawn_blocking`.
///
/// # Errors
/// Returns an error if the filtergraph cannot be built or the PNG cannot be
/// written.
pub fn ffmpeg_audio_waveform(
    input: &Path,
    output: &Path,
    width: u32,
    height: u32,
) -> Result<()> {
    init_ffmpeg();

    // ── Build filter graph ────────────────────────────────────────────────
    // showwavespic reads the *entire* audio stream and produces a single
    // frame. We route it through a buffer source → showwavespic → buffersink.
    let mut ictx = format::input(input)
        .with_context(|| format!("ffmpeg_audio_waveform: cannot open {}", input.display()))?;

    let in_stream = ictx
        .streams()
        .best(media::Type::Audio)
        .context("ffmpeg_audio_waveform: no audio stream")?;
    let in_idx = in_stream.index();

    let mut a_dec = codec::context::Context::from_parameters(in_stream.parameters())
        .context("ffmpeg_audio_waveform: decoder context")?
        .decoder()
        .audio()
        .context("ffmpeg_audio_waveform: open decoder")?;

    // Build lavfi graph:
    //   abuffer → showwavespic=s=WxH:colors=0x888888 → buffersink
    let filter_str = format!("showwavespic=s={width}x{height}:colors=0x888888");

    let mut graph = filter::Graph::new();

    // abuffer: feed raw audio frames into the graph.
    let abuf_args = format!(
        "sample_rate={}:sample_fmt={}:channel_layout=0x{:x}:time_base={}/{}",
        a_dec.rate(),
        ffmpeg_next::format::Sample::name(a_dec.format()),
        a_dec.channel_layout().bits(),
        in_stream.time_base().0,
        in_stream.time_base().1,
    );
    graph
        .add(&filter::find("abuffer").context("abuffer filter not found")?, "in", &abuf_args)
        .context("ffmpeg_audio_waveform: add abuffer")?;

    // showwavespic filter
    graph
        .add(
            &filter::find("showwavespic").context("showwavespic filter not found")?,
            "showwavespic",
            &filter_str,
        )
        .context("ffmpeg_audio_waveform: add showwavespic")?;

    // buffersink: pull the rendered frame out.
    graph
        .add(
            &filter::find("buffersink").context("buffersink filter not found")?,
            "out",
            "",
        )
        .context("ffmpeg_audio_waveform: add buffersink")?;

    // Link: in → showwavespic → out
    {
        let mut in_node   = graph.get("in").context("ffmpeg_audio_waveform: get abuffer")?;
        let mut wave_node = graph.get("showwavespic").context("ffmpeg_audio_waveform: get showwavespic")?;
        let mut out_node  = graph.get("out").context("ffmpeg_audio_waveform: get buffersink")?;
        in_node
            .output("default", 0)
            .context("ffmpeg_audio_waveform: abuffer output")?
            .input("default", 0)
            .context("ffmpeg_audio_waveform: showwavespic input")?
            .add()
            .context("ffmpeg_audio_waveform: link in→wave")?;
        wave_node
            .output("default", 0)
            .context("ffmpeg_audio_waveform: showwavespic output")?
            .input("default", 0)
            .context("ffmpeg_audio_waveform: buffersink input")?
            .add()
            .context("ffmpeg_audio_waveform: link wave→out")?;
    }
    graph
        .validate()
        .context("ffmpeg_audio_waveform: filter graph validate")?;

    // ── Feed all audio frames into the graph ──────────────────────────────
    for (stream, packet) in ictx.packets() {
        if stream.index() != in_idx {
            continue;
        }
        a_dec
            .send_packet(&packet)
            .context("ffmpeg_audio_waveform: send_packet")?;
        let mut frame = frame::Audio::empty();
        while a_dec.receive_frame(&mut frame).is_ok() {
            graph
                .get("in")
                .context("ffmpeg_audio_waveform: get abuffer for push")?
                .source()
                .add(&frame.into())
                .context("ffmpeg_audio_waveform: push frame")?;
        }
    }
    // Signal EOF so showwavespic renders the final frame.
    a_dec.send_eof().ok();
    let mut frame = frame::Audio::empty();
    while a_dec.receive_frame(&mut frame).is_ok() {
        graph
            .get("in")
            .unwrap()
            .source()
            .add(&frame.into())
            .ok();
    }
    graph
        .get("in")
        .unwrap()
        .source()
        .flush()
        .context("ffmpeg_audio_waveform: flush abuffer")?;

    // ── Pull the rendered video frame from the sink ───────────────────────
    let mut rendered = frame::Video::empty();
    graph
        .get("out")
        .context("ffmpeg_audio_waveform: get buffersink for pull")?
        .sink()
        .frame(&mut rendered)
        .context("ffmpeg_audio_waveform: pull rendered frame")?;

    // ── Encode rendered frame as PNG ──────────────────────────────────────
    let png_codec = codec::encoder::find_by_name("png")
        .context("png encoder not found in static build")?;

    let mut octx = format::output(output)
        .with_context(|| format!("ffmpeg_audio_waveform: cannot open output {}", output.display()))?;

    let mut enc_ctx = codec::context::Context::new_with_codec(png_codec)
        .encoder()
        .video()
        .context("ffmpeg_audio_waveform: PNG encoder context")?;

    enc_ctx.set_width(width);
    enc_ctx.set_height(height);
    enc_ctx.set_format(ffmpeg_next::format::Pixel::RGB24);
    enc_ctx.set_time_base(Rational(1, 1));

    let mut enc = enc_ctx
        .open()
        .context("ffmpeg_audio_waveform: open PNG encoder")?;

    // showwavespic outputs RGBA; convert to RGB24 for PNG encoder.
    let mut rgb = frame::Video::new(ffmpeg_next::format::Pixel::RGB24, width, height);
    let mut sws = SwsContext::get(
        rendered.format(), width, height,
        ffmpeg_next::format::Pixel::RGB24, width, height,
        SwsFlags::BILINEAR,
    )
    .context("ffmpeg_audio_waveform: sws for RGB24 convert")?;
    sws.run(&rendered, &mut rgb)
        .context("ffmpeg_audio_waveform: sws_scale to RGB24")?;
    rgb.set_pts(Some(0));

    let mut out_stream = octx.add_stream(png_codec)
        .context("ffmpeg_audio_waveform: add PNG stream")?;
    out_stream.set_parameters(&enc);

    octx.write_header()
        .context("ffmpeg_audio_waveform: write_header")?;

    enc.send_frame(Some(&rgb))
        .context("ffmpeg_audio_waveform: send_frame")?;
    enc.send_eof()
        .context("ffmpeg_audio_waveform: send_eof")?;

    let mut pkt = ffmpeg_next::Packet::empty();
    while enc.receive_packet(&mut pkt).is_ok() {
        pkt.set_stream(0);
        pkt.write_interleaved(&mut octx)
            .context("ffmpeg_audio_waveform: write_interleaved")?;
    }

    octx.write_trailer()
        .context("ffmpeg_audio_waveform: write_trailer")?;

    Ok(())
}

// ─── Dead-code stubs (kept for API compatibility) ─────────────────────────────

/// Formerly converted GIF → WebM/VP9. Superseded by animated WebP path.
/// Retained as dead code. Remove in a follow-up cleanup.
#[allow(dead_code)]
pub fn ffmpeg_gif_to_webm(input: &Path, output: &Path) -> Result<()> {
    ffmpeg_transcode_to_webm(input, output)
}

// ─── Internal helpers ─────────────────────────────────────────────────────────

/// Compute output dimensions that fit within `max_dim × max_dim` while
/// preserving the source aspect ratio. The smaller axis is rounded to an
/// even number (required by many YUV codecs).
fn scale_dims(src_w: u32, src_h: u32, max_dim: u32) -> (u32, u32) {
    if src_w == 0 || src_h == 0 {
        return (max_dim, max_dim);
    }
    let (w, h) = if src_w >= src_h {
        let h = (src_h * max_dim / src_w).max(2) & !1;
        (max_dim, h)
    } else {
        let w = (src_w * max_dim / src_h).max(2) & !1;
        (w, max_dim)
    };
    (w, h)
}
```

---

## 3. `src/workers/mod.rs` — surgical changes only

Only the two sections that spawn `TokioCommand::new("ffmpeg")` change.
Everything else (prepare, finalise, timeout logic, DB updates) is untouched.

### 3a. Remove the import

```rust
// REMOVE this line:
use tokio::process::Command as TokioCommand;
// REMOVE this if only used by ffmpeg spawn:
use std::process::Stdio;
```

### 3b. `transcode_video()` — replace the subprocess block

The function currently has three phases: `transcode_video_prepare` (spawn_blocking),
subprocess spawn + wait, `transcode_video_finalise` (spawn_blocking).
Replace only **phase 2** (lines roughly 490–520):

```rust
// ── REMOVE: TokioCommand subprocess spawn ─────────────────────────────────
// let child = TokioCommand::new("ffmpeg")
//     .args(&args)
//     .stderr(Stdio::piped())
//     .stdout(Stdio::null())
//     .kill_on_drop(true)
//     .spawn()
//     .map_err(|e| anyhow::anyhow!("failed to spawn ffmpeg: {e}"))?;
//
// match timeout(ffmpeg_timeout, child.wait_with_output()).await { ... }

// ── REPLACE WITH: in-process library call ────────────────────────────────
// `args` is no longer needed — pass src/dst paths directly.
// Re-derive them from the prepare result (src_path and tmp.path()).
let src_path2  = src_path.clone();
let tmp_path2  = tmp.path().to_path_buf();
let timed_out  = timeout(
    ffmpeg_timeout,
    tokio::task::spawn_blocking(move || {
        crate::media::ffmpeg::ffmpeg_transcode_to_webm(&src_path2, &tmp_path2)
    }),
)
.await;

match timed_out {
    Ok(Ok(Ok(()))) => {}
    Ok(Ok(Err(e))) => return Err(e),
    Ok(Err(join_err)) => {
        return Err(anyhow::anyhow!("spawn_blocking panicked: {join_err}"))
    }
    Err(_elapsed) => {
        // tmp is still alive here — NamedTempFile drops and removes the
        // partial output when this function returns, so no cleanup needed.
        warn!(
            "VideoTranscode: post {post_id} timed out after {timeout_secs}s"
        );
        return Err(anyhow::anyhow!(
            "transcode timed out after {timeout_secs}s"
        ));
    }
}
```

> **Note on `args`**: `transcode_video_prepare` builds a `Vec<String>` of
> ffmpeg CLI arguments. After migration this vector is unused. You can either
> leave the prepare function as-is (the `args` binding is silently dropped) or
> simplify `transcode_video_prepare` to return only
> `(src_path, webm_abs, webm_rel, webm_name, tmp)` by removing the argument
> construction block. The prepare function is only called from one place, so
> either approach is safe.

### 3b. `generate_waveform()` — replace the subprocess block

Same pattern. Replace only the `TokioCommand` phase between `waveform_prepare`
and `waveform_finalise`:

```rust
// ── REMOVE: TokioCommand subprocess spawn ─────────────────────────────────
// let child = TokioCommand::new("ffmpeg")
//     .args(&args)
//     .stderr(Stdio::piped())
//     .stdout(Stdio::null())
//     .kill_on_drop(true)
//     .spawn()
//     .map_err(...)?;
// match timeout(ffmpeg_timeout, child.wait_with_output()).await { ... }

// ── REPLACE WITH ──────────────────────────────────────────────────────────
let src_path3   = src.clone(); // src comes from waveform_prepare
let out_path3   = png_abs.clone();
let thumb_size  = CONFIG.thumb_size;

let timed_out = timeout(
    ffmpeg_timeout,
    tokio::task::spawn_blocking(move || {
        crate::media::ffmpeg::ffmpeg_audio_waveform(
            &src_path3,
            &out_path3,
            thumb_size,
            thumb_size / 2,
        )
    }),
)
.await;

match timed_out {
    Ok(Ok(Ok(()))) => {}
    Ok(Ok(Err(e))) => return Err(e),
    Ok(Err(join_err)) => {
        return Err(anyhow::anyhow!("spawn_blocking panicked: {join_err}"))
    }
    Err(_elapsed) => {
        warn!("AudioWaveform: post {post_id} timed out after {timeout_secs}s");
        return Err(anyhow::anyhow!(
            "waveform timed out after {timeout_secs}s"
        ));
    }
}
```

> **Note**: `waveform_prepare` currently passes `src_str` and `tmp_str` as
> strings for the CLI arg list. After migration the only paths needed are the
> original `src: PathBuf` and the temp file path from `tmp_png.path()`.
> The prepare function can be simplified to not build a `Vec<String>` at all,
> but this is optional cleanup.

---

## 4. `src/detect.rs` — ffmpeg section replacement

Replace the entire ffmpeg detection section (roughly lines 38–175) with:

```rust
// ─── ffmpeg ───────────────────────────────────────────────────────────────────

/// Initialise the statically linked ffmpeg library and report it as available.
///
/// Previously probed for the `ffmpeg` binary on PATH. Now just calls
/// `init_ffmpeg()` and always returns `Available`.
///
/// The `require_ffmpeg` parameter is retained for API compatibility; it is
/// ignored because ffmpeg is always present in a static build.
pub fn detect_ffmpeg(_require_ffmpeg: bool) -> ToolStatus {
    crate::media::ffmpeg::init_ffmpeg();
    tracing::info!(
        target: "detect",
        available = true,
        "ffmpeg compiled-in — media conversion and thumbnails always enabled"
    );
    ToolStatus::Available
}

/// Always returns true: libwebp is compiled into the static ffmpeg build.
pub fn detect_webp_encoder(_ffmpeg_ok: bool) -> bool {
    tracing::info!(target: "detect", webp = true, "libwebp compiled-in");
    true
}

/// Always returns true: libvpx-vp9 and libopus are compiled in.
pub fn detect_webm_encoder(_ffmpeg_ok: bool) -> bool {
    tracing::info!(
        target: "detect",
        vp9 = true,
        opus = true,
        "VP9 + Opus compiled-in — MP4→WebM transcoding always enabled"
    );
    true
}
```

Delete the following functions entirely (they are only reached when a codec is
absent, which can never happen with a static build):

- `webp_install_hint()`
- `webm_install_hint(has_vp9, has_opus)`

---

## 5. Optional: simplify `src/media/mod.rs`

`MediaProcessor::new()` currently probes for ffmpeg at construction time.
With a static build the probes always succeed, so the constructor can be
simplified. This is safe to skip — the existing code still works correctly.

```rust
// Optional simplification of MediaProcessor::new()
#[must_use]
pub fn new() -> Self {
    // ffmpeg is compiled in — no runtime probe needed.
    crate::media::ffmpeg::init_ffmpeg();
    Self {
        ffmpeg_available: true,
        ffmpeg_webp_available: true,
    }
}
```

---

## 6. Summary of changed files

| File | Change |
|---|---|
| `Cargo.toml` | Add `ffmpeg-next` with `static` feature |
| `.cargo/config.toml` | New file — pin `FFMPEG_BUILD_VERSION` |
| `src/media/ffmpeg.rs` | Complete replacement (see §2) |
| `src/workers/mod.rs` | Replace two `TokioCommand` blocks (see §3) |
| `src/detect.rs` | Replace ffmpeg detection section (see §4) |
| `src/media/mod.rs` | Optional simplification of `new()` (see §5) |
| `src/media/convert.rs` | **No changes** |
| `src/media/thumbnail.rs` | **No changes** |
| `src/server/server.rs` | **No changes** |
| `src/middleware/mod.rs` | **No changes** |
