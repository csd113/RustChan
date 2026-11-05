# Uploads and media previews

RustChan accepts pictures, video, audio, and optional PDFs or other files. The
site owner chooses which types each board allows. A preview is the thumbnail,
waveform, or converted version shown beside a post; a valid upload can remain
downloadable even when RustChan cannot generate its preview.

[All guides](../README.md#documentation) · [Install FFmpeg](../SETUP.md#install-ffmpeg) · [Administration](administration.md)

## What works without extra tools?

Built-in Rust libraries process common images, GIF/WebP animation, HEIC/HEIF,
many audio waveforms, and simple PDF previews. FFmpeg adds video thumbnails,
video conversion, and audio formats the built-in decoders do not cover. It is
optional unless the operator enables `require_ffmpeg`.

Common supported uploads include JPEG, PNG, GIF, WebP, HEIC/HEIF, BMP, TIFF,
MP4, WebM, MP3, OGG, FLAC, WAV, M4A, and AAC. The container and codec inside a file
matter as well as its extension. SVG uploads are rejected because they can
contain executable content. Generated SVG placeholders are separate.

If a valid file has no supported preview path, RustChan can keep the original
and show a placeholder. Missing FFmpeg or codecs does not make every upload
fail. Check startup capability warnings and the board's upload settings first.

## If an upload reports a storage error

An unwritable destination rejects the upload before the new post is committed.
If storage fails afterward, the response identifies the saved post and warns
that its attachment could not be published. Check that post before submitting
again. Restore storage access, preserve staged files and the database together,
and restart RustChan to replay the retained upload journal.

## Processing reference

This table describes coverage, not a guarantee that every file with an extension
will decode. RustChan checks content rather than trusting filenames.

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

Banners share the image/animation pipeline but have their own size and frame
limits; see [banner artwork](administration.md#banners-and-appearance).
The [AAC patch](../vendor/symphonia-codec-aac/RUSTCHAN-PATCH.md) detects in-band
SBR and requests FFmpeg compatibility instead of publishing incomplete AAC-LC
core audio. It does not reconstruct SBR.

### Image and metadata limits

Initial validation rejects bad headers, first-frame decode failures, and
excessive dimensions. Conversion failures can retain the original, with separate
logs for unsupported content, malformed data, resource limits, and I/O errors.
That fallback includes later GIF-frame corruption; retained WebP animations are
checked through their first frame, not exhaustively.

Generated conversions and thumbnails strip metadata, but retained originals can
still contain it. JPEG metadata removal preserves compressed scan data and keeps
only a fresh orientation tag and, when needed, the fixed Adobe color-transform
marker. A normalization failure can retain original bytes and metadata. Do not
assume that every served upload has had identifying metadata removed.

Orientation is applied once before conversion/resizing. Resizing uses associated
alpha at 16-bit precision with the Triangle filter. General ICC-to-sRGB conversion
and linear-light filtering are not implemented; converted files strip profiles.
The PNG path does not preserve APNG animation, and high-bit-depth input becomes
8-bit WebP.

### Resource boundaries

Decoded dimensions, animation frames, audio samples/packets, container work,
PDF syntax/expansion, queues, and active media work are bounded. FFmpeg output,
deadlines, and descendant-process cleanup are handled in
[`src/media/process.rs`](../src/media/process.rs). No production path invokes
`ffprobe` or an external image/PDF renderer.

`ffmpeg_timeout_secs` also sets a cooperative deadline across native image
validation, conversion, and thumbnails. Animation traversal is limited to
400 million decoded canvas pixels and 10,000 frames. Generated image writers and
spools are limited to 256 MiB, reduced to the upload image limit during uploads.
Temporary outputs are cleaned after failure and published atomically. HEIC
preflight checks every SPS, including duplicate IDs, and actual NAL headers in
primary/alpha grid tiles before allocating pictures.

Native codecs run inside the RustChan process. A codec call can exceed a deadline;
these limits cannot forcibly cancel it or contain a release-mode abort. Encoder
buffers and simultaneous decoder/resizer/compositor buffers are not covered by
one total allocation ceiling. Alpha filtering uses an eight-byte source-pixel
buffer, so large transparent images can use substantial memory. See
[security limits](../SECURITY.md#operational-security).

## Preview storage and delivery

Upload processing or durable background jobs generate previews. Still thumbnails
fit a square of `thumb_size` (default 250×250) without enlarging small images;
animation previews use the first composited frame. Pages and thumbnail requests
use stored files, without decoding or regenerating media on every request.

Heavy conversions use durable jobs. Publication validates output and coordinates
database references with filesystem operations that can be replayed after a
restart. A completed video conversion can redirect an old original URL to the
new artifact. See [database coordination](sqlite-engineering.md#transactions-and-filesystem-coordination).

Delivery checks board permissions and filesystem containment, then streams the
file. Public UUID media uses immutable caching; password-protected media requires
private revalidation, and media errors use private no-store. Preserve those
boundaries in [`src/handlers/board/media.rs`](../src/handlers/board/media.rs).
Backend routing lives in [`src/media/`](../src/media/) and
[`src/workers/mod.rs`](../src/workers/mod.rs).
