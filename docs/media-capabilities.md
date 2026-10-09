# Media capabilities

RustChan validates media content rather than trusting filenames. Upload types
must also be enabled by site and board policy. Supported preview processing is
bounded; valid files outside a preview capability keep their original download.
Malformed data and resource-limit failures follow the upload error contract.

## Processing backends

| Input or operation | Backend | Coverage and limits |
| --- | --- | --- |
| JPEG, PNG, BMP, TIFF, static WebP | `image` and EXIF parsing | Generated conversions/thumbnails apply resizing, orientation, alpha handling, metadata stripping, and lossless WebP encoding. Original WebP and a smaller original PNG can be retained with original metadata. SVG uploads are rejected; generated SVG placeholders are separate. |
| GIF and animated WebP | Rust compositor, resizer, and WebP writer | Composited frames, disposal, transparency, timing, and loops. Preserve a valid original GIF when its finite loop count cannot be represented by WebP. |
| HEIC/HEIF still images | `heic-rs` | Tiled primary pictures, container rotation, and auxiliary alpha. General HDR gain-map rendering and every ICC/EXIF combination are not supported. |
| Container classification and primary codec | Symphonia and bounded container validators | MP4/M4A, Matroska/WebM, Ogg, and common audio signatures. Unsupported audio-container mappings can use the explicit FFmpeg probe compatibility path. |
| PCM, MP1/2/3, FLAC, Vorbis, ALAC, AAC-LC waveforms | Symphonia | Streaming channel-mean envelopes. AAC-LC MP4 supports ordered, unit-rate forward edit ranges and empty edits; unsupported presentation edits select compatibility after validation. |
| Opus waveforms | `opus-pure` with Rust framing/mapping | Supported mono/stereo/surround mappings: Ogg families 0/1 and supported single-stream-container family 255. Other container/mapping combinations can require compatibility or be rejected. |
| HE-AAC/SBR/parametric stereo, Speex, AC-3, other uncovered audio | FFmpeg compatibility | Requires the corresponding installed decoder. Explicit unsupported capabilities may select fallback; corruption, I/O, cancellation, internal errors, and resource failures do not. |
| Bounded vector/text PDF first page | Hayro, embedded standard fonts, bounded Flate expansion | Small flat resource trees and supported vector/text operations, including supported compressed streams. Aggregate source/expanded content is limited to 2 MiB and the flat page tree to 4,096 objects. |
| Complex PDF | Built-in SVG preview | Inline/embedded images, embedded fonts, encryption, nested resource graphs, and other unbudgeted features retain the original PDF with a placeholder. |
| Video thumbnail | FFmpeg followed by Rust image processing | FFmpeg extracts a PNG frame; Rust encodes the WebP thumbnail. No FFmpeg WebP encoder is required. |
| Video conversion | FFmpeg | VP9/Opus WebM output requires `libvpx-vp9`, `libopus`, and a WebM muxer. Compatible VP8/VP9 WebM with Opus/Vorbis audio is preserved. AV1 input requires an AV1 decoder; output remains VP9/Opus. |

Banners use the same Rust image/animation processing with their own geometry and
frame limits; see [banner requirements](../SETUP.md#banner-artwork-requirements).
The [vendored AAC patch](../vendor/symphonia-codec-aac/RUSTCHAN-PATCH.md) detects
in-band SBR and requests compatibility rather than publishing incomplete AAC-LC
core audio. It does not implement SBR reconstruction.

## External tools and resource boundaries

FFmpeg is optional unless `require_ffmpeg` is enabled. It is used for video
thumbnails/transcoding, audited unsupported audio waveforms/container probes,
and capability discovery. There is no production `ffprobe` invocation or external
image/PDF renderer. Missing codecs leave valid originals available and use
placeholders where necessary.

Decoded dimensions, animation frames, audio samples/packets, container work,
PDF syntax/expansion, queue size, and active media work are bounded. Subprocess
output, deadlines, and descendant cleanup are managed centrally in
[`src/media/process.rs`](../src/media/process.rs). The configured
`ffmpeg_timeout_secs` applies to FFmpeg jobs. Native decoders run in-process;
these limits provide no universal allocation ceiling or forcible cancellation
for arbitrary library work. See [security limits](../SECURITY.md#operational-security).

Heavy conversion uses durable jobs. Publication checks generated content and
coordinates database references with restart-replayable filesystem intents.
Video conversions can redirect stale original URLs to the completed artifact.
See [the database guide](sqlite-engineering.md) for persistence boundaries.

## Maintenance

Previews are generated during upload or durable worker processing. Still
thumbnails fit the configured square (default 250×250) without enlarging small
images; animation previews use the first composited frame. Page rendering and
thumbnail GETs use stored paths rather than decoding or regenerating media.
Delivery checks current board permissions, validates filesystem containment,
and streams the file. Public UUID media uses immutable caching; view-password
media requires private revalidation, and media errors use private no-store.
Keep these authorization/cache boundaries when changing preview delivery in
[`src/handlers/board/media.rs`](../src/handlers/board/media.rs).

Backend routing lives in [`src/media/`](../src/media/) and
[`src/workers/mod.rs`](../src/workers/mod.rs). Small synthetic fixtures and
independent PCM references are maintained in
[`tests/fixtures/media/`](../tests/fixtures/media/README.md). Preserve their
provenance and license notices when replacing fixtures. Capability claims should
follow source and regression coverage; successful processing of one fixture
does not establish complete support for a format.
