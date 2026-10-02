# Non-video media backend migration

Current video pipeline note (2026-10-02): FFmpeg extracts PNG video frames; Rust encodes their WebP thumbnails. FFmpeg WebP encoder detection and installation requirements have been removed. WebM conversion retains VP9 + Opus with independent AV1 decoder/encoder diagnostics. Historical validation results below describe the implementation at the time of that audit.


> Historical report for the initial migration at `16f4b91`. The current [capability matrix and follow-up evidence](media-capabilities.md) supersede the uncommitted status, Opus compatibility limitations and unavailable Docker-base conclusions below. Remaining compatibility calls and PDF limitations are still explicit.

Implemented backend migration, with the explicitly authorized FFmpeg compatibility fallback for uncovered audio codecs/variants. All changes remain uncommitted. Rust 1.99 is now the minimum in Cargo, CI, release builds, Docker and setup documentation. Actual video frame decoding and VP9/Opus WebM transcoding remain on FFmpeg.

**Final verification conclusion:** suitable for review, but production readiness is not established. The final pass below records passing validation, local fixes, remaining AAC/Speex compatibility limits, and the failed Docker base-image resolution. Do not treat the initial migration results as release approval.

## Pre-implementation audit

| Original operation / call site | Class | Required behavior | Implemented replacement / retained backend | Semantic regression coverage |
| --- | --- | --- | --- | --- |
| `media/ffmpeg.rs::ffmpeg_image_to_webp`, used by `convert.rs` | IMAGE | JPEG, BMP, TIFF, HEIC/HEIF to metadata-free WebP; PNG retained only when smaller | Existing `image` lossless WebP encoder; `heic-rs` still decoder | Decode output, dimensions, orientation, alpha, metadata and PNG size policy |
| Same helper with GIF input | IMAGE | Animated WebP, composited frames, delays, transparency and repeat behavior | Existing `image` GIF compositor and lossless WebP encoder, bounded animation container writer | Decode every output frame and compare pixels, delays and loops |
| `ffmpeg_image_to_webp_scaled`, used by `banner.rs` | IMAGE | GIF and animated WebP banners scaled without flattening | Same animation pipeline, existing image WebP decoder | Partial frames, disposal, bounds and stored animation compatibility |
| `thumbnail.rs` image branch -> `ffmpeg_thumbnail` | IMAGE | First composited frame thumbnail, alpha, aspect ratio, no upload mutation | Existing `image` decoder/resizer/encoder plus HEIC still decoder | Thumbnail dimensions/pixels with FFmpeg absent |
| `thumbnail.rs` video branch -> `ffmpeg_thumbnail` | VIDEO | Decode compressed video frame, fit thumbnail; SVG fallback on failure | FFmpeg extracts a PNG frame; Rust encodes WebP (updated by the AV1 follow-up) | Existing video thumbnail tests |
| `probe_stream_kind`, called from upload validation and worker output validation | CONTAINER PROBING | Audio/video classification; MP4/M4A, WebM/MKV, standalone audio; malformed/ambiguous handling | Symphonia ISOBMFF/Matroska/audio format parsers | Old/new stream-kind and malformed fixture comparison |
| `probe_stream_kind_with_ffmpeg` -> two zero-frame stream-map commands | CONTAINER PROBING | Establish ambiguous WebM kind without decoding, neutral download if ambiguous | Same Rust container parsers; unsupported audio containers retain bounded zero-frame FFmpeg maps | Audio WebM, video WebM and ambiguous policy tests |
| `probe_video_codec`, called by transcode preparation/output validation | CONTAINER PROBING | First video codec, skip existing VP9, verify generated VP9 | Same container parsers; actual video processing retained | MP4 H.264 and WebM VP9 before/after |
| `probe_audio_codec`, called by MIME canonicalization | CONTAINER PROBING | Primary codec determines canonical audio MIME | Audio/container parser metadata | MP1/2/3, PCM, Vorbis, FLAC, AAC, Opus and M4A |
| `workers::generate_waveform` / `waveform_prepare` -> `showwavespic` | AUDIO | PNG of thumb width × half-height, streaming decode, deterministic envelope, atomic job persistence, timeout/cancellation | Symphonia plus focused pure-Rust Opus decoder; unavailable codecs/variants use the approved FFmpeg fallback | Supported formats, silence, short/stereo audio, malformed input, job persistence and no executable |
| `thumbnail.rs`: `pdftoppm`, `mutool draw`, `qlmanage -t` | PDF | Bounded first-page WebP or existing static SVG placeholder; rendering failure does not reject valid PDF | Hayro behind a conservative input/work gate; static fallback retained | Simple render, malformed/unsupported PDF fallback |
| `workers::transcode_video` -> `build_vp9_transcode_args` | VIDEO TRANSCODING | Existing libvpx-vp9/libopus flags, color normalization, tuning, timeouts, cancellation, atomicity and queue behavior | Retain existing FFmpeg backend | Existing transcode argument, integration and job tests unchanged |
| `detect.rs`, `media/ffmpeg.rs`: executable `-version` and `-encoders` probes | Capability reporting | Accurate startup/admin status without obsolete non-video tool requirements | Retain FFmpeg/video encoder detection; standalone FFprobe and image/PDF process detection removed | Startup with nonexistent executable paths and reporting tests |

Related audit surfaces: `media/process.rs` owns bounded subprocess output and process-group cleanup (retained for video); `utils/files/storage.rs` owns validation, orientation, size policy and atomic upload handoff; `workers/mod.rs` owns durable jobs and waveform persistence; application state, posting handlers, maintenance reports, configuration, Dockerfile, README, SETUP and browser media tests carry executable capability assumptions. SVG upload rejection and no-JS/UI hooks remain unchanged.

## Dependencies and implementation choices

| Direct dependency | Why selected | Enabled scope / native-library audit |
| --- | --- | --- |
| Existing `image` 0.25.10 | Reuse the existing mature codecs, resizer, orientation support and lossless WebP encoder | JPEG/PNG/GIF/WebP/BMP/TIFF/ICO; no new native image library |
| `gif` 0.14.2 (already transitive) | Read repeat metadata without retaining the whole animation | `default-features = false`; `std`, `color_quant`; pure Rust |
| [Symphonia 0.6.1](https://github.com/pdeljanov/Symphonia) | Established incremental audio decoders and content-based container parsers | Only required audio codecs and ISOBMFF/MKV/Ogg/WAV readers; experimental video **declarations** enable metadata IDs, with no video decoder registered |
| [opus-decoder 0.1.1](https://docs.rs/opus-decoder/0.1.1/opus_decoder/) | Symphonia supplies Opus packets but lacks an Opus decoder | No defaults, no native libopus, no build script; mono/stereo CELT and low-bitrate voice fixtures verified |
| [heic-rs 0.1.1](https://github.com/tbraun96/heic-rs) | Focused HEVC-intra still-picture decoder with container transforms and pixel bounds | `std` only, one decoding thread, no libheif/FFmpeg/system HEVC library or build script |
| [Hayro 0.7.1](https://github.com/LaurenzV/hayro) | First-page vector/text PDF rendering without a native PDF executable/library | No defaults; embedded standard fonts; Rust font, raster, compression and image dependencies |

`webpkit` was evaluated; its Rust 1.96 requirement is now covered by the requested Rust 1.99 minimum. It would duplicate existing codecs. A small checked RIFF animation writer reuses `image`'s lossless VP8L encoding instead. It writes full composited frames to temporary output without retaining encoded frames for the entire animation.

`cargo tree` and `cargo tree --edges features` were inspected. The added media dependency closures contain no FFmpeg/libwebp/libheif/native-Opus bindings or multimedia `*-sys` build dependencies. Existing SQLite/TLS/native dependencies elsewhere in the program remain. The lockfile adds 50 package versions and removes no previous package versions. `cargo deny --locked check` passed advisories, bans, licenses and sources (existing duplicate-version and `captcha` manifest-license warnings remain).

HEIC and Opus decoders are recent focused implementations. Their selected behavior is covered by representative real/generated files, rather than a claim of universal codec conformance. In particular, the Opus crate's multistream splitter rejects a valid standard libopus 5.1 fixture; RustChan deliberately does not use that implementation. Valid mapped Opus streams use the approved compatibility backend. The pinned Opus version maps both unimplemented features and internal transform failures to public `InternalError`. That error now fails closed; only validated multistream metadata explicitly selects compatibility.

## Before/after format coverage

| Format/operation | Previous behavior | Current behavior / verified evidence |
| --- | --- | --- |
| JPEG, BMP, TIFF | FFmpeg WebP conversion when available; image-crate fallback previews | Rust lossless WebP; exact decoded pixel equality, dimensions, thumbnails, stripped output metadata |
| PNG, transparent PNG | Keep converted WebP only when smaller | Same size policy in Rust; alpha and decoded pixel equality preserved |
| GIF | FFmpeg animated WebP when codec/tool available | Rust animated WebP; four partial/composited frames, Keep/Previous/Background disposal, transparent pixels, 30/110/230/70 ms delays, infinite and finite loops decoded and compared |
| Static WebP | Preserve upload and generate preview | Existing upload compatibility retained; Rust preview and alpha checks |
| Animated WebP | Preserve stored animation; optional scaled animation for banners | Source animation remains intact; scaled banner output is decoded to prove multiple frames, alpha and timing |
| HEIC/HEIF | FFmpeg still-picture conversion/preview | Rust tiled HEIC and iPhone primary-image decode, alpha and container rotation; exact decoded-pixel equality after lossless WebP encoding and bounded thumbnails |
| MP4/M4A, WebM, MKV | Separate FFprobe, or zero-frame FFmpeg stream inspection | Rust content-based tracks/primary codec inspection; nineteen fixtures compared with FFprobe during final verification, plus malformed input and misleading extensions |
| MP3, FLAC, WAV/PCM, Vorbis, AAC-LC, M4A/ALAC, common Opus, audio WebM/MKV | FFmpeg waveform; placeholder without FFmpeg | Streaming Rust audio and width-sized envelope; MP3/FLAC/WAV/Vorbis/AAC/M4A/Opus/audio-WebM fixtures verified; MKV FLAC track inspection verified |
| Opus mono/stereo, voice/SILK | FFmpeg | Rust, including exact encoder-delay/end-padding handling and header gain |
| Uncovered audio: AC-3, Speex, unsupported AAC profiles, mapped Opus/multistream | FFmpeg when present, otherwise placeholder | Approved FFmpeg compatibility path, selected only for unavailable formats/codecs/variants; AC-3 M4A and standard 5.1 Opus waveform output verified |
| PDF | External Poppler/MuPDF/QuickLook preview when available, otherwise SVG | Hayro renders a small bounded uncompressed vector/text subset; broader or unsafe rendering uses the existing SVG without changing/rejecting the PDF download |
| Video frame thumbnails | FFmpeg/libwebp or SVG | FFmpeg PNG extraction and Rust WebP encoding; SVG fallback on failure |
| Video → WebM | FFmpeg libvpx-vp9/libopus | Retained encoder arguments, color normalization, CPU tuning, timeouts, cancellation, atomic DB/file lifecycle; actual browser transcode, WebM playback markup and stale-MP4 redirect pass |

The compatibility fallback was explicitly authorized after reproducing AC-3 M4A failure in Rust and success with the original `showwavespic` command. It overrides the objective's earlier strict no-audio-FFmpeg rule. Common codecs never consult an executable to render their waveforms.

## Equivalence, privacy and resource limits

- Static lossless WebP pixels match the decoded source exactly; EXIF, ICC and XMP are absent from newly encoded WebP. JPEG orientation 6 remains correct through conversion and thumbnailing. The existing original-PNG size policy and original-file error fallback remain.
- GIF semantic tests decode every WebP frame and compare pixels, order, delays and repeat behavior against the original GIF compositor. A finite GIF repeat count is mapped to WebP total plays. Existing animated WebP scaling remains animated.
- HEIC fixtures include a 2048 × 1536 tiled Apple-encoded scene, 48 × 48 auxiliary alpha, 40 × 64 rotated color corners, and an actual 1512 × 850 iPhone HDR file. HEIF extension handling uses content. Pixel comparisons with FFmpeg gave mean absolute 8-bit channel differences of about 0.19 for the generated tiled scene and 4.90 for the phone primary SDR image; exact encoded bytes/colorspace rounding are not promised. HDR gain-map rendering is outside the existing primary-thumbnail behavior. Provenance and licenses are in `tests/fixtures/media/README.md`.
- The old Poppler renderer and Hayro produce the same 100 × 80 first-page geometry and red interior for the two-page vector PDF fixture. A valid compressed PDF is accepted unchanged and receives SVG. **PDF rendering coverage is deliberately narrower:** flat page trees, an object ceiling before parsing, and a source-size × viewport-pixel work ceiling precede rendering. Compressed streams, embedded images/fonts, recursion-capable resources and other unbudgeted features use placeholders because Hayro exposes no general allocation/work cancellation budget.
- Decoding is off the async executor. Existing upload limits remain; images have a 40-million-pixel budget and allocation limits. Animation processes frames incrementally, checks dimensions, limits frame count to 10,000, and uses the operator deadline. Output RIFF lengths/durations are checked, animated output is capped at 256 MiB before each frame write, and publication uses temporary files.
- HEIC source reads are bounded to the shared allocation limit; primary/alpha coded SPS dimensions are checked before plane allocation, tile counts are bounded, and the sum of coded tile areas is bounded. Codec-level work is cooperative at existing operation boundaries.
- Audio uses two streaming passes (count frames, then fill peak buckets), a 4-million-interleaved-sample packet ceiling, finite-sample checks, deterministic mean absolute channel amplitude, and existing cancellation/deadline checks. Tests cover silence, a single sample frame, opposite-phase six-channel PCM, 120-second PCM, malformed input, unavailable-codec selection and Opus trimming. No full-clip PCM buffer is retained.
- Symphonia container readers cache regular-file length and enforce cancellation, a deadline, a 64 MiB header-read ceiling, a 16 MiB packet-read ceiling, and a repeated-read budget before progressive parser allocations. Metadata tag/artwork sizes are capped. Malformed input, I/O errors, cancellation and resource failures do not select the compatibility decoder.
- Waveform persistence keeps validated paths, temporary output, database transactions, hashes, rollback cleanup and durable job recovery. Faster waveform jobs exposed a stale-SVG request race; protected-board/path checks now precede a temporary redirect to an existing completed PNG, including the race where the file disappears just before `ServeFile` opens it.

Useful debug measurements on this Apple-silicon host: isolated HEIC decode was about 1.52 s for the tiled scene, 0.97 s for the phone photo, and 1–2 ms for the small alpha/rotation fixtures. These are debug measurements and include no release-performance claim.

## Remaining process responsibilities

- `media/ffmpeg.rs::ffmpeg_thumbnail`: actual video-frame decoding to PNG. Rust now encodes the WebP preview; the AV1 follow-up removed FFmpeg WebP encoder detection.
- `workers::transcode_video` / `media/ffmpeg.rs::build_vp9_transcode_args`: retained video WebM transcoder. Both the argument builder and CPU profile selection were compared with HEAD and are unchanged.
- `workers::render_waveform_with_compatibility`: original bounded `showwavespic` command for uncovered audio only, sharing the worker deadline, process-group shutdown and atomic publication.
- `media/ffmpeg.rs::probe_uncovered_audio`: bounded zero-frame stream maps only after an unsupported audio container/parser result, preserving accepted codec/container variants.
- FFmpeg version/encoder detection and configuration executable validation remain for the retained backend. No standalone FFprobe or external PDF renderer process wrapper remains. Docker removes the unneeded standalone `/usr/bin/ffprobe` after installing FFmpeg.

## Initial migration validation (superseded by final verification below)

All checks use Rust/cargo 1.99.0. Tests include the existing video argument/lifecycle tests, newly generated media fixtures, strong image/animation pixel checks, real HEIC files, primary-codec/stream comparisons, common audio/voice decoding, multistream compatibility regression, PDF rendering/fallback, malformed inputs and atomic waveform persistence. Browser fixtures now contain real containers rather than header-only fake media; byte-exact PNG assertions allow the original smaller-WebP retention policy.

| Command / pass | Result |
| --- | --- |
| `cargo fmt --all --check` | Passed |
| `cargo check --workspace --all-targets --all-features` | Passed |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings -D clippy::all -D clippy::pedantic -D clippy::nursery -D clippy::cargo` | Passed |
| `cargo test --workspace --all-features` | Passed: 1,266 tests in each library/application suite, 5 CLI tests, 1 UI fixture test and 1 doc test |
| `cargo tree`; `cargo tree --edges features` | Inspected |
| `cargo deny --locked check` | Passed all policies; existing duplicate-version warnings |
| `CHAN_FFMPEG_PATH=__rustchan_non_video_tools_unavailable__ CHAN_REQUIRE_FFMPEG=0 cargo test --workspace --all-features media::` | Passed: 91 media tests in each library/application suite with FFmpeg unavailable |
| `cargo test --all-features waveform_common_codecs_work_without_tools_and_uncovered_codecs_use_fallback` | Passed for common Opus with tool disabled, AC-3/Opus 5.1 with tool restored, and malformed-input rejection |
| `npx playwright test tests/e2e/upload-validation.spec.ts --project=chromium --project=chromium-nojs --workers=2` | Passed: 55 scenarios with JS, 54 with no JS and one JS-only scenario skipped |
| `npx playwright test tests/e2e/non-video-media.spec.ts --project=chromium --project=chromium-nojs --workers=2` | Passed: all eight common formats complete waveform jobs without FFmpeg in both modes |
| Final combined upload-validation and non-video-media browser run, both Chromium projects | Passed: all 4 tests with the final implementation |
| `RUSTCHAN_E2E_FFMPEG_PATH=/tmp/rustchan-video-validation/ffmpeg-9.0.2/ffmpeg npm run test:e2e:media` | Passed: actual video WebP thumbnail, VP9/Opus WebM, redirect, audio waveform, internal PDF preview and responsive media layouts |
| `git diff --check` | Passed |

The installed FFmpeg lacked libwebp, so a temporary source build with libwebp/libvpx/libopus was used to run the unchanged strict video browser test. It lives only under `/tmp`; no system FFmpeg installation or runtime video architecture was replaced. Docker was subsequently attempted in the final verification below. Hosted CI/release jobs were not run remotely.

## Files changed

- `.github/workflows/ci.yml`
- `.github/workflows/release.yml`
- `Cargo.lock`
- `Cargo.toml`
- `Dockerfile`
- `README.md`
- `SETUP.md`
- `docs/admin-redesign-coverage.md`
- `docs/containers.md`
- `docs/non-video-media-migration.md`
- `docs/strict-lint-exceptions.tsv`
- `src/banner.rs`
- `src/config.rs`
- `src/config/admin.rs`
- `src/config/admin/runtime.rs`
- `src/config/template.rs`
- `src/detect.rs`
- `src/handlers/admin/mod.rs`
- `src/handlers/board/access_preferences.rs`
- `src/handlers/board/create_thread.rs`
- `src/handlers/board/media.rs`
- `src/handlers/mod.rs`
- `src/handlers/posting.rs`
- `src/handlers/setup.rs`
- `src/handlers/thread.rs`
- `src/media/audio.rs`
- `src/media/convert.rs`
- `src/media/ffmpeg.rs`
- `src/media/heif.rs`
- `src/media/images.rs`
- `src/media/images/tests.rs`
- `src/media/mod.rs`
- `src/media/pdf.rs`
- `src/media/probe.rs`
- `src/media/thumbnail.rs`
- `src/middleware/state.rs`
- `src/pending_fs.rs`
- `src/server/server.rs`
- `src/templates/admin.rs`
- `src/templates/admin/maintenance.rs`
- `src/templates/admin/network.rs`
- `src/templates/admin/site_health.rs`
- `src/test_fixtures.rs`
- `src/utils/files/storage.rs`
- `src/workers/mod.rs`
- `tests/cli_runtime.rs`
- `tests/e2e/README.md`
- `tests/e2e/helpers.ts`
- `tests/e2e/media-toolchain-check.mjs`
- `tests/e2e/media-toolchain.spec.ts`
- `tests/e2e/non-video-media.spec.ts`
- `tests/e2e/upload-regressions.spec.ts`
- `tests/e2e/upload-validation.spec.ts`
- `tests/fixtures/media/README.md`
- `tests/fixtures/media/ac3.m4a`
- `tests/fixtures/media/alpha.heic`
- `tests/fixtures/media/audio.mkv`
- `tests/fixtures/media/audio.webm`
- `tests/fixtures/media/compressed.pdf`
- `tests/fixtures/media/irot90.heic`
- `tests/fixtures/media/phone-hdr-LICENSE`
- `tests/fixtures/media/phone-hdr.heic`
- `tests/fixtures/media/photo-LICENSE-APACHE`
- `tests/fixtures/media/photo-LICENSE-MIT`
- `tests/fixtures/media/photo.heic`
- `tests/fixtures/media/simple.pdf`
- `tests/fixtures/media/speech-mode.opus`
- `tests/fixtures/media/surround.opus`
- `tests/fixtures/media/tone.aac`
- `tests/fixtures/media/tone.flac`
- `tests/fixtures/media/tone.m4a`
- `tests/fixtures/media/tone.mp3`
- `tests/fixtures/media/tone.ogg`
- `tests/fixtures/media/tone.opus`
- `tests/fixtures/media/tone.wav`
- `tests/fixtures/media/video.mkv`
- `tests/fixtures/media/video.mp4`
- `tests/fixtures/media/video.webm`

## Final verification and polish pass

The current candidate, including changed and untracked source, configuration, templates, documentation, tests and fixtures, was reviewed against the pre-migration HEAD. Existing user changes were preserved. No commit, branch, push, tag, release, backup deletion or known-good replacement was performed. No application dependency was added during this pass.

### Issues found and local fixes

- The broad `tests/` ignore hid new compile-time media fixtures and the new browser specification. Narrow exceptions now expose those inputs to Git while generated browser output remains ignored. These untracked files must accompany the candidate when it is eventually committed.
- Unsupported-codec routing was too broad: malformed container errors, resource failures and Opus internal decoder errors could select FFmpeg. Routing now recognizes explicit audited unsupported variants; malformed input, I/O, cancellation, deadlines and limits fail closed. AAC configuration checks distinguish valid unsupported profiles and explicit/implicit SBR metadata from malformed configurations.
- Standard Ogg Speex could disappear inside Symphonia's unsupported Ogg mapping. A bounded page validator now checks identification metadata, framing, CRC, sequence, continuation and completion before selecting compatibility. Real Speex, HE-AAC M4A, AC-3 and mapped surround Opus exercise the worker fallback.
- Container parsing could allocate from hostile packet lengths before the later sample limit. File reads now enforce cancellation/deadlines and header/packet read ceilings before progressive parser allocation. ADTS validation scans fixed headers and seeks over payload instead of buffering the file; reserved frequency indices are rejected.
- PDF page-tree recursion could occur before the existing post-parse object limit. Preflight now rejects recursive/indirect page trees, duplicate objects, revisions and unbudgeted stream/resource structures before Hayro parsing. A checked source-byte × viewport-pixel work ceiling also precedes rendering. Unsupported PDFs keep their original download and deterministic SVG.
- Animated WebP checked the RIFF limit only after encoding the entire animation. The writer now checks a 256 MiB output budget before each frame is published to its temporary output; failures preserve the original upload.
- Completed MKV transcodes could leave stale original URLs returning 404. Validated MP4/MKV URLs now redirect to the completed WebM, including the file-open race. Protected-board and path/symlink checks still precede redirect handling.
- A global test-only PDF renderer override could leak between concurrent tests. The override is now thread-local and guarded; production behavior is unchanged.
- Browser fixtures/assertions assumed obsolete MIME choices, optional local audio encoders and a no-JS mouse gesture that stalled Chromium. Tests now follow the stored media URL, reuse real bundled audio fixtures, assert exact intentional negative-upload diagnostics, and check no-JS resize behavior through CSS. Common-audio browser coverage adds ALAC and SILK/voice Opus.
- Added representative large transparent image, truncated image, malformed container, bounded-read, PDF vector/text and resource regressions. Refreshed the narrow lint-exception ledger, fixture provenance, setup version and stale FFprobe wording.

### Final validation

All local Rust checks used Rust/cargo 1.99.0. The following final checks passed after the production changes:

| Command / pass | Final result |
| --- | --- |
| `cargo fmt --all --check` | Passed |
| `cargo check --workspace --all-targets --all-features` | Passed |
| `cargo clippy --workspace --all-targets --all-features -- -D warnings -D clippy::all -D clippy::pedantic -D clippy::nursery -D clippy::cargo` | Passed |
| `cargo test --workspace --all-features` | 1,275 library + 1,275 application unit tests, 5 CLI integration tests, 1 UI fixture integration test and 1 doc test: **2,557 passed**, zero failures |
| `cargo tree`; `cargo tree --edges features` | Inspected; no new native multimedia requirement in the added dependency closures |
| `cargo deny --locked check` | Advisories, bans, licenses and sources passed; existing duplicate-version and `captcha` manifest-license warnings remain |
| `CHAN_FFMPEG_PATH=__rustchan_non_video_tools_unavailable__ CHAN_REQUIRE_FFMPEG=0 cargo test --workspace --all-features media::` | **100 + 100 media tests passed** with FFmpeg unavailable |
| Chromium + Chromium no-JS: upload validation, non-video media, media and phase-3 runtime | **14 passed, 4 intentional skips**; upload matrix covers 55 JS scenarios and 54 no-JS scenarios, with one JS-only scenario skipped; ten common audio formats complete jobs without FFmpeg |
| Chromium + Chromium no-JS: real media toolchain, auto-compression, opt-in upload regressions and phase-3 runtime | **14 passed, 8 intentional project/JS-only skips**; actual WebP video thumbnails, VP9/Opus WebM, redirects, playback markup and oversized-upload cleanup verified |
| Chromium + Chromium no-JS: affected PDF/media toolchain rerun after the final PDF work ceiling | **8 passed**, zero skips |
| `git diff --check` | Passed, including the final report edits |

The combined browser command was:

```sh
RUSTCHAN_AUDIT_OUTPUT=output/playwright/final-verification-browser-verified npx playwright test tests/e2e/upload-validation.spec.ts tests/e2e/non-video-media.spec.ts tests/e2e/media.spec.ts tests/e2e/phase3-media-runtime.spec.ts --project=chromium --project=chromium-nojs --workers=2
```

The real-video command was:

```sh
RUSTCHAN_UPLOAD_REGRESSION_E2E=1 RUSTCHAN_E2E_MEDIA_TOOLCHAIN=1 RUSTCHAN_E2E_FFMPEG_PATH=/tmp/rustchan-video-validation/ffmpeg-9.0.2/ffmpeg RUSTCHAN_AUDIT_OUTPUT=output/playwright/final-verification-real-media-verified npx playwright test tests/e2e/media-toolchain.spec.ts tests/e2e/auto-compress.spec.ts tests/e2e/upload-regressions.spec.ts tests/e2e/phase3-media-runtime.spec.ts --project=chromium --project=chromium-nojs --workers=2
```

The final affected browser rerun was:

```sh
RUSTCHAN_E2E_MEDIA_TOOLCHAIN=1 RUSTCHAN_E2E_FFMPEG_PATH=/tmp/rustchan-video-validation/ffmpeg-9.0.2/ffmpeg RUSTCHAN_AUDIT_OUTPUT=output/playwright/final-verification-pdf-budget npx playwright test tests/e2e/media-toolchain.spec.ts tests/e2e/media.spec.ts --project=chromium --project=chromium-nojs --workers=2
```

Final logs are under `/tmp/rustchan-final-verification/`; browser results are in the ignored `output/playwright/final-verification-*` directories. The temporary FFmpeg build includes libwebp/libvpx/libopus and did not replace the installed executable. A temporary Speex fixture encoder was built from the [official Speex release source](https://www.speex.org/downloads/); it is not an application dependency.

The retained `ffmpeg_thumbnail`, `build_vp9_transcode_args`, `vp9_encoding_profile`, `wait_for_ffmpeg_output`, `waveform_finalise` and `transcode_video_finalise` function bodies were compared with HEAD and remain unchanged. Nineteen real media fixtures were compared with FFprobe as an oracle, including multi-stream MP4, audio-only containers, M4A, WebM and MKV. FFprobe is not required for normal Rust probing.

### Unresolved compatibility and release limits

1. **In-band HE-AAC ADTS is a confirmed gap.** Remuxing the HE-AAC M4A fixture to ADTS preserves a valid 48 kHz, two-channel HE-AAC stream according to FFprobe. Symphonia 0.6.1 instead declares and decodes the 24 kHz AAC-LC core successfully, exposes no SBR profile/configuration through its public metadata, and does not return an unsupported error. The compatibility fallback is therefore not selected. Correct SBR reconstruction and waveform equivalence are not established. Header-signaled HE-AAC M4A now falls back correctly, but that does not repair this packet-signaled case. Accurate detection needs decoder support; a payload signature heuristic or routing all common AAC through FFmpeg would violate the intended routing policy.
2. **Speex coverage is deliberately limited to a validated single logical Ogg stream.** Chained/multiplexed Speex is rejected by the new validator. Other unvalidated AAC configurations and payload-level unsupported features also fail closed. These limits prevent a claim that every previously FFmpeg-supported upload remains equivalent.
3. **Docker build/runtime is unresolved.** Docker Desktop and the daemon were available. The actual `docker build --progress=plain --tag rustchan-final-verification:local .` attempt failed while resolving `docker.io/library/rust:1.99.0-bookworm`: `not found`. No builder step ran. The Linux production binary, runtime startup/health, non-root execution and in-container retained FFmpeg behavior remain unverified. An older Rust base image was not substituted and no image was pushed.
4. Image/HEIC codec calls remain cooperative at bounded operation boundaries; the wrapper cannot forcibly interrupt one library decode. Representative behavior and malformed-input/resource regressions passed, but this is not universal codec conformance or a formal bound on every decoder instruction. Hosted CI/release jobs were not run remotely.

### Release gates

| # | Gate | Assessment |
| --- | --- | --- |
| 1 | Common non-video media does not require FFmpeg | **Satisfied for the representative supported formats**, including ALAC and SILK/voice Opus |
| 2 | Video still works through FFmpeg | **Satisfied** by real thumbnails/transcoding/browser playback and unchanged argument builders |
| 3 | Approved uncommon-audio fallback works | **Partially satisfied**: AC-3, standard Speex, HE-AAC M4A and surround Opus pass; in-band HE-AAC ADTS remains uncovered |
| 4 | Malformed errors cannot incorrectly select compatibility fallback | **Satisfied for audited routing and regression cases**; explicit unsupported classification is separated from malformed/I/O/resource/cancellation errors |
| 5 | PDF fallback behaves intentionally | **Satisfied**: supported vector/text render; complex or unbudgeted inputs retain the original and receive SVG |
| 6 | No meaningful supported upload behavior was lost | **Not satisfied**: the AAC gap and Speex/other AAC limits prevent certification |
| 7 | Resource/security limits remain enforced | **Satisfied for audited wrappers and tested cases**, with cooperative library-decode limits disclosed above |
| 8 | Docker validates locally, or inability is explicitly reported | **Failure explicitly reported; build/runtime release validation remains unresolved** because the required base tag is unavailable |
| 9 | Rust 1.99 is consistent | **Satisfied** in active Cargo, CI, release, Docker and user/developer setup requirements; historical references are intentional |
| 10 | No unexplained working-tree changes | **Satisfied**: candidate changes reviewed and this pass's additions/fixes listed below |

The working tree is safe to inspect and review. It is **not certified for a production-ready migration commit or release** until the compatibility gaps and Docker validation are resolved. All changes remain uncommitted.

### Files changed during this final pass only

The broader candidate file list above includes earlier migration changes. This pass touched these 24 files:

- `.gitignore`
- `.github/ISSUE_TEMPLATE/bug_report.yml`
- `SETUP.md`
- `docs/non-video-media-migration.md`
- `docs/strict-lint-exceptions.tsv`
- `src/handlers/board/media.rs`
- `src/media/audio.rs`
- `src/media/images.rs`
- `src/media/images/tests.rs`
- `src/media/pdf.rs`
- `src/media/probe.rs`
- `src/media/probe/speex.rs` (new in this pass)
- `src/media/thumbnail.rs`
- `src/utils/files/storage.rs`
- `src/workers/mod.rs`
- `tests/e2e/non-video-media.spec.ts`
- `tests/e2e/phase3-media-runtime.spec.ts`
- `tests/e2e/upload-regressions.spec.ts`
- `tests/fixtures/media/README.md`
- `tests/fixtures/media/tone-alac.m4a` (new in this pass)
- `tests/fixtures/media/multiple-streams.mp4` (new in this pass)
- `tests/fixtures/media/he-aac.m4a` (new in this pass)
- `tests/fixtures/media/tone.spx` (new in this pass)
- `tests/fixtures/media/text.pdf` (new in this pass)

### Final `git status --short`

```text
 M .github/ISSUE_TEMPLATE/bug_report.yml
 M .github/workflows/ci.yml
 M .github/workflows/release.yml
 M .gitignore
 M Cargo.lock
 M Cargo.toml
 M Dockerfile
 M README.md
 M SETUP.md
 M docs/admin-redesign-coverage.md
 M docs/containers.md
 M docs/strict-lint-exceptions.tsv
 M src/banner.rs
 M src/config.rs
 M src/config/admin.rs
 M src/config/admin/runtime.rs
 M src/config/template.rs
 M src/detect.rs
 M src/handlers/admin/mod.rs
 M src/handlers/board/access_preferences.rs
 M src/handlers/board/create_thread.rs
 M src/handlers/board/media.rs
 M src/handlers/mod.rs
 M src/handlers/posting.rs
 M src/handlers/setup.rs
 M src/handlers/thread.rs
 M src/media/convert.rs
 M src/media/ffmpeg.rs
 M src/media/mod.rs
 M src/media/thumbnail.rs
 M src/middleware/state.rs
 M src/pending_fs.rs
 M src/server/server.rs
 M src/templates/admin.rs
 M src/templates/admin/maintenance.rs
 M src/templates/admin/network.rs
 M src/templates/admin/site_health.rs
 M src/test_fixtures.rs
 M src/utils/files/storage.rs
 M src/workers/mod.rs
 M tests/cli_runtime.rs
 M tests/e2e/README.md
 M tests/e2e/helpers.ts
 M tests/e2e/media-toolchain-check.mjs
 M tests/e2e/media-toolchain.spec.ts
 M tests/e2e/phase3-media-runtime.spec.ts
 M tests/e2e/upload-regressions.spec.ts
 M tests/e2e/upload-validation.spec.ts
?? docs/non-video-media-migration.md
?? src/media/audio.rs
?? src/media/heif.rs
?? src/media/images.rs
?? src/media/images/
?? src/media/pdf.rs
?? src/media/probe.rs
?? src/media/probe/
?? tests/e2e/non-video-media.spec.ts
?? tests/fixtures/
```
