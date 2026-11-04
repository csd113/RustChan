# Media capabilities

RustChan validates media content rather than trusting filenames. Upload types
must also be enabled by site and board policy. Supported preview processing is
bounded; valid files outside a preview capability keep their original download.
Initial image validation rejects invalid headers, first-frame decode failures,
and excessive decoded dimensions. Image conversion failures retain the original,
with unsupported, malformed, resource and I/O outcomes logged separately. This
legacy fallback also covers later GIF-frame corruption; retained WebP animations
are validated through their first frame, not exhaustively.

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

JPEG metadata removal preserves compressed scan data and retains only a fresh
orientation tag plus the fixed Adobe color-transform marker when needed. The
legacy original-byte fallback on normalization failure can retain source metadata.
Decode applies orientation once before WebP conversion and thumbnail resizing.
Resizing
uses associated alpha at 16-bit precision and the existing Triangle filter.
General ICC-to-sRGB conversion and linear-light filtering are not implemented;
converted output strips profiles. APNG animation is not preserved by the PNG
still path, and high-bit-depth inputs become 8-bit WebP.

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
`ffmpeg_timeout_secs` also supplies a cooperative deadline shared across native
image validation, conversion and thumbnail phases. Animation work is capped at
400 million decoded canvas pixels per traversal and 10,000 frames. Generated
image writers/spools are capped at 256 MiB, reduced to the upload image-size limit
when processing an upload. Temporary image outputs are cleaned on failure and
published atomically. HEIC preflight checks every SPS, including duplicate IDs,
and actual NAL headers in primary/alpha grid tiles before picture allocation.

Native decoders run in-process. Library calls can overrun a deadline; encoder
internal buffers and simultaneous decoder/resizer/compositor buffers remain
outside a universal allocation ceiling. Alpha filtering uses an 8-byte source
pixel buffer, and large transparent images can use substantial memory. These
checks cannot forcibly cancel a codec call or contain a release-mode abort. See
[security limits](../SECURITY.md#operational-security).

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
[`src/workers/mod.rs`](../src/workers/mod.rs). Capability claims should
follow the supported backend behavior; successful processing of one file
does not establish support for every codec profile or container variant.

## Storage failures

An unwritable media destination rejects the upload before committing a new
post. If storage fails after the post commits, the response identifies the saved
post and warns that its attachment could not be published. Check that post
before starting a new submission. Operators should restore storage access and
restart RustChan to replay its retained upload journal; keep the staged files
and database together until recovery completes.
