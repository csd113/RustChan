# Media capabilities after the Rust follow-up

This is the current capability audit for `ffmpeg-replace-with-rust`, superseding the historical conclusions in [the original migration report](non-video-media-migration.md). Full removal of FFmpeg except WebM transcoding is **not achieved**. The remaining audio/video compatibility paths are explicit and retained to avoid discarding functionality. No OxiMedia or OxideAV crate is used; the user declined OxideAV.

## Old behavior and current paths

“Old” means the pre-migration call sites, with the required executable and codec available. It is not a claim that every file of a named format worked. Unsupported previews preserve the original valid upload; corrupt uploads still follow the existing validation/error contract.

| Operation / input | Old backend and behavior | Current backend | Verified coverage / remaining limit |
| --- | --- | --- | --- |
| JPEG, PNG, BMP, TIFF, ICO, static WebP | FFmpeg conversion or existing image fallback; resizing, orientation, WebP/PNG size choice | `image`, EXIF orientation, lossless WebP | Decodable output, exact lossless pixels, alpha, orientation, dimensions, metadata stripping, smaller-original PNG policy; hostile/truncated and large transparent images |
| Animated GIF and animated WebP; banners | FFmpeg and existing first-frame fallbacks | Rust compositor, resizer and bounded animated WebP writer | Every composited frame, partial rectangles, disposal, transparency, delays and loop count. GIF 65,535 repeats require 65,536 plays, beyond WebP's finite u16 loop field: preserve the complete original GIF, with its actual MIME and extension, rather than alter its animation |
| HEIC/HEIF still images | External/native image decoder where installed, or image fallback | `heic-rs`, no native HEVC library | Real tiled HEIC, container rotation, auxiliary alpha and iPhone primary SDR picture; all EXIF/ICC/HDR combinations are not claimed. HDR gain-map rendering is outside the established thumbnail behavior |
| Image first-frame thumbnails | FFmpeg or image fallback | Rust image/animation/HEIC decoding | Bounded dimensions, composited first frame, alpha, orientation and original retention |
| MP4/M4A, Matroska/WebM, Ogg and common audio classification / primary codec | FFprobe or bounded FFmpeg stream maps | Symphonia container parsers; small bounded Ogg validators | Content signatures override misleading suffixes, audio-only WebM and multiple tracks, codec names, CRC/sequence/EOS, truncated/ambiguous rejection. Unsupported audio-container mappings still use the explicit FFmpeg probe compatibility path |
| PCM, MP1/2/3, FLAC, Vorbis, ALAC, AAC-LC waveforms | FFmpeg `showwavespic` | Symphonia PCM with streaming mean-absolute channel envelope | WAV/MP3/FLAC/Ogg/ADTS/M4A/Matroska/WebM fixtures, silence, tiny/long/out-of-phase/surround audio, finite sample and packet bounds, cancellation/deadline and atomic publication |
| AAC-LC movie edit lists | FFmpeg applied encoder delay, trim and empty edits | Bounded Rust MP4 metadata walker and streaming presentation timeline | M4A fixture: **5,760** presented 48 kHz frames, versus 7,168 raw decoded frames; independent stereo PCM envelope error ≤0.002. Unit-rate ordered forward ranges and empty edits are supported. Valid dwell, rate changes, repeated/backward edits and edited MP4 Opus use explicit compatibility; every edit is validated before compatibility selection, including malformed entries after a valid unsupported edit |
| Opus voice, stereo and surround | FFmpeg/libopus; earlier Rust surround fallback | `opus-pure` plus Rust multistream framing/mapping | Ogg families 0, 1 and single-stream-container family 255; SILK/CELT, self-delimited VBR/CBR/padding, gain, pre-skip and end trim. Three six-channel golden clips have **11,520** frames and per-channel PCM error ≤0.00004; channel-mean envelope compared independently. Public family 255 probing and decoding are tested |
| Other Opus mappings / containers | FFmpeg accepted mappings supported by its build | Audited compatibility where the existing demuxer exposes the unsupported mapping | Family 2 packet reconstruction is understood by the adapter, but its public Ogg mapping is not implemented. Chained/multiplexed family 255 containers require independent presentation state and are explicitly rejected. No claim of complete arbitrary Opus-container parity |
| HE-AAC / SBR / parametric stereo | FFmpeg full reconstruction where supported | **FFmpeg compatibility retained** | Explicit and sync-extension configurations select compatibility. Patched existing AAC decoder also catches in-band SBR after validating the complete raw-data block; it never publishes the former half-rate AAC-LC-only result. Truncated SBR payloads do not select compatibility |
| Speex NB/WB/UWB waveforms | FFmpeg Speex decode | **FFmpeg compatibility retained**; Rust Ogg framing/metadata validation | Single, all-Speex chained, all-Speex multiplexed and chained multiplexed fixture containers accepted, every logical stream validated (≤64 streams). Mixed-codec containers are rejected. Independent FFmpeg itself reports two decode/timestamp errors on the chained fixture; complete chained waveform parity is not claimed |
| AC-3 and other uncovered audio codecs | FFmpeg codecs installed on the host | **FFmpeg compatibility retained** | AC-3 fixture and precise unsupported-error routing. Generic malformed packet/container, resource, I/O, cancellation and decoder-internal errors cannot trigger fallback |
| Small vector/text PDF first page | `pdftoppm`, `mutool` or `qlmanage` where installed, else SVG | Hayro in Rust, embedded standard fonts | Original vector/text fixtures and a Flate-compressed vector fixture render correctly; compressed old-Poppler output independently confirms the former capability. Aggregate source/expanded content ≤2 MiB, flat page tree ≤4,096 objects, syntax/numeric/depth/work limits, decompression checksum/trailing-data validation |
| Inline-image, nested-resource, embedded-font, encrypted or other complex PDFs | Some documents rendered by an available old external renderer; otherwise SVG | **SVG preview; original PDF retained** | A valid inline RGB image fixture is verified separately with old Poppler. Current gate rejects inline image operators, decompression bombs, recursive/indirect trees and unbudgeted resources before Hayro. This is a remaining preview capability regression, not full PDF parity |
| Video first-frame thumbnail, including MP4/MKV/WebM | FFmpeg video decoding and WebP encoding | **FFmpeg retained** | Standalone H.264 candidate decodes ordinary High 8-bit CABAC, but genuine High 10-bit and High 4:4:4 fixtures fail while the old decoder succeeds. Removing the path would lose existing thumbnail capability. No broad video framework was added |
| Video → VP9/Opus **WebM** | FFmpeg/libvpx/libopus | **FFmpeg retained, intended exception** | Existing arguments, color handling, timeout/cancellation, atomic durable jobs, generated container validation and stale-original redirects |

## Exact subprocess boundary

There is no production `ffprobe` command and no external image/PDF renderer command. The runtime image removes `/usr/bin/ffprobe`. FFmpeg commands remain at:

1. `src/media/ffmpeg.rs::ffmpeg_thumbnail`, called only by the **video** thumbnail branch.
2. `src/workers/mod.rs::render_waveform_with_compatibility`, using the existing bounded waveform command only for audited unsupported audio capabilities.
3. `src/media/ffmpeg.rs::probe_uncovered_audio`, two bounded zero-frame audio/video maps after an explicitly unsupported audio container result.
4. `src/workers/mod.rs::transcode_video` and `build_vp9_transcode_args`, for VP9/Opus **WebM output**.
5. Startup/admin executable `-version` and `-encoders` capability checks for that retained backend.

`src/media/process.rs` bounds subprocess output, deadlines and process-group cleanup; `/bin/kill` is process cleanup, not codec processing. Retaining calls 1–3 conflicts with the requested WebM-only boundary. They remain visible because removing them under the current constraints would drop working capabilities.

## Dependency and candidate review

- Replace `opus-decoder` 0.1.1 with **`opus-pure` 0.2.2**, BSD-3-Clause, MSRV 1.88, no dependencies, build script, C or FFI. RustChan's MSRV remains **1.99**.
- Vendor the already-used **`symphonia-codec-aac` 0.6.1**, MPL-2.0, for one explicit SBR error after full block parsing. No decoder reconstruction or dependency closure is added. See [patch provenance](../vendor/symphonia-codec-aac/RUSTCHAN-PATCH.md); upstream MSRV 1.85 is below the application requirement.
- PDF expansion reuses existing `flate2` and its Rust backend; no new PDF dependency.
- `fdk-aac-rust` 0.2.3 was evaluated outside the application: strict decoding rejects the legitimate in-band fixture with `NonZeroTrailingBits`; its standard API returns 24 kHz core-sized PCM with unusable comparison error. It was not adopted.
- [`rusty_aac` 0.5.0](https://docs.rs/crate/rusty_aac/0.5.0) explicitly provides core-only HE-AAC, without SBR/PS reconstruction. Configuration recognition cannot establish PCM parity.
- [`speexdsp-rs`](https://github.com/rust-av/speexdsp-rs) is DSP/resampling, not a Speex codec. Available native Speex bindings violate the requested implementation boundary; OxideAV was declined. No suitable alternative decoder reached validation.
- `rusty_h264-decoder` 0.16.0 was tested on 8-bit High, High 10-bit and High 4:4:4 synthetic streams. Ordinary High/CABAC works, so stale claims that it cannot decode CABAC were discarded. The other two fail on bit depth/chroma support. A partial H.264 path would not replace all existing video-thumbnail codecs/profiles.
- Hayro lacks a general allocation/work or cooperative cancellation interface for arbitrary PDF programs ([upstream issue](https://github.com/LaurenzV/hayro/issues/1052)). The bounded subset is intentional; enabling arbitrary resource graphs would not satisfy the input/resource contract.

## Validation evidence

Validated on 2026-10-02 with native Apple Silicon Rust **1.99.0** (`b940084d7`, 2026-09-28) and an actual Linux arm64 Docker build using the official `rust:1.99.0-bookworm` image. The previously reported unavailable Docker tag is no longer a blocker. Build parallelism was limited to two Cargo jobs per environment.

```sh
cargo fmt --all --check
cargo check --locked --workspace --all-targets --all-features
cargo clippy --locked --workspace --all-targets --all-features
CHAN_FFMPEG_PATH=/nonexistent/rustchan-validation-ffmpeg CHAN_REQUIRE_FFMPEG=0 \
  cargo test --locked --workspace --all-features -- --test-threads=4
cargo doc --locked --workspace --all-features --no-deps
cargo deny --locked check
cargo build --release --locked --workspace --all-features
docker build --build-arg CARGO_BUILD_JOBS=2 -t rustchan-media-validation:local .
```

- The integrated no-tools workspace run passed **2,619** tests: **1,306** in each library/CLI test binary, five CLI integration tests, one UI fixture test and one rustdoc test. Seven Python update-packaging tests also passed. Formatting, check, canonical strict Clippy, documentation, locked dependency policy and release builds passed.
- A process-spy executable recorded **zero invocations** for all 16 Rust audio tests. The worker routing test recorded exactly seven `-version` capability checks for the seven uncovered audio fixtures, and no codec command. Repeating that test with the installed FFmpeg build containing Speex passed all seven real compatibility waveform paths. A separately built validation FFmpeg without Speex failed this check, demonstrating that fallback still requires the actual codec rather than merely an executable.
- Local Chromium and Chromium without JavaScript passed the media/upload/error suites: **14 tests passed, four intentional JavaScript-only skips**, including 55 JavaScript and 54 no-JavaScript upload-validation scenarios and 14 common audio formats per browser context. The separate restart suite passed **four** tests, and the opt-in real FFmpeg toolchain suite passed **one** test. Browser tests remain local; no hosted browser Actions were introduced.
- The final Linux runtime ran as UID **10001**, with `ffmpeg` and `ffprobe` physically absent from executable search paths and the saved FFmpeg file mode `000`. **42** real upload → processing → preview → download journeys passed across JavaScript and no-JavaScript contexts. These included all 14 audio fixtures, rotated/alpha HEIC, simple/compressed/unsupported PDF and both converted and maximum-loop GIFs. Original unsupported PDFs and the unrepresentable-loop GIF were retained byte for byte. Independent Pillow checks read generated previews and every animation frame; no FFmpeg participates in these production paths.
- Separately enabled Docker FFmpeg passed two real MP4 → VP9/Opus WebM browser journeys, with WebP thumbnails and stale-original redirects where the conversion raced the first response. Two actual Docker restart-policy journeys verified replacement process identities, readiness, saved settings application, JavaScript reconnect and no-JavaScript recovery. The runtime uses Debian FFmpeg **5.1.9** for the explicitly retained compatibility/video backend.
- Synthetic fixtures and independent PCM reference files are checked in under `tests/fixtures/media`. Private browser data, candidate experiments, screenshots, logs and preserved runnable binaries are kept outside `target` and outside the Git deliverable. The final handoff gives their location and the exact commit, image and required-CI results.

Native systemd restart operation remains unverified on this macOS host; the previous owner's managed-Linux tests used mocked service control. Windows and configured Linux/macOS build coverage depends on the required GitHub platform checks and is reported separately. Full HE-AAC/Speex reconstruction, arbitrary video thumbnails and complex PDF rendering remain the precise functional limits above; a successful test run does not establish the requested WebM-only FFmpeg boundary.
